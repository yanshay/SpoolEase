use core::{cell::RefCell, error::Error, net::SocketAddr};

use alloc::{
    boxed::Box,
    ffi::CString,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use edge_http::io::client::Connection;
use edge_nal_embassy::{Tcp, TcpBuffers};
use embassy_net::{IpAddress, Ipv4Address, tcp::TcpSocket};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::Channel,
    pubsub::{PubSubChannel, Subscriber},
};
use embassy_time::{Duration, Instant, with_deadline};
use embedded_io_async::Read;
use esp_mbedtls::{Certificate, ClientSessionConfig, X509};
use framework::{debug, error, info, prelude::Framework};
use serde::{Deserialize, Serialize};
use url::{Position, Url};

use crate::{
    gcode_analysis::{FilamentUsageEntry, GcodeFilamentCalc},
    my_ftp::{Error as MyFtpError, MyFtps},
    settings::{
        GCODE_ANALYSIS_PRINTER_FTP_RETRY_INTERVAL_SECS,
        GCODE_ANALYSIS_PRINTER_FTP_RETRY_TIMEOUT_SECS,
    },
    threemf_extractor::{FeedStatus, ThreemfExtractor},
};

const EXTRA_DEBUG: bool = false;

macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

#[derive(Debug, PartialEq, Default, Clone)]
pub struct FilamentUsage {
    pub data: Vec<FilamentUsageEntry>,
}

impl FilamentUsage {
    pub fn new(data: Vec<FilamentUsageEntry>) -> Self {
        Self { data }
    }

    // this function use base64 for weight_g - shorter and more accurate
    pub fn from_csv(csv: &str) -> Result<FilamentUsage, String> {
        let num_records = csv.lines().count();
        let mut data = Vec::<FilamentUsageEntry>::with_capacity(num_records);
        let mut csv_parser = serde_csv_core::Reader::<16>::new(); // 16 is max field size
        for line in csv.lines() {
            match csv_parser.deserialize(line.as_bytes()) {
                Ok(v) => {
                    data.push(v.0);
                }
                Err(err) => {
                    error!("Internal error deserializing FilamentUsageEntry : {err}");
                    return Err(format!(
                        "Internal error deserializing FilamentUsageEntry : {err}"
                    ));
                }
            }
        }
        Ok(FilamentUsage { data })
    }

    // this function use float for weight_g
    pub fn _from_csv(csv: &str) -> FilamentUsage {
        let mut filament_usage = FilamentUsage { data: Vec::new() };
        filament_usage._load_csv(csv);
        filament_usage
    }
    fn _load_csv(&mut self, csv: &str) {
        self.data.clear();
        let num_of_lines = csv.lines().count();
        self.data.reserve_exact(num_of_lines);
        for line in csv.lines() {
            let mut split = line.split(',');
            if let (Some(layer), Some(gcode_filament_id), Some(weight_g)) =
                (split.next(), split.next(), split.next())
                && let (Ok(layer), Ok(gcode_filament_id), Ok(weight_g)) = (
                    layer.parse::<i32>(),
                    gcode_filament_id.parse::<i32>(),
                    weight_g.parse::<f32>(),
                )
            {
                self.data.push(FilamentUsageEntry {
                    layer,
                    gcode_filament_id,
                    weight_g,
                })
            }
        }
    }

    pub fn to_csv(&self) -> Result<String, String> {
        const FILAMENTUSAGE_ENTRY_MAX_LEN: usize = 5 + 6 + 2 + 2 + 1 + 4; // layer i32, filament_id i32, f32 as base64, two commas, and EOL + spare
        let mut csv_writer = serde_csv_core::Writer::new();
        let mut buffer = alloc::vec![0;FILAMENTUSAGE_ENTRY_MAX_LEN];
        let csv = String::with_capacity(self.data.len() * FILAMENTUSAGE_ENTRY_MAX_LEN);
        let results: Result<String, String> = self.data.iter().try_fold(csv, |mut acc, v| {
            let res = csv_writer.serialize(v, buffer.as_mut_slice());
            match res {
                Ok(len) => {
                    let csv_row_res = core::str::from_utf8(&buffer[..len]);
                    match csv_row_res {
                        Ok(csv_row) => acc.push_str(csv_row),
                        Err(err) => {
                            let error = format!("Serialization of FilamentUsageEntry to a csv row didn't end up valid utf8: {err}");
                            return Err(error);
                        }
                    }
                }
                Err(err) => {
                    let error = format!("Error serializing FilamentUsageEntry to a csv row : {err}");
                    return Err(error);
                }
            }
            Ok(acc)
        });
        results
    }
}

