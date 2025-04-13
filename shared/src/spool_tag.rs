use core::cell::RefCell;

use alloc::{rc::Rc, string::String, vec::Vec};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;

use framework::prelude::*;
use serde::{Deserialize, Serialize};

pub const TAG_PLACEHOLDER: &str = "$tag-id$";

pub struct SpoolTag {
    tag_operation: &'static embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolTagObserver>>>,
}

pub trait SpoolTagObserver {
    fn on_tag_status(&mut self, status: &Status);
    fn on_pn532_status(&mut self, status: bool);
}

impl SpoolTag {
    pub fn write_tag(&self, text: &str, tray_id: usize) {
        self.tag_operation.signal(TagOperation::WriteTag(WriteTagRequest {
            text: String::from(text),
            tray_id,
        }));
    }

    pub fn read_tag(&self) {
        self.tag_operation.signal(TagOperation::ReadTag(ReadTagRequest {}));
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn SpoolTagObserver>>) {
        self.observers.push(observer);
    }

    pub fn notify_tag_status(&self, status: Status) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_status(&status);
        }
    }

    pub fn notify_pn532_status(&self, status: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_pn532_status(status);
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone)]
struct WriteTagRequest {
    text: String,
    tray_id: usize,
}

#[derive(Debug, Clone)]
struct ReadTagRequest {}

