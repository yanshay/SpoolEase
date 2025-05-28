// TODO:
// Deal with when to clear tag information, when we know spool taken out
// Deal with when to copy tag information between trays if only some data change but we know the spool is there
use crate::{
    app_config::PrinterConfig, settings::MAX_NUM_PRINTERS, ssdp::{SSDPInfo, SSDPPubSubChannel}
};
use alloc::{
    borrow::Cow,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use core::{cell::RefCell, str::FromStr};
use derivative::Derivative;
use embassy_futures::select::{select, Either};
use embassy_net::Ipv4Address;
use embassy_sync::{
    blocking_mutex::{
        raw::{CriticalSectionRawMutex, NoopRawMutex},
        Mutex,
    },
    channel::Channel,
    pubsub::PubSubChannel,
};
use embassy_time::{with_timeout, Duration, Timer};
use hashbrown::HashMap;
use mqttrust::QoS;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use shared::spool_tag::TAG_PLACEHOLDER;

use framework::prelude::*;

use crate::{
    app_config::AppConfig,
    bambu_api::{self, PrintAms, PrintTray},
    my_mqtt::BufferedMqttPacket,
};

const FILAMENT_URL_PREFIX: &str = "https://info.filament3d.org/";

pub struct BambuPrinter {
    pub bambu_model: Option<Rc<RefCell<Self>>>,
    pub log_filter: log::LevelFilter,
    pub printer_number: usize,       // number of printer in user's configuration,
    pub printer_index: usize, // index of printer in the array of printers, if a config is not good and skipped, then index would be different than number
    pub printer_serial: String, // mandatory, so configured is the same as actual
    pub printer_access_code: String, // mandatory, so configured is the same as actual
    pub configured_printer_name: Option<String>,
    pub configured_printer_ip: Option<Ipv4Address>,
    pub auto_restore_k: bool,
    pub printer_name: String,
    pub printer_selector_name: String, // configured_printer_name or if not set then printer_serial which is always available
    pub printer_ip: Ipv4Address,
    pub printer_uuid_to_encode: String,
    pub printer_connectivity_ok: Option<bool>,
    pub nozzle_diameter: Option<String>,
    inner_ams_trays: [Tray; 16],
    inner_virt_tray: Tray,
    ams_trays_dirty: [bool; 16],
    virty_tray_dirty: bool,
    pub calibrations: HashMap<String, HashMap<i32, Calibration>>,
    write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
    restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn BambuPrinterObserver>>>,
    app_config: Rc<RefCell<AppConfig>>,
    tray_exist_bits: Option<u32>,
    tray_read_done_bits: Option<u32>,
    tray_reading_bits: Option<u32>,
    pub ams_exist_bits: Option<u32>,
    printer_was_disconnected: bool,
    pending_k_restore_sequence: bool,
}

pub trait BambuPrinterObserver {
    fn on_trays_update(&mut self, bambu_printer: &mut BambuPrinter, prev_tray_reading_bits: Option<u32>, new_tray_reading_bits: Option<u32>);
    fn on_printer_connect_status(&self, bambu_printer: &mut BambuPrinter, status: bool);
}

// Special access to trays fields for dirty tracking
impl BambuPrinter {
    pub fn ams_trays(&self) -> &[Tray; 16] {
        &self.inner_ams_trays
    }
    pub fn set_ams_tray(&mut self, index: usize, tray: Tray) {
        if self.inner_ams_trays[index] != tray {
            self.inner_ams_trays[index] = tray;
            self.ams_trays_dirty[index] = true;
        }
    }
    pub fn update_ams_tray<F>(&mut self, index: usize, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        let prev_tray = self.inner_ams_trays[index].clone();
        f(&mut self.inner_ams_trays[index]);
        if prev_tray != self.inner_ams_trays[index] {
            self.ams_trays_dirty[index] = true;
        }
    }
    pub fn virt_tray(&self) -> &Tray {
        &self.inner_virt_tray
    }
    pub fn set_virt_tray(&mut self, tray: Tray) {
        if tray != self.inner_virt_tray {
            self.inner_virt_tray = tray;
            self.virty_tray_dirty = true;
        }
    }
    pub fn update_virt_tray<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        let prev_tray = self.inner_virt_tray.clone();
        f(&mut self.inner_virt_tray);
        if prev_tray != self.inner_virt_tray {
            self.virty_tray_dirty = true;
        }
    }
    pub fn update_any_tray<F>(&mut self, index: usize, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        if index == 254 {
            self.update_virt_tray(f);
        } else {
            self.update_ams_tray(index, f);
        }
    }

    pub fn init_printer_persistent_state(&mut self, state: PrinterPersistentState) {
       self.inner_ams_trays = state.ams_trays.into_owned();
       self.inner_virt_tray = state.virt_tray.into_owned();
       self.nozzle_diameter = state.nozzle_diameter;
    }

    pub async fn load_printer_state(framework: &Rc<RefCell<Framework>>, printer: &Rc<RefCell<BambuPrinter>>) {
        let path = Self::printer_state_file_path(&printer.borrow().printer_serial);
        let printer_number = printer.borrow().printer_number;
        let file_store = framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        match file_store.read_file_str(&path).await {
            Ok(state_str) => {
                match serde_json::from_str::<PrinterPersistentState>(&state_str) {
                    Ok(printer_state) => {
                        printer.borrow_mut().init_printer_persistent_state(printer_state);
                        term_info!("[{}] Restored printer state from SDCard", printer_number);
                    }
                    Err(err) => {
                        term_error!("[{}] Failed to parse printer state in file {} : {}", printer_number, path, err);
                    }
                }
            }
            Err(err) => {
                error!("[{printer_number}] Error reading state file {path} : {err}");
            }
        }
    }

    pub async fn store_printer_state(framework: &Rc<RefCell<Framework>>, printer: &Rc<RefCell<BambuPrinter>>) {
        let mut printer_state_str = None;
        let mut printer_serial = None;
        {
            let printer_borrow = printer.borrow();
            if printer_borrow.auto_restore_k && printer_borrow.pending_k_restore_sequence {
                // don't change store until restoring k is done
                return;
            }
            if printer_borrow.ams_trays_dirty.iter().any(|&v| v) || printer_borrow.virty_tray_dirty {
                printer_serial = Some(printer_borrow.printer_serial.clone());
                let printer_state = PrinterPersistentState {
                    ams_trays: Cow::Borrowed(printer_borrow.ams_trays()),
                    virt_tray: Cow::Borrowed(printer_borrow.virt_tray()),
                    nozzle_diameter: printer_borrow.nozzle_diameter.clone(),
                };
                printer_state_str = Some(serde_json::to_string(&printer_state).unwrap());
            }
        }
        if let (Some(printer_state_str), Some(printer_serial)) = (printer_state_str, printer_serial) {
            let file_store = framework.borrow().file_store();
            let path = Self::printer_state_file_path(&printer_serial);
            info!("[{}] Storing printer state to {}", printer.borrow().printer_number, path);
            // need to clean dirty before we store since it awaits,
            // but store might fail, and in that case we need to bring back dirty (add the dirty we had)
            // so let's save it to bring back in case of error
            let ams_trays_dirty = printer.borrow().ams_trays_dirty;
            let virt_tray_dirty = printer.borrow().virty_tray_dirty;
            printer.borrow_mut().virty_tray_dirty = false;
            printer.borrow_mut().ams_trays_dirty.fill(false);
            let mut file_store = file_store.lock().await;
            match file_store.create_write_file_str(&path, &printer_state_str).await {
                Ok(_) => { }
                Err(err) => {
                    let mut printer_borrow = printer.borrow_mut();
                    printer_borrow.virty_tray_dirty |= virt_tray_dirty;
                    for (x, y) in printer_borrow.ams_trays_dirty.iter_mut().zip(&ams_trays_dirty) { *x |= *y };
                    error!("[{}] Failed to store printer restart state : {err}", printer_borrow.printer_index);
                }
            }
        }
    }
    pub fn printer_state_file_path(printer_serial: &str) -> String {
        let len = printer_serial.len();
        let file_ext = &printer_serial[len - 3..];
        let file_name = &printer_serial[len - 11..len - 3];
        format!("/state/{file_name}.{file_ext}/startup.jsn")
    }
}

#[allow(clippy::too_many_arguments)]
impl BambuPrinter {
    pub fn new(
        printer_number: usize,
        printer_index: usize,
        printer_serial: &str,
        printer_access_code: &str,
        printer_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
    ) -> Rc<RefCell<BambuPrinter>> {
        let myself = Self::internal_new(
            printer_number,
            printer_index,
            printer_serial,
            printer_access_code,
            printer_name,
            printer_ip,
            auto_restore_k,
            write_packets,
            app_config,
            restart_printer,
            log_filter,
        );
        let myself_rc = Rc::new(RefCell::new(myself));
        myself_rc.borrow_mut().bambu_model = Some(myself_rc.clone());
        myself_rc
    }

