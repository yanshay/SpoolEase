use core::cell::RefCell;

use alloc::{
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;

use esp_hal::gpio::Input;
use framework::prelude::*;
use hashbrown::HashMap;
use ndef_rs::{NdefMessage, NdefRecord, TNF, payload::UriPayload};
use serde::{Deserialize, Serialize};
use serde_with::{hex::Hex, serde_as};

use crate::{
    nfc::{self, NfcTagType, get_nfc_tag_type},
    pn532_ext,
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
    fn is_tag_in_store(&mut self, tag_id: &[u8]) -> bool;
}

impl SpoolTag {
    pub fn emulate_tag(&self, url: &str) {
        self.tag_operation
            .signal(TagOperation::EmulateUrlTag(EmulateUrlTagRequest {
                url: String::from(url),
            }));
    }

    pub fn write_tag(&self, text: &str, check_uid: Option<Vec<u8>>, cookie: String) {
        self.tag_operation
            .signal(TagOperation::WriteTag(WriteTagRequest {
                text: String::from(text),
                check_uid,
                cookie,
            }));
    }

    pub fn erase_tag(&self, check_uid: Option<Vec<u8>>, cookie: String) {
        self.tag_operation
            .signal(TagOperation::EraseTag { check_uid, cookie });
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
    pub fn is_tag_in_store(&self, tag_uid: &[u8]) -> bool {
        // can check with only one observer so checking first
        if let Some(weak_observer) = self.observers.first() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().is_tag_in_store(tag_uid)
        } else {
            false
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone)]
struct WriteTagRequest {
    text: String,
    check_uid: Option<Vec<u8>>,
    cookie: String,
}

#[derive(Debug, Clone)]
struct ReadTagRequest {}

#[derive(Debug, Clone)]
struct EmulateUrlTagRequest {
    url: String,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
enum TagOperation {
    WriteTag(WriteTagRequest),
    ReadTag(ReadTagRequest),
    EmulateUrlTag(EmulateUrlTagRequest),
    #[allow(dead_code)]
    EraseTag {
        check_uid: Option<Vec<u8>>,
        cookie: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Failure {
    TagWriteFailure(String),
    TagEraseFailure(String),
    TagReadFailure,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ReadResult {
    TagInStore {
        #[serde_as(as = "Hex")]
        uid: Vec<u8>,
    },
    NDEF {
        #[serde_as(as = "Hex")]
        uid: Vec<u8>,
        #[serde_as(as = "Option<Hex>")]
        message: Option<Vec<u8>>,
    },
    BambulabTag {
        #[serde_as(as = "Hex")]
        uid: Vec<u8>,
        #[serde_as(as = "Option<hashbrown::HashMap<_, Hex>>")]
        data: Option<HashMap<i32, Vec<u8>>>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Status {
    FoundTagNowReading,
    FoundTagNowWriting,
    FoundTagNowErasing,
    WriteSuccess(
        /* Descriptor Written*/ String,
        /* Cookie */ String,
    ),
    ReadSuccess(ReadResult),
    EraseSuccess,
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
    freq_after_init: u32,
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
        .spawn_heap(nfc_task(
            spool_tag_rc.clone(),
            spi_device,
            irq,
            tag_operation,
            freq_after_init,
        ))
        .ok();

    spool_tag_rc
}

// Had to specify the I2C1 because can't have generic tasks in embassy, maybe there's some workaround in the following link
//https://github.com/embassy-rs/embassy/issues/1837
// #[embassy_executor::task]
async fn nfc_task(
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
    freq_after_init: u32,
) {
    Timer::after_millis(1000).await;

    // To switch from using IRQ to not using IRQ:
    //   1. use None::<pn532::spi::NoIRQ> instead of Some(irq)
    //   2. in sam_configuration set use_irq_pin to false (maybe not required)
    let mut interface = pn532::spi::SPIInterface {
        spi: spi_device,
        irq: Some(irq), // : None::<Input<'_>>
        // irq: None::<pn532::spi::NoIRQ>,
    };

    let timer = crate::pn532_ext::Esp32TimerAsync::new();

    let mut pn532: pn532::Pn532<_, _, 64> = pn532::Pn532::new(interface, timer);
    // pn532.wake_up().await.unwrap();

    term_info!("Initializing Tag Reader");

    let mut initialization_succeeded = false;
    let mut successful_retry = 0;
    let retries = 239;
    for retry in 0..=retries {
        if retry % 10 == 0 {
            if retry != 0 {
                term_error!("Still Initializing PN532 ({})", retry);
            }
        }
        // if retry %2 == 0 {
        Timer::after(Duration::from_millis(50)).await;
        pn532.wake_up().await.unwrap();
        Timer::after(Duration::from_millis(200)).await;
        // }
        if let Err(e) = pn532
            .process(
                &pn532::Request::sam_configuration(pn532::requests::SAMMode::Normal, true),
                0,
                embassy_time::Duration::from_millis(50), // longer? shorter?
            )
            .await
        {
            // if retry < 5 {
            //     term_error!("Error initializing Tag Reader {:?}", e);
            // }
            // Error, just wait before retrying
            if retry != retries {
                Timer::after(Duration::from_millis(100)).await;
            } else {
                term_error!("Error initializing Tag Reader {:?}", e);
                term_error!("  > Check Troubleshooting Guide in GitHub repo !!!");
            }
        } else {
            info!("Initialized Tag Reader successfully");
            initialization_succeeded = true;
            successful_retry = retry;
            break;
        }
    }

    // Change frequency to after init frequency
    let post_init_config = esp_hal::spi::master::Config::default()
            .with_frequency(esp_hal::time::Rate::from_khz(freq_after_init))
            .with_mode(esp_hal::spi::Mode::_0)
            .with_read_bit_order(esp_hal::spi::BitOrder::LsbFirst)
            .with_write_bit_order(esp_hal::spi::BitOrder::LsbFirst);
    pn532.interface.spi.bus_mut().apply_config(&post_init_config).unwrap();
    Timer::after_millis(500).await;

    if !initialization_succeeded {
        term_error!("Failed to initialize Tag Reader");
        spool_tag_rc.borrow().notify_pn532_status(false);
        return;
    } else {
        spool_tag_rc.borrow().notify_pn532_status(true);
    }

    match pn532
        .process(
            &pn532::Request::GET_FIRMWARE_VERSION,
            4,
            embassy_time::Duration::from_millis(200),
        )
        .await
    {
        Ok(fw) => {
            trace!("PN532 Firmware Version response: {:?}", fw);
            term_info!(
                "Established communication with Tag Reader ({})",
                successful_retry
            );
            spool_tag_rc.borrow().notify_pn532_status(true);
        }
        Err(err) => {
            term_error!("Failed to communicate with Tag Reader : {:?}", err);
            spool_tag_rc.borrow().notify_pn532_status(false);
            return;
        }
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

            let ndef_record = match NdefRecord::builder()
                .tnf(TNF::WellKnown)
                .payload(&UriPayload::from_string(&url_request.url))
                .build()
            {
                Ok(v) => v,
                Err(err) => {
                    panic!("Error generating NDEF record to emulate: {err:?}");
                }
            };

            let message = NdefMessage::from(ndef_record);

            let mut uid = [0u8; 3];
            getrandom::getrandom(&mut uid).unwrap();
            let res = select(
                tag_operation.wait(),
                pn532_ext::emulate_tag(&mut pn532, message, Some(uid), Duration::from_secs(60)),
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
            // debug!("Waiting for Tag");
            let res = select(
                tag_operation.wait(),
                pn532.process(
                    &pn532::Request::INLIST_ONE_ISO_A_TARGET,
                    17,
                    Duration::from_secs(60),
                ),
            )
            .await;
            let mut nfc_tag_type = NfcTagType::Unknown;
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
                        // debug!("Full inlist response: {response:x?}");
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
                        nfc_tag_type = get_nfc_tag_type(response);
                        if nfc_tag_type == NfcTagType::Unknown {
                            error!("Unknown tag type, this tag can't be used");
                            continue;
                        }
                        // else {
                        //     debug!("Scanned tag type is: {nfc_tag_type:?}");
                        // }
                        let uid_len = response[5] as usize;
                        if uid_len < 4 || 6 + uid_len > response.len() {
                            error!("Error with tag response, uid_len doesn't seem right {uid_len}");
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
                    // debug!("Found Tag with uid : {:x?}", uid.uid());
                    last_seen_tag = Some(uid);
                    // last_seen_tag_time = Instant::now();
                    if in_switch_operation {
                        if previous_operation_tag == last_seen_tag
                            && previous_operation_tag_last_seen_time.elapsed().as_millis() < 500
                        {
                            previous_operation_tag_last_seen_time = Instant::now();
                            // debug!("Same as previously acted upon tag, ignoring");
                            continue;
                        } else {
                            debug!(
                                "Scanned a new {nfc_tag_type:?} Tag with uid : {:x?}",
                                uid.uid()
                            );
                            in_switch_operation = false;
                            previous_operation_tag = None;
                        }
                    }
                    match &curr_operation_with_tag.as_ref() {
                        Some(TagOperation::WriteTag(write_tag_reuest)) => {
                            debug!("Performing write tag operation");
                            if nfc_tag_type != NfcTagType::NTAG {
                                spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                    Failure::TagWriteFailure(
                                        "Can't Encode MIFARE/Unknown Type Tags\nOnly NTAGs can be Encoded".to_string(),
                                    ),
                                ));
                                continue;
                            }
                            spool_tag_rc
                                .borrow()
                                .notify_tag_status(Status::FoundTagNowWriting);
                            let found_uid = last_seen_tag.as_ref().unwrap().uid();
                            if let Some(check_uid) = &write_tag_reuest.check_uid
                                && check_uid.as_slice() != found_uid
                            {
                                spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                    Failure::TagWriteFailure(
                                        "Tag Not Linked to Spool\nUse Correct Tag".to_string(),
                                    ),
                                ));
                                continue;
                            }
                            // let tag_uid =
                            //     URL_SAFE_NO_PAD.encode(last_seen_tag.as_ref().unwrap().uid());
                            let hex_tag = hex::encode_upper(uid.uid());
                            let final_tag_text =
                                write_tag_reuest.text.replace(TAG_PLACEHOLDER, &hex_tag);
                            match nfc::write_ndef_url_record(
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
                                            final_tag_text,
                                            write_tag_reuest.cookie.clone(),
                                        ));
                                    curr_operation_with_tag =
                                        Some(TagOperation::ReadTag(ReadTagRequest {}));
                                    previous_operation_tag_last_seen_time = Instant::now();
                                    previous_operation_tag = last_seen_tag;
                                    in_switch_operation = true;
                                }
                                Err(e) => {
                                    error!("Error writing to tag {:?}", e);
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagWriteFailure(
                                            "Error Writing to Tag\nTry Again".to_string(),
                                        ),
                                    ));
                                }
                            }
                        }
                        Some(TagOperation::ReadTag(_read_tag_request)) => {
                            debug!("Performing read tag operation");
                            spool_tag_rc
                                .borrow()
                                .notify_tag_status(Status::FoundTagNowReading);
                            let mut final_read_result = None;
                            // if spool_tag_rc.borrow().tag_operation.

                            if spool_tag_rc.borrow().is_tag_in_store(uid.uid()) {
                                final_read_result = Some(ReadResult::TagInStore {
                                    uid: uid.uid().to_vec(),
                                });
                            } else if nfc_tag_type == NfcTagType::NTAG {
                                let ntag_res = match crate::nfc::read_ndef_payload(
                                    &mut pn532,
                                    Duration::from_millis(2000),
                                )
                                .await
                                {
                                    Ok(v) => Ok(v),
                                    Err(nfc::Error::NotNdefFormatted) => Ok(None),
                                    Err(e) => Err(e),
                                };
                                match ntag_res {
                                    // TODO: combine
                                    Ok(read_ndef_message_payload) => {
                                        if let Some(payload) = &read_ndef_message_payload {
                                            debug!("Read NDEF message size {}", payload.len());
                                        } else {
                                            debug!("No NDEF message in tag");
                                        }
                                        final_read_result = Some(ReadResult::NDEF {
                                            uid: uid.uid().to_vec(),
                                            message: read_ndef_message_payload, // could be None if no NDEF found
                                        });
                                    }
                                    Err(e) => {
                                        error!("Error reading tag {:?}", e);
                                        spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                            Failure::TagReadFailure,
                                        ));
                                    }
                                }
                            } else if nfc_tag_type == NfcTagType::MifareClassic1K {
                                // try as Bambulab
                                match crate::nfc::read_bambulab_payload(
                                    &mut pn532,
                                    Duration::from_millis(2000),
                                    uid.uid(),
                                )
                                .await
                                {
                                    Ok(bambulab_payload) => {
                                        info!(
                                            "Scanned and read a Bambu Lab Spool Tag with UID {:X?}",
                                            uid.uid()
                                        );
                                        fn display_hex_ascii(data: &HashMap<i32, Vec<u8>>) {
                                            for (block_num, chunk) in data.iter() {
                                                let mut line = alloc::format!("[{block_num:2}]");
                                                for &b in chunk {
                                                    let item = // if b.is_ascii_graphic() || b == b' '
                                                    // {
                                                    //     &alloc::format!(" {} ", b as char)
                                                    // } else {
                                                        &alloc::format!("{b:02X} ");
                                                    // };
                                                    line.push_str(item);
                                                }
                                                debug!("{}", line);
                                            }
                                        }
                                        display_hex_ascii(&bambulab_payload);
                                        final_read_result = Some(ReadResult::BambulabTag {
                                            uid: uid.uid().to_vec(),
                                            data: Some(bambulab_payload), // could be None if no NDEF found
                                        });
                                    }
                                    Err(err) => match err {
                                        nfc::Error::NotBambulabTag => {
                                            // not Bambulab use for now only UID
                                            // TODO: In the future, handle NDEF on Mifare
                                            final_read_result = Some(ReadResult::NDEF {
                                                uid: uid.uid().to_vec(),
                                                message: None,
                                            })
                                        }
                                        _ => {
                                            error!("Error reading tag {:?}", err);
                                            spool_tag_rc.borrow().notify_tag_status(
                                                Status::Failure(Failure::TagReadFailure),
                                            );
                                        }
                                    },
                                }
                            } else if nfc_tag_type == NfcTagType::MifareClassic4K {
                                // not Bambulab use for now only UID
                                // TODO: In the future, handle NDEF on Mifare
                                final_read_result = Some(ReadResult::NDEF {
                                    uid: uid.uid().to_vec(),
                                    message: None,
                                })
                            } else {
                                error!(
                                    "Unknown tag type scanned with tag_res: {tag_res:x?}, not supported, please report on GitHub/Discord"
                                );
                            };

                            if let Some(final_read_result) = final_read_result {
                                // there is a result to return
                                spool_tag_rc
                                    .borrow()
                                    .notify_tag_status(Status::ReadSuccess(final_read_result));
                                curr_operation_with_tag =
                                    Some(TagOperation::ReadTag(ReadTagRequest {}));
                                previous_operation_tag_last_seen_time = Instant::now();
                                previous_operation_tag = last_seen_tag;
                                in_switch_operation = true;
                            }
                        }
                        Some(TagOperation::EraseTag {
                            check_uid,
                            cookie: _,
                        }) => {
                            debug!("Performing erase tag operation");
                            spool_tag_rc
                                .borrow()
                                .notify_tag_status(Status::FoundTagNowErasing);
                            let found_uid = last_seen_tag.as_ref().unwrap().uid();
                            if let Some(check_uid) = check_uid
                                && check_uid.as_slice() != found_uid
                            {
                                spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                    Failure::TagWriteFailure(
                                        "Not the Tag to Erase\nUse Correct Tag".to_string(),
                                    ),
                                ));
                                continue;
                            }
                            match nfc::erase_ndef_tag(&mut pn532, Duration::from_secs(2)).await {
                                Ok(()) => {
                                    spool_tag_rc
                                        .borrow()
                                        .notify_tag_status(Status::EraseSuccess);
                                    curr_operation_with_tag =
                                        Some(TagOperation::ReadTag(ReadTagRequest {}));
                                    previous_operation_tag_last_seen_time = Instant::now();
                                    previous_operation_tag = last_seen_tag;
                                    in_switch_operation = true;
                                }
                                Err(e) => {
                                    error!("Error erasing to tag {:?}", e);
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagEraseFailure(
                                            "Error Erasing Tag\nTry Again".to_string(),
                                        ),
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
                                        Failure::TagWriteFailure(
                                            "Error Scanning for Tag".to_string(),
                                        ),
                                    ));
                                }
                                Some(TagOperation::EraseTag {
                                    check_uid: _,
                                    cookie: _,
                                }) => {
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagEraseFailure(
                                            "Error Scanning for Tag".to_string(),
                                        ),
                                    ));
                                }
                                Some(TagOperation::ReadTag(_read_tag_request)) => {
                                    spool_tag_rc.borrow().notify_tag_status(Status::Failure(
                                        Failure::TagReadFailure,
                                    ));
                                }
                                Some(TagOperation::EmulateUrlTag(_)) => {
                                    panic!(
                                        "Arrived to EmulateUrlTag while scanning - Software Bug (2)!!!"
                                    );
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
