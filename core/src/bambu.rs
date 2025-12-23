// TODO:
// Deal with when to clear tag information, when we know spool taken out
// Deal with when to copy tag information between trays if only some data change but we know the spool is there

pub mod bambu_print;

use crate::bambu_api::PrintDeviceNozzleInfo;
use crate::spool_record::{FullSpoolRecord, SpoolRecord};
use crate::tag_standards::SPOOLEASE_V1_TAG_TYPE;
use crate::view_model::{self, StoreStateRequestChannel};
use crate::{
    app_config::{PrinterConfig, MATERIALS},
    bambu_api::GcodeState,
    settings::MAX_NUM_PRINTERS,
    ssdp::{SSDPInfo, SSDPPubSubChannel},
    store::Store,
};
use alloc::{
    borrow::Cow,
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use bambu_print::PrintProject;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use core::{cell::RefCell, mem::swap, str::FromStr};
use derivative::Derivative;
use embassy_futures::select::{select, Either};
use embassy_net::Ipv4Address;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubChannel};
use embassy_time::{with_timeout, Duration, Timer};
use hashbrown::HashMap;
use mqttrust::QoS;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::Fetch3mf;

use framework::prelude::*;

use crate::{
    app_config::AppConfig,
    bambu_api::{self, PrintAms, PrintTray},
    my_mqtt::BufferedMqttPacket,
};

#[allow(dead_code)]
const EXTRA_DEBUG: bool = false;

#[allow(unused_macros)]
macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

#[allow(unused_macros)]
macro_rules! warnex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            warn!($($t)*);
        }
    };
}

const FILAMENT_URL_PREFIX: &str = "https://info.filament3d.org/";
const FILAMENT_URL_PREFIX_V1_TAG: &str = "https://info.filament3d.org/V1";

#[allow(dead_code)]
#[derive(Debug)]
pub struct TrayBits {
    pub tray_exist_bits: Option<u32>,
    pub tray_read_done_bits: Option<u32>,
    pub tray_reading_bits: Option<u32>,
}

#[derive(Debug, PartialEq)]
pub enum NozzleType {
    Standard,
    HighFlow,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq)]
// DON'T CHANGE - PERSISTED TO STATE
pub struct Extruder {
    pub id: u32,
    pub diameter: Option<String>,
    nozzle_type: Option<String>, // this is the string code, not to be used, instead use nozzle_type_code
}

impl Extruder {
    pub fn nozzle_type_code(&self) -> Option<NozzleType> {
        // nozzle diameter exist in both old and new printers.
        // If it is not set it means that nozzle_type is also unknown.
        // Otherwise, None for nozzle type means standard (since high flow doesn't exist for old printers)
        #[allow(clippy::question_mark)]
        if self.diameter.is_none() {
            return None;
        }
        Some(self.nozzle_type.as_ref().map_or(NozzleType::Standard, |s| {
            if s.len() < 8 || s.as_bytes()[1] != b'H' {
                NozzleType::Standard
            } else {
                NozzleType::HighFlow
            }
        }))
    }
}

#[derive(Default, Debug, Clone)]
pub struct AmsInfo {
    pub extruder: u32,
}

type WritePacketsChannel = Channel<NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 20>;
type ReadPacketsPubSub = PubSubChannel<NoopRawMutex, BufferedMqttPacket, 20, 2, 1>;
pub struct BambuPrinter {
    pub bambu_model: Option<Rc<RefCell<Self>>>,
    pub log_filter: log::LevelFilter,
    pub printer_number: usize,                   // number of printer in user's configuration,
    pub printer_index: usize, // index of printer in the array of printers, if a config is not good and skipped, then index would be different than number
    pub printer_serial: String, // mandatory, so configured is the same as actual
    pub printer_access_code: String, // mandatory, so configured is the same as actual
    pub configured_printer_name: Option<String>, // the name from config, could be empty
    pub inner_printer_name: String, // Unknown or Configured name or from SSDP if discovered
    pub printer_selector_name: String, // Will be assigned printer name from config OR printer serial (which is always availble) if config not available
    pub configured_printer_ip: Option<Ipv4Address>,
    pub auto_restore_k: bool,
    pub track_print_consume: bool,
    pub fetch_3mf: Fetch3mf,
    pub printer_ip: Ipv4Address,
    pub printer_uuid_to_encode: String,
    pub printer_connectivity_ok: Option<bool>,
    // inner_nozzle_diameter: Option<String>,
    inner_extruders: [Extruder; 2],
    inner_ams_trays: Vec<Tray>, // [Tray; 24], // 16 in standard AMS, 8 in HT (H2D)
    inner_virt_trays: [Tray; 2],
    force_store_state: bool,
    extruders_dirty: bool,
    ams_trays_dirty: [bool; 24],
    virt_trays_dirty: bool,
    tray_exist_bits_dirty: bool,
    tray_read_done_bits_dirty: bool,
    ams_exist_bits_dirty: bool,
    calibrations_dirty: bool,
    printer_name_dirty: bool,
    pub calibrations: Vec<Calibration>,
    write_packets: Rc<WritePacketsChannel>,
    #[allow(dead_code)]
    restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn BambuPrinterObserver>>>,
    app_config: Rc<RefCell<AppConfig>>,
    inner_tray_exist_bits: Option<u32>,
    inner_tray_read_done_bits: Option<u32>,
    tray_reading_bits: Option<u32>,
    pub inner_ams_exist_bits: Option<u32>,
    printer_was_disconnected: bool,
    pending_k_restore_sequence: bool,
    pub curr_print_project: Option<PrintProject>,
    pub loaded_print_project: Option<PrintProject>,
    inner_extruder_state: Option<i32>,
    relevant_extruder_state_dirty: bool,
    tray_tar: [i32; 2],
    tray_now: [i32; 2],
    tray_pre: [i32; 2],
    gcode_state: GcodeState,
    layer_num: i32,
    pub locked_mode: Option<bool>, // None, unknown, treat as unlocked, false - dev mode, true - locked
    store_state_request_channel: Rc<StoreStateRequestChannel>,
    ams_info: Vec<AmsInfo>,
}

pub trait BambuPrinterObserver {
    fn on_trays_update(
        &mut self,
        bambu_printer: &mut BambuPrinter,
        prev_tray_bits: &TrayBits,
        new_tray_bits: &TrayBits,
        removed_tags: &HashMap<usize, SpoolId>,
    );
    fn on_printer_connect_status(&self, bambu_printer: &mut BambuPrinter, status: bool);
    fn on_request_gcode_analysis(&mut self, bambu_printer: &mut BambuPrinter, print_project: &PrintProject) -> i32;
    fn on_cancel_gcode_analysis(&mut self, job_number: i32);
}

// Special access to trays fields for dirty tracking
impl BambuPrinter {
    pub fn is_locked(&self) -> bool {
        self.locked_mode.unwrap_or_default()
    }

