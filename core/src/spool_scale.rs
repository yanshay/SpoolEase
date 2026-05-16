use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    cell::RefCell,
    net::{Ipv4Addr, SocketAddr},
    str::FromStr,
};
use edge_http::{
    io::client::Connection,
    ws::{MAX_BASE64_KEY_LEN, MAX_BASE64_KEY_RESPONSE_LEN, NONCE_LEN},
};
use edge_nal_embassy::{Tcp, TcpBuffers};
use edge_ws::{FrameHeader, FrameType};
use embassy_executor::Spawner;
use embassy_futures::select::select3;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Instant, Timer};
use embedded_io_async::Write;
use framework::{debug, error, framework_web_app::encrypt, info, mk_static, prelude::*, term_error, term_info, utils::random_u32, warn};
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use shared::{
    gcode_analysis_task::{FilamentUsage, GcodeAnalysisNotification, GcodeAnalysisRequest},
    scale::{ConsoleToScale, OtaProgressUpdate, ScaleToConsole},
};

use crate::{app_config::AppConfig, ssdp::SSDPPubSubChannel};

pub type ConsoleToScaleChannel = Channel<NoopRawMutex, ConsoleToScale, 5>;

#[allow(dead_code)]
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum ScaleWeight {
    Unknown,
    Stable(i32),
    Unstable(i32),
}

pub struct SpoolScale {
    pub weight: ScaleWeight,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>>,
    console_to_scale: &'static ConsoleToScaleChannel,
    pub connected_scale: Option<(Option<String>, Ipv4Addr)>,
    pub available_scales: HashSet<(Option<String>, Ipv4Addr)>,
}

pub trait SpoolScaleObserver {
    fn on_scale_loaded(&mut self, weight: i32);
    fn on_scale_load_changed_stable(&mut self, weight: i32);
    fn on_scale_load_changed_unstable(&mut self, weight: i32);
    fn on_scale_load_removed(&mut self);
    fn on_scale_raw_samples_avg(&mut self, raw_data: i32);
    fn on_scale_connected(&mut self);
    fn on_scale_disconnected(&mut self);
    fn on_scale_uncalibrated(&mut self);
    fn on_term_text(&mut self, text: &str);
    fn on_tag_status(&mut self, status: &shared::spool_tag::Status);
    fn on_pn532_status(&mut self, status: bool);
    fn on_button_pressed(&mut self, scale_weight: ScaleWeight) -> Option<bool>;
    fn on_gcode_analysis(&mut self, job_number: i32, printer_index: usize, gcode_analysis: FilamentUsage);
    fn on_gcode_analysis_failed(&mut self, job_number: i32, printer_index: usize);
    fn on_gcode_analysis_canceled(&mut self, job_number: i32, printer_index: usize);
    fn on_gcode_analysis_completed(&mut self, job_number: i32, printer_index: usize);
    fn on_scale_version(&mut self, scale_version: &str);
    fn on_ota_progress_update(&mut self, update: OtaProgressUpdate);
}

impl SpoolScale {
    // Notifications from Console to Scale  ////////////////////////

    pub fn calibrate(&self, weight: i32) {
        self.console_to_scale
            .try_send(ConsoleToScale::Calibrate(weight))
            .unwrap_or_else(|e| error!("Failed sending calibrate request to scale {e:?}"));
    }

    pub fn button_response(&self, success: bool) {
        self.console_to_scale
            .try_send(ConsoleToScale::ButtonResponse(success))
            .unwrap_or_else(|e| error!("Failed sending button response request to scale {e:?}"));
    }