    fn internal_new(
        printer_number: usize,
        printer_index: usize,
        printer_serial: &str,
        printer_access_code: &str,
        printer_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
    ) -> Self {
        let unknown = Tray {
            state: TrayState::Unknown,
            filament: Filament::Unknown,
            k_from_tray: None,
            cali_idx: None,
            tag_info: None,
        };

        let array = printer_serial.as_bytes();
        let key: &[u8; 16] = b"SpoolEaseIsGreat"; // doesn't really matter, just can't ever change
        let hasher = siphasher::sip::SipHasher24::new_with_key(key);
        let hashed_serial = hasher.hash(array);
        let hashed_encoded_serial = URL_SAFE_NO_PAD.encode(hashed_serial.to_le_bytes());
        let printer_uuid_to_encode = hashed_encoded_serial;

        // Define a user oriented name for selection
        let printer_selector_name = if let Some(printer_name) = &printer_name {
            printer_name.clone()
        } else {
            printer_serial.to_string()
        };

        Self {
            bambu_model: None,
            printer_number,
            printer_index,
            printer_serial: String::from(printer_serial),
            printer_access_code: String::from(printer_access_code),
            configured_printer_ip: *printer_ip,
            configured_printer_name: printer_name.clone(),
            auto_restore_k,
            printer_ip: printer_ip.unwrap_or(Ipv4Address::new(0, 0, 0, 0)),
            printer_name: printer_name.clone().unwrap_or("Unknown".to_string()),
            printer_selector_name,
            printer_uuid_to_encode,
            printer_connectivity_ok: None,
            nozzle_diameter: None,
            inner_ams_trays: [
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
            ],
            inner_virt_tray: unknown,
            ams_trays_dirty: [false; 16],
            virty_tray_dirty: false,
            calibrations: HashMap::new(),
            write_packets,
            observers: Vec::new(),
            app_config,
            tray_exist_bits: None,
            tray_read_done_bits: None,
            tray_reading_bits: None,
            ams_exist_bits: None,
            restart_printer,
            log_filter,
            printer_was_disconnected: true,
            pending_k_restore_sequence: true,
        }
    }

    pub fn reset_printer(&mut self) {
        let empty = Self::internal_new(
            self.printer_number,
            self.printer_index,
            &self.printer_serial,
            &self.printer_access_code,
            &self.configured_printer_name,
            &self.configured_printer_ip,
            self.auto_restore_k,
            self.write_packets.clone(),
            self.app_config.clone(),
            self.restart_printer.clone(),
            self.log_filter,
        );
        *self = Self {
            observers: self.observers.clone(),
            bambu_model: self.bambu_model.clone(),
            ..empty
        };
        self.restart_printer.signal(1);
    }