pub type GcodeAnalysisRequestChannel = Channel<NoopRawMutex, GcodeAnalysisRequest, 5>;

pub type GcodeAnalysisNotificationChannel =
    PubSubChannel<NoopRawMutex, GcodeAnalysisNotification, 5, 5, 1>;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum Fetch3mf {
    #[default]
    CloudHttp,
    PrinterFtp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum GcodeAnalysisNotification {
    Cancel { job_number: i32 },
}

type GcodeAnalysisNotificationSubscriber<'a> =
    Subscriber<'a, NoopRawMutex, GcodeAnalysisNotification, 5, 5, 1>;

#[derive(Debug, Serialize, Deserialize)]
// This is serialized between scale/console, if modified consider backwards compatibility
pub struct GcodeAnalysisRequest {
    pub fetch_3mf: Fetch3mf,
    pub ip: Ipv4Address,
    pub serial: String,
    pub access_code: String,
    pub threemf_ftp_filename: String, // not including full path, just fixed name based on printer model
    pub printer_index: usize,
    pub printer_number: usize,
    pub job_number: i32,
    pub threemf_url: String,
    pub gcode_filename_in_3mf: String,
    pub ftp_memory_save: bool,
    pub printer_selector_name: String,
}

pub trait GcodeAnalyzerObserver {
    fn on_gcode_analysis(
        &mut self,
        job_number: i32,
        printer_index: usize,
        filament_usage: FilamentUsage,
    );
    fn on_canceled(&mut self, job_number: i32, printer_index: usize);
    fn on_failed(&mut self, job_number: i32, printer_index: usize);
    fn on_completed(&mut self, job_number: i32, printer_index: usize);
}

enum FetchSubtaskResult {
    Failed,
    Canceled,
    Ok,
}

enum PrinterFtpAttemptResult {
    Failed,
    FileNotFound,
    Canceled,
    Ok,
}

pub async fn fetch_gcode_analysis_once_task(
    framework: Rc<RefCell<Framework>>,
    notifications_channel: Rc<GcodeAnalysisNotificationChannel>,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
    gcode_analysis_request: GcodeAnalysisRequest,
) {
    info!("Started one-shot fetch_gcode_analysis task");
    run_gcode_analysis_request(
        framework,
        gcode_analysis_request,
        notifications_channel,
        observer,
    )
    .await;
}

async fn run_gcode_analysis_request(
    framework: Rc<RefCell<Framework>>,
    gcode_analysis_request: GcodeAnalysisRequest,
    notifications_channel: Rc<GcodeAnalysisNotificationChannel>,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
) {
    let job_number = gcode_analysis_request.job_number;
    let printer_index = gcode_analysis_request.printer_index;
    let printer_log_id = gcode_analysis_request.printer_number;

    info!("[{printer_log_id}] Received gcode analysis request");
    let fetch_res = match gcode_analysis_request.fetch_3mf {
        Fetch3mf::CloudHttp
            if !(gcode_analysis_request.threemf_url.starts_with("file://")
                || gcode_analysis_request.threemf_url.starts_with("ftp://")
                || gcode_analysis_request.threemf_url.starts_with("brtc://")) =>
        {
            fetch_gcode_analysis_task_cloud_http(
                framework.clone(),
                gcode_analysis_request,
                observer.clone(),
                notifications_channel.clone(),
            )
            .await
        }
        _ => {
            if gcode_analysis_request.fetch_3mf == Fetch3mf::CloudHttp {
                info!(
                    "[{printer_log_id}] Configuration is to fetch 3mf file over HTTP, but file is from sdcard, so using to ftp"
                );
            }
            fetch_gcode_analysis_task_printer_ftp(
                framework.clone(),
                gcode_analysis_request,
                observer.clone(),
                notifications_channel.clone(),
            )
            .await
        }
    };

    match fetch_res {
        FetchSubtaskResult::Failed => observer
            .upgrade()
            .unwrap()
            .borrow_mut()
            .on_failed(job_number, printer_index),
        FetchSubtaskResult::Canceled => observer
            .upgrade()
            .unwrap()
            .borrow_mut()
            .on_canceled(job_number, printer_index),
        FetchSubtaskResult::Ok => observer
            .upgrade()
            .unwrap()
            .borrow_mut()
            .on_completed(job_number, printer_index),
    }
}

pub async fn fetch_gcode_analysis_task(
    framework: Rc<RefCell<Framework>>,
    requests_channel: Rc<GcodeAnalysisRequestChannel>,
    notifications_channel: Rc<GcodeAnalysisNotificationChannel>,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
    mut launch_gcode_analysis_request: Option<GcodeAnalysisRequest>,
) {
    info!("Started fetch_gcode_analysis task");
    let receiver = requests_channel.receiver();
    loop {
        info!("Gcode Analysis Task: Waiting for analysis request");
        let gcode_analysis_request = if launch_gcode_analysis_request.is_some() {
            // if launched with an analysis request start with it (consume it so next time will receive)
            launch_gcode_analysis_request.take().unwrap()
        } else {
            receiver.receive().await
        };
        run_gcode_analysis_request(
            framework.clone(),
            gcode_analysis_request,
            notifications_channel.clone(),
            observer.clone(),
        )
        .await;
    }
}

fn is_cancel_for_job(notification: GcodeAnalysisNotification, job_number: i32) -> bool {
    match notification {
        GcodeAnalysisNotification::Cancel {
            job_number: canceled_job_number,
        } => canceled_job_number == job_number,
    }
}

fn received_cancel_for_job(
    notifications: &mut GcodeAnalysisNotificationSubscriber<'_>,
    job_number: i32,
) -> bool {
    notifications
        .try_next_message_pure()
        .is_some_and(|notification| is_cancel_for_job(notification, job_number))
}

async fn wait_for_retry_or_cancel(
    notifications: &mut GcodeAnalysisNotificationSubscriber<'_>,
    job_number: i32,
    retry_at: Instant,
) -> bool {
    loop {
        if Instant::now() >= retry_at {
            return false;
        }

        match with_deadline(retry_at, notifications.next_message_pure()).await {
            Ok(notification) => {
                if is_cancel_for_job(notification, job_number) {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
}

fn is_ftp_file_not_found(err: &MyFtpError) -> bool {
    matches!(err, MyFtpError::Ftp { response } if response.code == 550)
}

async fn fetch_gcode_analysis_task_printer_ftp(
    framework: Rc<RefCell<Framework>>,
    gcode_analysis_request: GcodeAnalysisRequest,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
    notifications_channel: Rc<GcodeAnalysisNotificationChannel>,
) -> FetchSubtaskResult {
    let job_number = gcode_analysis_request.job_number;
    let printer_log_id = gcode_analysis_request.printer_number;
    let mut notifications = match notifications_channel.subscriber() {
        Ok(subscriber) => subscriber,
        Err(err) => {
            error!("[{printer_log_id}] Error getting notification subscriber : {err:?}");
            return FetchSubtaskResult::Failed;
        }
    };

    let retry_interval_secs = GCODE_ANALYSIS_PRINTER_FTP_RETRY_INTERVAL_SECS;
    let retry_timeout_secs = GCODE_ANALYSIS_PRINTER_FTP_RETRY_TIMEOUT_SECS;
    let retry_started_at = Instant::now();
    let retry_timeout = if retry_timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(retry_timeout_secs))
    };
    let retry_timeout_at = retry_timeout.map(|timeout| retry_started_at + timeout);
    let mut attempt = 1_u64;

    loop {
        match fetch_gcode_analysis_task_printer_ftp_attempt(
            framework.clone(),
            &gcode_analysis_request,
            observer.clone(),
            &mut notifications,
            attempt,
        )
        .await
        {
            PrinterFtpAttemptResult::Ok => return FetchSubtaskResult::Ok,
            PrinterFtpAttemptResult::Canceled => return FetchSubtaskResult::Canceled,
            PrinterFtpAttemptResult::Failed => return FetchSubtaskResult::Failed,
            PrinterFtpAttemptResult::FileNotFound => {
                if retry_interval_secs == 0 {
                    error!(
                        "[{printer_log_id}] 3mf(ftp) file not found on attempt {attempt}; retry interval is 0, there will be no more retries; failed to find the 3mf file {}",
                        gcode_analysis_request.threemf_url
                    );
                    return FetchSubtaskResult::Failed;
                }

                if let Some(timeout) = retry_timeout
                    && retry_started_at.elapsed() >= timeout
                {
                    error!(
                        "[{printer_log_id}] 3mf(ftp) retry timeout reached after {} seconds; there will be no more retries; failed to find the 3mf file {}",
                        retry_started_at.elapsed().as_secs(),
                        gcode_analysis_request.threemf_url
                    );
                    return FetchSubtaskResult::Failed;
                }

                let retry_at = Instant::now() + Duration::from_secs(retry_interval_secs);
                if let Some(timeout_at) = retry_timeout_at
                    && retry_at > timeout_at
                {
                    error!(
                        "[{printer_log_id}] 3mf(ftp) file not found on attempt {attempt}; next retry would exceed the configured retry timeout of {retry_timeout_secs} seconds, there will be no more retries; failed to find the 3mf file {}",
                        gcode_analysis_request.threemf_url
                    );
                    return FetchSubtaskResult::Failed;
                }

                info!(
                    "[{printer_log_id}] 3mf(ftp) file not found on attempt {attempt}; will retry in {retry_interval_secs} seconds"
                );
                if wait_for_retry_or_cancel(&mut notifications, job_number, retry_at).await {
                    info!(
                        "[{printer_log_id}] Gcode analysis job {job_number} canceled while waiting to retry 3mf(ftp) fetch"
                    );
                    return FetchSubtaskResult::Canceled;
                }
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn fetch_gcode_analysis_task_printer_ftp_attempt(
    framework: Rc<RefCell<Framework>>,
    gcode_analysis_request: &GcodeAnalysisRequest,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
    notifications: &mut GcodeAnalysisNotificationSubscriber<'_>,
    attempt: u64,
) -> PrinterFtpAttemptResult {
    let job_number = gcode_analysis_request.job_number;
    let printer_index = gcode_analysis_request.printer_index;
    let printer_log_id = gcode_analysis_request.printer_number;
    let stack = framework.borrow().stack;
    let tls = framework.borrow().tls;
    let threemf_url = &gcode_analysis_request.threemf_url;
    let ip = gcode_analysis_request.ip;
    let serial = &gcode_analysis_request.serial;
    let access_code = &gcode_analysis_request.access_code;

    // let plate_idx = gcode_analysis_request.plate_idx;
    let gcode_filename_in_3mf = &gcode_analysis_request.gcode_filename_in_3mf;

    let mut data_rx_buffer = alloc::vec![0u8;16384];
    let mut data_tx_buffer = alloc::vec![0u8;1024];
    let mut data_socket = TcpSocket::new(stack, &mut data_rx_buffer, &mut data_tx_buffer);
    data_socket.set_timeout(Some(Duration::from_secs(10)));

    let mut control_rx_buffer = alloc::vec![0u8;4096];
    let mut control_tx_buffer = alloc::vec![0u8;1024];
    let mut control_socket = TcpSocket::new(stack, &mut control_rx_buffer, &mut control_tx_buffer);
    control_socket.set_timeout(Some(Duration::from_secs(10)));

    const VSFTPD: bool = false; // for debugging with local vsftpd

    let debug = gcode_analysis_request.printer_selector_name.to_lowercase() == "simulator";

    let ftp_endpoint = if VSFTPD {
        (Ipv4Address::new(192, 168, 10, 86), 990)
    } else if debug {
        (Ipv4Address::new(192, 168, 10, 78), 990)
    } else {
        (ip, 990)
    };

    let server_name = if !VSFTPD {
        CString::new(serial.as_str()).unwrap()
    } else {
        CString::new("vsftpd").unwrap()
    };

    #[allow(clippy::if_same_then_else)]
    let certificates = if !VSFTPD {
        None
        // Some(
        //     Certificate::new(X509::PEM(
        //         core::ffi::CStr::from_bytes_with_nul(
        //             concat!(include_str!(../../console/src/certs/bambulab.pem"), "\0").as_bytes(),
        //         )
        //         .unwrap(),
        //     ))
        //     .unwrap(),
        // )
    } else {
        None
        // Some(
        //     Certificate::new(X509::PEM(
        //         core::ffi::CStr::from_bytes_with_nul(
        //             concat!(include_str!("./certs/vmware-vsftpd.pem"), "\0").as_bytes(),
        //         )
        //         .unwrap(),
        //     ))
        //     .unwrap(),
        // )
    };

    let mut ftps = MyFtps::new(
        control_socket,
        tls,
        ftp_endpoint,
        server_name.as_c_str(),
        certificates,
    );

    if received_cancel_for_job(notifications, job_number) {
        return PrinterFtpAttemptResult::Canceled;
    }

    info!("[{printer_log_id}] Connecting to printer ftp for 3mf fetch attempt {attempt}");
    match ftps.connect().await {
        Ok(_) => {
            info!("[{printer_log_id}] Connected to printer ftp for 3mf fetch attempt {attempt}");
        }
        Err(err) => {
            error!(
                "[{printer_log_id}] Error connecting to printer ftp on 3mf fetch attempt {attempt} : {err:?}"
            );
            return PrinterFtpAttemptResult::Failed;
        }
    }

    if received_cancel_for_job(notifications, job_number) {
        return PrinterFtpAttemptResult::Canceled;
    }

    let username = if !VSFTPD { "bblp" } else { "ftpuser" };
    let password = if !VSFTPD {
        access_code.as_str()
    } else {
        "ftppassword"
    };

    match ftps.login(username, password).await {
        Ok(success) => {
            if success {
                info!(
                    "[{printer_log_id}] Login to printer ftp succeeded for 3mf fetch attempt {attempt}"
                );
            } else {
                error!(
                    "[{printer_log_id}] Login to printer ftp failed on 3mf fetch attempt {attempt}"
                );
                return PrinterFtpAttemptResult::Failed;
            }
        }
        Err(err) => {
            error!(
                "[{printer_log_id}] Error in login to printer ftp on 3mf fetch attempt {attempt}: {err:?}"
            );
            return PrinterFtpAttemptResult::Failed;
        }
    }

    if received_cancel_for_job(notifications, job_number) {
        return PrinterFtpAttemptResult::Canceled;
    }

    // it looks like in the gcode file name (not in the bbl file name) bambu uses for gcode filename the text until "." in case there is such

    let mut threemf_filenames = if let Some(filename) = threemf_url.strip_prefix("file:///sdcard") {
        // seen on X1C
        // file:///sdcard/Skadis_Storage_Box_Scale_Small_Plate 1.gcode.3mf
        // file is in the ftp root
        alloc::vec![filename.to_string()]
    } else if let Some(filename) = threemf_url.strip_prefix("file:///mnt/sdcard") {
        // seen on X1C when printing from console
        // file:///mnt/sdcard/80_92_120_140mm_Fan_Dust_Filter.gcode.3mf
        // the replacement is since such case was witnessed on x1c when printing from console
        // file is in the ftp root
        alloc::vec![filename.replace("%25", "%").to_string()]
    } else if let Some(filename) = threemf_url.strip_prefix("ftp:/") {
        // ftp://Cable_Organizer_Cable_Clip.gcode.3mf
        // file is in the ftp root
        alloc::vec![filename.to_string()]
    } else if let Some(filename) = threemf_url.strip_prefix("brtc://emmc") {
        // seen on P2S when printing in DEV mode
        // brtc://emmc/ftp.gcode.3mf
        alloc::vec![format!("/cache{filename}")]
    } else if let Some(filename) = threemf_url.strip_prefix("file:///media/usb0") {
        // seen on H2D when doing a reprint
        // file:///media/usb0/Cube.gcode.3mf
        alloc::vec![format!("{filename}")]
    } else {
        // this is the case where we use the subtask_name field from the mqtt
        // after some chars were fixed (in view_model) based on the printer type
        // not nice to do it there, but didn't want to propgate printer type here now.

        // one example for such case is when file is sent over http but setting is to retrieve using ftp
        alloc::vec![
            format!("/{}.gcode.3mf", gcode_analysis_request.threemf_ftp_filename),
            format!("/{}.3mf", gcode_analysis_request.threemf_ftp_filename),
            format!(
                "/cache/{}.gcode.3mf",
                gcode_analysis_request.threemf_ftp_filename
            ),
            format!("/cache/{}.3mf", gcode_analysis_request.threemf_ftp_filename),
        ]
    };

    if VSFTPD {
        threemf_filenames.clear();
        threemf_filenames.push("Cube + Cube.3mf".to_string());
    }

    let mut buf = alloc::vec![0;16384];
    let mut gcode_calc = GcodeFilamentCalc::new();
    let mut total_read = 0;
    let mut threemf_extractor = Box::new(ThreemfExtractor::new(gcode_filename_in_3mf, 16384));
    info!(
        "[{printer_log_id}] Fetching 3mf(ftp) attempt {attempt} - first of: {threemf_filenames:?} and extracting {gcode_filename_in_3mf}"
    );
    match ftps
        .start_retrieve_first_of(
            threemf_filenames.as_slice(),
            data_socket,
            if !VSFTPD {
                gcode_analysis_request.ftp_memory_save
            } else {
                false
            },
        )
        .await
    {
        Ok(file_length) => {
            info!("[{printer_log_id}] 3mf(ftp) retrieve started on attempt {attempt}");
            if let Some(file_length) = file_length {
                info!("[{printer_log_id}] 3mf(ftp) file size is {file_length} bytes");
            }
            let mut last_report = Instant::now();
            let report_intervals = Duration::from_secs(30);
            let mut time_on_last_send = Instant::now();
            let mut total_on_last_send = 0;
            let success = loop {
                if received_cancel_for_job(notifications, job_number) {
                    return PrinterFtpAttemptResult::Canceled;
                }
                match ftps.retrieve(&mut buf).await {
                    Ok(n) => {
                        if n == 0 {
                            debugex!(">>>>> In the client of ftps.retrieve, received n == 0");
                            break false;
                        }
                        match process_incoming_data(
                            &mut threemf_extractor,
                            &mut gcode_calc,
                            &buf,
                            n,
                            &mut total_read,
                            &mut total_on_last_send,
                            &mut time_on_last_send,
                            gcode_filename_in_3mf,
                            &mut last_report,
                            &report_intervals,
                            printer_log_id,
                            "ftp",
                        ) {
                            ProcessResponse::Break => {
                                // reached end of file in the data stream
                                debugex!(
                                    ">>>> Finished the required gcode file, don't need more data"
                                );
                                break true;
                            }
                            ProcessResponse::Return => {
                                return PrinterFtpAttemptResult::Failed;
                            }
                            ProcessResponse::Continue => (),
                            ProcessResponse::SendAndContinue => {
                                observer.upgrade().unwrap().borrow_mut().on_gcode_analysis(
                                    job_number,
                                    printer_index,
                                    FilamentUsage::new(gcode_calc.layers_extruded.clone()),
                                );
                            }
                        }
                    }
                    Err(err) => {
                        error!(
                            "[{printer_log_id}] Error while reading gcode file {gcode_filename_in_3mf} on 3mf fetch attempt {attempt} {err:?}"
                        );
                        return PrinterFtpAttemptResult::Failed;
                    }
                };
            };
            // first thing, let's send final data available (ftp could still fail, and even partial data is better than nothing)
            gcode_calc.done();
            info!(
                "[{printer_log_id}] Completed reading and processing gcode file '{gcode_filename_in_3mf}' in one of '{threemf_filenames:?}' on 3mf fetch attempt {attempt}"
            );
            observer.upgrade().unwrap().borrow_mut().on_gcode_analysis(
                job_number,
                printer_index,
                FilamentUsage::new(gcode_calc.layers_extruded),
            );

            if !success {
                debugex!(
                    "[{printer_log_id}] Inflate did not recognize end of file data stream, it's probably totally fine, but logged"
                );
            }

            // Close ftp session - can fail

            // needs to take place after all types of retrieve end, whether stream ends or interesting data ends
            debugex!(">>>> Calling end_retrieve from the client");
            if let Err(err) = ftps.end_retrieve().await {
                debugex!(
                    "[{printer_log_id}] Ftp reported error on end_retrieve, code -1 or 426 are by design : {err:?} ",
                );
            }
        }
        Err(err) => {
            if is_ftp_file_not_found(&err) {
                info!(
                    "[{printer_log_id}] 3mf(ftp) retrieve attempt {attempt} did not find one of {threemf_filenames:?}: {err:?}"
                );
                let _ = ftps.quit().await;
                let _ = ftps.close().await;
                return PrinterFtpAttemptResult::FileNotFound;
            }
            error!(
                "[{printer_log_id}] Error initiating retrieve of 3mf file one of {threemf_filenames:?} on attempt {attempt} {err:?}"
            );
            let _ = ftps.quit().await;
            let _ = ftps.close().await;
            return PrinterFtpAttemptResult::Failed;
        }
    };

    match ftps.quit().await {
        Ok(success) => {
            debugex!(
                "[{printer_log_id}] Ftp Quit status {success} (true - quit took place, false - control channel closed earlier)"
            );
        }
        Err(err) => {
            debugex!("[{printer_log_id}] Error in Ftp Quit: {:?}", err);
        }
    }

    match ftps.close().await {
        Ok(_) => {
            debugex!("[{printer_log_id}] Ftp closed");
        }
        Err(err) => {
            debugex!("[{printer_log_id}] Error closing ftp: {:?}", err);
        }
    }

    PrinterFtpAttemptResult::Ok
}

async fn fetch_gcode_analysis_task_cloud_http(
    framework: Rc<RefCell<Framework>>,
    gcode_analysis_request: GcodeAnalysisRequest,
    observer: alloc::rc::Weak<RefCell<dyn GcodeAnalyzerObserver>>,
    notifications_channel: Rc<GcodeAnalysisNotificationChannel>,
) -> FetchSubtaskResult {
    let stack = framework.borrow().stack;
    let tls = framework.borrow().tls;

    let printer_log_id = gcode_analysis_request.printer_number;
    let printer_index = gcode_analysis_request.printer_index;
    let job_number = gcode_analysis_request.job_number;

    let mut notifications = match notifications_channel.subscriber() {
        Ok(subscriber) => subscriber,
        Err(err) => {
            error!("Error getting notification subscriber : {err:?}");
            return FetchSubtaskResult::Failed;
        }
    };

    let threemf_ftp_filename = gcode_analysis_request.threemf_ftp_filename;
    let gcode_filename_in_3mf = gcode_analysis_request.gcode_filename_in_3mf;
    let threemf_url = gcode_analysis_request.threemf_url;

    let mut host_name = "";
    let url;
    if let Ok(url_parsed) = Url::parse(&threemf_url) {
        url = url_parsed;
        if let Some(host_name_part) = url.host_str() {
            host_name = host_name_part;
        }
    } else {
        return FetchSubtaskResult::Failed;
    }

    if host_name.is_empty() {
        error!("Can't resolve host from URL {threemf_url}");
        return FetchSubtaskResult::Failed;
    }

    let Ok(ips) = stack
        .dns_query(host_name, embassy_net::dns::DnsQueryType::A)
        .await
    else {
        error!("Failed to resolve Dns for {host_name}, Internet accessible?",);
        return FetchSubtaskResult::Failed;
    };

    if let Some(GcodeAnalysisNotification::Cancel {
        job_number: canceled_job_number,
    }) = notifications.try_next_message_pure()
        && canceled_job_number == job_number
    {
        return FetchSubtaskResult::Canceled;
    }

    if ips.is_empty() {
        error!("Failed to resolve Dns for {host_name}, Internet accessible?",);
        return FetchSubtaskResult::Failed;
    }

    info!("[{printer_log_id}] Resolved DNS for {host_name} {:?}", ips);

    let s3_cert = CString::new(include_str!("./certs/s3.amazonaws.com.pem")).unwrap();
    let servername = CString::new(host_name).unwrap();
    let certificates = ClientSessionConfig {
        ca_chain: Some(Certificate::new(X509::PEM(s3_cert.as_c_str())).unwrap()),
        server_name: Some(servername.as_c_str()),
        ..ClientSessionConfig::new()
    };

    let mut tcp_buffers_boxed = Box::new(TcpBuffers::<1, 1024, 16384>::new());
    let tcp_buffers = &mut *tcp_buffers_boxed;
    let tcp = Tcp::new(stack, tcp_buffers);

    let tls_connector = Box::new(esp_mbedtls::TlsConnector::new(tls, tcp, &certificates));

    let IpAddress::Ipv4(addr) = ips[0] else {
        error!("Unsupported reply from Dns");
        return FetchSubtaskResult::Failed;
    };

    let mut conn_buf_boxed = Box::new([0_u8; 4096]);
    let conn_buf = &mut *conn_buf_boxed;

    let mut conn: Box<Connection<_, 32>> = Box::new(Connection::new(
        &mut *conn_buf,
        &*tls_connector,
        SocketAddr::new(core::net::IpAddr::V4(addr), 443),
    ));

    info!("[{printer_log_id}] Initiating connection for:");
    info!("[{printer_log_id}]   Full URL: {threemf_url}");
    info!("[{printer_log_id}]   Host: {host_name}");
    info!(
        "[{printer_log_id}]   Path: {}",
        &url[Position::BeforePath..]
    );
    if let Err(err) = conn
        .initiate_request(
            true,
            edge_http::Method::Get,
            &url[Position::BeforePath..],
            &[("Host", host_name)],
        )
        .await
    {
        error!("[{printer_log_id}] Failed to initiate request for 3mf file : {err:?}");
        return FetchSubtaskResult::Failed;
    }

    if let Some(GcodeAnalysisNotification::Cancel {
        job_number: canceled_job_number,
    }) = notifications.try_next_message_pure()
        && canceled_job_number == job_number
    {
        return FetchSubtaskResult::Canceled;
    }

    if let Err(err) = conn.initiate_response().await {
        error!("Failed to initiate fetch response for metadata : {err:?}");
        return FetchSubtaskResult::Failed;
    };

    if let Some(GcodeAnalysisNotification::Cancel {
        job_number: canceled_job_number,
    }) = notifications.try_next_message_pure()
        && canceled_job_number == job_number
    {
        return FetchSubtaskResult::Canceled;
    }

    let headers = match conn.headers() {
        Ok(headers) => headers,
        Err(err) => {
            error!("Failed to read response headers : {err:?}");
            return FetchSubtaskResult::Failed;
        }
    };

    let status_code = headers.code;
    if status_code != 200 {
        error!("Failed to fetch 3mf file : Http error code {status_code}");
        return FetchSubtaskResult::Failed;
    }

    // it looks like in the gcode file name (not in the bbl file name) bambu uses for gcode filename the text until "." in case there is such
    let threemf_filename = format!("{threemf_ftp_filename}.3mf");

    let mut buf = alloc::vec![0;16384];
    let mut gcode_calc = GcodeFilamentCalc::new();
    let mut total_read = 0;
    let mut threemf_extractor = Box::new(ThreemfExtractor::new(&gcode_filename_in_3mf, 16384));
    info!(
        "[{printer_log_id}] Fetching 3mf(http): {threemf_filename} and extracing {gcode_filename_in_3mf}"
    );
    let mut last_report = Instant::now();
    let report_intervals = Duration::from_secs(30);
    let mut total_on_last_send = 0;
    let mut time_on_last_send = Instant::now();
    loop {
        if let Some(GcodeAnalysisNotification::Cancel {
            job_number: canceled_job_number,
        }) = notifications.try_next_message_pure()
            && canceled_job_number == job_number
        {
            return FetchSubtaskResult::Canceled;
        }
        match conn.read(&mut buf).await {
            Ok(n) => {
                match process_incoming_data(
                    &mut threemf_extractor,
                    &mut gcode_calc,
                    &buf,
                    n,
                    &mut total_read,
                    &mut total_on_last_send,
                    &mut time_on_last_send,
                    &gcode_filename_in_3mf,
                    &mut last_report,
                    &report_intervals,
                    printer_log_id,
                    "http",
                ) {
                    ProcessResponse::Break => break,
                    ProcessResponse::Return => return FetchSubtaskResult::Failed,
                    ProcessResponse::Continue => (),
                    ProcessResponse::SendAndContinue => {
                        observer.upgrade().unwrap().borrow_mut().on_gcode_analysis(
                            job_number,
                            printer_index,
                            FilamentUsage::new(gcode_calc.layers_extruded.clone()),
                        );
                    }
                }
            }
            Err(err) => {
                error!(
                    "[{printer_log_id}] Error while reading gcode file {gcode_filename_in_3mf} {err:?}"
                );
                return FetchSubtaskResult::Failed;
            }
        };
    }
    gcode_calc.done();

    info!(
        "[{printer_log_id}] Completed reading and processing gcode file {gcode_filename_in_3mf} in {threemf_filename}"
    );

    observer.upgrade().unwrap().borrow_mut().on_gcode_analysis(
        job_number,
        printer_index,
        FilamentUsage::new(gcode_calc.layers_extruded),
    );
    conn.close().await.ok();
    FetchSubtaskResult::Ok
}

enum ProcessResponse {
    Break,
    Return,
    Continue,
    SendAndContinue,
}

#[allow(clippy::too_many_arguments)]
fn process_incoming_data(
    threemf_extractor: &mut ThreemfExtractor,
    gcode_calc: &mut GcodeFilamentCalc,
    buf: &[u8],
    n: usize,
    total_read: &mut usize,
    total_on_last_send: &mut usize,
    time_on_last_send: &mut Instant,
    gcode_file_name_in_3mf: &str,
    last_report: &mut Instant,
    report_intervals: &Duration,
    printer_log_id: usize,
    fetch_3mf_str: &str,
) -> ProcessResponse {
    if n == 0 {
        return ProcessResponse::Break;
    }
    *total_read += n;
    match threemf_extractor.feed_data(&buf[..n], |out| -> Result<bool, Box<dyn Error>> {
        gcode_calc.add_buffer(out).map_err(|err| err.to_string())?;
        Ok(true)
    }) {
        Ok(status) => match status {
            FeedStatus::NeedMoreData => (),
            FeedStatus::StreamEnded => return ProcessResponse::Break,
            FeedStatus::OutputProcessorEnded => return ProcessResponse::Break,
        },
        Err(err) => {
            error!(
                "[{printer_log_id}] Error while processing gcode data in file {gcode_file_name_in_3mf} {err:?}"
            );
            return ProcessResponse::Return;
        }
    }
    if last_report.elapsed() > *report_intervals {
        info!(
            "[{printer_log_id}] 3MF Download ({fetch_3mf_str}): {total_read} bytes processed, {} consumtion entries generted",
            gcode_calc.layers_extruded.len()
        );
        *last_report = Instant::now();
    }
    if *total_on_last_send < 1024 * 1024 && time_on_last_send.elapsed() > Duration::from_secs(60) {
        *time_on_last_send = Instant::now();
        *total_on_last_send = *total_read;
        info!(
            "[{printer_log_id}] Sending partial consumption information after {total_read} bytes downloaded"
        );
        return ProcessResponse::SendAndContinue;
    }
    ProcessResponse::Continue
}

// fn replace_3mf_ftp_chars(s: &str) -> String {
//     let forbidden = "[]\\/:*?\"<>|";
//     s.chars()
//         .map(|c| if forbidden.contains(c) { '_' } else { c })
//         .collect()
// }
