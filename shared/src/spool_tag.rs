use core::cell::RefCell;

use alloc::{rc::Rc, string::String, vec::Vec};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;

use framework::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    ndef,
    pn532_ext::{self},
};

pub const TAG_PLACEHOLDER: &str = "$tag-id$";

pub struct SpoolTag {
    tag_operation: &'static embassy_sync::signal::Signal<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        TagOperation,
    >,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolTagObserver>>>,
}

pub trait SpoolTagObserver {
    fn on_tag_status(&mut self, status: &Status);
    fn on_pn532_status(&mut self, status: bool);
    fn on_emulated_tag_read(&mut self);
}

impl SpoolTag {
    pub fn emulate_tag(&self, url: &str) {
        self.tag_operation
            .signal(TagOperation::EmulateUrlTag(EmulateUrlTagRequest {
                url: String::from(url),
            }));
    }

    pub fn write_tag(&self, text: &str, tray_id: usize) {
        self.tag_operation
            .signal(TagOperation::WriteTag(WriteTagRequest {
                text: String::from(text),
                tray_id,
            }));
    }

    pub fn read_tag(&self) {
        self.tag_operation
            .signal(TagOperation::ReadTag(ReadTagRequest {}));
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

    pub fn notify_emulated_tag_read(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_emulated_tag_read();
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
struct EmulateUrlTagRequest {
    url: String,
}

#[derive(Debug, Clone)]
enum TagOperation {
    WriteTag(WriteTagRequest),
    ReadTag(ReadTagRequest),
    EmulateUrlTag(EmulateUrlTagRequest),
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
    data: [u8; 10],
    len: usize,
}
impl Uid {
    pub fn from(src: &[u8]) -> Self {
        let mut myself = Self {
            data: [0u8; 10],
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
    spi_device: ExclusiveDevice<
        esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
        esp_hal::gpio::Output<'static>,
        embassy_time::Delay,
    >,
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
        .spawn(nfc_task(
            spool_tag_rc.clone(),
            spi_device,
            irq,
            tag_operation,
        ))
        .ok();

    spool_tag_rc
}

// Had to specify the I2C1 because can't have generic tasks in embassy, maybe there's some workaround in the following link
//https://github.com/embassy-rs/embassy/issues/1837
#[embassy_executor::task]
pub async fn nfc_task(
    spool_tag_rc: Rc<RefCell<SpoolTag>>,
    spi_device: ExclusiveDevice<
        esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
        esp_hal::gpio::Output<'static>,
        embassy_time::Delay,
    >,
    irq: esp_hal::gpio::Input<'static>,
    tag_operation: &'static embassy_sync::signal::Signal<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        TagOperation,
    >,
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

    let mut pn532: pn532::Pn532<_, _, 64> = pn532::Pn532::new(interface, timer);
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
        .process(
            &pn532::Request::GET_FIRMWARE_VERSION,
            4,
            embassy_time::Duration::from_millis(200),
        )
        .await
    {
        trace!("PN532 Firmware Version response: {:?}", fw);
        term_info!(
            "Established communication with Tag Reader ({})",
            successful_retry
        );
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

    let mut last_seen_tag;

    let mut in_switch_operation = false;

    loop {
        if let Some(TagOperation::EmulateUrlTag(url_request)) = &curr_operation_with_tag {
            debug!("Emulating Tag");

            let ndef_record = ndef::Record::new_url_record(&url_request.url);
            let mut uid = [0u8; 3];
            getrandom::getrandom(&mut uid).unwrap();
            let res = select(
                tag_operation.wait(),
                pn532_ext::emulate_tag(&mut pn532, ndef_record, Some(uid), Duration::from_secs(60)),
            )
            .await;
            match res {
                Either::First(new_tag_operation) => {
                    debug!("Received request for operation {new_tag_operation:?} from now on");
                    if !matches!(new_tag_operation, TagOperation::EmulateUrlTag(_)) {
                        // Need to wake up the device from powedown during tag emulation
                        // First try fails on TimeOutAck and 2nd pass.
                        // Using the inrelease command because this is what used in the Elechouse C++ code, not sure the command is relevant
                        // debug!(">>>>> Swtiching from emulate, so doing inrelease to wake up, first time will be an error");
                        const RELEASE_TAG_ALL: pn532::Request<1> =
                            pn532::Request::new(pn532::requests::Command::InRelease, [0]);
                        let mut power_up_ok = false;
                        for _i in 0..5 {
                            let res = pn532
                                .process(&RELEASE_TAG_ALL, 1, Duration::from_millis(10))
                                .await;
                            if res.is_ok() {
                                power_up_ok = true;
                                break;
                            }
                        }
                        if !power_up_ok {
                            error!("Failed to power on PN532 after emulating tag");
                        }
                    }
                    curr_operation_with_tag = Some(new_tag_operation.clone());
                    in_switch_operation = true;
                    continue;
                }
                Either::Second(emulate_res) => match emulate_res {
                    Ok(tag_fully_read) => {
                        if tag_fully_read {
                            debug!("Emulated Tag fully read");
                            // Let phone time to move away, so wallet app won't pop when moving to read (maybe better do that on switch to read based on time of emulate scan)
                            Timer::after_millis(1000).await;
                            // We notify after so whatever client does is not based on assumption that it has moved to read, maybe need to add events on switch of mode and realy on that rather on the commands on the client
                            spool_tag_rc.borrow().notify_emulated_tag_read();
                        } else {
                            debug!("Emulated Tag not (fully) read");
                        }
                    }
                    Err(err) => {
                        error!("Error while emulating tag : {err:?}");
                    }
                },
            }
        } else {
            debug!("Waiting for Tag");
            let res = select(
                tag_operation.wait(),
                pn532.process(
                    &pn532::Request::INLIST_ONE_ISO_A_TARGET,
                    17,
                    Duration::from_secs(60),
                ),
            )
            .await;

            let tag_res = match res {
                Either::First(new_tag_operation) => {
                    debug!("Received request for operation {new_tag_operation:?} from now on");
                    curr_operation_with_tag = Some(new_tag_operation.clone());
                    in_switch_operation = true;
                    // previous_operation_tag_last_seen_time = last_seen_tag_time;
                    // previous_operation_tag = last_seen_tag;
                    continue;
                }
                // This section is to avoid borrow checker issues, creating a res that does not require keeping borrowed PN532
                Either::Second(s) => match s {
                    Ok(response) => {
                        debug!("Full inlist response: {response:x?}");
                        let number_of_tags_found = response[0];
                        if number_of_tags_found == 0 {
                            // no tag found, shouldn't occure
                            error!("PN532 inlisted 0 tags found, should not occur!");
                            continue;
                        }
                        if number_of_tags_found != 1 {
                            error!(
                                "Found more than one tag ({number_of_tags_found}), ignoring all"
                            );
                            continue;
                        }
                        let uid_len = response[5] as usize;
                        if uid_len < 4 || 6 + uid_len > response.len() {
                            error!("Error with tag response, uid_len doen't seem right {uid_len}");
                            continue;
                        }
                        let uid = &response[6..6 + uid_len];
                        Ok(Uid::from(uid))
                    }
                    Err(e) => Err(e),
                },
            };

            // Now the real work
            match tag_res {
                Ok(uid) => {
                    debug!("Found Tag with uid : {:x?}", uid.data);
                    last_seen_tag = Some(uid);
                    // last_seen_tag_time = Instant::now();
                    if in_switch_operation {
                        if previous_operation_tag == last_seen_tag
                            && previous_operation_tag_last_seen_time.elapsed().as_millis() < 500
                        {
                            previous_operation_tag_last_seen_time = Instant::now();
                            debug!("Same as previously acted upon tag, ignoring");
                            continue;
                        } else {
                            in_switch_operation = false;
                            previous_operation_tag = None;
                        }
                    }

                    match &curr_operation_with_tag.as_ref() {
                        Some(TagOperation::WriteTag(write_tag_reuest)) => {
                            debug!("Performing write tag operation");
                            spool_tag_rc
                                .borrow()
                                .notify_tag_status(Status::FoundTagNowWriting);
                            let tag_uid =
                                URL_SAFE_NO_PAD.encode(last_seen_tag.as_ref().unwrap().uid());
                            let final_tag_text =
                                write_tag_reuest.text.replace(TAG_PLACEHOLDER, &tag_uid);
                            match crate::nfc::write_ndef_url_record(
                                &mut pn532,
                                &final_tag_text,
                                Duration::from_secs(2),
                            )
                            .await
                            {
                                Ok(_num_bytes_written) => {
                                    debug!("Wrote {} to tag", final_tag_text);
                                    spool_tag_rc
                                        .borrow()
                                        .notify_tag_status(Status::WriteSuccess(
                                            write_tag_reuest.tray_id,
                                            final_tag_text,
                                        ));
                                    curr_operation_with_tag =
                                        Some(TagOperation::ReadTag(ReadTagRequest {}));
                                    previous_operation_tag_last_seen_time = Instant::now();
                                    previous_operation_tag = last_seen_tag;
                                    in_switch_operation = true;
                                }
                                Err(e) => {
                                    term_error!("Error writing to tag {:?}", e);
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagWriteFailure,
                                    ));
                                }
                            }
                        }
                        Some(TagOperation::ReadTag(_read_tag_request)) => {
                            debug!("Performing read tag operation");
                            spool_tag_rc
                                .borrow()
                                .notify_tag_status(Status::FoundTagNowReading);
                            match crate::nfc::read_ndef_record(
                                &mut pn532,
                                Duration::from_millis(2000),
                            )
                            .await
                            {
                                Ok(read_record) => {
                                    debug!("{}", read_record.url_payload());
                                    spool_tag_rc.borrow().notify_tag_status(Status::ReadSuccess(
                                        read_record.url_payload(),
                                    ));
                                    curr_operation_with_tag =
                                        Some(TagOperation::ReadTag(ReadTagRequest {}));
                                    previous_operation_tag_last_seen_time = Instant::now();
                                    previous_operation_tag = last_seen_tag;
                                    in_switch_operation = true;
                                }
                                Err(e) => {
                                    error!("Error reading tag {:?}", e);
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagReadFailure,
                                    ));
                                }
                            }
                        }

                        Some(TagOperation::EmulateUrlTag(_)) => {
                            panic!("Arrived to EmulateUrlTag while scanning - Software Bug (1)!!!");
                        }
                        None => (),
                    }
                }
                Err(e) => {
                    match e {
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
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagWriteFailure,
                                    ));
                                }
                                Some(TagOperation::ReadTag(_read_tag_request)) => {
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagReadFailure,
                                    ));
                                }
                                Some(TagOperation::EmulateUrlTag(_)) => {
                                    panic!("Arrived to EmulateUrlTag while scanning - Software Bug (2)!!!");
                                }
                                None => {}
                            }
                        }
                    }
                }
            }
        }
    }
}