    pub fn report_printer_connectivity(&mut self, status: bool) {
        if self.printer_connectivity_ok == Some(true) && !status {
            self.printer_was_disconnected = true;
            self.pending_k_restore_sequence = true;
        }
        self.printer_connectivity_ok = Some(status);
        self.notify_printer_connect_status(status);
    }
    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn BambuPrinterObserver>>) {
        self.observers.push(observer);
    }
    pub fn _clear_all_subscriptions(&mut self) {
        self.observers.clear();
    }

    pub fn get_tag_k_for_current_nozzle(&self, tag_info: &TagInformation) -> String {
        if let Some(matched_calibration) = self.get_tag_matching_calibration_for_current_nozzle(tag_info) {
            return matched_calibration.k_value.clone();
        }
        "".to_string()
    }

    fn get_calibration(&self, nozzle_diameter: &str, cali_idx: i32) -> Option<&Calibration> {
        let nozzle_calibrations = self.calibrations.get(nozzle_diameter)?;
        let calibration = nozzle_calibrations.get(&cali_idx)?;
        Some(calibration)
    }

    fn get_cali_k_value(&self, nozzle_diameter: &str, cali_idx: i32) -> Option<String> {
        self.get_calibration(nozzle_diameter, cali_idx)
            .map(|calibration| calibration.k_value.clone())
        // let nozzle_calibrations = match self.calibrations.get(nozzle_diameter) {
        //     Some(calibrations) => calibrations,
        //     None => return None,
        // };
        // let calibration = match nozzle_calibrations.get(&cali_idx) {
        //     Some(calibration) => calibration,
        //     None => return None,
        // };
        //
        // Some(calibration.k_value.clone())
    }

    pub fn get_tray_resolved_k_value(&self, tray: &Tray) -> String {
        let mut k_result = "(0.020)".to_string();
        if let Some(k_from_tray) = &tray.k_from_tray {
            k_result = format!("({k_from_tray:.3})");
        }
        if let Some(cali_idx) = tray.cali_idx {
            if let Some(nozzle_diameter) = &self.nozzle_diameter {
                if let Some(k_value) = self.get_cali_k_value(nozzle_diameter, cali_idx) {
                    let k_float = f32::from_str(&k_value).unwrap_or_default();
                    k_result = format!("{:.3}", k_float);
                }
            };
        }
        k_result
    }

    pub fn get_tray_calibration(&self, tray: &Tray) -> Option<&Calibration> {
        if let Some(cali_idx) = tray.cali_idx {
            if let Some(nozzle_diameter) = &self.nozzle_diameter {
                return self.get_calibration(nozzle_diameter, cali_idx);
            }
        }
        None
    }

    fn tray_from_update(&self, tray_update: &PrintTray) -> Result<Option<Tray>, String> {
        if let (Some(tray_type_update), Some(tray_info_idx_update), Some(tray_color_update)) =
            (&tray_update.tray_type, &tray_update.tray_info_idx, &tray_update.tray_color)
        {
            // Remember: tray_type is the material(PLA, PETG, etc), tray_info_idx is the filament_id (some code)
            // when there is data in the tray data then
            let mut new_tray = Tray::default(); // Everything is unknown at start
                                                // when adding filament to a tray when the printer doesn't know what is inside, tray_info_idx and tray_type
                                                // will arrive as empty, so this is a fine condition. In the past I thought it couldn't be.
                                                // I'm still unclear when filament settings are cleared form tray.

            // Sometimes the tray arrives with tray_type, tray_info_idx, color filled with 00000000 (also last two are 00),  which may be an error, not sure
            // if strange issues seem to appear, check that out and maybe deal with that case
            // TODO: ends with 0 is actually valid. If setting only filament type and not color it is FFFFFF00
            // Need to deal with that, probably also in the GUI, maybe it's for transparent
            if tray_type_update.ends_with("00") {
                warn!("[{}] ???? tray_type with 00 suffix", self.printer_number);
                debug!("[{}] {:?}", self.printer_number, tray_update);
                return Err("tray_type junk".to_string());
            }
            if tray_info_idx_update.starts_with("00") { // tray_info_idx CAN end with 00, but not start with 00 afaik
                // might end with 00, so checking if starts with 00
                warn!("[{}] ???? tray_info_idx with 00 suffix", self.printer_number);
                debug!("[{}] {:?}", self.printer_number, tray_update);
                return Err("tray_info_idx junk".to_string());
            }

            if !tray_color_update.ends_with("FF") {
                warn!("[{}] ???? tray_color with Not FF suffix, either color not set in Bambustudio or transparent", self.printer_number);
                debug!("[{}] {:?}", self.printer_number, tray_update);
                // return Err("tray_color junk".to_string());
            }

            new_tray.filament = if tray_type_update.is_empty() {
                Filament::Unknown
            } else {
                Filament::Known(FilamentInfo::from(tray_update))
            };

            new_tray.cali_idx = tray_update.cali_idx;
            new_tray.k_from_tray = tray_update.k;

            Ok(Some(new_tray))
        } else {
            Ok(None)
        }
    }

    // Arguments:
    //   old_tray is the tray as known prior to this update
    //   tray_update is the tray information received from the printer
    //   tray_id is the tray_id in case of AMS or None in case of External spool
    // Return value:
    //   if tray not changed from old_tray, or something wrong with tray, returns None
    pub fn get_updated_tray(&self, old_tray: &Tray, tray_update: Option<&PrintTray>, tray_id: Option<usize>) -> Option<Tray> {
        if let Some(tray_id) = tray_id {
            // AMS tray
            if let Some(tray_exist_bits) = self.tray_exist_bits {
                let tray_exist = ((tray_exist_bits >> tray_id) & 0x01) != 0;

                if tray_exist {
                    let tray_reading = self.tray_reading_bits.is_some_and(|x| ((x >> tray_id) & 0x01) != 0);
                    let tray_read_done = self.tray_read_done_bits.is_some_and(|x| ((x >> tray_id) & 0x01) != 0);

                    let mut new_tray = if let Some(tray_update) = tray_update {
                        if let Ok(tray_update) = self.tray_from_update(tray_update) {
                            // TODO: in case I a tray w/o any information (but with exist bit) then I just copy old, is it ok?
                            tray_update.unwrap_or_else(|| {
                                let mut new_tray = old_tray.clone();
                                new_tray.state = TrayState::Empty;
                                new_tray
                            })
                        } else {
                            // Update is bad so ignoring it
                            return None;
                        }
                    } else {
                        // If no update data for try (but tray exist) copy previous tray
                        let mut new_tray = old_tray.clone();
                        new_tray.state = TrayState::Empty;
                        new_tray
                    };
                    new_tray.state = TrayState::Spool;
                    new_tray.tag_info = old_tray.tag_info.clone(); // TODO: can 'take' if it work properly (need to mut old_tray)

                    if tray_reading {
                        new_tray.state = TrayState::Reading;
                    }
                    if tray_read_done {
                        new_tray.state = TrayState::Ready;
                    }
                    Some(new_tray)
                } else {
                    // In case the tray is empty (so no ready bits), we still want to keep the filamen-info of the tray, but set it as empty
                    // special case handling (different than Bambustudio).
                    // we remember historical color, K, etc (which the printer also remembers, just doesn't report)
                    let mut new_tray = old_tray.clone();
                    new_tray.state = TrayState::Empty;
                    new_tray.tag_info = None; // if spool is removed, erase tag info, we can't tell what happens
                    Some(new_tray)
                }
            } else {
                //  if tray_exist_bits not available yet, then tray should be unknown
                Some(Tray::unknown())
            }
        } else {
            // External Tray
            if let Some(tray_update) = tray_update {
                if tray_update.id.is_none() {
                    // This is a special case of message I saw that arrives only for external tray, with id: None
                    // It includes only informtion updates to certain parts, unlike how AMS work where a complete update
                    // is received.
                    // It might be required handling in cases when color change is driven without the MQTT command, maybe on X1C through display. Don't know yet.
                    // Can support it, the easy way, with push_all request in such case which will reupdate everything.
                    self.request_full_update_sync();
                    None

                    // Or by handling every bit there in a tedios way (code below is only partial)
                    // let mut new_tray = old_tray.clone();
                    // new_tray.k_from_tray = tray_update.k.or(old_tray.k_from_tray);
                    // new_tray.cali_idx = tray_update.cali_idx.or(old_tray.cali_idx);
                    // new_tray.filament.
                    // ... more
                    //
                    // return Some(new_tray);
                } else if let Ok(tray_update) = self.tray_from_update(tray_update) {
                    if let Some(mut new_tray) = tray_update {
                        // External tray with data is always considered Ready
                        if matches!(new_tray.filament, Filament::Unknown) {
                            new_tray.state = TrayState::Empty;
                            new_tray.tag_info = None;
                        } else {
                            new_tray.state = TrayState::Ready;
                            new_tray.tag_info = old_tray.tag_info.clone(); // TODO: can take if work properly
                        }
                        return Some(new_tray);
                    } else {
                        // Empty tray data means tray empty in case of external
                        return Some(Tray::unknown());
                    }
                } else {
                    // Error in tray information, don't change anything
                    None
                }
            } else {
                // No new information, don't change anything
                None
            }
        }
    }

    pub fn get_ams_and_tray_id(tray_id: usize) -> (usize, usize) {
        if tray_id < 254 {
            let ams_id = tray_id / 4;
            let ams_tray_id = tray_id - ams_id * 4;
            (ams_id, ams_tray_id)
        } else {
            (254, tray_id)
        }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__push_status__ams(&mut self, ams: &PrintAms) -> bool {
        let mut change_made = false;

        // first check which ams's exist
        if let Some(ams_exist_bits) = &ams.ams_exist_bits {
            let ams_exist_bits = ams_exist_bits.parse::<u32>();
            if let Ok(ams_exist_bits) = ams_exist_bits {
                if self.ams_exist_bits.is_none() || self.ams_exist_bits.unwrap() != ams_exist_bits {
                    self.ams_exist_bits = Some(ams_exist_bits);
                    change_made = true;
                }
            }
        }

        // tray_exist_bits seem to be bits for all ams systems (due to where it is in the struct hierrchy)
        // and the lowest most bits seem to be the first ams trays bits
        // for now handle only the first ams
        // if tray_exist_bits are specified it means they may have changed, so update them
        // the stored value is the one we'll reference later

        // tray_exist_bits - which trays contain a spool
        if let Some(tray_exist_bits) = &ams.tray_exist_bits {
            if let Ok(tray_exist_bits) = u32::from_str_radix(tray_exist_bits, 16) {
                if self.tray_exist_bits != Some(tray_exist_bits) {
                    self.tray_exist_bits = Some(tray_exist_bits);
                    change_made = true;
                }
            }
        }
        // tray_read_done - which trays (from those that exist) that have been "read" (meaning ready from ams perspective)
        if let Some(tray_read_done_bits) = &ams.tray_read_done_bits {
            if let Ok(tray_read_done_bits) = u32::from_str_radix(tray_read_done_bits, 16) {
                if self.tray_read_done_bits != Some(tray_read_done_bits) {
                    self.tray_read_done_bits = Some(tray_read_done_bits);
                    change_made = true;
                }
            }
        }
        // tray_reading - which trays (from those that exist) that are currently being "read" (meaning ams is rotating them to get them ready)
        if let Some(tray_reading_bits) = &ams.tray_reading_bits {
            if let Ok(tray_reading_bits) = u32::from_str_radix(tray_reading_bits, 16) {
                if self.tray_reading_bits != Some(tray_reading_bits) {
                    self.tray_reading_bits = Some(tray_reading_bits);
                    change_made = true;
                }
            }
        }

        for tray_id in 0..self.ams_trays().len() {
            let (ams_id, ams_tray_id) = BambuPrinter::get_ams_and_tray_id(tray_id);
            let ams_id_str = format!("{ams_id}");
            let source_tray = if let Some(amss) = &ams.ams {
                let ams = amss.iter().find(|v| v.id == ams_id_str);
                if let Some(ams_data) = ams {
                    ams_data.tray.iter().find(|v| v.id == Some(ams_tray_id as u32))
                } else {
                    None
                }
            } else {
                None
            };
            let old_tray = &self.ams_trays()[tray_id];
            let new_tray = self.get_updated_tray(old_tray, source_tray, Some(tray_id));
            if let Some(new_tray) = new_tray {
                change_made = true;
                self.set_ams_tray(tray_id, new_tray);
            }
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__push_status__vt_tray(&mut self, v_tray: &PrintTray) -> bool {
        let old_tray = self.virt_tray().clone();
        let new_tray = self.get_updated_tray(&old_tray, Some(v_tray), None);
        if let Some(new_tray) = new_tray {
            self.set_virt_tray(new_tray);
            return true;
        }
        false
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__ams_filament_setting(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;

        // updating ONLY filament and not state for the theoretical case when filament is set externally when there isn't a spool
        // theoretically possible if want to supssport that in this app using nfc as a source for example
        if let Some(tray_id) = print.tray_id {
            let tray_info_idx = print.tray_info_idx.as_ref().cloned().unwrap_or_default();
            let new_filament = if tray_info_idx.is_empty() {
                // not even filament type available, this means a reset
                Filament::Unknown
            } else {
                Filament::Known(FilamentInfo {
                    tray_info_idx,
                    tray_type: print.tray_type.as_ref().cloned().unwrap_or_default(),
                    tray_color: print.tray_color.as_ref().cloned().unwrap_or_default(),
                    nozzle_temp_max: print.nozzle_temp_max.unwrap_or(250),
                    nozzle_temp_min: print.nozzle_temp_min.unwrap_or(190),
                })
            };
            if tray_id == 254 {
                // External tray handling
                // Handle external tray
                if new_filament == Filament::Unknown {
                    self.update_virt_tray(|virt_tray| {
                        virt_tray.state = TrayState::Empty;
                        virt_tray.tag_info = None;
                    });
                } else {
                    self.update_virt_tray(|virt_tray| {
                        virt_tray.state = TrayState::Ready;
                    });
                }
                self.update_virt_tray(|virt_tray| {
                    virt_tray.filament = new_filament;
                });
            } else {
                // AMS Tray handling
                // Handle AMS tray
                if let Some(ams_id) = print.ams_id {
                    // no change to tray state in case of AMS
                    let ams_id = usize::try_from(ams_id).unwrap();
                    self.update_ams_tray(ams_id * 4 + usize::try_from(tray_id).unwrap(), |ams_tray| {
                        ams_tray.filament = new_filament;
                        ams_tray.k_from_tray = None;
                    });
                    // Is this correct to do?
                }
            }
            change_made = true;
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_sel(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        if let (Some(tray_id), Some(cali_idx)) = (&print.tray_id, &print.cali_idx) {
            if *tray_id >= 0 {
                let tray_id: usize = (*tray_id).try_into().unwrap();
                self.update_any_tray(tray_id, |tray| {
                    tray.cali_idx = if *cali_idx == -1 { None } else { Some(*cali_idx) };
                });

                change_made = true;
            }
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_get(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        let nozzle_diameter = match &print.nozzle_diameter {
            Some(nozzle_diameter) => nozzle_diameter,
            None => return false,
        };
        // filament_id either empty string (so entire list) or something
        let filament_id = match &print.filament_id {
            Some(filament_id) => filament_id,
            None => return false,
        };

        if let Some(ref filaments) = print.filaments {
            change_made = true;
            let nozzle_calibrations = self.calibrations.entry_ref(nozzle_diameter).or_default(); //insert(HashMap::new()) let calibration = Calibration::from(filament);
            if filament_id.is_empty() {
                nozzle_calibrations.clear();
            } else {
                nozzle_calibrations.retain(|_k, v| &v.filament_id != filament_id);
            }
            for filament in filaments {
                let calibration = Calibration::from(filament);
                nozzle_calibrations.insert(filament.cali_idx, calibration);
            }
        }

        change_made
    }

    pub fn process_print_message(&mut self, print: &bambu_api::PrintData) -> bool {
        if let Some(sequence_id) = &print.sequence_id {
            if self.log_filter >= log::Level::Debug {
                debug!("[{}] -> Message {}", self.printer_number, sequence_id);
            }
        } else if self.log_filter >= log::Level::Warn {
            warn!("[{}] -> Message with No sequence_id ?", self.printer_number);
        }
        // important: Can't issue event from here because this method is called with a mut reference (even if behind RefCell)
        // Therefore, to issue an event need to call update_ams_trays_done afterwards through a non mut reference (so not borrow_mut if refcell)
        //   in order to issue the event on observers
        let mut change_made = false;
        if let Some(command) = &print.command {
            if command == "push_status" {
                // get a snapshot of trays before, to later be able to update cali_idx if removed
                let full_push_status = print.ams.is_some() && print.vt_tray.is_some();
                let prev_state = if full_push_status && self.auto_restore_k && self.printer_was_disconnected {
                    // TODO: To save memory (a few kb's, might be needed in the future) copy from ams_trays only the data requried and not entire tray
                    Some((self.ams_trays().to_vec(), self.virt_tray().clone(), self.nozzle_diameter.clone()))
                } else {
                    None
                };
                let mut nozzle_diameter_change_made = false;
                let mut ams_change_made = false;
                let mut vt_tray_change_made = false;
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    let old_nozzle_diameter = self.nozzle_diameter.clone();
                    self.nozzle_diameter = Some(nozzle_diameter.clone());
                    nozzle_diameter_change_made = old_nozzle_diameter != self.nozzle_diameter;
                }
                if let Some(ams) = &print.ams {
                    ams_change_made = self.process_print_message__push_status__ams(ams);
                }
                if let Some(v_tray) = &print.vt_tray {
                    vt_tray_change_made = self.process_print_message__push_status__vt_tray(v_tray);
                }

                if full_push_status && self.auto_restore_k && self.printer_was_disconnected {
                    self.printer_was_disconnected = false;
                    let mut triggered_k_restore_sequence = false;
                    if let Some(prev_state) = prev_state {
                        if self.ams_trays()[..] != prev_state.0 || *self.virt_tray() != prev_state.1 {
                            let spawner = self.app_config.borrow().framework.borrow().spawner;
                            spawner
                                .spawn(fix_k_on_restart(
                                    self.bambu_model.as_ref().unwrap().clone(),
                                    prev_state.0, // ams_trays
                                    prev_state.1, // virt_tray
                                    prev_state.2, // nozzle_diameter
                                ))
                                .ok();
                            triggered_k_restore_sequence = true;
                        }
                    } 
                    if !triggered_k_restore_sequence {
                        // no need to restore since trays received are same as should
                        term_info!("[{}] Pressure advance (k) ok at printer startup", self.printer_number);
                        self.pending_k_restore_sequence = false;
                    }
                }
                change_made = nozzle_diameter_change_made || ams_change_made || vt_tray_change_made;

            } else if command == "ams_filament_setting" {
                change_made = self.process_print_message__ams_filament_setting(print)
            } else if command == "extrusion_cali_set" || command == "extrusion_cali_del" {
                // trigger request command for cali_get (request, not response)
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    self.fetch_filament_calibrations(nozzle_diameter);
                }
                change_made = true;
            } else if command == "extrusion_cali_sel" {
                // update the tray with the new k factor
                change_made = self.process_print_message__extrusion_cali_sel(print)
            } else if command == "extrusion_cali_get" {
                // TODO: Check: distinguish between command that was sent and the result, which are structured the same
                // here we want to process only the results (the one that includes the list of filaments )
                change_made = self.process_print_message__extrusion_cali_get(print);
            }
            if self.log_filter >= log::Level::Debug {
                debug!("[{}]    {command} message", &self.printer_number);
            }
        }
        change_made
    }

    pub fn notify_printer_connect_status(&mut self, status: bool) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_printer_connect_status(self, status);
        }
    }

    pub fn update_ams_trays_done(&mut self, prev_trays_reading_bits: Option<u32>, new_trays_reading_bits: Option<u32>) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer
                .borrow_mut()
                .on_trays_update(self, prev_trays_reading_bits, new_trays_reading_bits);
        }
    }

    // TODO: Unify sending messages, no need for two functions

    pub fn publish_payload(&self, payload: String) {
        if self.log_filter >= log::Level::Debug {
            debug!("[{}] MQTT Publish: {}", self.printer_number, payload);
        }

        let topic_name = format!("device/{}/request", &self.printer_serial);
        let topic_name = topic_name.as_str();

        let packet = mqttrust::Packet::Publish(mqttrust::Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            pid: Some(mqttrust::encoding::v4::Pid::new()),
            retain: false,
            topic_name,
            payload: payload.as_bytes(),
        });
        let message = BufferedMqttPacket::try_from(packet).unwrap();
        let _ = self.write_packets.try_send(message);
    }

    // TODO: Unify sending messages, no need for two functions

    pub async fn publish_payload_async(
        printer_serial: &String,
        printer_number: usize,
        log_filter: log::LevelFilter,
        write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
        payload: String,
    ) {
        if log_filter >= log::Level::Debug {
            debug!("[{}] MQTT Publish: {}", printer_number, payload);
        }
        let topic_name = format!("device/{}/request", printer_serial);
        let topic_name = topic_name.as_str();

        let packet = mqttrust::Packet::Publish(mqttrust::Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            pid: Some(mqttrust::encoding::v4::Pid::new()),
            retain: false,
            topic_name,
            payload: payload.as_bytes(),
        });
        let message = BufferedMqttPacket::try_from(packet).unwrap();
        write_packets.send(message).await;
    }

    pub fn request_full_update_sync(&self) {
        let cmd = crate::bambu_api::PushAllCommand::new();
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);
    }

    pub async fn request_full_update_async(
        printer_serial: &String,
        printer_number: usize,
        log_filter: log::LevelFilter,
        write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
    ) {
        let cmd = crate::bambu_api::PushAllCommand::new();
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, printer_number, log_filter, write_packets, payload).await;
    }

    pub fn fetch_filament_calibrations(&self, nozzle_diameter: &str) {
        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);
    }

    pub async fn fetch_filament_calibrations_async(
        printer_serial: &String,
        printer_number: usize,
        log_filter: log::LevelFilter,
        write_packets: Rc<embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>>,
        nozzle_diameter: &str,
    ) {
        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, printer_number, log_filter, write_packets, payload).await;
    }

    pub fn set_tray_filament(&mut self, tray_id: i32, tag_info: &TagInformation) {
        let ams_id: u32;
        let ams_tray_id;

        if tray_id == 254 {
            ams_id = 255;
            ams_tray_id = 254
        } else {
            ams_id = u32::try_from(tray_id).unwrap() / 4;
            ams_tray_id = tray_id % 4;
        }
        // setting_id can't be extracted from just tray information, it's available only if there is a cali_idx on the tray.
        // on the other hand it is required to set tray information.
        // So if we have calibration information, we send the setting_id from there. If we don't we send None and it seems to work
        // The slicer have the setting-if from the data it has when it selects everything together

        let matching_calibration = self.get_tag_matching_calibration_for_current_nozzle(tag_info);

        let setting_id = matching_calibration.as_ref().map(|calibration| calibration.setting_id.as_str());

        if let Some(filament) = &tag_info.filament {
            let cmd = crate::bambu_api::AmsFilamentSettingCommand::new(
                ams_id,
                ams_tray_id, // here we need the tray_id within the specific ams
                &filament.tray_info_idx,
                setting_id,
                &filament.tray_type,
                &filament.tray_color,
                filament.nozzle_temp_min,
                filament.nozzle_temp_max,
            );
            let payload = serde_json::to_string_pretty(&cmd).unwrap();
            self.publish_payload(payload);

            let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
                &self.nozzle_diameter.clone().unwrap_or_default(),
                tray_id,                 // here we need the original tray_id
                &filament.tray_info_idx, // tray_info_idx is filament_id in this command
                if let Some(calibration) = &matching_calibration {
                    Some(calibration.cali_idx)
                } else {
                    Some(-1)
                },
            );
            let payload = serde_json::to_string_pretty(&cmd).unwrap();
            self.publish_payload(payload);
            self.update_any_tray(tray_id as usize, |tray| {
                tray.tag_info = Some(tag_info.clone());
            });
        }
    }

    pub fn get_tag_matching_calibration_for_current_nozzle(&self, tag_info: &TagInformation) -> Option<Calibration> {
        // cali_idx, setting_id
        // Now process it

        // Now we have a list of calibrations from the filament.
        // We need to select for each nozzle size in the printer (even if no value in filament settings), a matching calibration from the printer, if possible.
        // We can either match a perfect match or we can deduce of no perfect match
        // We can deduce for a certain nozzle also based on information we have on other nozzle diameters in the filaments calibrations

        // within the same nozzle/printer-type setting_id & filament_id will be the same
        // setting_id differs across nozzles/printer-types
        // filament_id is the same across nozzles/printer-types

        // Go through nozzle sizes 0.2, 0.4, 0.6 and 0.8
        //    Go through printer calibrations of the iterated-nozzle-size (if there are any) with the same filamentm_id and:
        //    First, look at the calibration for that nozzle size in the filament calibrations.
        // Same printer-type/nozzle (so same setting-id)
        //      A1- check if any printer calibration match to the setting_id & pa-profile-name (uncleaned)- if it is there's an exact match
        //      A2- check if any printer calibration has the same setting_id && setting-name (cleaned) - if it is there's a match (similar match)
        //      A3- check if any printer calibration same setting-id && same k value - if it is there's a match (not exact)
        //      Afuture: 4- check if any printer calibration has a similar name & close k - if it is there's a match (similar match)
        //    Next, go through calibrations of other nozzle sizes in the filament calibrations
        //      B1- check if any printer calibration has only the same setting-name exactly (ignore setting-id)
        //      B2- check if any printer calibration has only the same setting-name cleaned (ignore setting-id)
        //      B3- check if any printer calibration has a similar name - if it is then there's a match
        //    If all failed, then no match
        //

        fn clean_compare(a: &str, b: &str) -> bool {
            // Create filtered iterators that:
            // 1. Skip whitespace
            // 2. Skip chars_to_ignore
            // 3. Convert to lowercase for case-insensitive comparison
            let chars_to_ignore = &['.', '-', ','];
            let iter_a = a
                .chars()
                .filter(|&c| !c.is_whitespace() && !chars_to_ignore.contains(&c))
                .flat_map(|c| c.to_lowercase());

            let iter_b = b
                .chars()
                .filter(|&c| !c.is_whitespace() && !chars_to_ignore.contains(&c))
                .flat_map(|c| c.to_lowercase());

            // Compare the filtered iterators
            iter_a.eq(iter_b)
        }

        fn similar_compare(_s1: &str, _s2: &str) -> bool {
            // TODO: implement Metaphone Double
            false
        }

        let tag_filament_id = if let Some(filament_info) = &tag_info.filament {
            &filament_info.tray_info_idx
        } else {
            return None;
        };

        let printer_nozzle = if let Some(nozzle_diameter) = &self.nozzle_diameter {
            nozzle_diameter
        } else {
            return None;
        };

        let printer_calibrations = self.calibrations.get(printer_nozzle)?;

        // If there is filament calibration for that nozzle size (assumption there can be only one, which makes sense)
        if let Some(filament_calibration) = tag_info.calibrations.get(printer_nozzle) {
            // there could be several tht match filament_id, setting_id (even common)
            let same_filament_id_nozzle_printer_type_calibrations = printer_calibrations
                .iter()
                .filter(|&c| c.1.filament_id == *tag_filament_id && c.1.setting_id == filament_calibration.setting_id);

            // A1
            if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
            {
                return Some(calibration_match.1.clone());
            // Starting here, we can improve by finding several that match and select the closest
            // A2
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
            {
                return Some(calibration_match.1.clone());
            // A3
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| printer_calibration.1.k_value == filament_calibration.k_value)
            // because we are on same printer-type/nozzle this should be ok
            {
                return Some(calibration_match.1.clone());
            // A4 : TODO: use metaphone double to compare strings
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
            {
                return Some(calibration_match.1.clone());
            }
        };

        for (_, filament_calibration) in &tag_info.calibrations {
            // TODO: When tag has several calibrations for different nozzles, here we can iterate over them as well
            // (so compare man to many) since name from another nozzle diameter could help finding for another nozzle
            // size, it's just name mathing
            let same_filament_id_printer_calibrations = printer_calibrations.iter().filter(|&c| c.1.filament_id == *tag_filament_id);
            // B1
            if let Some(calibration_match) = same_filament_id_printer_calibrations
                .clone()
                .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
            {
                return Some(calibration_match.1.clone());
            }
            // Starting here, we can improve by finding several that match and select the closest
            // B2
            else if let Some(calibration_match) = same_filament_id_printer_calibrations
                .clone()
                .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
            {
                return Some(calibration_match.1.clone());
            // B3
            } else if let Some(calibration_match) = same_filament_id_printer_calibrations
                .clone()
                .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
            {
                return Some(calibration_match.1.clone());
            }
        }

        None
    }

    pub fn get_tag_info_to_encode(&self, tray_id: usize) -> Result<TagInformation, String> {
        let tray = if tray_id == 254 {
            self.virt_tray()
        } else if (0..self.ams_trays().len()).contains(&tray_id) {
            &self.ams_trays()[tray_id]
        } else {
            return Err("Unexpected Software Error (1)".to_string());
        };
        // This is NOT good without full multi-printer tag support, so when tag is only a single printer
        //  because we could mix info from different printers
        //  later we'll add that
        // if there's a tag in that tray, lets start with that to include any info it has inside
        // let mut tag_info = tray.tag_info.as_ref().unwrap_or(&TagInformation::default()).clone();

        let mut tag_info = TagInformation::default();
        // Take the color from what the use actually sees, potentially different from what is in the tag in that tray
        if let Filament::Known(filament_info) = &tray.filament {
            tag_info.filament = Some(filament_info.clone());
        } else {
            return Err("Unknown Filament in Slot".to_string());
        }
        // Now take the calibration of current nozzle from the tray as well
        if let (Some(curr_nozzle_diameter), Some(tray_cali_idx)) = (&self.nozzle_diameter, tray.cali_idx) {
            if let Some(nozzle_calibrations) = self.calibrations.get(curr_nozzle_diameter) {
                if let Some(calibration) = &nozzle_calibrations.get(&tray_cali_idx) {
                    tag_info.calibrations.insert(curr_nozzle_diameter.clone(), (*calibration).clone());
                }
            }
        }

        Ok(tag_info)
    }
}

#[derive(Derivative)]
#[derivative(PartialEq)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Tray {
    pub state: TrayState,
    pub filament: Filament,
    pub k_from_tray: Option<f32>,
    pub cali_idx: Option<i32>,
    #[derivative(PartialEq = "ignore")]
    pub tag_info: Option<TagInformation>, // calibration for nozzles
}

