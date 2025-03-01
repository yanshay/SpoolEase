use core::cell::RefCell;

use alloc::{rc::Rc, string::String, vec::Vec};
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;

use framework::prelude::*;

use crate::app_config::AppConfig;

pub const TAG_PLACEHOLDER: &str = "$tag-id$";

pub struct SpoolTag {
    tag_operation: &'static embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolTagObserver>>>,
}

pub trait SpoolTagObserver {
    fn on_tag_status(&mut self, status: &Status);
}

impl SpoolTag {
    pub fn write_tag(&self, text: &str, tray_id: usize) {
        self.tag_operation.signal(TagOperation::WriteTag(WriteTagRequest {
            text: String::from(text),
            tray_id,
        }));
    }

    pub fn cancel_operation(&self) {
        self.tag_operation.reset();
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn SpoolTagObserver>>) {
        self.observers.push(observer);
    }

    pub fn notify_status(&self, status: Status) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_status(&status);
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
struct WriteTagRequest {
    text: String,
    tray_id: usize,
}

#[derive(Debug)]
struct ReadTagRequest {}

#[derive(Debug)]
enum TagOperation {
    WriteTag(WriteTagRequest),
    ReadTag(ReadTagRequest),
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Failure {
    TagWriteFailure,
    TagReadFailure,
}

#[derive(Debug)]
pub enum Status {
    FoundTagNowReading,
    FoundTagNowWriting,
    WriteSuccess(/*tray_id*/ usize),
    ReadSuccess(String),
    Failure(Failure),
}

/////////////////////////////////////////////////////////////////////////////////////////////////

pub async fn init(
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
    app_config: Rc<RefCell<AppConfig>>,
) -> Rc<RefCell<SpoolTag>> {
    let spawner = embassy_executor::Spawner::for_current_executor().await;

    let tag_operation = mk_static!(
        embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>,
        embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>::new()
    );

    let spool_tag_rc = Rc::new(RefCell::new(SpoolTag {
        tag_operation,
        observers: Vec::new(),
    }));

    spawner.spawn(nfc_task(spool_tag_rc.clone(), spi_device, irq, tag_operation, app_config)).ok();

    spool_tag_rc
}

// Had to specify the I2C1 because can't have generic tasks in embassy, maybe there's some workaround in the following link
//https://github.com/embassy-rs/embassy/issues/1837
#[embassy_executor::task]
pub async fn nfc_task(
    spool_tag_rc: Rc<RefCell<SpoolTag>>,
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
    tag_operation: &'static embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>,
    app_config: Rc<RefCell<AppConfig>>,
) {
    // To switch from using IRQ to not using IRQ:
    //   1. use None::<pn532::spi::NoIRQ> instead of Some(irq)
    //   2. in sam_configuration set use_irq_pin to false (maybe not required)
    let interface = pn532::spi::SPIInterface {
        spi: spi_device,
        irq: Some(irq),
        // irq: None::<pn532::spi::NoIRQ>,
    };

    let timer = crate::pn532_ext::Esp32TimerAsync::new();

    let mut pn532: pn532::Pn532<_, _, 32> = pn532::Pn532::new(interface, timer);
    // pn532.wake_up().await.unwrap();

    info!("Configuring pn532");

    let mut initialization_succeeded = false;
    let mut successful_retry = 0;
    let retries = 10;
    for retry in 0..=retries {
        if retry % 5 == 0 {
            pn532.wake_up().await.unwrap();
            Timer::after(Duration::from_millis(30)).await;
        }
        if let Err(e) = pn532
            .process(
                &pn532::Request::sam_configuration(pn532::requests::SAMMode::Normal, true),
                0,
                embassy_time::Duration::from_millis(1000),
            )
            .await
        {
            // Error, just wait before retrying
            if retry != retries {
                Timer::after(Duration::from_millis(100)).await;
            } else {
                term_error!("Error initializing Tag Reader {:?}", e);
            }
        } else {
            info!("Initialized Tag Reader successfully");
            initialization_succeeded = true;
            successful_retry = retry;
            break;
        }
    }

    if !initialization_succeeded {
        app_config.borrow_mut().report_pn532(false);
        return;
    } else {
        app_config.borrow_mut().report_pn532(true);
    }

    if let Ok(fw) = pn532
        .process(&pn532::Request::GET_FIRMWARE_VERSION, 4, embassy_time::Duration::from_millis(200))
        .await
    {
        trace!("PN532 Firmware Version response: {:?}", fw);
        term_info!("Established communication with Tag Reader ({})", successful_retry);
        app_config.borrow_mut().report_pn532(true);
    } else {
        term_error!("Failed to communicate with Tag Reader");
        app_config.borrow_mut().report_pn532(false);
        return;
    }

    info!("Entering wait for tag loop in nfc task");

    let mut previous_tag = None;
    let mut previous_tag_scan_time = Instant::now();

    loop {
        // Wait for Tag and read its UUID
        debug!("Waiting for Tag");

        let res = pn532.process(&pn532::Request::INLIST_ONE_ISO_A_TARGET, 17, Duration::from_secs(60)).await;

        match res {
            Ok(uid) => {
                debug!("Found Tag with uid : {:?}", uid);
                let uid = uid.to_vec();
                if previous_tag.as_ref() == Some(&uid) && previous_tag_scan_time.elapsed() < Duration::from_millis(500) {
                    debug!("It's the same as previous tag so ignoring");
                    previous_tag_scan_time = Instant::now();
                    // allow using previous tag only if 0.5 seconds passed since seeing it right before
                    continue;
                } else {
                    info!("It's a new tag so using !!!");
                }

                previous_tag = Some(uid);

                let operation_with_tag = tag_operation.try_take();

                match operation_with_tag.unwrap_or(TagOperation::ReadTag(ReadTagRequest {})) {
                    TagOperation::WriteTag(write_tag_reuest) => {
                        spool_tag_rc.borrow().notify_status(Status::FoundTagNowWriting);
                        let tag_uid = URL_SAFE.encode(previous_tag.as_ref().unwrap());
                        let tag_uid = tag_uid.trim_end_matches('=');
                        let final_tag_text = write_tag_reuest.text.replace(TAG_PLACEHOLDER, &tag_uid);
                        match crate::nfc::write_ndef_url_record(&mut pn532, &final_tag_text, Duration::from_secs(2)).await {
                            Ok(_num_bytes_written) => {
                                debug!("Wrote {} to tag", final_tag_text);
                                spool_tag_rc.borrow().notify_status(Status::WriteSuccess(write_tag_reuest.tray_id));
                            }
                            Err(e) => {
                                term_error!("Error writing to tag {:?}", e);
                                spool_tag_rc.borrow().notify_status(Status::Failure(Failure::TagWriteFailure));
                            }
                        }
                        previous_tag_scan_time = Instant::now();
                    }
                    TagOperation::ReadTag(_read_tag_request) => {
                        spool_tag_rc.borrow().notify_status(Status::FoundTagNowReading);
                        match crate::nfc::read_ndef_record(&mut pn532, Duration::from_secs(2)).await {
                            Ok(read_record) => {
                                debug!("{}", read_record.url_payload());
                                spool_tag_rc.borrow().notify_status(Status::ReadSuccess(read_record.url_payload()));
                            }
                            Err(e) => {
                                error!("Error reading tag {:?}", e);
                                spool_tag_rc.borrow().notify_status(Status::Failure(Failure::TagReadFailure));
                            }
                        }
                        previous_tag_scan_time = Instant::now();
                    }
                }
            }
            Err(e) => match e {
                pn532::Error::TimeoutResponse => {
                    // This is not really an error - every 60 seconds (which is timeout provided, will take place)
                    previous_tag = None;
                }
                pn532::Error::TimeoutAck => {
                    // Doesn't seem to be an error in case of using IRQ?
                    warn!("TimeoutAck Error, Error?");
                    previous_tag = None; // ??
                }
                pn532::Error::BadAck => {
                    // Doesn't seem to be an error in case of using IRQ?
                    warn!("BadAck Error, Error?");
                    previous_tag = None; // ??
                }
                _ => {
                    warn!("Error when waiting for tag {:?}", e);
                    let operation_with_tag = tag_operation.try_take();
                    match operation_with_tag.unwrap_or(TagOperation::ReadTag(ReadTagRequest {})) {
                        TagOperation::WriteTag(_write_tag_request) => {
                            spool_tag_rc.borrow().notify_status(Status::Failure(Failure::TagWriteFailure));
                        }
                        TagOperation::ReadTag(_read_tag_request) => {
                            spool_tag_rc.borrow().notify_status(Status::Failure(Failure::TagReadFailure));
                        }
                    }
                }
            },
        }
    }
}