    pub fn printer_name(&self) -> &String {
        &self.inner_printer_name
    }
    pub fn set_printer_name(&mut self, new_printer_name: &str) {
        if self.inner_printer_name != new_printer_name {
            self.inner_printer_name = new_printer_name.to_string();
            self.printer_name_dirty = true;
        }
    }
    pub fn ams_trays(&self) -> &Vec<Tray> {
        &self.inner_ams_trays
    }
    pub fn swap_ams_tray<'a>(&mut self, mut index: usize, tray: &'a mut Tray) -> &'a mut Tray {
        if (128..=135).contains(&index) {
            index = index - 128 + 16;
        } else if !(0..self.inner_ams_trays.len()).contains(&index) {
            error!("Unsupported tray index {index}, probably an unsupported AMS");
            return tray;
        }
        // Handle other AMS's
        if self.inner_ams_trays[index] != *tray {
            swap(&mut self.inner_ams_trays[index], tray);
            self.ams_trays_dirty[index] = true;
            // extra test because meta is excluded from partialeq for Tray
            if self.inner_ams_trays[index].meta_info != tray.meta_info {
                self.ams_trays_dirty[index] = true;
            }
        }
        tray
    }
    pub fn update_ams_tray<F>(&mut self, mut index: usize, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        if (128..=135).contains(&index) {
            index = index - 128 + 16;
        } else if !(0..self.inner_ams_trays.len()).contains(&index) {
            error!("Unsupported tray index {index}, probably an unsupported AMS");
            return;
        }
        let prev_tray = self.inner_ams_trays[index].clone();
        f(&mut self.inner_ams_trays[index]);
        // extra test if meta_info because meta is excluded from partialeq for Tray (also in vt_tray)
        if prev_tray != self.inner_ams_trays[index] || prev_tray.meta_info != self.inner_ams_trays[index].meta_info {
            self.ams_trays_dirty[index] = true;
        }
    }
    pub fn virt_trays(&self) -> &[Tray; 2] {
        &self.inner_virt_trays
    }
    pub fn set_virt_tray(&mut self, extruder_id: u32, tray: Tray) {
        if tray != self.inner_virt_trays[extruder_id as usize] {
            self.inner_virt_trays[extruder_id as usize] = tray;
            self.virt_trays_dirty = true;
        }
    }
    pub fn update_virt_tray<F>(&mut self, extruder_id: u32, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        let prev_tray = self.inner_virt_trays[extruder_id as usize].clone();
        f(&mut self.inner_virt_trays[extruder_id as usize]);
        // extra test if meta_info because meta is excluded from partialeq for Tray (also in ams_trays)
        if prev_tray != self.inner_virt_trays[extruder_id as usize] || prev_tray.meta_info != self.inner_virt_trays[extruder_id as usize].meta_info {
            self.virt_trays_dirty = true;
        }
    }
    pub fn update_any_tray<F>(&mut self, index: usize, f: F)
    where
        F: FnOnce(&mut Tray),
    {
        match index {
            255 => self.update_virt_tray(0, f),
            254 => self.update_virt_tray(1, f),
            _ => self.update_ams_tray(index, f),
        }
    }
    pub fn get_any_tray(&self, mut index: usize) -> &Tray {
        match index {
            254 => &self.virt_trays()[1],
            255 => &self.virt_trays()[0],
            _ => {
                if (128..=135).contains(&index) {
                    index = index - 128 + 16;
                }
                &self.ams_trays()[index]
            }
        }
    }
    pub fn nozzle_diameter(&self, extruder_id: u32) -> &Option<String> {
        &self.inner_extruders[extruder_id as usize].diameter
    }

    pub fn set_nozzle_diameter(&mut self, extruder_id: u32, new_nozzle_diameter: Option<String>) {
        if new_nozzle_diameter != self.inner_extruders[extruder_id as usize].diameter {
            info!(
                "[{}] Nozzle diameter for extruder {extruder_id} changed from {:?} to {:?}",
                self.printer_number, self.inner_extruders[extruder_id as usize].diameter, new_nozzle_diameter
            );
            self.inner_extruders[extruder_id as usize].diameter = new_nozzle_diameter;
            self.extruders_dirty = true;
        }
    }
    pub fn set_extruder_info(&mut self, extruder_id: u32, new_nozzle_info: &PrintDeviceNozzleInfo) -> bool {
        if extruder_id != 0 && extruder_id != 1 {
            return false;
        }
        let new_extruder = Extruder {
            id: extruder_id,
            diameter: Some(format!("{:.1}", new_nozzle_info.diameter)),
            nozzle_type: Some(new_nozzle_info.nozzle_type.clone()),
        };

        if new_extruder != self.inner_extruders[extruder_id as usize] {
            info!(
                "[{}] Extruder {extruder_id} info changed from {:?} to {:?}",
                self.printer_number, self.inner_extruders[extruder_id as usize], new_extruder,
            );
            self.inner_extruders[extruder_id as usize] = new_extruder;
            self.extruders_dirty = true;
            return true;
        }
        false
    }
    pub fn get_extruder(&self, extruder_id: u32) -> &Extruder {
        &self.inner_extruders[extruder_id as usize]
    }

    pub fn ams_exist_bits(&self) -> &Option<u32> {
        &self.inner_ams_exist_bits
    }
    pub fn set_ams_exist_bits(&mut self, new_ams_exist_bits: Option<u32>) {
        if new_ams_exist_bits != self.inner_ams_exist_bits {
            self.inner_ams_exist_bits = new_ams_exist_bits;
            self.ams_exist_bits_dirty = true;
        }
    }

    pub fn tray_exist_bits(&self) -> &Option<u32> {
        &self.inner_tray_exist_bits
    }
    pub fn set_tray_exist_bits(&mut self, new_tray_exist_bits: Option<u32>) {
        if new_tray_exist_bits != self.inner_tray_exist_bits {
            self.inner_tray_exist_bits = new_tray_exist_bits;
            self.tray_exist_bits_dirty = true;
        }
    }

    pub fn tray_read_done_bits(&self) -> &Option<u32> {
        &self.inner_tray_read_done_bits
    }
    pub fn set_tray_read_done_bits(&mut self, new_tray_read_done_bits: Option<u32>) {
        if new_tray_read_done_bits != self.inner_tray_read_done_bits {
            self.inner_tray_read_done_bits = new_tray_read_done_bits;
            self.tray_read_done_bits_dirty = true;
        }
    }

    pub fn set_extruder_state(&mut self, new_extruder_state: i32) -> bool {
        let mut relevant_part_changed = false;
        if self.inner_extruder_state != Some(new_extruder_state) {
            if self.inner_extruder_state.unwrap_or_default() & 0xFF != new_extruder_state & 0xFF {
                // for dirty purpose, at this time, only low 8 bits matter
                // A bit ugly and risky, need to update the test if using additional bits
                self.relevant_extruder_state_dirty = true;
                relevant_part_changed = true;
            }
            self.inner_extruder_state = Some(new_extruder_state);
        }
        relevant_part_changed
    }

    pub fn extruder_state(&self) -> &Option<i32> {
        &self.inner_extruder_state
    }

    pub fn num_extruders(&self) -> u32 {
        if let Some(extruder_state) = self.extruder_state() {
            *extruder_state as u32 & 0x0f
        } else {
            1
        }
    }

    pub fn init_printer_persistent_state(&mut self, mut state: PrinterPersistentState, store: &Rc<Store>) {
        self.inner_ams_trays = core::mem::take(state.ams_trays.to_mut());
        self.inner_ams_trays.resize(24, Tray::default());
        if let Some(mut virt_trays) = state.virt_trays {
            self.inner_virt_trays = core::mem::take(virt_trays.to_mut());
        } else if let Some(mut virt_tray) = state.virt_tray {
            self.inner_virt_trays[0] = core::mem::take(virt_tray.to_mut());
            self.inner_virt_trays[1] = Tray::default();
        }
        if state.nozzle_diameter.is_some() {
            self.inner_extruders[0].diameter = state.nozzle_diameter;
        }
        if let Some(mut extruders) = state.extruders {
            self.inner_extruders = core::mem::take(extruders.to_mut());
        }
        self.inner_extruder_state = state.extruder_state;
        self.inner_ams_exist_bits = state.ams_exist_bits;
        self.inner_tray_exist_bits = state.tray_exist_bits;
        self.inner_tray_read_done_bits = state.tray_read_done_bits;
        self.calibrations = core::mem::take(state.calibrations.to_mut());
        if self.configured_printer_ip.is_some() {
            // meaning won't have name from SSDP
            // in this case the name should be taken from the configuration and not from the state, could be it is newer
            if self.inner_printer_name == default_printer_name() {
                // can't be in case printer ip configured, since web config forces name, but lets be defensive about it and support such case
                self.inner_printer_name = state.printer_name.clone();
            }
        } else {
            // we will get a name from SSDP, and could be such name is in the state and is better for this printer_name in case it exists, but only if it exists not as unknown in state
            if state.printer_name != default_printer_name() {
                // override printer_name (which could be from configured name) only if the value stored is not
                self.inner_printer_name = state.printer_name.clone();
            }
        }

        // this is for upgrading tray from using the old tag_info to the id.
        // It happens only until the first state store takes place again, because then the old tag_info is not serialized and the id will be there
        for tray_id in (0..self.ams_trays().len()).chain(core::iter::once(254)) {
            let old_id = self.get_any_tray(tray_id).meta_info.old_tag_info.as_ref().and_then(|v| v.id.clone());
            if let Some(old_id) = old_id {
                self.update_any_tray(tray_id, |v| v.meta_info.spool_id = Some(old_id));
                self.force_store_state = true;
            }
        }
        // Some of this section (not all) can be potentially removed in the future since state consume_since_weight should be available and updated
        // This is only for transition time where the there was no consumed_since_weight in the metainfo for correct display calculation
        // The removal of non existing ID's need to stay
        for tray_id in (0..self.ams_trays().len()).chain([254, 255]) {
            if self.get_any_tray(tray_id).meta_info.consumed_since_weight == 0.0 {
                if let Some(spool_id) = self.get_any_tray(tray_id).meta_info.spool_id.as_ref() {
                    let spool_record = store.get_spool_by_id(spool_id.as_str());
                    if let Some(spool_record) = spool_record {
                        self.update_any_tray(tray_id, |tray| tray.meta_info.consumed_since_weight = spool_record.consumed_since_weight);
                    } else {
                        self.update_any_tray(tray_id, |tray| tray.meta_info.spool_id = None);
                    }
                }
            }

            // if self.get_any_tray(tray_id).meta_info.consumed_since_weight == 0.0 {
            //     if let Some(tag_id) = self.get_any_tray(tray_id).meta_info.tag_info.as_ref().and_then(|v| v.tag_id.clone()) {
            //         let spool_record = store.get_spool_by_tag_id(&tag_id);
            //         if let Some(spool_record) = spool_record {
            //             self.update_any_tray(tray_id, |tray| tray.meta_info.consumed_since_weight = spool_record.consumed_since_weight);
            //         }
            //     }
            // }
        }
        // for tray in self.inner_ams_trays.iter_mut().chain(core::iter::once(&mut self.inner_virt_tray)) {
        //     if let Some(tag_info) = &tray.meta_info.tag_info {
        //         if tray.meta_info.consumed_since_weight == 0.0 {
        //             if let Some(tag_id) = &tag_info.tag_id {
        //                 let spool_record = store.get_spool_by_tag_id(tag_id);
        //                 if let Some(spool_record) = spool_record {
        //                     tray.meta_info.consumed_since_weight = spool_record.consumed_since_weight;
        //                 }
        //             }
        //         }
        //     }
        // }
    }

    pub async fn load_printer_state(
        framework: &Rc<RefCell<Framework>>,
        printer: &Rc<RefCell<BambuPrinter>>,
        store: &Rc<Store>,
    ) -> Result<(), String> {
        let path = Self::printer_state_file_path(&printer.borrow().printer_serial);
        let printer_number = printer.borrow().printer_number;
        loop {
            if store.is_initialized() {
                break;
            }
            Timer::after_millis(100).await;
        }
        Timer::after_millis(250).await;

        let mut err_str = String::new();

        for trial in 1..=3 {
            // in separate section for file_store to be release for later load
            let file_store = framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            match file_store.read_file_str(&path).await {
                Ok(state_str) => {
                    if state_str.trim().is_empty() {
                        err_str = format!("[{printer_number}] Loaded empty state file {path} in trial {trial}");
                        term_error!("{err_str}"); // will retry after timeout below
                    } else {
                        match serde_json::from_str::<PrinterPersistentState>(&state_str) {
                            Ok(printer_state) => {
                                printer.borrow_mut().init_printer_persistent_state(printer_state, store);
                                term_info!("[{}] Restored printer state from SDCard", printer_number);
                                return Ok(());
                            }
                            Err(err) => {
                                err_str = format!("[{printer_number}] Failed to Parse Printer State File (Check Terminal for More Info)");
                                term_error!("[{}] Failed to parse printer state in file {} : {}", printer_number, path, err);
                                error!("[{printer_number} Printer state state file content: {state_str}]");
                                return Err(err_str);
                            }
                        }
                    }
                }
                Err(err) => {
                    term_error!(
                        "[{}] Can't read printer state file (ok, if first printer run) {} : {}",
                        printer_number,
                        path,
                        err
                    );
                    return Ok(());
                }
            }
            Timer::after_millis(250).await;
        }
        Err(err_str)
    }

    // Ok(true) - saved, Ok(false) nothing to save
    pub async fn store_printer_state(
        framework: &Rc<RefCell<Framework>>,
        printer: &Rc<RefCell<BambuPrinter>>,
        view_model: &Rc<RefCell<crate::view_model::ViewModel>>,
    ) -> Result<bool, String> {
        let mut printer_state_str = None;
        let mut printer_serial = None;
        {
            let printer_borrow = printer.borrow();
            if printer_borrow.auto_restore_k && printer_borrow.pending_k_restore_sequence {
                // don't change store until restoring k is done
                return Ok(false);
            }
            let ams_trays_dirty = printer_borrow.ams_trays_dirty.iter().any(|&v| v);

            if ams_trays_dirty
                || printer_borrow.virt_trays_dirty
                || printer_borrow.extruders_dirty
                || printer_borrow.ams_exist_bits_dirty
                || printer_borrow.tray_exist_bits_dirty
                || printer_borrow.tray_read_done_bits_dirty
                || printer_borrow.calibrations_dirty
                || printer_borrow.printer_name_dirty
                || printer_borrow.force_store_state
                || printer_borrow.relevant_extruder_state_dirty
            {
                debug!(
                    "[{}] Dirty status: AMS slots({}), Ext slots({}), Extruders({}), AmsExists: ({}), Tray Exists: ({}), Try Read Done ({}), Calibrations ({}), Printer Name ({}), Relevant Extruder State ({}), Forced Store ({})",
                    printer_borrow.printer_number, ams_trays_dirty, printer_borrow.virt_trays_dirty, printer_borrow.extruders_dirty,
                    printer_borrow.ams_exist_bits_dirty, printer_borrow.tray_exist_bits_dirty, printer_borrow.tray_read_done_bits_dirty, printer_borrow.calibrations_dirty, printer_borrow.printer_name_dirty, printer_borrow.relevant_extruder_state_dirty,
                    printer_borrow.force_store_state,
                );
                printer_serial = Some(printer_borrow.printer_serial.clone());
                let printer_state = PrinterPersistentState {
                    ams_trays: Cow::Borrowed(printer_borrow.ams_trays()),
                    virt_tray: None,
                    virt_trays: Some(Cow::Borrowed(printer_borrow.virt_trays())),
                    nozzle_diameter: None, // for backwards compatibility before dual extruder printer
                    ams_exist_bits: printer_borrow.inner_ams_exist_bits,
                    tray_exist_bits: printer_borrow.inner_tray_exist_bits,
                    tray_read_done_bits: printer_borrow.inner_tray_read_done_bits,
                    calibrations: Cow::Borrowed(&printer_borrow.calibrations),
                    printer_name: printer_borrow.inner_printer_name.clone(),
                    extruders: Some(Cow::Borrowed(&printer_borrow.inner_extruders)),
                    extruder_state: *printer_borrow.extruder_state(),
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
            let virt_trays_dirty = printer.borrow().virt_trays_dirty;
            let ams_trays_dirty = printer.borrow().ams_trays_dirty;
            let extruders_dirty = printer.borrow().extruders_dirty;
            let ams_exist_bits_dirty = printer.borrow().ams_exist_bits_dirty;
            let tray_exist_bits_dirty = printer.borrow().tray_exist_bits_dirty;
            let tray_read_done_bits_dirty = printer.borrow().tray_read_done_bits_dirty;
            let calibrations_dirty = printer.borrow().calibrations_dirty;
            let printer_name_dirty = printer.borrow().printer_name_dirty;
            let relevant_extruder_state_dirty = printer.borrow().relevant_extruder_state_dirty;

            printer.borrow_mut().ams_trays_dirty.fill(false);
            printer.borrow_mut().virt_trays_dirty = false;
            printer.borrow_mut().extruders_dirty = false;
            printer.borrow_mut().ams_exist_bits_dirty = false;
            printer.borrow_mut().tray_exist_bits_dirty = false;
            printer.borrow_mut().tray_read_done_bits_dirty = false;
            printer.borrow_mut().calibrations_dirty = false;
            printer.borrow_mut().printer_name_dirty = false;
            printer.borrow_mut().force_store_state = false;
            printer.borrow_mut().relevant_extruder_state_dirty = false;
            let mut file_store = file_store.lock().await;

            let undo_store = |code: i32| {
                let mut printer_borrow = printer.borrow_mut();
                printer_borrow.virt_trays_dirty |= virt_trays_dirty;
                for (x, y) in printer_borrow.ams_trays_dirty.iter_mut().zip(&ams_trays_dirty) {
                    *x |= *y
                }
                printer_borrow.extruders_dirty |= extruders_dirty;
                printer_borrow.ams_exist_bits_dirty |= ams_exist_bits_dirty;
                printer_borrow.tray_exist_bits_dirty |= tray_exist_bits_dirty;
                printer_borrow.tray_read_done_bits_dirty |= tray_read_done_bits_dirty;
                printer_borrow.calibrations_dirty |= calibrations_dirty;
                printer_borrow.printer_name_dirty |= printer_name_dirty;
                printer_borrow.relevant_extruder_state_dirty |= relevant_extruder_state_dirty; 
                printer_borrow.force_store_state = true; // is is set to true in case we miss something or forget in the future
                view_model.borrow().message_box(
                    &format!("State Store Error ({code})"),
                    "Unexpected Error Storing State, Will Retry",
                    "Please report on Github/Discord !!!",
                    crate::app::StatusType::Error,
                    0,
                );
            };
            if printer_state_str.trim().is_empty() {
                term_error!("[{}] Somehow stored state is an empty string", printer.borrow().printer_number);
                view_model.borrow().message_box(
                    "State Store Error", 
                    &format!("[{}] Printer State to Store is Empty",printer.borrow().printer_number),
                    "Please report on Github/Discord !!!",
                    crate::app::StatusType::Error,
                    0);
            }
            match file_store.create_write_file_str(&path, &printer_state_str).await {
                Ok(_) => {
                    let verify_read_str = file_store.read_file_str(&path).await;
                    match verify_read_str {
                        Ok(verify_read_str) => {
                            if verify_read_str == printer_state_str {
                                info!("[{}] Store state verification passed", printer.borrow().printer_number);
                                Ok(true)
                            } else {
                                undo_store(1);
                                error!(
                                    "[{}] During store state verification read data differ from written data",
                                    printer.borrow().printer_number
                                );
                                Err(String::from("Verification of state store failed"))
                            }
                        }
                        Err(err) => {
                            undo_store(2);
                            error!(
                                "[{}] Failed to verify store printer restart state : {err}",
                                printer.borrow().printer_number
                            );
                            Err(String::from("Error reading state store to verify : {err}"))
                        }
                    }
                }
                Err(err) => {
                    undo_store(3);
                    error!("[{}] Failed to store printer restart state : {err}", printer.borrow().printer_number);
                    Err(String::from("Error storing state : {err}"))
                }
            }
        } else {
            Ok(false)
        }
    }

    pub fn printer_state_file_path(printer_serial: &str) -> String {
        let len = printer_serial.len();
        let file_ext = &printer_serial[len - 3..];
        let file_name = &printer_serial[len - 11..len - 3];
        format!("/state/{file_name}.{file_ext}/startup.jsn")
    }
    pub fn printer_state_path_for_file(&self, file: &str) -> String {
        let len = self.printer_serial.len();
        let file_ext = &self.printer_serial[len - 3..];
        let file_name = &self.printer_serial[len - 11..len - 3];
        format!("/state/{file_name}.{file_ext}/{file}")
    }
}

#[allow(clippy::too_many_arguments)]
impl BambuPrinter {
    pub fn new(
        printer_number: usize,
        printer_index: usize,
        printer_serial: &str,
        printer_access_code: &str,
        printer_config_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        track_print_consume: bool,
        fetch_3mf: Fetch3mf,
        write_packets: Rc<WritePacketsChannel>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
        store_state_request_channel: Rc<StoreStateRequestChannel>,
    ) -> Rc<RefCell<BambuPrinter>> {
        let myself = Self::internal_new(
            printer_number,
            printer_index,
            printer_serial,
            printer_access_code,
            printer_config_name,
            printer_ip,
            auto_restore_k,
            track_print_consume,
            fetch_3mf,
            write_packets,
            app_config,
            restart_printer,
            log_filter,
            store_state_request_channel,
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
        printer_config_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        track_print_consume: bool,
        fetch_3mf: Fetch3mf,
        write_packets: Rc<WritePacketsChannel>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
        store_state_request_channel: Rc<StoreStateRequestChannel>,
    ) -> Self {
        let array = printer_serial.as_bytes();
        let key: &[u8; 16] = b"SpoolEaseIsGreat"; // doesn't really matter, just can't ever change
        let hasher = siphasher::sip::SipHasher24::new_with_key(key);
        let hashed_serial = hasher.hash(array);
        let hashed_encoded_serial = URL_SAFE_NO_PAD.encode(hashed_serial.to_le_bytes());
        let printer_uuid_to_encode = hashed_encoded_serial;

        // Define a user oriented name for selection
        let printer_selector_name = if let Some(printer_name) = &printer_config_name {
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
            configured_printer_name: printer_config_name.clone(),
            inner_printer_name: printer_config_name.clone().unwrap_or(default_printer_name()),
            printer_selector_name,
            auto_restore_k,
            track_print_consume,
            fetch_3mf,
            printer_ip: printer_ip.unwrap_or(Ipv4Address::new(0, 0, 0, 0)),
            printer_uuid_to_encode,
            printer_connectivity_ok: None,
            // inner_nozzle_diameter: None,
            inner_extruders: core::array::from_fn(|_| Extruder::default()),
            extruders_dirty: false,
            relevant_extruder_state_dirty: false,
            inner_ams_trays: alloc::vec![Tray::default();24],
            inner_virt_trays: [Tray::default(), Tray::default()],
            ams_trays_dirty: [false; 24],
            force_store_state: false,
            virt_trays_dirty: false,
            ams_exist_bits_dirty: false,
            tray_exist_bits_dirty: false,
            tray_read_done_bits_dirty: false,
            calibrations_dirty: false,
            printer_name_dirty: false,
            calibrations: Vec::new(),
            write_packets,
            observers: Vec::new(),
            app_config,
            inner_tray_exist_bits: None,
            inner_tray_read_done_bits: None,
            tray_reading_bits: None,
            inner_ams_exist_bits: None,
            restart_printer,
            log_filter,
            printer_was_disconnected: true,
            pending_k_restore_sequence: true,
            curr_print_project: None,
            loaded_print_project: None,
            inner_extruder_state: None, // h2d field for knowing which extruder is active
            tray_tar: [255, 255],       // format for these fields: 0-16 (regular AMS), 128-135 (AMS-HT), 254 (external), 255 (none)
            tray_now: [255, 255],
            tray_pre: [255, 255],
            gcode_state: GcodeState::Unknown,
            layer_num: -1,
            locked_mode: None,
            store_state_request_channel,
            ams_info: alloc::vec![AmsInfo::default();14], // 0..3: standard ams, 4..11: 128..135 (HT), 12: 254 (external - left?), 13: 255 (external - right?)
        }
    }

    pub fn model(&self) -> PrinterModel {
        // https://wiki.bambulab.com/en/general/find-sn
        let sn_prefix = &self.printer_serial[..self.printer_serial.char_indices().nth(3).map_or(self.printer_serial.len(), |(i, _)| i)];
        match sn_prefix {
            "094" => PrinterModel::H2D,
            "239" => PrinterModel::H2DPro,
            "00M" => PrinterModel::X1C,
            "03W" => PrinterModel::X1E,
            "01P" => PrinterModel::P1S,
            "01S" => PrinterModel::P1P,
            "039" => PrinterModel::A1,
            "030" => PrinterModel::A1Mini,
            "22E" => PrinterModel::P2S,
            "31B" => PrinterModel::H2C,
            _ => PrinterModel::Unknown,
        }
    }

    pub fn model_series(&self) -> PrinterModelSeries {
        match self.model() {
            PrinterModel::Unknown => PrinterModelSeries::Unknown,
            PrinterModel::X1 => PrinterModelSeries::X1,
            PrinterModel::X1C => PrinterModelSeries::X1,
            PrinterModel::X1E => PrinterModelSeries::X1,
            PrinterModel::P1P => PrinterModelSeries::P1,
            PrinterModel::P1S => PrinterModelSeries::P1,
            PrinterModel::P2S => PrinterModelSeries::P2,
            PrinterModel::A1Mini => PrinterModelSeries::A1,
            PrinterModel::A1 => PrinterModelSeries::A1,
            PrinterModel::H2D => PrinterModelSeries::H2,
            PrinterModel::H2DPro => PrinterModelSeries::H2,
            PrinterModel::_H2S => PrinterModelSeries::H2,
            PrinterModel::H2C => PrinterModelSeries::H2,
        }
    }

    #[allow(dead_code)]
    pub fn reset_printer(&mut self) {
        let empty = Self::internal_new(
            self.printer_number,
            self.printer_index,
            &self.printer_serial,
            &self.printer_access_code,
            &self.configured_printer_name,
            &self.configured_printer_ip,
            self.auto_restore_k,
            self.track_print_consume,
            self.fetch_3mf,
            self.write_packets.clone(),
            self.app_config.clone(),
            self.restart_printer.clone(),
            self.log_filter,
            self.store_state_request_channel.clone(),
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

    fn get_calibration(&self, extruder: &Extruder, cali_idx: i32) -> Option<&Calibration> {
        let extruder_id = extruder.id as i32;
        let nozzle_diameter = extruder.diameter.as_deref()?;
        let nozzle_type_code = extruder.nozzle_type_code()?;

        self.calibrations.iter().find(|cal| {
            cal.extruder == extruder_id && cal.diameter == nozzle_diameter && cal.nozzle_type_code() == nozzle_type_code && cal.cali_idx == cali_idx
        })
    }

    fn get_cali_k_value(&self, extruder: &Extruder, cali_idx: i32) -> Option<String> {
        self.get_calibration(extruder, cali_idx).map(|calibration| calibration.k_value.clone())
    }

    pub fn get_tray_resolved_k_value(&self, tray: &Tray, tray_id: i32) -> String {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        let mut k_result = "(0.020)".to_string();
        if let Some(k_from_tray) = &tray.k_from_tray {
            k_result = format!("({k_from_tray:.3})");
        }

        if let Ok(extruder) = self.get_extruder_for_tray(tray_id) {
            if let Some(cali_idx) = tray.cali_idx {
                if let Some(k_value) = self.get_cali_k_value(extruder, cali_idx) {
                    let k_float = f32::from_str(&k_value).unwrap_or_default();
                    k_result = format!("{:.3}", k_float);
                }
            }
        }
        k_result
    }

    fn tray_from_update(&self, tray_update: &PrintTray) -> Result<Option<Tray>, String> {
        if let (Some(tray_type_update), Some(tray_info_idx_update), Some(_tray_color_update)) =
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
            if tray_info_idx_update.starts_with("00") {
                // tray_info_idx CAN end with 00, but not start with 00 afaik
                // might end with 00, so checking if starts with 00
                warn!("[{}] ???? tray_info_idx with 00 suffix", self.printer_number);
                debug!("[{}] {:?}", self.printer_number, tray_update);
                return Err("tray_info_idx junk".to_string());
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
    pub fn get_updated_tray(&self, old_tray: &Tray, tray_update: Option<&PrintTray>, tray_id: i32) -> Option<Tray> {
        if tray_id != 255 && tray_id != 254 {
            // AMS tray
            if let Some(tray_exist_bits) = self.tray_exist_bits() {
                let tray_exist = ((tray_exist_bits >> tray_id) & 0x01) != 0;

                if tray_exist {
                    let tray_reading = self.tray_reading_bits.is_some_and(|x| ((x >> tray_id) & 0x01) != 0);
                    let tray_read_done = self.tray_read_done_bits().is_some_and(|x| ((x >> tray_id) & 0x01) != 0);

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
                        // TODO: This is not optimal because it still returns a tray and therefore drives UI update
                        // even when no data changed. Better also compare Tray and return None if nothing changed
                        // but need to be careful about that (in case flags changed but not content)
                        // Maybe outside of this separate tray update from flags update (reading/read-done,tray_tar/now/pre, etc.)
                        let mut new_tray = old_tray.clone();
                        new_tray.state = TrayState::Empty;
                        new_tray
                    };
                    new_tray.state = TrayState::Spool;
                    new_tray.meta_info = old_tray.meta_info.clone(); // TODO: can 'take' if it work properly (need to mut old_tray)

                    if tray_reading {
                        new_tray.state = TrayState::Reading;
                    }
                    if tray_read_done {
                        new_tray.state = self.get_tray_detailed_ready_state(tray_id);
                    }
                    Some(new_tray)
                } else {
                    // TODO: This is wrong! The correct thing to do is to upadte the information from the printer
                    //       and not just assume that nothing changed, an external command could change the color
                    //       it's currently being handled by monitoring those commands and dealing with them as well
                    //       but not sure they are sent when modified via printer console
                    //       Also - IF outside DEV mode push_all is not accepted, this will be important to speed up
                    //       showing AMS colors at least, because at least on P1S it can be very long time until
                    //       printer sends a message that contains the ams_exist_bits and tray_exist_bits

                    // In case the tray is empty (so no ready bits), we still want to keep the filamen-info of the tray, but set it as empty
                    // special case handling (different than Bambustudio).
                    // we remember historical color, K, etc (which the printer also remembers, just doesn't report)
                    let mut new_tray = old_tray.clone();
                    new_tray.state = TrayState::Empty;
                    new_tray.meta_info = TrayMetaInfo::default(); // if spool is removed, erase tag info and consume information
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
                        if matches!(new_tray.filament, Filament::Unknown) {
                            new_tray.state = TrayState::Empty;
                            new_tray.meta_info = TrayMetaInfo::default();
                        } else {
                            new_tray.state = self.get_tray_detailed_ready_state(tray_id);
                            if old_tray.state == TrayState::Loaded && new_tray.state == TrayState::Empty {
                                // this is the case of unloading external tray
                                new_tray.meta_info = TrayMetaInfo::default();
                            } else {
                                new_tray.meta_info = old_tray.meta_info.clone();
                                // TODO: can take if work properly
                            }
                        }
                        Some(new_tray)
                    } else {
                        // Empty tray data means tray empty in case of external
                        Some(Tray::unknown())
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

    fn get_active_extruder_from_extruder_state(extruder_state: &Option<i32>) -> Option<usize> {
        let extruder_index = (extruder_state.unwrap_or_default() >> 4 & 0xF) as usize;
        if extruder_index <= 1 {
            Some(extruder_index)
        } else {
            None
        }
    }

    fn get_active_extruder(&self) -> Option<usize> {
        Self::get_active_extruder_from_extruder_state(self.extruder_state())
    }

    fn get_common_tray_active(active_extruder: usize, extruder_tray_now: i32) -> Option<i32> {
        if extruder_tray_now == 255 {
            None
        } else if extruder_tray_now == 254 {
            if active_extruder == 0 {
                Some(255)
            } else {
                Some(254)
            }
        } else {
            Some(extruder_tray_now)
        }
    }
    fn get_tray_active(&self) -> Option<i32> {
        let active_extruder = self.get_active_extruder()?;
        Self::get_common_tray_active(active_extruder, self.tray_now[active_extruder])
    }

    fn get_print_data_tray_active(print: &bambu_api::PrintData, current_tray_active: Option<i32>) -> Option<i32> {
        // the active tray, usually also printing, convention like tray_xxx fields.
        // always a single value, also in multi extruder printers
        if let Some(extruder) = print.device.as_ref().and_then(|d| d.extruder.as_ref()) {
            let active_extruder = Self::get_active_extruder_from_extruder_state(&extruder.state)?;
            let extruder_snow = extruder.info[active_extruder].snow;
            Self::get_common_tray_active(active_extruder, extruder_snow)
        } else if let Some(tray_now) = &print.ams.as_ref().and_then(|ams| ams.tray_now) {
            // single tray printer w/o extruder field
            Self::get_common_tray_active(0, *tray_now)
        } else {
            // no change, so return the current
            current_tray_active
        }
    }

    fn get_tray_detailed_ready_state(&self, tray_id: i32) -> TrayState {
        // tray_id - 0..24
        // tray_active: tray_xxx format (0..16, 128..135, 254, 255)

        // Normal trays are ready or loaded in this function (which checks for detailed ready)
        // External are either Empty or Loaded, no such thing as ready

        let perliminary_res = if let Some(active_tray_id) = self.get_tray_active() {
            if active_tray_id == tray_id {
                TrayState::Loaded
            } else {
                TrayState::Ready
            }
        } else {
            TrayState::Ready
        };

        if (tray_id == 254 || tray_id == 255) && perliminary_res == TrayState::Ready {
            TrayState::Empty
        } else {
            perliminary_res
        }

        // loading/unloading is more complex, should also use "ams_status" and maybe "ams_rfid" from mqtt
        // See Bambustudio statuspanel.cpp & DeviceManager.cpp
        // ams_status_main and ams_status_sub
        // It seems to be as follows, but not implemented, not needed and not sure fully reliable
        // assume switch from slot 2 to slot 1:
        //
        // tray_tar   tray_now  tray_pre  ams_status&0xFF
        //
        //    2          2         2                        initial state
        //    1          2                     2, 3, 4      unloading tray_now
        //    1          1                     5, 6, 7      loading tray_now (same as tar now)
        //    1          1         1    ?ams_status = 768   loaded/printing (maybe earlier using additional field)
    }

    pub fn get_ams_and_slot_id(tray_id: usize) -> (usize, usize) {
        if tray_id < 16 {
            // AMS
            let ams_id = tray_id / 4;
            let ams_tray_id = tray_id % 4;
            (ams_id, ams_tray_id)
        } else if tray_id < 24 {
            // AMS HT
            let ams_id = 128 + (tray_id - 16);
            let ams_tray_id = 0;
            (ams_id, ams_tray_id)
        } else {
            // 254/255 tray_id option
            (tray_id, 0)
        }
    }

    // Done: TODO: External (2)
    #[allow(clippy::manual_map)]
    pub fn get_tray_index_from_print_msg(ams_id: Option<i32>, tray_id: Option<i32>, _slot_id: Option<i32>) -> Option<usize> {
        // returns either index into the ams_trays or 254/255 for external trays (other functions may depend on this)
        if let (Some(ams_id), Some(tray_id)) = (ams_id, tray_id) {
            if ams_id <= 3 {
                Some((ams_id * 4 + tray_id) as usize)
            } else if ams_id < 128 + 8 {
                // AMS HT
                Some((16 + ams_id - 128) as usize)
            } else if ams_id == 255 {
                Some(255)
            } else {
                Some(tray_id as usize)
            }
        } else if let Some(tray_id) = tray_id {
            if tray_id == 254 {
                Some(255)
            } else {
                Some(tray_id as usize)
            }
        } else {
            None
        }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__vir_slots(&mut self, vir_slot: &[PrintTray]) -> (bool, Option<SpoolId>) {
        let mut changed_res = false;
        let mut removed_tag_res = None;
        for vt_slot in vir_slot {
            if let Some(external_slot_id) = vt_slot.id {
                let extruder_id = if external_slot_id == 254 { 1 } else { 0 };
                let (changed, removed_tag) = self.process_print_message__vt_tray(extruder_id, vt_slot);
                changed_res |= changed;
                removed_tag_res = removed_tag;
            }
        }
        (changed_res, removed_tag_res)
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__vt_tray(&mut self, extruder_id: u32, v_tray: &PrintTray) -> (bool, Option<SpoolId>) {
        let old_tray = self.virt_trays()[extruder_id as usize].clone();
        let external_tray_id = if extruder_id == 0 { 255 } else { 254 };
        let new_tray = self.get_updated_tray(&old_tray, Some(v_tray), external_tray_id);
        if let Some(new_tray) = new_tray {
            let removed_tag = if old_tray.state == TrayState::Loaded && new_tray.state != TrayState::Loaded {
                old_tray.meta_info.spool_id
            } else {
                None
            };
            self.set_virt_tray(extruder_id, new_tray);
            return (true, removed_tag);
        }
        (false, None)
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__ams_filament_setting(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;

        // updating ONLY filament and not state for the theoretical case when filament is set externally when there isn't a spool
        // theoretically possible if want to supssport that in this app using nfc as a source for example
        if let Some(tray_index) = Self::get_tray_index_from_print_msg(print.ams_id, print.tray_id, print.slot_id) {
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
            // tray_id == 254 in the response message is for old firmwares
            // in new firmwares the response message arrives with ams_id==255 && tray_id==Some(0) (not like the command which is tray 254 and slot_id 0)
            // check first the use of ams_id, if not available then switch to tray_index 254
            if (print.ams_id.is_none() && tray_index == 254) || print.ams_id == Some(255) || print.ams_id == Some(254) {
                // External tray handling
                let extruder_id = if print.ams_id == Some(254) { 1 } else { 0 };
                let virt_tray_id = if extruder_id == 1 { 254 } else { 255 };
                let virt_tray_state = self.get_tray_detailed_ready_state(virt_tray_id);
                self.update_virt_tray(extruder_id, |virt_tray| {
                    virt_tray.state = virt_tray_state;
                });
                // in case of external slot, clearing filament should remove tag
                // that's because setting filament from spool marks the tag
                // can in addition do it all when spool is removed after loaded, but that's elsewhere to do
                if new_filament == Filament::Unknown {
                    self.update_virt_tray(extruder_id, |virt_tray| {
                        virt_tray.meta_info = TrayMetaInfo::default();
                    });
                }
                self.update_virt_tray(extruder_id, |virt_tray| {
                    virt_tray.filament = new_filament;
                });
            } else {
                // Handle AMS tray
                self.update_ams_tray(tray_index, |ams_tray| {
                    ams_tray.filament = new_filament;
                    ams_tray.k_from_tray = None;
                });
                // no change to tray state in case of AMS
            }
            change_made = true;
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_sel(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        if let (Some(tray_id), Some(cali_idx)) = (
            Self::get_tray_index_from_print_msg(print.ams_id, print.tray_id, print.slot_id),
            &print.cali_idx,
        ) {
            self.update_any_tray(tray_id, |tray| {
                tray.cali_idx = if *cali_idx == -1 || *cali_idx == 0 { None } else { Some(*cali_idx) };
            });
            change_made = true;
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_get(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        // ignore if filament_id isn't ""
        if let Some(nozzle_diameter) = &print.nozzle_diameter {
            if print.filament_id.as_deref() == Some("") {
                if let Some(ref filaments) = print.filaments {
                    // filaments is really calibrations
                    change_made = true;
                    self.calibrations.retain(|cal| &cal.diameter != nozzle_diameter);
                    for filament in filaments {
                        let calibration = Calibration::from(filament, nozzle_diameter);
                        self.calibrations.push(calibration);
                    }
                    self.calibrations_dirty = true;
                }
            }
        }

        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__common(&mut self, print: &bambu_api::PrintData) -> (bool, HashMap<usize, SpoolId>) {
        let mut removed_tags = HashMap::<usize, String>::new();
        // let command = print.command.unwrap_or_default();
        if let Some(fun) = &print.fun {
            if let Ok(fun) = u64::from_str_radix(fun, 16) {
                if fun & 0x20000000 != 0 {
                    // locked mode
                    self.locked_mode = Some(true)
                } else {
                    // dev mode
                    self.locked_mode = Some(false)
                }
            }
        }
        // Get a snapshot of current trays and diameter before any later change, to later be able to update cali_idx if removed
        // leave this section here because later changes will affect it (like self.nozzle_diameter)

        let full_push_status = print.ams.is_some() && (print.vt_tray.is_some() || print.vir_slot.is_some());
        let prev_state = if full_push_status && self.auto_restore_k && self.printer_was_disconnected {
            // TODO: To save memory (a few kb's, might be needed in the future) copy from ams_trays only the data requried and not entire tray
            Some((self.ams_trays().to_vec(), self.virt_trays()[0].clone(), self.nozzle_diameter(0).clone()))
        } else {
            None
        };

        let mut print_project_caused_change = false;
        if self.curr_print_project.is_some() {
            print_project_caused_change = self.process_print_message__print_project_logic(print);
        }

        // print related field monitored globally unrelated to print
        // should come AFTER processing of print_project_logic

        if let Some(gcode_state) = print.gcode_state {
            self.gcode_state = gcode_state;
        }

        if let Some(layer_num) = print.layer_num {
            self.layer_num = layer_num;
        }

        // Deal with nozzle diameter
        let mut extruders_change_made = false;
        if let Some(nozzles) = print.device.as_ref().and_then(|d| d.nozzle.as_ref()) {
            for nozzle in &nozzles.info {
                if nozzle.id == 0 || nozzle.id == 1 {
                    extruders_change_made |= self.set_extruder_info(nozzle.id as u32, nozzle);
                }
            }
        } else if let Some(nozzle_diameter) = &print.nozzle_diameter {
            let old_nozzle_diameter = self.nozzle_diameter(0).clone();
            self.set_nozzle_diameter(0, Some(nozzle_diameter.clone()));
            extruders_change_made = old_nozzle_diameter != *self.nozzle_diameter(0);
        }

        // Deal with tray_xxx - need to do before ams because depends on both AMS and Device sections, and used at the end of ams processing
        let tray_xxx_change_made = self.process_print_message__tray_xxx(print);

        // Deal with ams changes
        let mut ams_change_made = false;
        if let Some(ams) = &print.ams {
            (ams_change_made, removed_tags) = self.process_print_message__ams(ams);
        }

        // Deal with external tray changes
        let mut vt_tray_change_made = false;
        let removed_tag;
        if let Some(vir_slot) = &print.vir_slot {
            // this is hd2 version of external slotS
            (vt_tray_change_made, removed_tag) = self.process_print_message__vir_slots(vir_slot);
            if let Some(removed_tag) = removed_tag {
                removed_tags.insert(254, removed_tag);
            }
        } else if let Some(v_tray) = &print.vt_tray {
            // this is older printers external slot
            (vt_tray_change_made, removed_tag) = self.process_print_message__vt_tray(0, v_tray);
            if let Some(removed_tag) = removed_tag {
                removed_tags.insert(254, removed_tag);
            }
        } else if tray_xxx_change_made {
            // I believe this situation can happen only in pre H2D printers ( in H2D always seen vir_slot)
            // Still, let's handle it just in case
            for (extruder_id, external_tray_id) in [(0u32, 255), (1u32, 254)] {
                let new_vt_tray_detailed_ready_state = self.get_tray_detailed_ready_state(external_tray_id);
                let curr_vt_tray_detailed_ready_state = self.virt_trays()[extruder_id as usize].state;
                self.update_virt_tray(0, |tray| tray.state = new_vt_tray_detailed_ready_state);

                if curr_vt_tray_detailed_ready_state == TrayState::Loaded && new_vt_tray_detailed_ready_state != TrayState::Loaded {
                    let mut vt_tray = self.virt_trays()[extruder_id as usize].clone();
                    let spool_id = vt_tray.meta_info.spool_id.take();
                    vt_tray.meta_info = TrayMetaInfo::default();
                    self.set_virt_tray(extruder_id, vt_tray);
                    if let Some(spool_id) = spool_id {
                        removed_tags.insert(external_tray_id as usize, spool_id);
                    }
                }
            }
        }

        // Check if any change affects need for special restore state case
        if full_push_status && self.auto_restore_k && self.printer_was_disconnected {
            self.printer_was_disconnected = false;
            let mut triggered_k_restore_sequence = false;
            if let Some(prev_state) = prev_state {
                if self.ams_trays()[..] != prev_state.0 || self.virt_trays()[0] != prev_state.1 {
                    let spawner = self.app_config.borrow().framework.borrow().spawner;
                    spawner
                        .spawn_heap(fix_k_on_restart(
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

        // Report back to caller
        let change_made = extruders_change_made || ams_change_made || vt_tray_change_made || print_project_caused_change || tray_xxx_change_made;
        (change_made, removed_tags)
    }

    fn normalized_h2d_tray_xxx(h2d_tray_xxx: i32) -> i32 {
        // this returns normalized the h2d snow/star/spre to be compatible with older printers tray_now/tray_tar/tray_pre
        // this need to be called only on data from message
        // the self.tray_xxx are already normalized

        let ams_id = h2d_tray_xxx >> 8;
        let tray_in_ams = h2d_tray_xxx & 0xFF; // maybe will need to change if ams
        match ams_id {
            0..3 => ams_id * 4 + (tray_in_ams & 0x03), // 0x03 because no support for more than 4 slots ams
            128..135 => ams_id,
            254 | 255 => {
                if tray_in_ams != 255 {
                    254
                } else {
                    255
                }
            } // TODO: test on h2d how do tray_xxx report with external spool
            _ => 255,
        }
    }

    fn update_h2d_tray_xxx(curr_tray_xxx: &mut i32, message_sxxx: &i32, changed: &mut bool) {
        let new_tray_xxx = Self::normalized_h2d_tray_xxx(*message_sxxx);
        if new_tray_xxx != *curr_tray_xxx {
            *curr_tray_xxx = new_tray_xxx;
            *changed = true;
        }
    }

    fn update_std_tray_xxx(curr_tray_xxx: &mut i32, message_tray_xxx: &Option<i32>, changed: &mut bool) {
        if let Some(new_tray_xxx) = message_tray_xxx {
            if new_tray_xxx != curr_tray_xxx {
                *curr_tray_xxx = *new_tray_xxx;
                *changed = true;
            }
        }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__tray_xxx(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut tray_xxx_change_made = false;

        if let Some(extruder) = print.device.as_ref().and_then(|d| d.extruder.as_ref()) {
            if let Some(state) = &extruder.state {
                self.set_extruder_state(*state);
            }
            for extruder_info in &extruder.info {
                if extruder_info.id > 1 {
                    break;
                };
                Self::update_h2d_tray_xxx(
                    &mut self.tray_tar[extruder_info.id as usize],
                    &extruder_info.star,
                    &mut tray_xxx_change_made,
                );
                Self::update_h2d_tray_xxx(
                    &mut self.tray_now[extruder_info.id as usize],
                    &extruder_info.snow,
                    &mut tray_xxx_change_made,
                );
                Self::update_h2d_tray_xxx(
                    &mut self.tray_pre[extruder_info.id as usize],
                    &extruder_info.spre,
                    &mut tray_xxx_change_made,
                );
            }
        } else if let Some(ams) = &print.ams {
            // old version tray_xxx data
            Self::update_std_tray_xxx(&mut self.tray_tar[0], &ams.tray_tar, &mut tray_xxx_change_made);
            Self::update_std_tray_xxx(&mut self.tray_now[0], &ams.tray_now, &mut tray_xxx_change_made);
            Self::update_std_tray_xxx(&mut self.tray_pre[0], &ams.tray_pre, &mut tray_xxx_change_made);
        }
        tray_xxx_change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__ams(&mut self, ams: &PrintAms) -> (bool, HashMap<usize, SpoolId>) {
        let mut change_made = false;
        let prev_tray_exist_bits = *self.tray_exist_bits();

        // first check which ams's exist
        if let Some(ams_exist_bits) = &ams.ams_exist_bits {
            let ams_exist_bits = u32::from_str_radix(ams_exist_bits, 16);
            if let Ok(ams_exist_bits) = ams_exist_bits {
                if self.ams_exist_bits().is_none() || *self.ams_exist_bits() != Some(ams_exist_bits) {
                    self.set_ams_exist_bits(Some(ams_exist_bits));
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
                if *self.tray_exist_bits() != Some(tray_exist_bits) {
                    self.set_tray_exist_bits(Some(tray_exist_bits));
                    change_made = true;
                }
            }
        }
        // tray_read_done - which trays (from those that exist) that have been "read" (meaning ready from ams perspective)
        if let Some(tray_read_done_bits) = &ams.tray_read_done_bits {
            if let Ok(tray_read_done_bits) = u32::from_str_radix(tray_read_done_bits, 16) {
                if *self.tray_read_done_bits() != Some(tray_read_done_bits) {
                    self.set_tray_read_done_bits(Some(tray_read_done_bits));
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

        // IPORTANT Note: For now doesn't seem relevatnt to change_made, nor for persistent state
        if let Some(ams_units) = &ams.ams {
            for ams in ams_units {
                if let Some(ams_info) = ams.info {
                    let extruder = (ams_info >> 8) & 0x0F;
                    let ams_id = ams.id;
                    let ams_index = match ams_id {
                        0..3 => ams_id,
                        128..135 => ams_id - 128 + 4,
                        254 | 255 => ams_id - 254 + 12,
                        _ => {
                            error!("[{}] Bad ams_id encountered: {ams_id}", self.printer_number);
                            continue;
                        }
                    } as usize;
                    self.ams_info[ams_index].extruder = extruder;
                }
            }
            // The two external tray are also treated sometimes as AMS to get extruder
            // They are not in the ams list, so setting them fixed here
            self.ams_info[12].extruder = 1; // 254 is left, extruder 1
            self.ams_info[13].extruder = 0; // 255 is right, extruder 0
        }

        let mut removed_tags: HashMap<usize, SpoolId> = HashMap::new();

        let mut _derived_ams_exist_bits = 0;
        for tray_id in 0..self.ams_trays().len() {
            let spool_removed = if let (Some(prev_tray_exist_bits), Some(new_tray_exist_bits)) = (&prev_tray_exist_bits, self.tray_exist_bits()) {
                (((prev_tray_exist_bits >> tray_id) & 0x01) != 0) && (((new_tray_exist_bits >> tray_id) & 0x01) == 0)
            } else {
                false
            };
            let (ams_id, ams_tray_id) = BambuPrinter::get_ams_and_slot_id(tray_id);
            let source_tray = if let Some(amss) = &ams.ams {
                let ams = amss.iter().find(|v| v.id == ams_id as u32);
                if let Some(ams_data) = ams {
                    _derived_ams_exist_bits |= 1 << ams_id;
                    ams_data.tray.iter().find(|v| v.id == Some(ams_tray_id as u32))
                } else {
                    None
                }
            } else {
                None
            };

            let old_tray = &self.ams_trays()[tray_id];
            let new_tray = self.get_updated_tray(old_tray, source_tray, tray_id as i32);
            if let Some(mut new_tray) = new_tray {
                change_made = true;
                let prev_tray = self.swap_ams_tray(tray_id, &mut new_tray);

                if spool_removed {
                    if let Some(prev_spool_id) = prev_tray.meta_info.spool_id.take() {
                        if self.ams_trays()[tray_id].meta_info.spool_id.is_none() {
                            // Before there was a tag and spool removed, add it to the list
                            removed_tags.insert(tray_id, prev_spool_id);
                        }
                    }
                }
            }

            // This is taken care of insidte get_updated_tray, but leaving here for now, just in case
            // debugex!(">>>> Checking tray {tray_id} ready state;")
            // if self.ams_trays()[tray_id].state == TrayState::Ready {
            //     let detailed_tray_ready_state = self.get_tray_detailed_ready_state(Some(tray_id));
            //     if detailed_tray_ready_state != TrayState::Ready {
            //         self.update_ams_tray(tray_id, |tray| tray.state = detailed_tray_ready_state);
            //         change_made = true;
            //     }
            // }
        }

        // Optional for the future if we want to speed up initialization w/o push_all
        // if self.ams_exist_bits.is_none() {
        //     self.ams_exist_bits = Some(_derived_ams_exist_bits);
        // }
        (change_made, removed_tags)
    }

    pub fn process_print_message(&mut self, print: &bambu_api::PrintData) -> (bool, HashMap<usize, SpoolId>) {
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
        let mut removed_tags = HashMap::new();
        let mut processed_specific_command = false;
        if let Some(command) = &print.command {
            processed_specific_command = true;
            if command == "ams_filament_setting" {
                change_made = change_made || self.process_print_message__ams_filament_setting(print)
            } else if command == "extrusion_cali_set" || command == "extrusion_cali_del" {
                // trigger request command for cali_get (request, not response)
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    self.fetch_filament_calibrations(nozzle_diameter);
                }
                change_made = true;
            } else if command == "extrusion_cali_sel" {
                // update the tray with the new k factor
                change_made = change_made || self.process_print_message__extrusion_cali_sel(print)
            } else if command == "extrusion_cali_get" {
                // TODO: Check: distinguish between command that was sent and the result, which are structured the same
                // here we want to process only the results (the one that includes the list of filaments )
                change_made = change_made || self.process_print_message__extrusion_cali_get(print);
            } else if command == "project_file" {
                change_made = change_made || self.process_print_message__project_file(print);
            } else {
                processed_specific_command = false;
            }
            if self.log_filter >= log::Level::Debug {
                debug!("[{}]    {command} message", &self.printer_number);
            }
        }
        if !processed_specific_command {
            (change_made, removed_tags) = self.process_print_message__common(print);
            if self.loaded_print_project.is_some() {
                if let Some(gcode_state) = print.gcode_state {
                    let loaded_project_id = self.loaded_print_project.as_ref().unwrap().project_id.clone();
                    if [GcodeState::RUNNING, GcodeState::PREPARE, GcodeState::PAUSE].contains(&gcode_state) {
                        if let Some(project_id) = print.project_id.clone() {
                            if loaded_project_id == project_id {
                                let print_project = self.loaded_print_project.take();
                                if let Some(print) = &print_project {
                                    self.update_trays_from_print_job(print);
                                }
                                self.curr_print_project = print_project;
                                info!("[{}] Resume tracking print project id {}", self.printer_number, loaded_project_id);
                            } else {
                                info!(
                                    "[{}] Resume tracking print loaded project id {} different than running project_id {}",
                                    self.printer_number, loaded_project_id, project_id
                                );
                                self.loaded_print_project = None;
                                let _ = self
                                    .store_state_request_channel
                                    .try_send(view_model::StoreStateRequest::DeletePrintProject {
                                        printer_index: self.printer_index,
                                    });
                            }
                        } else {
                            warn!(
                                "[{}] On trying to resume print received {:?} but without project_id, continue waitinge;",
                                self.printer_number, gcode_state
                            );
                        }
                    } else {
                        info!(
                            "[{}] Can't resume tracking print loaded project id {} because it ended before SpoolEase restarted",
                            self.printer_number, loaded_project_id
                        );
                        self.loaded_print_project = None;
                        let _ = self
                            .store_state_request_channel
                            .try_send(view_model::StoreStateRequest::DeletePrintProject {
                                printer_index: self.printer_index,
                            });
                    }
                }
            }
        }
        (change_made, removed_tags)
    }

    pub fn notify_printer_connect_status(&mut self, status: bool) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_printer_connect_status(self, status);
        }
    }

    pub fn update_ams_trays_done(&mut self, prev_trays_bits: &TrayBits, new_trays_bits: &TrayBits, removed_tags: &HashMap<usize, SpoolId>) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_trays_update(self, prev_trays_bits, new_trays_bits, removed_tags);
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
        write_packets: Rc<WritePacketsChannel>,
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

    pub async fn request_version_info_async(
        printer_serial: &String,
        printer_number: usize,
        log_filter: log::LevelFilter,
        write_packets: Rc<WritePacketsChannel>,
    ) {
        let cmd = crate::bambu_api::GetVersionCommand::new();
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, printer_number, log_filter, write_packets, payload).await;
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
        write_packets: Rc<WritePacketsChannel>,
    ) {
        let cmd = crate::bambu_api::PushAllCommand::new();
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, printer_number, log_filter, write_packets, payload).await;
    }

    pub fn fetch_filament_calibrations(&self, nozzle_diameter: &str) {
        // Is this command also causing errors when printer is locked?

        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        if !self.is_locked() {
            self.publish_payload(payload);
        }
    }

    pub async fn fetch_filament_calibrations_async(
        printer_serial: &String,
        printer_number: usize,
        log_filter: log::LevelFilter,
        write_packets: Rc<WritePacketsChannel>,
        nozzle_diameter: &str,
    ) {
        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, printer_number, log_filter, write_packets, payload).await;
    }

    pub fn fill_filament_defaults_if_needed(&self, filament: &mut FilamentInfo) -> bool {
        // fill in temps based on material only type only (and then replace the tray_info_idx/slicer-material to the base material one) if not available
        // returns false if can't send to printer due to lack of information
        if filament.tray_type.is_empty() {
            return false;
        }
        let mut res = true;
        if filament.nozzle_temp_min == 0 || filament.nozzle_temp_max == 0 || filament.tray_info_idx.is_empty() {
            res = false;
            for (line_index, material_line) in MATERIALS.lines().enumerate() {
                if line_index == 0 {
                    continue;
                } // skip title line
                let mut split = material_line.split(',');
                if let Some(material) = split.next() {
                    if material == filament.tray_type {
                        if let (Some(filament_id), Some(nozzle_temp_low), Some(nozzle_temp_high)) = (split.next(), split.next(), split.next()) {
                            if let (Ok(nozzle_temp_low), Ok(nozzle_temp_high)) = (nozzle_temp_low.parse::<u32>(), nozzle_temp_high.parse::<u32>()) {
                                filament.tray_info_idx = filament_id.to_string();
                                filament.nozzle_temp_min = nozzle_temp_low;
                                filament.nozzle_temp_max = nozzle_temp_high;
                                res = true;
                            }
                        }
                    }
                }
            }
        }
        res
    }

    pub fn get_quad_for_set_filament_from_tray_id(&self, tray_id: i32) -> (i32, i32, i32, i32) {
        // (ams_id, ams_tray_id, slot_id, original_tray_id)

        let ams_id;
        let mut ams_tray_id_for_set_filament;
        let slot_id;
        let original_tray_id;

        (ams_id, ams_tray_id_for_set_filament) = Self::get_ams_and_slot_id(tray_id as usize);

        if tray_id < 16 {
            // AMS
            slot_id = ams_tray_id_for_set_filament as i32;
            original_tray_id = tray_id;
        } else if tray_id < 16 + 8 {
            // AMS-HT
            slot_id = 0;
            original_tray_id = (ams_id * 4 + ams_tray_id_for_set_filament) as i32; // seems like this is what Bambustudio is placing there (so 512 for first HT, others are assumed)
        } else {
            // external
            ams_tray_id_for_set_filament = 254;
            slot_id = 0;
            if self.num_extruders() == 1 {
                original_tray_id = 254;
            } else {
                original_tray_id = tray_id;
            }
        }
        (ams_id as i32, ams_tray_id_for_set_filament as i32, slot_id, original_tray_id)
    }

    pub fn reset_tray(&mut self, tray_id: i32) {
        let (ams_id, ams_tray_id, slot_id, original_tray_id) = self.get_quad_for_set_filament_from_tray_id(tray_id);
        let cmd = crate::bambu_api::AmsFilamentSettingCommand::new(
            ams_id,
            ams_tray_id, // here we need the tray_id within the specific ams (newer versions)
            slot_id,     // slot number within ams
            "",
            Some(""),
            "",
            "",
            0,
            0,
        );
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        if !self.is_locked() {
            self.publish_payload(payload);
        }

        let extruder_id = self.get_extruder_id_for_tray(tray_id).unwrap_or_default();
        let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
            &self.nozzle_diameter(extruder_id).clone().unwrap_or_default(),
            ams_id,
            original_tray_id, // here we need the original tray_id
            slot_id,
            "", // tray_info_idx is filament_id in this command
            Some(-1),
        );
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        if !self.is_locked() {
            self.publish_payload(payload);
        }
    }

    pub fn get_ams_info_index_for_tray(&self, tray_id: i32) -> Result<usize, String> {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        let ams_id = match tray_id {
            0..=15 => tray_id / 4,
            16..=23 => tray_id - 16 + 4,
            254 => 12,
            255 => 13,
            _ => {
                let err_str = format!("[{}] Error mapping slot {tray_id} to ams_id to get calibrations", self.printer_number);
                error!("{}", err_str);
                return Err(err_str);
            }
        } as usize;
        Ok(ams_id)
    }

    pub fn get_extruder_id_for_tray(&self, tray_id: i32) -> Result<u32, String> {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        let ams_info_index = self.get_ams_info_index_for_tray(tray_id)?;
        Ok(self.ams_info[ams_info_index].extruder)
    }

    pub fn get_extruder_for_tray(&self, tray_id: i32) -> Result<&Extruder, String> {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        Ok(self.get_extruder(self.get_extruder_id_for_tray(tray_id)?))
    }

    pub fn set_tray_filament(&mut self, tray_id: i32, full_spool_rec: &FullSpoolRecord, temp_min: u32, temp_max: u32) {
        let (ams_id_for_set_filament, ams_tray_id, slot_id, original_tray_id) = self.get_quad_for_set_filament_from_tray_id(tray_id);

        // setting_id can't be extracted from just tray information, it's available only if there is a cali_idx on the tray.
        // on the other hand it is required to set tray information.
        // So if we have calibration information, we send the setting_id from there. If we don't we send None and it seems to work
        // The slicer have the setting-if from the data it has when it selects everything together

        let extruder_id = self.get_extruder_id_for_tray(tray_id).unwrap_or_default();
        let matching_calibration = self.get_matching_printer_calibration_for_extruder(full_spool_rec, extruder_id);

        let setting_id: Option<&str> = matching_calibration.as_ref().and_then(|c| c.setting_id.as_deref());

        let mut filament = FilamentInfo {
            tray_info_idx: full_spool_rec.spool_rec.slicer_filament.clone(),
            tray_type: full_spool_rec.spool_rec.material_type.clone(),
            tray_color: full_spool_rec.spool_rec.color_code.clone(),
            nozzle_temp_min: temp_min,
            nozzle_temp_max: temp_max,
        };
        let filament_ok_to_send = self.fill_filament_defaults_if_needed(&mut filament);

        if filament_ok_to_send {
            let cmd = crate::bambu_api::AmsFilamentSettingCommand::new(
                ams_id_for_set_filament,
                ams_tray_id, // here we need the tray_id within the specific ams (newer versions)
                slot_id,     // slot number within ams
                &filament.tray_info_idx,
                setting_id,
                &filament.tray_type,
                &filament.tray_color,
                filament.nozzle_temp_min,
                filament.nozzle_temp_max,
            );
            let payload = serde_json::to_string_pretty(&cmd).unwrap();
            if !self.is_locked() {
                self.publish_payload(payload);
            }

            let extruder_id = self.get_extruder_id_for_tray(tray_id).unwrap_or_default();
            let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
                &self.nozzle_diameter(extruder_id).clone().unwrap_or_default(),
                ams_id_for_set_filament,
                original_tray_id, // here we need the original tray_id
                slot_id,
                &filament.tray_info_idx, // tray_info_idx is filament_id in this command
                if let Some(calibration) = &matching_calibration {
                    Some(calibration.cali_idx)
                } else {
                    Some(-1)
                },
            );
            let payload = serde_json::to_string_pretty(&cmd).unwrap();
            if !self.is_locked() {
                self.publish_payload(payload);
            }

            self.update_any_tray(tray_id as usize, |tray| {
                tray.meta_info = TrayMetaInfo::default();
                tray.meta_info.spool_id = Some(full_spool_rec.spool_rec.id.clone());
                tray.meta_info.consumed_since_weight = full_spool_rec.spool_rec.consumed_since_weight;
            });
        } else {
            error!("Error trying to set slot information due to missing information (material type at least is required)");
        }
    }

    pub fn add_calibration_to_printer(
        &mut self,
        extruder_id: i32,
        nozzle_diameter: &str,
        nozzle_id: &str,
        filament_id: &str,
        setting_id: &str,
        k_value: &str,
        name: &str,
    ) {
        let cmd = crate::bambu_api::ExtrusionCaliSetCommand::new(extruder_id, nozzle_diameter, nozzle_id, filament_id, setting_id, k_value, name);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);
    }

    pub fn get_matching_printer_calibration_for_extruder(&self, full_spool_rec: &FullSpoolRecord, extruder_id: u32) -> Option<Calibration> {
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

        let printer_nozzle_diameter = self.nozzle_diameter(extruder_id).as_ref()?;
        let extruder = self.get_extruder(extruder_id);
        let relevant_printer_calibrations = self.calibrations.iter().filter(|cal| {
            cal.extruder as u32 == extruder_id
                && cal.diameter == *printer_nozzle_diameter
                && cal.nozzle_type_code() == extruder.nozzle_type_code().unwrap_or(NozzleType::Standard)
        });
        let filament_id = &full_spool_rec.spool_rec.slicer_filament;
        // Using the new K from SpoolRecordExt

        // If there is filament calibration for that nozzle size (assumption there can be only one, which makes sense)
        if let Some(nozzle_k) = full_spool_rec.spool_rec_ext.get_calibration(
            &self.printer_serial,
            extruder_id as i32,
            self.nozzle_diameter(extruder_id).as_ref()?,
            extruder.nozzle_type_code().unwrap_or(NozzleType::Standard),
        ) {
            // here need to test not against printer nozzle but also consider the AMS tray which is the target, meaning, can't set/show it in staging
            // is tag_info modified when loaded into staging based on current printer? Or only displayed?
            // This also means, that we probably want the nozzle diameter when encoded to be the kay (so K4~0~HH-0.4) where 0 is extruder and after that comes the nozzle type
            // Need to check what does it mean that there isn't really a printer nozzle diameter

            // there could be several tht match filament_id, setting_id (even common)
            // Important Note: On A1,P1,X1 - calibration includes setting_id, so if we have it we encode it and then when scanning will compare it.
            //                 On H2D - Setting_id is not included - therefore the second part of below filter will compare None (on calibration) to filament setting_id
            //                 This is ok, because if there is no match it means it is not exact match from this printer type, it was encoded on pritner with setting_id and loaded to h2d
            let same_filament_id_nozzle_printer_type_calibrations = relevant_printer_calibrations.filter(|&c| c.filament_id == *filament_id);
            // A1
            if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone() // only cloning the iterator, not the data
                .find(|printer_calibration| printer_calibration.name == nozzle_k.name)
            {
                return Some(calibration_match.clone());
            // Starting here, we can improve by finding several that match and select the closest
            // A2
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| clean_compare(&printer_calibration.name, &nozzle_k.name))
            {
                return Some(calibration_match.clone());
            // A3
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| printer_calibration.k_value == nozzle_k.k_value)
            // because we are on same printer-type/nozzle this should be ok
            {
                return Some(calibration_match.clone());
            // A4 : TODO: use metaphone double to compare strings
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| similar_compare(&printer_calibration.name, &nozzle_k.name))
            {
                return Some(calibration_match.clone());
            }
        }

        // // If there is filament calibration for that nozzle size (assumption there can be only one, which makes sense)
        // if let Some(filament_calibration) = tag_info.calibrations.get(printer_nozzle) {
        //     // here need to test not against printer nozzle but also consider the AMS tray which is the target, meaning, can't set/show it in staging
        //     // is tag_info modified when loaded into staging based on current printer? Or only displayed?
        //     // This also means, that we probably want the nozzle diameter when encoded to be the kay (so K4~0~HH-0.4) where 0 is extruder and after that comes the nozzle type
        //     // Need to check what does it mean that there isn't really a printer nozzle diameter
        //
        //     // there could be several tht match filament_id, setting_id (even common)
        //     // Important Note: On A1,P1,X1 - calibration includes setting_id, so if we have it we encode it and then when scanning will compare it.
        //     //                 On H2D - Setting_id is not included - therefore the second part of below filter will compare None (on calibration) to filament setting_id
        //     //                 This is ok, because if there is no match it means it is not exact match from this printer type, it was encoded on pritner with setting_id and loaded to h2d
        //     let same_filament_id_nozzle_printer_type_calibrations = printer_calibrations
        //         .iter()
        //         .filter(|&c| c.1.filament_id == *tag_filament_id && c.1.setting_id == filament_calibration.setting_id);
        //
        //     // A1
        //     if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
        //     {
        //         return Some(calibration_match.1.clone());
        //     // Starting here, we can improve by finding several that match and select the closest
        //     // A2
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     // A3
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.k_value == filament_calibration.k_value)
        //     // because we are on same printer-type/nozzle this should be ok
        //     {
        //         return Some(calibration_match.1.clone());
        //     // A4 : TODO: use metaphone double to compare strings
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        // };
        //
        // // This never takes place because tag_info contains only one clibration
        // for (_, filament_calibration) in &tag_info.calibrations {
        //     // TODO: When tag has several calibrations for different nozzles, here we can iterate over them as well
        //     // (so compare man to many) since name from another nozzle diameter could help finding for another nozzle
        //     // size, it's just name mathing
        //     let same_filament_id_printer_calibrations = printer_calibrations.iter().filter(|&c| c.1.filament_id == *tag_filament_id);
        //     // B1
        //     if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        //     // Starting here, we can improve by finding several that match and select the closest
        //     // B2
        //     else if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     // B3
        //     } else if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        // }

        None
    }
}

pub type SpoolId = String;

#[derive(Serialize, Deserialize, Debug, Clone, Default, Derivative)]
#[derivative(PartialEq)]
// IMPORTANT: Don't change names, will hurt persistence
pub struct TrayMetaInfo {
    pub spool_id: Option<SpoolId>,
    #[serde(rename = "tag_info", skip_serializing)]
    pub old_tag_info: Option<TagInformationV1>, // calibration for nozzles
    #[serde(default)]
    pub consumed_since_load: f32,
    #[serde(default)]
    pub consumed_since_load_saved: f32,
    // fields which are not going to be stored as state should not be considered in partialeq since that's what decides if to save state or not
    #[serde(skip)]
    #[derivative(PartialEq = "ignore")]
    pub used_in_print: bool,
    #[serde(default)]
    pub consumed_since_weight: f32,
}

#[derive(Derivative)]
#[derivative(PartialEq)]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
// IMPORTANT: Don't change names, will hurt persistence
pub struct Tray {
    pub state: TrayState,
    pub filament: Filament,
    pub k_from_tray: Option<f32>,
    pub cali_idx: Option<i32>,
    #[derivative(PartialEq = "ignore")]
    #[serde(flatten)] // for backwards compatibility with PrinterPersistentState stored printer state
    pub meta_info: TrayMetaInfo,
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
#[derive(Debug)]
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
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

// static ENCODING_MAP: Lazy<Mutex<CriticalSectionRawMutex, HashMap<char, &str>>> = Lazy::new(|| {
//     let char_hashmap: HashMap<char, &str> = ENCODING_TABLE.into_iter().collect();
//     Mutex::new(char_hashmap)
// });

fn my_decode_from_url_part(text: &str) -> String {
    // % must be last (because some originated from encodings and will need to be replaced first)
    // let name = name.replace("%7E", "/").replace("%2F", "/").replace("%28", "(").replace("%29", ")").replace("%26", "&").replace("%3F", "?").replace("%20", " ").replace("%25", "%");
    efficient_decode(text, &ENCODING_TABLE)
}

// fn my_encode_to_url_part(text: &str) -> String {
//     // % must be first (because later added)
//     // let name = name.replace("%", "%25").replace("/", "%2F").replace("&", "%26").replace("?", "%3F").replace(" ", "%20").replace("(", "%28").replace(")", "%29").replace( "~","%7E");
//     ENCODING_MAP.lock(|encoding_map| efficient_encode(text, encoding_map))
// }

///// Encodes specific characters in a string based on a provided mapping.
///// Minimizes allocations while still returning a String.
/////
///// # Arguments
///// * `input` - The string to encode
///// * `char_map` - A mapping of characters to their encoded string representation
/////
///// # Returns
///// The encoded string
//pub fn efficient_encode(input: &str, char_map: &HashMap<char, &str>) -> String {
//    // Pre-calculate output size to avoid reallocations
//    let mut capacity = 0;
//    for c in input.chars() {
//        capacity += match char_map.get(&c) {
//            Some(replacement) => replacement.len(),
//            None => c.len_utf8(),
//        };
//    }
//
//    // Pre-allocate output string with exact capacity needed
//    let mut result = String::with_capacity(capacity);
//
//    // Process each character
//    for c in input.chars() {
//        match char_map.get(&c) {
//            Some(replacement) => result.push_str(replacement),
//            None => result.push(c),
//        }
//    }
//
//    result
//}

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
    pub extruder: i32,
    pub diameter: String,
    pub nozzle_id: String, // this is "" for old printers, and HS01-0.4 for example for new (so not nozzle_type), nozzle_type is a function below
    pub filament_id: String,
    pub k_value: String,
    // n_coef: f32,
    pub setting_id: Option<String>,
    pub name: String,
    pub cali_idx: i32,
}

impl Calibration {
    pub fn nozzle_type_code(&self) -> NozzleType {
        if self.nozzle_id.len() < 8 || self.nozzle_id.as_bytes()[1] != b'H' {
            NozzleType::Standard
        } else {
            NozzleType::HighFlow
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct OldTagCalibration {
    pub filament_id: String,
    pub k_value: String,
    // n_coef: f32,
    pub setting_id: Option<String>,
    pub name: String,
    pub cali_idx: i32,
}

impl OldTagCalibration {
    pub fn new_minimal(k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            k_value: formatted_k_value(k_value),
            filament_id: String::from(filament_id),
            setting_id: if setting_id.is_empty() { None } else { Some(setting_id.to_string()) },
            name: String::from(name),
            cali_idx,
        }
    }
}

impl From<&Calibration> for OldTagCalibration {
    fn from(v: &Calibration) -> Self {
        // this "Filament" in bambu_api is really calibrations, bambulab naming ...
        Self {
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: v.k_value.clone(),
            // n_coef: f32::from_str(&v.n_coef).unwrap_or(-1.0),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }
}

fn default_printer_name() -> String {
    String::from("Unknown")
}
#[derive(Serialize, Deserialize, Debug)]
pub struct PrinterPersistentState<'a> {
    pub ams_trays: Cow<'a, Vec<Tray>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virt_tray: Option<Cow<'a, Tray>>, // for Backwards compatibility prior to 0.5.0-b.48
    pub virt_trays: Option<Cow<'a, [Tray; 2]>>,
    pub nozzle_diameter: Option<String>,
    #[serde(default)]
    pub ams_exist_bits: Option<u32>,
    #[serde(default)]
    pub tray_exist_bits: Option<u32>,
    #[serde(default)]
    pub tray_read_done_bits: Option<u32>,
    #[serde(default)]
    pub calibrations: Cow<'a, Vec<Calibration>>,
    #[serde(default = "default_printer_name")]
    pub printer_name: String,
    #[serde(default)]
    pub extruders: Option<Cow<'a, [Extruder; 2]>>,
    #[serde(default)]
    pub extruder_state: Option<i32>,
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

impl Calibration {
    pub fn from(v: &bambu_api::Filament, diameter: &str) -> Self {
        // this "Filament" in bambu_api is really calibrations, bambulab naming ...
        Self {
            extruder: v.extruder_id.unwrap_or_default(),
            nozzle_id: v.nozzle_id.clone().unwrap_or_default(),
            diameter: diameter.to_string(),
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: formatted_k_value(&v.k_value),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }

    pub fn _new_minimal(diameter: &str, k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            extruder: 0,
            nozzle_id: String::new(),
            diameter: diameter.to_string(),
            k_value: formatted_k_value(k_value),
            filament_id: String::from(filament_id),
            setting_id: if setting_id.is_empty() { None } else { Some(setting_id.to_string()) },
            name: String::from(name),
            cali_idx,
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
    store_state_request_channel: Rc<StoreStateRequestChannel>,
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

    let printer_config_name = printer_config.name.clone();
    let printer_ip = printer_config.ip;
    let log_filter = if let Some(log_filter) = &printer_config.log_filter {
        *log_filter
    } else {
        log::LevelFilter::Warn
    };
    let auto_restore_k = printer_config.auto_restore_k;
    let track_print_consume = printer_config.track_print_consume;
    let fetch_3mf = printer_config.fetch_3mf;

    // == Setup MQTT ==================================================================
    let write_packets = Rc::new(WritePacketsChannel::new());

    let read_packets = Rc::new(ReadPacketsPubSub::new());

    let restart_printer = Rc::new(embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>::new());

    let bambu_printer = BambuPrinter::new(
        printer_number,
        printer_index,
        &printer_serial,
        &printer_access_code,
        &printer_config_name,
        &printer_ip,
        auto_restore_k,
        track_print_consume,
        fetch_3mf,
        write_packets.clone(),
        app_config.clone(),
        restart_printer.clone(),
        log_filter,
        store_state_request_channel,
    );

    spawner
        .spawn_heap(restartable_mqtt_task(
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

    BambuPrinter::request_version_info_async(&printer_serial, printer_number, log_filter, write_packets.clone()).await;

    // fetch first setting for all nozzles, need that in advance before getting filaments
    let nozzle_diameters = ["0.2", "0.6", "0.8", "0.4"];
    for nozzle_diameter in nozzle_diameters {
        debug!("[{printer_number}] Request calibration information for nozzle {nozzle_diameter}");
        BambuPrinter::fetch_filament_calibrations_async(&printer_serial, printer_number, log_filter, write_packets.clone(), nozzle_diameter).await;
        Timer::after_millis(200).await;
    }

    // Now request full update, and wait until data is processed and have the nozzle diameter at hand for next request
    BambuPrinter::request_full_update_async(&printer_serial, printer_number, log_filter, write_packets.clone()).await;
    while bambu_printer.borrow().nozzle_diameter(0).is_none() {
        Timer::after_millis(100).await;
    }

    // Get again the filaments for current nozzle size,
    // that's because in slicer they don't check if data received from printer it's current nozzle or not
    // it's a bug there, can even be reproduced in the slicer by switching in the manage results to another nozzle diameter
    let curr_nozzle_diameter = bambu_printer.borrow().nozzle_diameter(0).as_ref().unwrap().clone();
    BambuPrinter::fetch_filament_calibrations_async(&printer_serial, printer_number, log_filter, write_packets, &curr_nozzle_diameter).await;
}

// static CLEAN_CRLF_WHITESPACE_RE: Lazy<regex::bytes::Regex> = Lazy::new(|| regex::bytes::Regex::new(r"\s*[\r\n]\s*").unwrap());
static CLEAN_CRLF_WHITESPACE_RE: Lazy<regex::bytes::Regex> = Lazy::new(|| regex::bytes::Regex::new(r"[ \t\r\n]*[\r\n][ \t\r\n]*").unwrap());

fn clean_bytes_to_string(input: &[u8]) -> String {
    // Step 1: Replace in &[u8] without converting to &str first
    let replaced: Cow<[u8]> = CLEAN_CRLF_WHITESPACE_RE.replace_all(input, b" " as &[u8]);

    // Step 2: Convert result into String
    match replaced {
        Cow::Borrowed(b) => String::from_utf8_lossy(b).into_owned(), // No change → borrow
        Cow::Owned(b) => String::from_utf8(b).expect("invalid UTF-8"),
    }
}

#[embassy_executor::task(pool_size = MAX_NUM_PRINTERS)]
pub async fn incoming_messages_task(read_packets: Rc<ReadPacketsPubSub>, bambu_printer: Rc<RefCell<BambuPrinter>>) {
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
                            let parse_res = Box::new(serde_json::from_slice::<bambu_api::Message>(payload));

                            if log_level >= log::Level::Trace {
                                let cleaned_log = clean_bytes_to_string(payload);
                                trace!("[{printer_log_id}] [Q:{}] [SIM] {cleaned_log}", subscriber.len());
                            }

                            if let Ok(message) = *parse_res {
                                if log_level >= log::Level::Trace {
                                    trace!("[{}] {:?}", printer_log_id, message);
                                }

                                match message {
                                    bambu_api::Message::Print(print) => {
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
                                            let previous_tray_bits = TrayBits {
                                                tray_reading_bits: bambu_printer.borrow().tray_reading_bits,
                                                tray_read_done_bits: *bambu_printer.borrow().tray_read_done_bits(),
                                                tray_exist_bits: *bambu_printer.borrow().tray_exist_bits(),
                                            };
                                            let (change_made, removed_tags) = (*bambu_printer.borrow_mut()).process_print_message(&print.print);
                                            let updated_tray_bits = TrayBits {
                                                tray_reading_bits: bambu_printer.borrow().tray_reading_bits,
                                                tray_read_done_bits: *bambu_printer.borrow().tray_read_done_bits(),
                                                tray_exist_bits: *bambu_printer.borrow().tray_exist_bits(),
                                            };
                                            if change_made {
                                                (*bambu_printer.borrow_mut()).update_ams_trays_done(
                                                    &previous_tray_bits,
                                                    &updated_tray_bits,
                                                    &removed_tags,
                                                );
                                            }
                                        }
                                    }
                                    bambu_api::Message::Info(_info) => {}
                                }
                            } else if log_level >= log::Level::Debug {
                                if log_level >= log::Level::Trace {
                                    debug!("[{printer_log_id}] Previous message couldn't be parsed {parse_res:?}");
                                } else {
                                    let cleaned_log = clean_bytes_to_string(payload);
                                    debug!("[{printer_log_id}] Unprocessed message {parse_res:?} : {cleaned_log:?}");
                                }
                            }
                        }
                        mqttrust::Packet::Suback(mqttrust::encoding::v4::Suback { pid: _, return_codes: _ }) => {
                            // Subscribed, now time to request for update
                            let spawner = unsafe {embassy_executor::Spawner::for_current_executor().await};
                            spawner.spawn(fetch_initial_info(bambu_printer.clone())).ok();
                        }
                        _ => {
                            if log_level >= log::Level::Trace {
                                trace!("[{printer_log_id}] Ignoring {:?}", packet);
                            }
                        }
                    }
                } else {
                    error!("Unparsable MQTT message, this means an internal bug");
                }
            }
            Err(_) => {
                // always TimeoutError
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
// #[embassy_executor::task(pool_size = 5)]
pub async fn restartable_mqtt_task(
    framework: Rc<RefCell<Framework>>,
    rx_socket_buffer_size: usize,
    tx_socket_buffer_size: usize,
    read_packets: Rc<ReadPacketsPubSub>,
    write_packets: Rc<WritePacketsChannel>,
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
    read_packets: Rc<ReadPacketsPubSub>,
    write_packets: Rc<WritePacketsChannel>,
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
        printer_name = bambu_printer.borrow().configured_printer_name.clone().unwrap_or(default_printer_name());
    }

    // Final name, theoretically if name explicitly supplied and IP not,  this could override the supplied name
    bambu_printer.borrow_mut().printer_ip = printer_ip;
    bambu_printer.borrow_mut().set_printer_name(&printer_name);

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

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct TagInformationV1 {
    pub id: Option<String>,
    pub tag_id: Option<Vec<u8>>,
    pub filament: Option<FilamentInfo>,
    pub weight_advertised: Option<i32>,
    pub weight_core: Option<i32>,
    pub weight_new: Option<i32>,
    pub brand: Option<String>,
    pub filament_subtype: Option<String>,
    pub color_name: Option<String>,
    pub note: Option<String>,
    pub encode_time: Option<i32>,
    // for old tags support (where K was on tag)
    pub calibrations: HashMap<String, OldTagCalibration>,
    pub calibrations_printer_name: String, // has value only if calibrations has any value
    pub calibrations_printer_uuid: String, // has value only if calibrations has any value
}

impl TagInformationV1 {
    pub fn _from(v: &SpoolRecord, min_max_temp: (u32, u32)) -> Self {
        // TODO: need to deal with case of no data or partial data for filament_info?
        let filament_info = {
            FilamentInfo {
                tray_info_idx: v.slicer_filament.clone(),
                tray_type: v.material_type.clone(),
                tray_color: v.color_code.clone(),
                nozzle_temp_max: min_max_temp.1,
                nozzle_temp_min: min_max_temp.0,
            }
        };
        Self {
            id: Some(v.id.clone()),
            tag_id: if v.tag_id.is_empty() {
                None
            } else {
                hex::decode(v.tag_id.as_bytes()).ok()
            },
            filament: Some(filament_info),
            weight_advertised: v.weight_advertised,
            weight_core: v.weight_core,
            weight_new: v.weight_new,
            brand: if v.brand.is_empty() { None } else { Some(v.brand.clone()) },
            filament_subtype: if v.material_subtype.is_empty() {
                None
            } else {
                Some(v.material_subtype.clone())
            },
            color_name: if v.color_name.is_empty() { None } else { Some(v.color_name.clone()) },
            note: if v.note.is_empty() { None } else { Some(v.note.clone()) },
            encode_time: v.encode_time,
            calibrations: HashMap::new(),
            calibrations_printer_name: String::new(),
            calibrations_printer_uuid: String::new(),
        }
    }
    pub fn to_spool_rec(&self) -> SpoolRecord {
        let empty = &String::new();
        // let empty_filament = &FilamentInfo::default(),
        let calibration_filament_id = if let Some(key) = self.calibrations.keys().next() {
            self.calibrations.get(key).map(|c| c.filament_id.clone())
        } else {
            None
        };
        SpoolRecord {
            id: self.id.as_ref().unwrap_or(empty).clone(),
            tag_id: self.tag_id.as_ref().map(hex::encode_upper).unwrap_or_default(),
            material_type: self.filament.as_ref().map(|f| f.tray_type.clone()).unwrap_or_default(),
            material_subtype: self.filament_subtype.as_ref().unwrap_or(empty).clone(),
            color_name: self.color_name.as_ref().unwrap_or(empty).clone(),
            color_code: self.filament.as_ref().map(|f| f.tray_color.clone()).unwrap_or_default(),
            note: self.note.as_ref().unwrap_or(empty).clone(),
            brand: self.brand.as_ref().unwrap_or(empty).clone(),
            weight_advertised: self.weight_advertised,
            weight_core: self.weight_core,
            weight_new: self.weight_new,
            weight_current: None,
            slicer_filament: calibration_filament_id.unwrap_or_default(),
            added_time: None,
            encode_time: self.encode_time,
            added_full: self.weight_new.map(|_| true), // Some(true) if weight_new exists
            consumed_since_add: 0.0,
            consumed_since_weight: 0.0,
            ext_has_k: false, // this means if in the store, so need to be set to true when saving store
            data_origin: SPOOLEASE_V1_TAG_TYPE.to_string(),
            tag_type: "".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KInfo {
    pub printers: HashMap<String, KPrinter>,
}

impl KInfo {
    fn _get_filament_k_for(&self, printer: &str, extruder: i32, diameter: &str, nozzle_id: &str) -> Option<&KNozzleId> {
        self.printers
            .get(printer)?
            .extruders
            .get(&extruder)?
            .diameters
            .get(diameter)?
            .nozzles
            .get(nozzle_id)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KPrinter {
    pub extruders: HashMap<i32, KExtruder>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KExtruder {
    pub diameters: HashMap<String, KNozzleDiameter>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KNozzleDiameter {
    pub nozzles: HashMap<String, KNozzleId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KNozzleId {
    pub name: String,
    pub k_value: String,
    pub cali_idx: i32,
    pub setting_id: Option<String>,
}

// k_info.printers["01P..."][0]["0.4"]["HH00"].name / .k

impl TagInformationV1 {
    // pub fn _to_v1_descriptor(&self, _printer_name: Option<&str>, _printer_uuid: Option<&str>) -> Option<String> {
    //     let brand_part = self
    //         .brand
    //         .as_ref()
    //         .map(|s| format!("&B={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let filament_subtype_part = self
    //         .filament_subtype
    //         .as_ref()
    //         .map(|s| format!("&MS={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let color_name_part = self
    //         .color_name
    //         .as_ref()
    //         .map(|s| format!("&CN={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let note_part = self.note.as_ref().map(|s| format!("&N={}", my_encode_to_url_part(s))).unwrap_or_default();
    //
    //     let mut material_part = String::new();
    //     let mut color_part = String::new();
    //     let mut nozzle_temp_min_part = String::new();
    //     let mut nozzle_temp_max_part = String::new();
    //     let mut tray_info_idx_part = String::new();
    //
    //     if let Some(filament) = &self.filament {
    //         material_part = if filament.tray_type.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&M={}", filament.tray_type.trim()) // changed due to a bug in inventory that got CR into material
    //         };
    //         color_part = if filament.tray_color.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&C={}", filament.tray_color)
    //         };
    //         nozzle_temp_min_part = if filament.nozzle_temp_min == 0 {
    //             String::new()
    //         } else {
    //             format!("&NN={}", filament.nozzle_temp_min)
    //         };
    //         nozzle_temp_max_part = if filament.nozzle_temp_max == 0 {
    //             String::new()
    //         } else {
    //             format!("&NX={}", filament.nozzle_temp_max)
    //         };
    //         tray_info_idx_part = if filament.tray_info_idx.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&FI={}", filament.tray_info_idx)
    //         };
    //     }
    //     let advertised_weight_part = self.weight_advertised.map(|v| format!("&WA={}", v)).unwrap_or_default();
    //     let weight_core_part = self.weight_core.map(|v| format!("&WC={}", v)).unwrap_or_default();
    //     let weight_new_part = self.weight_new.map(|v| format!("&WN={}", v)).unwrap_or_default();
    //     let encode_time_part = self.encode_time.map(|v| format!("&DE={}", v)).unwrap_or_default();
    //
    //     Some(format!("{FILAMENT_URL_PREFIX}V1?ID={TAG_PLACEHOLDER}{encode_time_part}{material_part}{filament_subtype_part}{color_part}{color_name_part}{brand_part}{advertised_weight_part}{weight_core_part}{weight_new_part}{nozzle_temp_min_part}{nozzle_temp_max_part}{note_part}{tray_info_idx_part}"))
    // }

    // TODO: remove all the printer parts, should only parse, the rest of the matching thould go elsewhere

    pub fn from_v1_descriptor(descriptor: &str) -> Result<Self, Error> {
        let mut filament_info_result = FilamentInfo::new();
        let mut calibrations_result = HashMap::new();
        let mut weight_advertised = None;
        let mut weight_core = None;
        let mut weight_new = None;
        let mut brand = None;
        let mut filament_subtype = None;
        let mut color_name = None;
        let mut note = None;
        let mut tag_id = None;
        let mut encode_time = None;

        if !(descriptor.starts_with(FILAMENT_URL_PREFIX_V1_TAG)) {
            // below the code should still use the base FILAMENT_URL_PREFIX
            return Err(Error::ParseError);
        }
        // let descriptor = descriptor.trim_start_matches(FILAMENT_URL_PREFIX);

        let mut id = false;
        let mut v = false;
        let mut m = false;
        let mut _fi = false;
        let mut c = false;
        let mut _nn = false;
        let mut _nx = false;
        for param in descriptor.strip_prefix(FILAMENT_URL_PREFIX).unwrap_or(descriptor).split(['&', '/', '?']) {
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
                        if let Ok(tag_id_bytes) = URL_SAFE_NO_PAD.decode(param_value) {
                            tag_id = Some(tag_id_bytes);
                        } else {
                            error!("Error decoding tag id from tag descriptor {descriptor}");
                            return Err(Error::ParseError);
                        }
                    }
                    // Material / Tray Type (material code in some other form)
                    "M" => {
                        filament_info_result.tray_type = String::from(param_value.trim()); // trimmed due to a bug in inventory that added CR, hope won't make issues
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
                        _nn = true;
                    }
                    // Nozzle maX Temp
                    "NX" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_max = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        _nx = true;
                    }
                    // "K4" | "K2" | "K6" | "K8" => (),
                    // // Filament Id/ Tray Index (material code in some form) - looks like Bambu specific
                    "FI" => {
                        filament_info_result.tray_info_idx = String::from(param_value);
                        _fi = true;
                    }
                    "WA" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_advertised = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
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
                    "DE" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            encode_time = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
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
                        let calibration = OldTagCalibration::new_minimal(k_value, &filament_info_result.tray_info_idx, setting_id, &name, -1);
                        calibrations_result.insert(nozzle_diameter, calibration);
                    }
                    _ => (), // previous run already identified unrecognized parameters, here we skip also those that were ok so can't error
                }
            }
        }

        if v && id && m && c {
            Ok(Self {
                id: None,
                // origin_descriptor: descriptor.to_string(),
                tag_id,
                filament: Some(filament_info_result),
                weight_advertised,
                weight_core,
                weight_new,
                brand,
                filament_subtype,
                color_name,
                note,
                encode_time,

                // for old k calibration handling
                calibrations: calibrations_result,
                calibrations_printer_name: my_decode_from_url_part(calibrations_printer_name),
                calibrations_printer_uuid: calibrations_printer_uuid.to_string(),
            })
        } else {
            Err(Error::MissingFields)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PrinterModel {
    Unknown,
    X1,
    X1C,
    X1E,
    P1P,
    P1S,
    P2S,
    A1Mini,
    A1,
    H2D,
    H2DPro,
    H2C,
    _H2S,
}

pub enum PrinterModelSeries {
    Unknown,
    X1,
    P1,
    A1,
    H2,
    P2,
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
                    "N7" => PrinterModel::P2S,
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

pub async fn fix_k_on_restart(
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    prev_ams_trays: Vec<Tray>,
    prev_virt_tray: Tray,
    prev_nozzle: Option<String>,
) {
    Timer::after_secs(1).await;
    let printer_number = bambu_printer.borrow().printer_number;
    term_info!("[{}] Checking pressure advance (k) at printer startup", printer_number);
    if prev_nozzle != *bambu_printer.borrow().nozzle_diameter(0) {
        term_info!(
            "[{}] Nozzle diameter changed ({:?}->{:?}), K restore not relevant",
            printer_number,
            prev_nozzle,
            *bambu_printer.borrow().nozzle_diameter(0)
        );
        bambu_printer.borrow_mut().pending_k_restore_sequence = false;
        return;
    }
    let mut set_tray_cali_idx: [Option<i32>; 24] = [None; 24];
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
                &bambu_borrow.virt_trays()[0]
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
    let nozzle_diameter = &bambu_printer.borrow().nozzle_diameter(0).clone().unwrap_or_default();
    let printer_serial = bambu_printer.borrow().printer_serial.clone();
    let log_filter = bambu_printer.borrow().log_filter;

    for (id, prev_tray) in prev_ams_trays
        .iter()
        .enumerate()
        .chain(core::iter::once(&prev_virt_tray).map(|v| (255, v)))
    {
        {
            let set_tray = if id == 255 { &set_virt_cali_idx } else { &set_tray_cali_idx[id] };
            if set_tray.is_some() {
                if let Filament::Known(filament_info) = &prev_tray.filament {
                    let original_tray_id = if id == 255 { 254 } else { id };
                    let (ams_id, slot_id) = BambuPrinter::get_ams_and_slot_id(original_tray_id);
                    // TODO: (if change) check ams_id against 255
                    if ams_id != 255 && ams_id != 254 {
                        info!("[{}] Updating pressure advance of AMS {} slot {}", printer_number, ams_id, slot_id);
                    } else {
                        info!("[{}] Updating pressure advance of external slot", printer_number);
                    }
                    let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
                        nozzle_diameter,
                        ams_id as i32,
                        original_tray_id as i32, // here we need the original tray_id
                        slot_id as i32,
                        &filament_info.tray_info_idx, // tray_info_idx is filament_id in this command
                        *set_tray,
                    );
                    let payload = serde_json::to_string_pretty(&cmd).unwrap();

                    if !bambu_printer.borrow().is_locked() {
                        BambuPrinter::publish_payload_async(&printer_serial, printer_number, log_filter, write_packets.clone(), payload).await;
                    }
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