impl Tray {
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            state: TrayState::Empty,
            ..Default::default()
        }
    }
    pub fn unknown() -> Self {
        Self {
            state: TrayState::Unknown,
            ..Default::default()
        }
    }
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub enum TrayState {
    #[default]
    Unknown,
    Empty,     // Empty - known to be empty
    Spool,     // When a spool is placed into the slot
    Reading,   // Reading - during the process of inserting spool into AMS
    Ready,     // Ready - there is a spool, it is not loaded to the extruder now
    Loading,   // Loading - during the process of loading into the extruder
    Unloading, // Unloading - during the process of unloading from the extruder
    Loaded,    // Loaded - in the extruder
}

////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
pub enum Error {
    ParseError,
    MissingFields,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub enum Filament {
    #[default]
    Unknown,
    Known(FilamentInfo),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FilamentInfo {
    pub tray_info_idx: String, // e.g. "GFL99"
    pub tray_type: String,     // e.g. "PLA"
    pub tray_color: String,    // e.g. "2323F7FF"
    pub nozzle_temp_max: u32,  // e.g. 250
    pub nozzle_temp_min: u32,  // w.g. 190
}

impl FilamentInfo {
    pub fn new() -> Self {
        Self {
            tray_info_idx: String::from(""),
            tray_type: String::from(""),
            tray_color: String::from(""),
            nozzle_temp_max: 0,
            nozzle_temp_min: 0,
        }
    }
}

const ENCODING_TABLE: [(char, &str); 9] = [
    ('%', "%25"),
    ('/', "%2F"),
    ('&', "%26"),
    ('?', "%3F"),
    (' ', "%20"),
    ('#', "%23"),
    ('(', "%28"),
    (')', "%29"),
    ('~', "%7E"),
];

static ENCODING_MAP: Lazy<Mutex<CriticalSectionRawMutex, HashMap<char, &str>>> = Lazy::new(|| {
    let char_hashmap: HashMap<char, &str> = ENCODING_TABLE.into_iter().collect();
    Mutex::new(char_hashmap)
});

fn my_decode_from_url_part(text: &str) -> String {
    // % must be last (because some originated from encodings and will need to be replaced first)
    // let name = name.replace("%7E", "/").replace("%2F", "/").replace("%28", "(").replace("%29", ")").replace("%26", "&").replace("%3F", "?").replace("%20", " ").replace("%25", "%");
    efficient_decode(text, &ENCODING_TABLE)
}

fn my_encode_to_url_part(text: &str) -> String {
    // % must be first (because later added)
    // let name = name.replace("%", "%25").replace("/", "%2F").replace("&", "%26").replace("?", "%3F").replace(" ", "%20").replace("(", "%28").replace(")", "%29").replace( "~","%7E");
    ENCODING_MAP.lock(|encoding_map| efficient_encode(text, encoding_map))
}

/// Encodes specific characters in a string based on a provided mapping.
/// Minimizes allocations while still returning a String.
///
/// # Arguments
/// * `input` - The string to encode
/// * `char_map` - A mapping of characters to their encoded string representation
///
/// # Returns
/// The encoded string
pub fn efficient_encode(input: &str, char_map: &HashMap<char, &str>) -> String {
    // Pre-calculate output size to avoid reallocations
    let mut capacity = 0;
    for c in input.chars() {
        capacity += match char_map.get(&c) {
            Some(replacement) => replacement.len(),
            None => c.len_utf8(),
        };
    }

    // Pre-allocate output string with exact capacity needed
    let mut result = String::with_capacity(capacity);

    // Process each character
    for c in input.chars() {
        match char_map.get(&c) {
            Some(replacement) => result.push_str(replacement),
            None => result.push(c),
        }
    }

    result
}

/// Decodes a string by replacing encoded sequences with their original characters.
/// Minimizes allocations while still returning a String.
///
/// # Arguments
/// * `input` - The string to decode
/// * `char_map` - A mapping of characters to their encoded string representation
///
/// # Returns
/// The decoded string
pub fn efficient_decode(input: &str, char_table: &[(char, &str)]) -> String {
    // Pre-allocate with input size (likely sufficient since decoding usually results in shorter strings)
    let mut result = String::with_capacity(input.len());

    // Use slice for efficient substring comparison
    let input_bytes = input.as_bytes();
    let mut i = 0;

    while i < input_bytes.len() {
        let mut found = false;

        // Try to match each encoded sequence at current position
        for (original, encoded) in char_table {
            let encoded_bytes = encoded.as_bytes();

            if i + encoded_bytes.len() <= input_bytes.len() && &input_bytes[i..i + encoded_bytes.len()] == encoded_bytes {
                result.push(*original);
                i += encoded_bytes.len();
                found = true;
                break;
            }
        }

        // If no encoded sequence matches, copy original character
        if !found {
            // Get one complete UTF-8 character
            let char_len = if (input_bytes[i] & 0x80) == 0 {
                1 // ASCII
            } else if (input_bytes[i] & 0xE0) == 0xC0 {
                2 // 2-byte UTF-8
            } else if (input_bytes[i] & 0xF0) == 0xE0 {
                3 // 3-byte UTF-8
            } else {
                4 // 4-byte UTF-8
            };

            // Safe because we're checking bounds and copying valid UTF-8 sequences
            if i + char_len <= input_bytes.len() {
                result.push_str(core::str::from_utf8(&input_bytes[i..i + char_len]).unwrap());
                i += char_len;
            } else {
                // Handle truncated UTF-8 at end of string (shouldn't happen with valid UTF-8)
                i += 1;
            }
        }
    }

    result
}

impl From<bambu_api::PrintTray> for FilamentInfo {
    fn from(v: bambu_api::PrintTray) -> Self {
        Self {
            tray_info_idx: v.tray_info_idx.unwrap_or_default(),
            tray_type: v.tray_type.unwrap_or_default(),
            tray_color: v.tray_color.unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
        }
    }
}

impl From<&bambu_api::PrintTray> for FilamentInfo {
    fn from(v: &bambu_api::PrintTray) -> Self {
        Self {
            tray_info_idx: v.tray_info_idx.as_ref().cloned().unwrap_or_default(),
            tray_type: v.tray_type.as_ref().cloned().unwrap_or_default(),
            tray_color: v.tray_color.as_ref().cloned().unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
        }
    }
}
/////////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Calibration {
    pub filament_id: String,
    pub k_value: String,
    n_coef: f32,
    setting_id: String,
    pub name: String,
    cali_idx: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PrinterPersistentState<'a> {
    pub ams_trays: Cow<'a, [Tray; 16]>,
    pub virt_tray: Cow<'a, Tray>,
    pub nozzle_diameter: Option<String>,
}

fn formatted_k_value(k: &str) -> String {
    if k.is_empty() {
        return "".to_string();
    }
    let formatted_k_value = if k.starts_with("(") {
        let k = k.trim_matches(['(', ')']);
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("({:.3})", k_value)
    } else {
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("{:.3}", k_value)
    };
    formatted_k_value
}

impl From<&bambu_api::Filament> for Calibration {
    fn from(v: &bambu_api::Filament) -> Self {
        // this "Filament" in bambu_api is really calibrations, bambulab naming ...
        Self {
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: formatted_k_value(&v.k_value),
            n_coef: f32::from_str(&v.n_coef).unwrap_or(-1.0),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }
}

impl Calibration {
    pub fn new_minimal(k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            k_value: formatted_k_value(k_value),
            filament_id: String::from(filament_id),
            setting_id: String::from(setting_id),
            name: String::from(name),
            cali_idx,
            ..Default::default()
        }
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn init(
    framework: Rc<RefCell<Framework>>,
    printer_number: usize, // number of printer in user's configuration,
    printer_index: usize, // index of printer in the array of printers, if a config is not good and skipped, then index would be different than number
    printer_config: &PrinterConfig,
    app_config: Rc<RefCell<AppConfig>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) -> Result<Rc<RefCell<BambuPrinter>>, String> {
    let spawner = framework.borrow().spawner;
    let printer_serial = if let Some(printer_serial) = &printer_config.serial {
        printer_serial.clone()
    } else {
        return Err("Missing printer serial".to_string());
    };

    let printer_access_code = if let Some(printer_access_code) = &printer_config.access_code {
        printer_access_code.clone()
    } else {
        return Err("Missing printer access code".to_string());
    };

    let printer_name = printer_config.name.clone();
    let printer_ip = printer_config.ip;
    let log_filter = if let Some(log_filter) = &printer_config.log_filter {
        *log_filter
    } else {
        log::LevelFilter::Warn
    };
    let auto_restore_k = printer_config.auto_restore_k;

    // == Setup MQTT ==================================================================
    let write_packets = Rc::new(embassy_sync::channel::Channel::<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        crate::my_mqtt::BufferedMqttPacket,
        3,
    >::new());

    let read_packets = Rc::new(embassy_sync::pubsub::PubSubChannel::<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        crate::my_mqtt::BufferedMqttPacket,
        5,
        2,
        1,
    >::new());

    let restart_printer = Rc::new(embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>::new());

    let bambu_printer = BambuPrinter::new(
        printer_number,
        printer_index,
        &printer_serial,
        &printer_access_code,
        &printer_name,
        &printer_ip,
        auto_restore_k,
        write_packets.clone(),
        app_config.clone(),
        restart_printer.clone(),
        log_filter,
    );

    spawner
        .spawn(restartable_mqtt_task(
            framework,
            8192,
            4096,
            read_packets.clone(),
            write_packets,
            bambu_printer.clone(),
            restart_printer,
            ssdp_pub_sub,
        ))
        .ok();

    spawner.spawn(incoming_messages_task(read_packets, bambu_printer.clone())).ok();

    Ok(bambu_printer)
}

// Important: This is the initial load task. Because it issues more commands than can fit the Channel, it can't await while borrowing bambu_printer
// in order to sendi messages over the channel. If it would, then it would await while bambu_printer is borrowed, and the response invokes the printer
// and will panic due to borrow_mut (response) while already borrowed here (RefCell will panic at runtine).
// This was tested to verify this indeed happens.
// Therefore, the code takes the data required from the bambu_printer and pass it to the functions that aren't methods because of that.
// TODO: more elegant to just pass Rc<RefCell<BambuPrinter>> to the async function and have it take the needed items
#[embassy_executor::task(pool_size = 5)]
// #[embassy_executor::task]
pub async fn fetch_initial_info(bambu_printer: Rc<RefCell<BambuPrinter>>) {
    let write_packets = bambu_printer.borrow().write_packets.clone();
    let printer_serial = bambu_printer.borrow().printer_serial.clone();
    let printer_number = bambu_printer.borrow().printer_number;
    let log_filter = bambu_printer.borrow().log_filter;

    // fetch first setting for all nozzles, need that in advance before getting filaments
    let nozzle_diameters = ["0.2", "0.6", "0.8", "0.4"];
    for nozzle_diameter in nozzle_diameters {
        debug!("[{printer_number}] Request calibration information for nozzle {nozzle_diameter}");
        BambuPrinter::fetch_filament_calibrations_async(&printer_serial, printer_number, log_filter, write_packets.clone(), nozzle_diameter).await;
        Timer::after_millis(200).await;
    }

    // Now request full update, and wait until data is processed and have the nozzle diameter at hand for next request
    BambuPrinter::request_full_update_async(&printer_serial, printer_number, log_filter, write_packets.clone()).await;
    while bambu_printer.borrow().nozzle_diameter.is_none() {
        Timer::after_millis(100).await;
    }

    // Get again the filaments for current nozzle size,
    // that's because in slicer they don't check if data received from printer it's current nozzle or not
    // it's a bug there, can even be reproduced in the slicer by switching in the manage results to another nozzle diameter
    let curr_nozzle_diameter = bambu_printer.borrow().nozzle_diameter.as_ref().unwrap().clone();
    BambuPrinter::fetch_filament_calibrations_async(&printer_serial, printer_number, log_filter, write_packets, &curr_nozzle_diameter).await;
}

#[embassy_executor::task(pool_size = MAX_NUM_PRINTERS)]
pub async fn incoming_messages_task(
    read_packets: Rc<PubSubChannel<NoopRawMutex, BufferedMqttPacket, 5, 2, 1>>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
) {
    let mut subscriber = read_packets.subscriber().unwrap();
    const KEEP_ALIVE_SEC: u32 = 20;
    let printer_log_id = bambu_printer.borrow().printer_number;
    let log_level = bambu_printer.borrow().log_filter;

    let mut printer_known_to_be_up = false;
    loop {
        let wait_res = with_timeout(Duration::from_secs(KEEP_ALIVE_SEC as u64), subscriber.next_message_pure()).await;
        match wait_res {
            Ok(packet) => {
                printer_known_to_be_up = true;
                if let Ok(p) = mqttrust::Packet::try_from(&packet) {
                    #[allow(clippy::single_match)]
                    match p {
                        mqttrust::Packet::Publish(mqttrust::Publish {
                            dup: _,
                            qos: _,
                            pid: _,
                            retain: _,
                            topic_name: _,
                            payload,
                        }) => {
                            let parse_res = serde_json::from_slice::<bambu_api::Print>(payload);
                            if let Ok(print) = parse_res {
                                if log_level >= log::Level::Trace {
                                    trace!("[{}] {:?}", printer_log_id, print);
                                }
                                let mut skip = false;
                                if let Some(print_result) = &print.print.result {
                                    if print_result == "fail" {
                                        if log_level >= log::Level::Warn {
                                            warn!("[{}] Printer reported an error message, ignoring message", printer_log_id);
                                            warn!("[{}] {:?}", printer_log_id, print);
                                        }
                                        skip = true;
                                    }
                                }
                                if !skip {
                                    let previous_reading_bits = bambu_printer.borrow().tray_reading_bits;
                                    let change_made = (*bambu_printer.borrow_mut()).process_print_message(&print.print);
                                    let updated_reading_bits = bambu_printer.borrow().tray_reading_bits;
                                    if change_made {
                                        (*bambu_printer.borrow_mut()).update_ams_trays_done(previous_reading_bits, updated_reading_bits);
                                    }
                                }
                            } else if log_level >= log::Level::Debug {
                                debug!(
                                    "[{}] Unprocessed message {:?} : {:?}",
                                    printer_log_id,
                                    parse_res,
                                    core::str::from_utf8(payload)
                                );
                            }
                        }
                        mqttrust::Packet::Suback(mqttrust::encoding::v4::Suback { pid: _, return_codes: _ }) => {
                            // Subscribed, now time to request for update
                            let spawner = embassy_executor::Spawner::for_current_executor().await;
                            spawner.spawn(fetch_initial_info(bambu_printer.clone())).ok();
                        }
                        _ => (),
                    }
                } else {
                    error!("Unparsable MQTT message, this means an internal bug");
                }
            }
            Err(_) => {
                if printer_known_to_be_up {
                    if log_level >= log::Level::Warn {
                        warn!("[{}] Printer connectivity issues suspected (uncertain), checking", printer_log_id);
                    }
                    let write_packets = bambu_printer.borrow().write_packets.clone();
                    let printer_serial = bambu_printer.borrow().printer_serial.clone();
                    let printer_number = bambu_printer.borrow().printer_number;
                    let log_filter = bambu_printer.borrow().log_filter;
                    BambuPrinter::request_full_update_async(&printer_serial, printer_number, log_filter, write_packets).await;
                    printer_known_to_be_up = false;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[embassy_executor::task(pool_size = MAX_NUM_PRINTERS)]
pub async fn restartable_mqtt_task(
    framework: Rc<RefCell<Framework>>,
    rx_socket_buffer_size: usize,
    tx_socket_buffer_size: usize,
    read_packets: Rc<PubSubChannel<NoopRawMutex, BufferedMqttPacket, 5, 2, 1>>,
    write_packets: Rc<Channel<NoopRawMutex, BufferedMqttPacket, 3>>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    loop {
        let printer_mqtt_task = bambu_mqtt_task(
            framework.clone(),
            bambu_printer.clone(),
            rx_socket_buffer_size,
            tx_socket_buffer_size,
            read_packets.clone(),
            write_packets.clone(),
            ssdp_pub_sub,
        );
        match select(printer_mqtt_task, restart_printer.wait()).await {
            Either::First(_) => {
                // we arrive here only if something is wrong with config, so the only thing to do
                // is wait for printer restart
                restart_printer.wait().await;
            }
            Either::Second(_) => {}
        }
        write_packets.clear();
        read_packets.clear();
    }
}

// Usage example, this should be in the client code using the generic_mqtt_task, specific per scenario
// This indirection is because embassy can't have generic functions as tasks
// https://github.com/embassy-rs/embassy/issues/2454#issuecomment-2336644031
// This is specific to the hw and required detailes (buffer sizes, etc.)
pub async fn bambu_mqtt_task(
    framework: Rc<RefCell<Framework>>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    rx_socket_buffer_size: usize,
    tx_socket_buffer_size: usize,
    read_packets: Rc<PubSubChannel<NoopRawMutex, BufferedMqttPacket, 5, 2, 1>>,
    write_packets: Rc<Channel<NoopRawMutex, BufferedMqttPacket, 3>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    let stack = framework.borrow().stack;
    let printer_serial = bambu_printer.borrow().printer_serial.clone();
    let printer_log_id = bambu_printer.borrow().printer_number;
    let log_level = bambu_printer.borrow().log_filter;

    let subscribe_topics = [mqttrust::SubscribeTopic {
        topic_path: &format!("device/{}/report", printer_serial),
        qos: mqttrust::QoS::AtLeastOnce,
    }];

    if log_level >= log::Level::Info {
        info!("[{}] Waiting for IP in Bambu Mqtt Task", printer_log_id);
    }
    // let mut wait_counter = 0;
    // const SKIP_CHECKS: i32 = 4;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after(Duration::from_millis(250)).await;
    }
    if log_level >= log::Level::Info {
        info!("[{}] From Bambu MQTT - got IP", printer_log_id);
    }
    Timer::after(Duration::from_millis(250)).await; // So log will come after wifi log

    let printer_ip: Ipv4Address;
    let printer_name: String;

    if bambu_printer.borrow().configured_printer_ip.is_none() {
        term_info!("[{}] No Printer IP configured, discovering Printer", printer_log_id);
        let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
        loop {
            let ssdp_info = ssdp_subscribe.next_message().await;
            match ssdp_info {
                embassy_sync::pubsub::WaitResult::Lagged(_) => (),
                embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                    if let Ok(ssdp_info) = TryInto::<BambuSSDPInfo>::try_into(ssdp_info) {
                        if printer_serial == ssdp_info.serial.unwrap_or("".to_string()) {
                            printer_ip = ssdp_info.ip.unwrap();
                            printer_name = ssdp_info.name.unwrap();
                            term_info!("[{}] Discovered printer {}", printer_log_id, printer_name);
                            break;
                        }
                    }
                }
            }
        }
    } else {
        printer_ip = bambu_printer.borrow().configured_printer_ip.unwrap();
        printer_name = bambu_printer.borrow().configured_printer_name.clone().unwrap_or(String::from("Unknown"));
    }

    // Final name, theoretically if name explicitly supplied and IP not,  this could override the supplied name
    bambu_printer.borrow_mut().printer_ip = printer_ip;
    bambu_printer.borrow_mut().printer_name = printer_name;

    let remote_endpoint = (printer_ip, 8883);
    let password = {
        let bambu_printer_borrow = bambu_printer.borrow();
        Some(bambu_printer_borrow.printer_access_code.clone().into_bytes())
    };

    crate::my_mqtt::generic_mqtt_task(
        framework,
        remote_endpoint,
        &printer_serial,
        Some("bblp"),
        password,
        0,
        &subscribe_topics,
        rx_socket_buffer_size,
        tx_socket_buffer_size,
        write_packets,
        read_packets,
        Duration::from_secs(20),
        bambu_printer,
    )
    .await
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TagInformation {
    pub filament: Option<FilamentInfo>,
    pub calibrations: HashMap<String, Calibration>,
    pub calibrations_printer_name: String, // has value only if calibrations has any value
    pub calibrations_printer_uuid: String, // has value only if calibrations has any value
    pub weight_core: Option<i32>,
    pub weight_new: Option<i32>,
    pub brand: Option<String>,
    pub filament_subtype: Option<String>,
    pub color_name: Option<String>,
    pub note: Option<String>,
}

impl TagInformation {
    pub fn to_descriptor(&self, printer_name: Option<&str>, printer_uuid: Option<&str>) -> Option<String> {
        let mut inner_calibrations_part = String::new();

        // if printer_name not supplied, it means a reuest to encode using printer information in tag (e.g. encoding from staging)
        let (encoded_printer_name, encoded_printer_uuid) = match printer_name {
            Some(printer_name) => {
                if !printer_name.is_empty() {
                    (&my_encode_to_url_part(printer_name), printer_uuid.unwrap_or_default())
                } else {
                    (&"".to_string(), "")
                }
            }
            None => {
                // use tag_name printer_name if available
                (&self.calibrations_printer_name, self.calibrations_printer_uuid.as_str())
            }
        };

        let already_encoded_k_prefix = &format!("{}~{}", encoded_printer_name, encoded_printer_uuid);
        let k_prefix = if !already_encoded_k_prefix.is_empty() {
            format!("&{}(", already_encoded_k_prefix)
        } else {
            "&".to_string()
        };
        let k_postfix = if !k_prefix.is_empty() { ")" } else { "" };

        let brand_part = self
            .brand
            .as_ref()
            .map(|s| format!("&B={}", my_encode_to_url_part(s)))
            .unwrap_or_default();
        let filament_subtype_part = self
            .filament_subtype
            .as_ref()
            .map(|s| format!("&MS={}", my_encode_to_url_part(s)))
            .unwrap_or_default();
        let color_name_part = self
            .color_name
            .as_ref()
            .map(|s| format!("&CN={}", my_encode_to_url_part(s)))
            .unwrap_or_default();
        let note_part = self.note.as_ref().map(|s| format!("&N={}", my_encode_to_url_part(s))).unwrap_or_default();

        for calibration_kv in self.calibrations.iter() {
            if let Some(cal_nozzle_diameter_char) = calibration_kv.0.chars().nth(2) {
                let calibration = calibration_kv.1;
                inner_calibrations_part += &format!(
                    "K{}={}~{}~{}",
                    cal_nozzle_diameter_char,
                    calibration.k_value.trim_end_matches('0'),
                    &calibration.setting_id,
                    &my_encode_to_url_part(&calibration.name)
                );
            }
        }
        let calibrations_part = if inner_calibrations_part.is_empty() {
            inner_calibrations_part
        } else {
            format!("{k_prefix}{inner_calibrations_part}{k_postfix}")
        };
        self.filament.as_ref().map(|filament| format!(
                "{FILAMENT_URL_PREFIX}V1?ID={TAG_PLACEHOLDER}{}{}&M={}&C={}&NN={}&NX={}{}&FI={}{brand_part}{filament_subtype_part}{color_name_part}{note_part}",
                self.weight_core.map(|v| format!("&WC={}", v)).unwrap_or_default(),
                self.weight_new.map(|v| format!("&WN={}", v)).unwrap_or_default(),
                filament.tray_type,
                filament.tray_color,
                filament.nozzle_temp_min,
                filament.nozzle_temp_max,
                calibrations_part,
                filament.tray_info_idx,
            ))
    }

    // TODO: remove all the printer parts, should only parse, the rest of the matching thould go elsewhere

    pub fn from_descriptor(descriptor: &str) -> Result<Self, Error> {
        let mut filament_info_result = FilamentInfo::new();
        let mut calibrations_result = HashMap::new();
        let mut weight_core = None;
        let mut weight_new = None;
        let mut brand = None;
        let mut filament_subtype = None;
        let mut color_name = None;
        let mut note = None;

        if !(descriptor.starts_with(FILAMENT_URL_PREFIX)) {
            return Err(Error::ParseError);
        }
        let descriptor = descriptor.trim_start_matches(FILAMENT_URL_PREFIX);

        let mut id = false;
        let mut v = false;
        let mut m = false;
        let mut fi = false;
        let mut c = false;
        let mut nn = false;
        let mut nx = false;
        for param in descriptor.split(['&', '/', '?']) {
            if param == "V1" {
                v = true;
                continue;
            }
            if let Some((param_name, param_value)) = param.split_once("=") {
                // note that this process only values of name=value. Others are currently not processed here (like V1, and TagId)
                match param_name {
                    // Tag ID
                    "ID" => {
                        id = true;
                    }
                    // Material / Tray Type (material code in some other form)
                    "M" => {
                        filament_info_result.tray_type = String::from(param_value);
                        m = true;
                    }
                    // Color / Tray Color
                    "C" => {
                        filament_info_result.tray_color = String::from(param_value);
                        c = true;
                    }
                    // Nozzle miN Temp
                    "NN" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_min = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        nn = true;
                    }
                    // Nozzle maX Temp
                    "NX" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_max = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        nx = true;
                    }
                    // "K4" | "K2" | "K6" | "K8" => (),
                    // // Filament Id/ Tray Index (material code in some form) - looks like Bambu specific
                    "FI" => {
                        filament_info_result.tray_info_idx = String::from(param_value);
                        fi = true;
                    }
                    "WC" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_core = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    "WN" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_new = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    "B" => {
                        brand = Some(my_decode_from_url_part(param_value));
                    }
                    "MS" => {
                        // Material Subtype
                        filament_subtype = Some(my_decode_from_url_part(param_value));
                    }
                    "CN" => {
                        color_name = Some(my_decode_from_url_part(param_value));
                    }
                    "N" => {
                        note = Some(my_decode_from_url_part(param_value));
                    }
                    _ => (), //return Err(Error::ParseError), TODO: verify match to pattern, or even run what's coming next inside here
                }
            }
        }

        // Processing of K Factor //////////////////
        // TODO: IMPORTANT: This assumes a single printer info, the printer name is thrown away.
        // Therefore, scanning/encoding to/from the staging at this point probably change information to current printer, which is not good in case of multiple printers
        // An easy solution is to store also copy of original string in staging and just encode it directly

        // First just collect data from tag

        let mut calibrations_printer_name = "";
        let mut calibrations_printer_uuid = "";
        // Second pass on parts that need to be processed after the first
        let re = Regex::new(r"^(.*)\((K.*)\)$").unwrap();
        for param in descriptor.split(&['/', '&', '?']) {
            let mut param = param;
            if let Some(captures) = re.captures(param) {
                // to get k data use match 2
                if let Some(param_match) = captures.get(2) {
                    param = param_match.as_str();
                }
                if let Some(param_match) = captures.get(1) {
                    let printer_name_and_uuid = param_match.as_str();
                    (calibrations_printer_name, calibrations_printer_uuid) =
                        printer_name_and_uuid.split_once('~').unwrap_or((printer_name_and_uuid, ""));
                }
                // to get the printer name (formatted as name~serial , use match 1 and don't forget to my_decode_from_url_part the data
                // currently not used, could compare to current printer name and ignore
            }

            // this is just calibrations loaded from the filament, without any matching, all with cali_idx = -1
            if let Some((param_name, param_value)) = param.split_once("=") {
                match param_name {
                    // K - Pressure Advance Factor for Nozzle Diameter 0.4, 0.2, 0.6, 0.8
                    "K4" | "K2" | "K6" | "K8" => {
                        //TODO: Currently we set the filament calibration only if it is found in the printer tables
                        // In the future consider adding the calibarion to the printer if it's not available
                        let nozzle_diameter_digit = param_name.chars().nth(1).unwrap();
                        let nozzle_diameter = format!("0.{}", nozzle_diameter_digit);

                        let mut k_parts = param_value.splitn(3, '~');

                        let k_value = k_parts.next().ok_or(Error::ParseError)?.trim_end_matches("0");
                        let setting_id = k_parts.next().ok_or(Error::ParseError)?;
                        let name = k_parts.next().ok_or(Error::ParseError)?;
                        let name = my_decode_from_url_part(name);
                        let calibration = Calibration::new_minimal(k_value, &filament_info_result.tray_info_idx, setting_id, &name, -1);
                        calibrations_result.insert(nozzle_diameter, calibration);
                    }
                    _ => (), // previous run already identified unrecognized parameters, here we skip also those that were ok so can't error
                }
            }
        }

        if v && id && m && fi && c && nn && nx {
            Ok(Self {
                filament: Some(filament_info_result),
                calibrations: calibrations_result,
                calibrations_printer_name: calibrations_printer_name.to_string(),
                calibrations_printer_uuid: calibrations_printer_uuid.to_string(),
                weight_core,
                weight_new,
                brand,
                filament_subtype,
                color_name,
                note,
            })
        } else {
            Err(Error::MissingFields)
        }
    }
}

#[derive(Clone, Debug)]
pub enum PrinterModel {
    Unknown,
    X1,
    X1C,
    X1E,
    P1P,
    P1S,
    A1Mini,
    A1,
}

#[derive(Clone, Debug)]
pub enum PrinterConnectMode {
    Unknown,
    Cloud,
    Lan,
}

#[derive(Clone, Debug, Default)]
pub struct BambuSSDPInfo {
    pub serial: Option<String>,
    pub name: Option<String>,
    pub ip: Option<Ipv4Address>,
    pub _model: Option<PrinterModel>,
    pub _connect_mode: Option<PrinterConnectMode>,
}

impl TryFrom<SSDPInfo> for BambuSSDPInfo {
    type Error = &'static str;
    fn try_from(v: SSDPInfo) -> Result<Self, Self::Error> {
        if v.nt.contains("urn:bambulab-com:device:3dprinter") {
            Ok(Self {
                serial: Some(v.usn),
                name: v.custom.get("DevName.bambu.com:").cloned(),
                ip: embassy_net::Ipv4Address::from_str(&v.location).ok(),
                _model: v.custom.get("DevModel.bambu.com").map(|s| match s.as_str() {
                    "3DPrinter-X1" => PrinterModel::X1,
                    "3DPrinter-X1-Carbon" => PrinterModel::X1C,
                    "C11" => PrinterModel::P1P,
                    "C12" => PrinterModel::P1S,
                    "C13" => PrinterModel::X1E,
                    "N1" => PrinterModel::A1Mini,
                    "N2" => PrinterModel::A1,
                    _ => PrinterModel::Unknown,
                }),

                _connect_mode: v.custom.get("DevModel.bambu.com").map(|s| match s.as_str() {
                    "lan" => PrinterConnectMode::Lan,
                    "cloud" => PrinterConnectMode::Cloud,
                    _ => PrinterConnectMode::Unknown,
                }),
            })
        } else {
            Err("Not a Bambulab Printer SSDP")
        }
    }
}

#[embassy_executor::task(pool_size = 2)] // up to two printers in parallel
pub async fn fix_k_on_restart(
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    prev_ams_trays: Vec<Tray>,
    prev_virt_tray: Tray,
    prev_nozzle: Option<String>,
) {
    Timer::after_secs(1).await;
    let printer_number = bambu_printer.borrow().printer_number;
    term_info!("[{}] Checking pressure advance (k) at printer startup", printer_number);
    if prev_nozzle != bambu_printer.borrow().nozzle_diameter {
        term_info!("[{}] Nozzle diameter changed, K restore not relevant", printer_number);
        bambu_printer.borrow_mut().pending_k_restore_sequence = false;
        return;
    }
    let mut set_tray_cali_idx: [Option<i32>; 16] = [None; 16];
    let mut set_virt_cali_idx: Option<i32> = None;

    {
        // block start, so borrow will be dropped
        let bambu_borrow = bambu_printer.borrow();
        for (id, prev_tray) in prev_ams_trays
            .iter()
            .enumerate()
            .chain(core::iter::once(&prev_virt_tray).map(|v| (254, v)))
        {
            let curr_tray = if id == 254 {
                bambu_borrow.virt_tray()
            } else {
                &bambu_borrow.ams_trays()[id]
            };
            let set_tray = if id == 254 { &mut set_virt_cali_idx } else { &mut set_tray_cali_idx[id] };
            if let Filament::Known(curr_filament_info) = &curr_tray.filament {
                if let Filament::Known(prev_filament_info) = &prev_tray.filament {
                    if curr_filament_info == prev_filament_info {
                        // Turn both Some(-1) and None to Some(-1)
                        let prev_cali_idx_normalized = prev_tray.cali_idx.or(Some(-1));
                        let curr_cali_idx_normalized = curr_tray.cali_idx.or(Some(-1));

                        // if curr idx isn't set and previously it was set, return it to previous state
                        if curr_cali_idx_normalized == Some(-1) && prev_cali_idx_normalized != Some(-1) {
                            // set_tray_cali_idx[id] = prev_cali_idx_normalized; // -1 means to set -1, value means set to that cali_idx
                            *set_tray = prev_cali_idx_normalized; // -1 means to set -1, value means set to that cali_idx
                        } else {
                            // set_tray_cali_idx[id] = None; // None means not do anything
                            *set_tray = None; // None means not do anything
                        }
                    }
                }
            }
        }
    }

    let write_packets = bambu_printer.borrow().write_packets.clone();
    let nozzle_diameter = &bambu_printer.borrow().nozzle_diameter.clone().unwrap_or_default();
    let printer_serial = bambu_printer.borrow().printer_serial.clone();
    let log_filter = bambu_printer.borrow().log_filter;

    for (id, prev_tray) in prev_ams_trays
        .iter()
        .enumerate()
        .chain(core::iter::once(&prev_virt_tray).map(|v| (254, v)))
    {
        {
            let set_tray = if id == 254 { &set_virt_cali_idx } else { &set_tray_cali_idx[id] };
            if set_tray.is_some() {
                if let Filament::Known(filament_info) = &prev_tray.filament {
                    let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(id);
                    if ams_id != 254 {
                        info!("[{}] Updating pressure advance of AMS {} slot {}", printer_number, ams_id, tray_id);
                    } else {
                        info!("[{}] Updating pressure advance of external slot", printer_number);
                    }
                    let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
                        nozzle_diameter,
                        id as i32,                    // here we need the original tray_id
                        &filament_info.tray_info_idx, // tray_info_idx is filament_id in this command
                        *set_tray,
                    );
                    let payload = serde_json::to_string_pretty(&cmd).unwrap();
                    BambuPrinter::publish_payload_async(&printer_serial, printer_number, log_filter, write_packets.clone(), payload).await;
                    Timer::after_millis(250).await;
                }
            }
        }
    }

    Timer::after_millis(500).await; // wait until last K change is absorbed by the printer
    bambu_printer.borrow_mut().pending_k_restore_sequence = false;
    term_info!("[{}] Completed K restore where required", printer_number);
}

// PRINTER_USN = "YOUR_PRINTER_SN" # This is the serial number of the printer. https://wiki.bambulab.com/en/general/find-sn
// PRINTER_DEV_MODEL = "3DPrinter-X1-Carbon" # "3DPrinter-X1-Carbon", "3DPrinter-X1", "C11" (for P1P), "C12" (for P1S), "C13" (for X1E), "N1" (A1 mini), "N2S" (A1)
// PRINTER_DEV_NAME = "X1C-1" # The friendly name displayed in Bambu Studio / Orca Slicer. Set this to whatever you want.
// PRINTER_DEV_SIGNAL = "-44" # Fake wifi signal strength
// PRINTER_DEV_CONNECT = "lan" # printer is in lan only mode
// PRINTER_DEV_BIND = "free" # and is not bound to any cloud account
// PRINTER_IP = None # If you want to hardcode the printer IP, set it here. Otherwise, pass it as the first argument to the script.
// TARGET_PORT = 2021 # The port used for SSDP discovery