    #[allow(dead_code)]
    pub fn request_gcode_analysis(&self, gcode_analysis_request: GcodeAnalysisRequest) -> Result<(), String> {
        if let Err(err) = self
            .console_to_scale
            .try_send(ConsoleToScale::RequestGcodeAnalysis { gcode_analysis_request })
        {
            Err(format!("Failed sending request_gcode_analysis to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn read_tag(&self) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::ReadTag) {
            Err(format!("Failed sending read_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn write_tag(&self, text: &str, check_uid: Option<Vec<Vec<u8>>>, cookie: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::WriteTag {
            text: text.to_string(),
            check_uid,
            cookie,
        }) {
            Err(format!("Failed sending write_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn erase_tag(&self, check_uid: Option<Vec<u8>>, cookie: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::EraseTag { check_uid, cookie }) {
            Err(format!("Failed sending erase_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }
    #[allow(dead_code)]
    pub fn emulate_tag(&self, url: &str) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::EmulateTag { url: url.to_string() }) {
            Err(format!("Failed sending request_gcode_analysis to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn gcode_analysis_notify(&self, gcode_analysis_notification: GcodeAnalysisNotification) -> Result<(), String> {
        if let Err(err) = self
            .console_to_scale
            .try_send(ConsoleToScale::GcodeAnalysisNotify { gcode_analysis_notification })
        {
            Err(format!("Failed sending gcode_analysis_notify to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn update_firmware(&self, ota_domain: &str, ota_path: &str, ota_toml_filename: &str, ota_cert: &str) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::UpdateFirmware {
            ota_domain: ota_domain.to_string(),
            ota_path: ota_path.to_string(),
            ota_toml_filename: ota_toml_filename.to_string(),
            ota_cert: ota_cert.to_string(),
        }) {
            Err(format!("Failed sending update_firmware to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn tags_in_store(&self, tags_in_store: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::TagsInStore { tags: tags_in_store }) {
            Err(format!("Failed sending tags_in_store to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    // Technical Stuff  ////////////////////////

    pub fn process_message(&mut self, _frame_header: &FrameHeader, payload: &[u8]) {
        let parse_res = serde_json::from_slice::<ScaleToConsole>(payload);
        if let Ok(scale_to_console) = parse_res {
            match scale_to_console {
                ScaleToConsole::NewLoad(weight) => {
                    self.weight = ScaleWeight::Unstable(weight);
                    self.notify_scale_loaded(weight);
                }
                ScaleToConsole::LoadChangedUnstable(weight) => {
                    self.weight = ScaleWeight::Unstable(weight);
                    self.notify_scale_load_changed_unstable(weight);
                }
                ScaleToConsole::LoadChangedStable(weight) => {
                    self.weight = ScaleWeight::Stable(weight);
                    self.notify_scale_load_changed_stable(weight);
                }
                ScaleToConsole::LoadRemoved => {
                    let first_update = matches!(self.weight, ScaleWeight::Unknown);
                    self.weight = ScaleWeight::Stable(0);
                    if !first_update {
                        self.notify_scale_load_removed();
                    }
                }
                ScaleToConsole::RawSamplesAvg(raw_data) => {
                    self.notify_scale_raw_samples_avg(raw_data);
                }
                ScaleToConsole::Uncalibrated => {
                    self.notify_scale_uncalibrated();
                }
                ScaleToConsole::Term(text) => {
                    self.notify_term_text(&text);
                }
                ScaleToConsole::TagStatus(status) => {
                    self.notify_tag_status(&status);
                }
                ScaleToConsole::PN532Status(status) => {
                    self.notify_pn532_status(status);
                }
                ScaleToConsole::ButtonPressed => {
                    self.notify_button_pressed();
                }
                ScaleToConsole::GcodeAnalysis {
                    job_number,
                    printer_index,
                    filament_usage_csv,
                } => {
                    self.notify_gcode_analysis(job_number, printer_index, filament_usage_csv);
                }
                ScaleToConsole::GcodeAnalysisFailed { job_number, printer_index } => {
                    self.notify_gcode_analysis_failed(job_number, printer_index);
                }
                ScaleToConsole::GcodeAnalysisCanceled { job_number, printer_index } => {
                    self.notify_gcode_analysis_canceled(job_number, printer_index);
                }
                ScaleToConsole::GcodeAnalysisCompleted { job_number, printer_index } => {
                    self.notify_gcode_analysis_completed(job_number, printer_index);
                }
                ScaleToConsole::ScaleVersion { version } => {
                    self.notify_scale_version(&version);
                }
                ScaleToConsole::OtaProgressUpdate(update) => self.notify_ota_progress_update(&update),
            }
        } else {
            warn!(
                "Received an unsupported message from Scale, Console version update probably available : {}",
                String::from_utf8_lossy(payload)
            );
        }
    }
    pub fn connected(&self) {
        // don't change to &mut, if changed will panic on borrow since during connect notification sending data back to scale that needs borrow
        // one solution is to pass reference to self to the object being notified so it can use it instead of borrowing (maybe possible)
        self.notify_scale_connected();
    }
    pub fn disconnected(&mut self) {
        self.notify_scale_disconnected();
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>) {
        self.observers.push(observer);
    }

    // Notifications from Scale to Console  ////////////////////////

    pub fn notify_scale_loaded(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_loaded(weight);
        }
    }
    pub fn notify_scale_load_changed_stable(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_stable(weight);
        }
    }
    pub fn notify_scale_load_changed_unstable(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_unstable(weight);
        }
    }
    pub fn notify_scale_load_removed(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_removed();
        }
    }
    pub fn notify_scale_raw_samples_avg(&self, raw_data: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_raw_samples_avg(raw_data);
        }
    }
    pub fn notify_scale_connected(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_connected();
        }
    }
    pub fn notify_scale_disconnected(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_disconnected();
        }
    }
    pub fn notify_scale_uncalibrated(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_uncalibrated();
        }
    }
    pub fn notify_term_text(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_term_text(text);
        }
    }
    pub fn notify_tag_status(&mut self, status: &shared::spool_tag::Status) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_status(status);
        }
    }
    pub fn notify_pn532_status(&mut self, status: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_pn532_status(status);
        }
    }
    pub fn notify_button_pressed(&mut self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            let observer_immediate_response = observer.borrow_mut().on_button_pressed(self.weight);
            if let Some(success) = observer_immediate_response {
                self.button_response(success);
            }
        }
    }
    pub fn notify_gcode_analysis(&mut self, job_number: i32, printer_index: usize, filament_usage_csv: String) {
        // Optimized to create only as many clones as required (in case of several observers)
        if self.observers.is_empty() {
            return;
        }
        // let num_records = filament_usage_csv.lines().count();
        // let mut data = Vec::<FilamentUsageEntry>::with_capacity(num_records);
        // let mut csv_parser = serde_csv_core::Reader::<16>::new(); // 16 is max field size
        // for line in filament_usage_csv.lines() {
        //     match csv_parser.deserialize(line.as_bytes()) {
        //         Ok(v) => {
        //             data.push(v.0);
        //         }
        //         Err(err) => {
        //             error!("Internal error deserializing FilamentUsageEntry : {err}");
        //             return;
        //         }
        //     }
        // }
        let filament_usage = match FilamentUsage::from_csv(&filament_usage_csv) {
            Ok(v) => v,
            Err(err) => {
                error!("Internal error deserializing FilamentUsageEntry : {err}");
                return;
            }
        };

        if let Some((last, rest)) = self.observers.split_last() {
            for weak_observer in rest.iter() {
                let observer = weak_observer.upgrade().unwrap();
                observer.borrow_mut().on_gcode_analysis(job_number, printer_index, filament_usage.clone());
            }
            let observer = last.upgrade().unwrap();
            observer.borrow_mut().on_gcode_analysis(job_number, printer_index, filament_usage);
        }
    }
    pub fn notify_gcode_analysis_failed(&self, job_number: i32, printer_index: usize) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_gcode_analysis_failed(job_number, printer_index);
        }
    }
    pub fn notify_gcode_analysis_canceled(&self, job_number: i32, printer_index: usize) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_gcode_analysis_canceled(job_number, printer_index);
        }
    }
    pub fn notify_gcode_analysis_completed(&self, job_number: i32, printer_index: usize) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_gcode_analysis_completed(job_number, printer_index);
        }
    }
    pub fn notify_scale_version(&self, scale_version: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_version(scale_version);
        }
    }
    pub fn notify_ota_progress_update(&self, update: &OtaProgressUpdate) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_progress_update(update.clone());
        }
    }
}