#[derive(Debug, Clone)]
enum TagOperation {
    WriteTag(WriteTagRequest),
    ReadTag(ReadTagRequest),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Failure {
    TagWriteFailure,
    TagReadFailure,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Status {
    FoundTagNowReading,
    FoundTagNowWriting,
    WriteSuccess(/*tray_id*/ usize, /* Descriptor Written*/ String),
    ReadSuccess(String),
    Failure(Failure),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Uid {
    data: [u8;10],
    len: usize,
}
impl Uid {
    pub fn from(src: &[u8]) -> Self {
        let mut myself = Self {
            data: [0u8;10],
            len: src.len(),
        };
        myself.data[..src.len()].copy_from_slice(src);
        myself
    }
    pub fn uid(&self) -> &[u8] {
        &self.data[..self.len]
    }
}
/////////////////////////////////////////////////////////////////////////////////////////////////

pub fn init(
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
    spawner: Spawner,
) -> Rc<RefCell<SpoolTag>> {
    let tag_operation = mk_static!(
        embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>,
        embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, TagOperation>::new()
    );

    let spool_tag_rc = Rc::new(RefCell::new(SpoolTag {
        tag_operation,
        observers: Vec::new(),
    }));

    spawner
        .spawn(nfc_task(spool_tag_rc.clone(), spi_device, irq, tag_operation))
        .ok();

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
    let retries = 59;
    for retry in 0..=retries {
        if retry % 20 == 0 {
            if retry != 0 {
                term_error!("Challenging PN532 Initialization ({})", retries);
            }
            pn532.wake_up().await.unwrap();
            Timer::after(Duration::from_millis(100)).await
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
        spool_tag_rc.borrow().notify_pn532_status(false);
        return;
    } else {
        spool_tag_rc.borrow().notify_pn532_status(true);
    }

    if let Ok(fw) = pn532
        .process(&pn532::Request::GET_FIRMWARE_VERSION, 4, embassy_time::Duration::from_millis(200))
        .await
    {
        trace!("PN532 Firmware Version response: {:?}", fw);
        term_info!("Established communication with Tag Reader ({})", successful_retry);
        spool_tag_rc.borrow().notify_pn532_status(true);
    } else {
        term_error!("Failed to communicate with Tag Reader");
        spool_tag_rc.borrow().notify_pn532_status(false);
        return;
    }

    info!("Entering wait for tag loop in nfc task");

    let mut curr_operation_with_tag = Some(TagOperation::ReadTag(ReadTagRequest {}));

    let mut previous_operation_tag = None;
    let mut previous_operation_tag_last_seen_time = Instant::now();

    let mut last_seen_tag = None;
    let mut last_seen_tag_time = Instant::from_ticks(0);

    let mut in_switch_operation = false;

    loop {
        debug!("Waiting for Tag");

        let copied_res = {
            // This complexity is to deal with compiler borrowing issues in select, copying the result to be separate from pn532 internal buffers (only because of select and the borrow checker)
            let res = select(
                tag_operation.wait(),
                pn532.process(&pn532::Request::INLIST_ONE_ISO_A_TARGET, 17, Duration::from_secs(60)),
            )
            .await;

            match res {
                Either::First(ref f) => Either::First(f.clone()),
                Either::Second(s) => match s {
                    Ok(response) => {
                        let number_of_tags_found = response[0];
                        if number_of_tags_found == 0 { // no tag found, shouldn't occure
                            continue;
                        }
                        if number_of_tags_found != 1 {
                            error!("Found more than one tag ({number_of_tags_found}), ignoring all");
                            continue;
                        }
                        let uid_len = response[5] as usize;
                        if uid_len < 4 || 6+uid_len > response.len() {
                            error!("Error with tag response, uid_len doen't seem right {uid_len}");
                            continue;
                        } 
                        let uid = &response[6..6+uid_len];
                        Either::Second(Ok(Uid::from(uid)))
                    }
                    Err(e) => Either::Second(Err(e)),
                },
            }
        };

        match copied_res {
            Either::First(tag_operation) => {
                curr_operation_with_tag = Some(tag_operation);
                in_switch_operation = true;
                previous_operation_tag_last_seen_time = last_seen_tag_time;
                previous_operation_tag = last_seen_tag;
            }
            Either::Second(tag_res) => {
                match tag_res {
                    Ok(uid) => {
                        debug!("Found Tag with uid : {:?}", uid);
                        last_seen_tag = Some(uid);
                        last_seen_tag_time = Instant::now();
                        if in_switch_operation {
                            if previous_operation_tag == last_seen_tag && previous_operation_tag_last_seen_time.elapsed().as_millis() < 500 {
                                previous_operation_tag_last_seen_time = Instant::now();
                                continue;
                            } else {
                                in_switch_operation = false;
                                previous_operation_tag = None;
                            }
                        }

                        match &curr_operation_with_tag.as_ref() {
                            Some(TagOperation::WriteTag(write_tag_reuest)) => {
                                spool_tag_rc.borrow().notify_tag_status(Status::FoundTagNowWriting);
                                let tag_uid = URL_SAFE_NO_PAD.encode(last_seen_tag.as_ref().unwrap().uid());
                                let final_tag_text = write_tag_reuest.text.replace(TAG_PLACEHOLDER, &tag_uid);
                                match crate::nfc::write_ndef_url_record(&mut pn532, &final_tag_text, Duration::from_secs(2)).await {
                                    Ok(_num_bytes_written) => {
                                        debug!("Wrote {} to tag", final_tag_text);
                                        spool_tag_rc
                                            .borrow()
                                            .notify_tag_status(Status::WriteSuccess(write_tag_reuest.tray_id, final_tag_text));
                                        curr_operation_with_tag = Some(TagOperation::ReadTag(ReadTagRequest {}));
                                        previous_operation_tag_last_seen_time = Instant::now();
                                        previous_operation_tag = last_seen_tag;
                                        in_switch_operation = true;
                                    }
                                    Err(e) => {
                                        term_error!("Error writing to tag {:?}", e);
                                        spool_tag_rc.borrow().notify_tag_status(Status::Failure(Failure::TagWriteFailure));
                                    }
                                }
                            }
                            Some(TagOperation::ReadTag(_read_tag_request)) => {
                                spool_tag_rc.borrow().notify_tag_status(Status::FoundTagNowReading);
                                match crate::nfc::read_ndef_record(&mut pn532, Duration::from_millis(500)).await {
                                    Ok(read_record) => {
                                        debug!("{}", read_record.url_payload());
                                        spool_tag_rc.borrow().notify_tag_status(Status::ReadSuccess(read_record.url_payload()));
                                        curr_operation_with_tag = Some(TagOperation::ReadTag(ReadTagRequest {}));
                                        previous_operation_tag_last_seen_time = Instant::now();
                                        previous_operation_tag = last_seen_tag;
                                        in_switch_operation = true;
                                    }
                                    Err(e) => {
                                        error!("Error reading tag {:?}", e);
                                        spool_tag_rc.borrow().notify_tag_status(Status::Failure(Failure::TagReadFailure));
                                    }
                                }
                            }
                            None => (),
                        }
                    }
                    Err(e) => match e {
                        pn532::Error::TimeoutResponse => {
                            // This is not really an error - every 60 seconds (which is timeout provided, will take place)
                            // previous_operation_tag = None;
                        }
                        pn532::Error::TimeoutAck => {
                            // Doesn't seem to be an error in case of using IRQ?
                            warn!("TimeoutAck Error, Error?");
                            // previous_operation_tag = None; // ??
                        }
                        pn532::Error::BadAck => {
                            // Doesn't seem to be an error in case of using IRQ?
                            warn!("BadAck Error, Error?");
                            // previous_operation_tag = None; // ??
                        }
                        _ => {
                            warn!("Error when waiting for tag {:?}", e);
                            match &curr_operation_with_tag {
                                Some(TagOperation::WriteTag(_write_tag_request)) => {
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(Failure::TagWriteFailure));
                                }
                                Some(TagOperation::ReadTag(_read_tag_request)) => {
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(Failure::TagReadFailure));
                                }
                                None => {}
                            }
                        }
                    },
                }
            }
        }
    }
}