pub fn init(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    stack: Stack<'static>,
    spawner: Spawner,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) -> Rc<RefCell<SpoolScale>> {
    let console_to_scale = mk_static!(ConsoleToScaleChannel, ConsoleToScaleChannel::new());

    let spool_scale_rc = Rc::new(RefCell::new(SpoolScale {
        weight: ScaleWeight::Unknown,
        observers: Vec::new(),
        console_to_scale,
        connected_scale: None,
        available_scales: HashSet::new(),
    }));

    spawner.spawn_heap(monitor_scales_task(spool_scale_rc.clone(), ssdp_pub_sub)).ok();

    if let Some(spool_scale_config) = &app_config.clone().borrow().configured_scale
        && spool_scale_config.available
    {
        spawner
            .spawn_heap(spool_scale_task(framework, app_config, stack, spool_scale_rc.clone(), ssdp_pub_sub))
            .ok();
    }

    spool_scale_rc
}

// #[embassy_executor::task]
pub async fn monitor_scales_task(spool_scale_rc: Rc<RefCell<SpoolScale>>, ssdp_pub_sub: &'static SSDPPubSubChannel) {
    let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
    loop {
        let ssdp_info = ssdp_subscribe.next_message().await;
        match ssdp_info {
            embassy_sync::pubsub::WaitResult::Lagged(_) => (),
            embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale")
                    && let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location)
                {
                    let spoolscale_ip = found_ip;
                    let spoolscale_name = Some(ssdp_info.usn);
                    spool_scale_rc.borrow_mut().available_scales.insert((spoolscale_name, spoolscale_ip));
                }
            }
        }
    }
}

// #[embassy_executor::task]
pub async fn spool_scale_task(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    stack: Stack<'static>,
    spool_scale_rc: Rc<RefCell<SpoolScale>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    info!("Task spool_scale_task started");
    let console_to_scale = spool_scale_rc.borrow().console_to_scale;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after_millis(250).await;
    }

    let mut configured_ip = None;
    let mut configured_name = None;
    let spoolscale_ip;
    let mut spoolscale_name = None;

    if let Some(configured_scale) = &app_config.borrow().configured_scale {
        configured_ip = configured_scale.ip;
        configured_name = configured_scale.name.clone();
        spoolscale_name = configured_scale.name.clone();
    }

    if let Some(configured_ip) = configured_ip {
        spoolscale_ip = configured_ip
    } else {
        term_info!(
            "No SpoolScale IP configured, discovering {}",
            configured_name.as_ref().unwrap_or(&"".to_string())
        );
        let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
        loop {
            let ssdp_info = ssdp_subscribe.next_message().await;
            match ssdp_info {
                embassy_sync::pubsub::WaitResult::Lagged(_) => (),
                embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                    if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale") {
                        if let Some(spoolscale_name) = &configured_name
                            && ssdp_info.usn != *spoolscale_name
                        {
                            debug!("Found a SpoolScale, but with name {} and not {spoolscale_name}", ssdp_info.usn);
                            continue;
                        }
                        if let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location) {
                            spoolscale_ip = found_ip;
                            spoolscale_name = Some(ssdp_info.usn);
                            term_info!("Discovered SpoolScale at {}", spoolscale_ip);
                            break;
                        }
                    }
                }
            }
        }
    }

    spool_scale_rc.borrow_mut().connected_scale = Some((spoolscale_name.clone(), spoolscale_ip));

    let tcp_buffers = Box::new(TcpBuffers::<1, 1024, 1024>::new());
    let tcp = Tcp::new(stack, &*tcp_buffers);
    let tcp = edge_nal::WithTimeout::new(15000, tcp);

    let mut first_connect = true;
    let mut connect_error_counter = 0;
    let mut conn_buf = alloc::vec![0_u8; 128*1024]; // large size for gcode_analysis
    'connect_loop: loop {
        Framework::wait_for_wifi(&framework).await;
        if first_connect {
            first_connect = false;
        } else {
            Timer::after_secs(2).await;
        }
        let mut conn: Connection<_> = Connection::new(&mut conn_buf, &tcp, SocketAddr::new(core::net::IpAddr::V4(spoolscale_ip), 81));

        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).unwrap();
        let mut nonce_base64_buf = [0_u8; MAX_BASE64_KEY_LEN];
        if connect_error_counter % 10 == 0 {
            term_info!("Connecting to SpoolScale at {}", spoolscale_ip);
        }
        if let Err(err) = conn
            .initiate_ws_upgrade_request(Some(&spoolscale_ip.to_string()), None, "/ws", None, &nonce, &mut nonce_base64_buf)
            .await
        {
            if connect_error_counter % 10 == 0 && connect_error_counter != 0 {
                term_error!("SpoolScale: Error connecting to Scale ({:?})", err);
            }
            connect_error_counter += 1;
            continue 'connect_loop;
        }
        if let Err(err) = conn.initiate_response().await {
            term_error!("SpoolScale: Error initiating web socket response ({:?})", err);
            continue 'connect_loop;
        }

        let mut buf = [0_u8; MAX_BASE64_KEY_RESPONSE_LEN];
        let upgrade_accepted_res = conn.is_ws_upgrade_accepted(&nonce, &mut buf);
        match upgrade_accepted_res {
            Ok(true) => (),
            Ok(false) => {
                term_error!("SpoolScale: Upgrading to websocket rejected");
                continue 'connect_loop;
            }
            Err(err) => {
                term_error!("SpoolScale: Error during websocket upgrade {:?}", err);
                continue 'connect_loop;
            }
        }

        if let Err(err) = conn.complete().await {
            error!("SpoolScale: Error completing the connection {:?}", err);
            return;
        }

        // Now we have the TCP socket in a state where it can be operated as a WS connection
        // Send some traffic to a WS echo server and read it back

        let (mut socket, buf) = conn.release();

        connect_error_counter = 0;

        term_info!("Connection with SpoolScale established");

        spool_scale_rc.borrow().connected();

        'send_recv_loop: loop {
            // max timeout_for_ping need to be less than above WithTimeout wrapper
            let timeout_for_ping = 12000 + (random_u32() % 2000);
            let with_timeout_res = select3(
                Timer::after_millis(timeout_for_ping as u64),
                FrameHeader::recv(&mut socket),
                console_to_scale.receive(),
            )
            .await;
            match with_timeout_res {
                embassy_futures::select::Either3::First(_timeout_res) => {
                    // Sending Ping on timeout
                    let now = Instant::now().as_ticks();
                    let ping_header = FrameHeader {
                        frame_type: FrameType::Ping,
                        payload_len: 8,
                        mask_key: None,
                    };
                    let send_ping_header_res = ping_header.send(&mut socket).await;
                    match send_ping_header_res {
                        Ok(_) => {
                            let send_ping_payload_res = ping_header.send_payload(&mut socket, &now.to_le_bytes()).await;
                            match send_ping_payload_res {
                                Ok(_) => {
                                    let res = socket.flush().await;
                                    match res {
                                        Ok(_) => {
                                            debug!("SpoolScale: Sent Ping");
                                        }
                                        Err(send_ping_flush_err) => {
                                            error!("SpoolScale: Error sending Ping payload (1) {send_ping_flush_err:?}, disconnecting");
                                            break 'send_recv_loop;
                                        }
                                    }
                                }
                                Err(send_ping_payload_err) => {
                                    error!("SpoolScale: Error sending Ping payload (2) {send_ping_payload_err:?}");
                                }
                            }
                        }
                        Err(send_ping_header_err) => {
                            error!("SpoolScale: Error sending Ping header {send_ping_header_err:?}");
                        }
                    }
                    // in case of timeut (which is the Err(_timeout_err) case we want to continue send_recv_loop
                    continue 'send_recv_loop;
                }
                embassy_futures::select::Either3::Second(from_scale_res) => {
                    match from_scale_res {
                        Ok(header) => {
                            let recv_payload_res = header.recv_payload(&mut socket, buf).await;
                            if let Ok(payload) = recv_payload_res {
                                match header.frame_type {
                                    FrameType::Text(_fragmented) => {
                                        spool_scale_rc.borrow_mut().process_message(&header, payload);
                                    }
                                    FrameType::Binary(_) => {
                                        error!("Got binary message, header: {header}, payload: {payload:?}");
                                    }
                                    FrameType::Ping => {
                                        let pong_header = FrameHeader {
                                            frame_type: FrameType::Pong,
                                            payload_len: header.payload_len,
                                            mask_key: header.mask_key,
                                        };
                                        let send_pong_header_res = pong_header.send(&mut socket).await;
                                        match send_pong_header_res {
                                            Ok(_) => {
                                                let res = pong_header.send_payload(&mut socket, payload).await;
                                                match res {
                                                    Ok(_) => {
                                                        let flush_res = socket.flush().await;
                                                        match flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Received Ping, replied with Pong");
                                                            }
                                                            Err(err) => {
                                                                error!("SpoolScale: Error sending Pong reply {err:?}, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Pong payload (3) {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                error!("SpoolScale: Error sending Pong header {err:?}");
                                                break 'send_recv_loop;
                                            }
                                        }
                                    }
                                    FrameType::Pong => {
                                        let tick_res: Result<&[u8; 8], _> = payload.try_into();
                                        if let Ok(ticks) = tick_res {
                                            let ping_ticks = u64::from_le_bytes(*ticks);
                                            let ping_instant = Instant::from_ticks(ping_ticks);
                                            let elapsed_duration = ping_instant.elapsed();
                                            debug!("SpoolScale: Ping-Pong duration was {} millis", elapsed_duration.as_millis());
                                        } else {
                                            warn!("SpoolScale: Received pong wrongly formatted, header: {header:?}, payload: {payload:?}");
                                        }
                                    }
                                    FrameType::Close => {
                                        let close_resp_header = FrameHeader {
                                            frame_type: FrameType::Close,
                                            payload_len: header.payload_len,
                                            mask_key: header.mask_key,
                                        };
                                        let close_resp_header_res = close_resp_header.send(&mut socket).await;
                                        match close_resp_header_res {
                                            Ok(_) => {
                                                let close_resp_payload_res = close_resp_header.send_payload(&mut socket, payload).await;
                                                match close_resp_payload_res {
                                                    Ok(_) => {
                                                        let close_resp_flush_res = socket.flush().await;
                                                        match close_resp_flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Replied to Close, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                            Err(close_resp_flush_err) => {
                                                                error!(
                                                                    "SpoolScale: Error sending Close reply {close_resp_flush_err:?}, disconnecting"
                                                                );
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Close Response payload {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(close_resp_header_err) => {
                                                error!("SpoolScale: Error sending Close Response header {close_resp_header_err:?}");
                                                break 'send_recv_loop;
                                            }
                                        }
                                    }
                                    FrameType::Continue(_fragmented) => {
                                        warn!(
                                            "SpoolScale Recv(continue): header: {header}, payload: {}",
                                            core::str::from_utf8(payload).unwrap()
                                        );
                                    }
                                }

                                if !header.frame_type.is_final() {
                                    warn!("SpoolScale: Unexpected fragmented frame header: {header:?}, payload: {payload:?}");
                                }
                            } else {
                                error!("SpoolScale: Error while reading payload {:?}", recv_payload_res.err().unwrap());
                                // can continue, will try to read next header and if will fail, will fail on the header and disconnect
                            }
                        }
                        Err(recv_header_err) => {
                            match recv_header_err {
                                edge_ws::Error::Io(io_err) => {
                                    error!("SpoolScale: IO error while reading header, disconnecting {io_err:?}");
                                    // breaking out of the loop, because when an IO error happens here, it happens continuously and turns to a busy loop
                                    break 'send_recv_loop;
                                }
                                // edge_ws::Error::Incomplete(_) => todo!(),
                                // edge_ws::Error::Invalid => todo!(),
                                // edge_ws::Error::BufferOverflow => todo!(),
                                // edge_ws::Error::InvalidLen => todo!(),
                                _ => {
                                    error!("SpoolScale: Error receiving web-socket header {recv_header_err:?}");
                                    break 'send_recv_loop;
                                }
                            }
                        }
                    }
                }
                embassy_futures::select::Either3::Third(console_to_scale) => {
                    let json_res = serde_json::to_string(&console_to_scale);
                    match json_res {
                        Ok(mut json) => {
                            if matches!(console_to_scale, ConsoleToScale::RequestGcodeAnalysis { .. })
                                || matches!(console_to_scale, ConsoleToScale::UpdateFirmware { .. })
                            {
                                let key = &app_config.borrow().scale_encryption_key.borrow();
                                json = if key.is_empty() {
                                    term_error!("Empty SpoolScale Security Key configured in Console , can't send message to scale");
                                    if matches!(console_to_scale, ConsoleToScale::UpdateFirmware { .. }) {
                                        spool_scale_rc.borrow().notify_ota_progress_update(&OtaProgressUpdate::Failed {
                                            text: "Scale Security Key not set in Console\nCan't Update Scale Firmware".to_string(),
                                        });
                                    }
                                    continue;
                                } else {
                                    encrypt(key, &json)
                                };
                            }
                            let send_to_scale_header = FrameHeader {
                                frame_type: FrameType::Text(false),
                                payload_len: json.len() as u64,
                                mask_key: None,
                            };

                            let send_to_scale_header_res = send_to_scale_header.send(&mut socket).await;
                            match send_to_scale_header_res {
                                Ok(_) => {
                                    let send_to_scale_payload_res = send_to_scale_header.send_payload(&mut socket, json.as_bytes()).await;
                                    match send_to_scale_payload_res {
                                        Ok(_) => {
                                            let res = socket.flush().await;
                                            match res {
                                                Ok(_) => {
                                                    // log at most 200 characters
                                                    let idx = json.char_indices().nth(200).map(|(i, _)| i).unwrap_or(json.len());
                                                    let str_to_print: &str = &json[..idx];
                                                    debug!(
                                                        "SpoolScale: Sent message to scale: {str_to_print}{}",
                                                        if str_to_print.len() < json.len() { "  ..." } else { "" }
                                                    );
                                                }
                                                Err(send_to_scale_flush_err) => {
                                                    error!("SpoolScale: Error sending message payload {send_to_scale_flush_err:?}, disconnecting");
                                                    break 'send_recv_loop;
                                                }
                                            }
                                        }
                                        Err(send_to_scale_payload_err) => {
                                            error!("SpoolScale: Error sending Ping payload {send_to_scale_payload_err:?}");
                                        }
                                    }
                                }
                                Err(send_to_scale_header_err) => {
                                    error!("SpoolScale: Error sending Ping header {send_to_scale_header_err:?}");
                                }
                            }
                        }
                        Err(err) => {
                            error!("SpoolScale: Error serializing data {:?}, {:?}", console_to_scale, err)
                        }
                    }
                }
            }
        }
        spool_scale_rc.borrow_mut().disconnected();
    }
}
