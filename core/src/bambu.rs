// TODO:
// Deal with when to clear tag information, when we know spool taken out
// Deal with when to copy tag information between trays if only some data change but we know the spool is there

pub mod bambu_api;
pub mod bambu_print;
pub mod bambu_ssdp;
pub mod calibration;
pub mod driver_specific;
pub mod filament;
pub mod mqtt;
pub mod outgoing;
pub mod printer_state;
pub mod process_incoming;
pub mod protocol;
pub mod tray;

use crate::bambu::bambu_api::{GcodeState, PrintDeviceNozzleInfo};
use crate::bambu::calibration::Calibration;
use crate::bambu::driver_specific::BambuAmsType;
use crate::bambu::filament::{Filament, FilamentInfo};
use crate::bambu::mqtt::{ReadPacketsPubSub, WritePacketsChannel, restartable_mqtt_task};
use crate::bambu::process_incoming::incoming_messages_task;
use crate::bambu::protocol::ProtocolState;
use crate::bambu::tray::{Tray, TrayBits, canonical_tray_id};
use crate::{
    app_config::{BambuPrinterConfig, PrinterMode, UseAmsScan},
    printer::{
        PrinterRuntimePersistenceRequest, PrinterRuntimePersistenceRequestChannel, PrinterRuntimePersistenceRequestKind, PrinterSnapshotState,
    },
    settings::{MQTT_TCP_RX_BUFFER_BYTES, MQTT_TCP_TX_BUFFER_BYTES},
    ssdp::SSDPPubSubChannel,
};
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use bambu_print::PrintProject;
use core::cell::RefCell;
use embassy_net::Ipv4Address;
use embassy_time::Timer;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::Fetch3mf;
use shared::utils::channel_send;

use framework::prelude::*;

use crate::app_config::AppConfig;

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

#[derive(Default, Debug, Clone, PartialEq)]
pub struct AmsInfo {
    pub ams_type: BambuAmsType,
    pub bound_extruders: Vec<u32>,
    pub bound_switcher_pos: Option<BambuFilaSwitchPos>,
    pub humidity: Option<i32>,
    pub temp: Option<f32>,
}

impl AmsInfo {
    pub fn external(extruder_id: u32) -> Self {
        Self {
            ams_type: BambuAmsType::ExternalSpool,
            bound_extruders: alloc::vec![extruder_id],
            bound_switcher_pos: None,
            humidity: None,
            temp: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BambuFilaSwitchPos {
    InB,
    InA,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct BambuFilaSwitchSlot {
    pub ams_id: i32,
    pub slot_id: i32,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct BambuFilaSwitchState {
    pub installed: bool,
    // Protocol index 0 is Inlet B, protocol index 1 is Inlet A.
    pub in_slots: [Option<BambuFilaSwitchSlot>; 2],
    pub out_extruders: [Option<u32>; 2],
    pub stat: Option<i32>,
    pub info: Option<i32>,
}

#[allow(dead_code)]
impl BambuFilaSwitchState {
    pub fn in_a_slot(&self) -> Option<&BambuFilaSwitchSlot> {
        self.in_slots[1].as_ref()
    }

    pub fn in_b_slot(&self) -> Option<&BambuFilaSwitchSlot> {
        self.in_slots[0].as_ref()
    }
}

pub struct BambuPrinter {
    pub bambu_model: Option<Rc<RefCell<Self>>>,
    pub log_filter: log::LevelFilter,
    pub printer_number: usize,                   // number of printer in user's configuration,
    pub printer_serial: String, // mandatory, so configured is the same as actual
    pub printer_access_code: String, // mandatory, so configured is the same as actual
    pub configured_printer_name: Option<String>, // the name from config, could be empty
    pub inner_printer_name: String, // Unknown or Configured name or from SSDP if discovered
    pub printer_selector_name: String, // Will be assigned printer name from config OR printer serial (which is always availble) if config not available
    pub configured_printer_ip: Option<Ipv4Address>,
    pub auto_restore_k: bool,
    pub track_print_consume: bool,
    pub fetch_3mf: Fetch3mf,
    pub ignore_certificates: bool,
    pub printer_mode: PrinterMode,
    pub use_ams_scan: UseAmsScan,
    pub printer_ip: Ipv4Address,
    pub printer_connectivity_ok: Option<bool>,
    protocol_state: ProtocolState,
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
    pub locked_mode: Option<bool>, // None, unknown, treat as unlocked, false - dev mode, true - locked
    runtime_persistence_request_channel: Rc<PrinterRuntimePersistenceRequestChannel>,
    pub ams_info: Vec<AmsInfo>,
    pub fila_switch: BambuFilaSwitchState,
    pub gcode_state: GcodeState,
    pub layer_num: Option<i32>,
    pub total_layer_num: Option<i32>,
    pub mc_percent: Option<i32>,
    pub mc_remaining_time: Option<i32>,
    pub print_error: Option<i32>,
    pub gcode_file_prepare_percent: Option<i32>,
    pub subtask_name: Option<String>,
    pub stg_cur: Option<i32>,
    pub hms: Option<Vec<bambu_api::Hms>>,
    // Partial generic slot state, not a full Bambu printer cache.
    // Only spool_id, consumption counters, and used_in_print are meaningful here;
    // read live printer data from BambuPrinter fields instead.
    // used because these fields are printer field, but need to be updated from outside
    // by the generic system when storing state/updating inventory
    snapshot_state: Option<PrinterSnapshotState>,
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
    fn on_tag_scanned(&self, tray_id: i32, tag_id: &str, only_spool_id: bool);
    fn on_slot_consumption_reported(&mut self, _tray_id: i32, _grams: f32) {}
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

}

#[allow(clippy::too_many_arguments)]
impl BambuPrinter {
    pub fn new(
        printer_number: usize,
        printer_serial: &str,
        printer_access_code: &str,
        printer_config_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        track_print_consume: bool,
        fetch_3mf: Fetch3mf,
        ignore_certificates: bool,
        printer_mode: PrinterMode,
        use_ams_scan: UseAmsScan,
        write_packets: Rc<WritePacketsChannel>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
        runtime_persistence_request_channel: Rc<PrinterRuntimePersistenceRequestChannel>,
    ) -> Rc<RefCell<BambuPrinter>> {
        let myself = Self::internal_new(
            printer_number,
            printer_serial,
            printer_access_code,
            printer_config_name,
            printer_ip,
            auto_restore_k,
            track_print_consume,
            fetch_3mf,
            ignore_certificates,
            printer_mode,
            use_ams_scan,
            write_packets,
            app_config,
            restart_printer,
            log_filter,
            runtime_persistence_request_channel,
        );
        let myself_rc = Rc::new(RefCell::new(myself));
        myself_rc.borrow_mut().bambu_model = Some(myself_rc.clone());
        myself_rc
    }

    fn internal_new(
        printer_number: usize,
        printer_serial: &str,
        printer_access_code: &str,
        printer_config_name: &Option<String>,
        printer_ip: &Option<Ipv4Address>,
        auto_restore_k: bool,
        track_print_consume: bool,
        fetch_3mf: Fetch3mf,
        ignore_certificates: bool,
        printer_mode: PrinterMode,
        use_ams_scan: UseAmsScan,
        write_packets: Rc<WritePacketsChannel>,
        app_config: Rc<RefCell<AppConfig>>,
        restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
        log_filter: log::LevelFilter,
        runtime_persistence_request_channel: Rc<PrinterRuntimePersistenceRequestChannel>,
    ) -> Self {
        // Define a user oriented name for selection
        let printer_selector_name = if let Some(printer_name) = &printer_config_name {
            printer_name.clone()
        } else {
            printer_serial.to_string()
        };

        Self {
            bambu_model: None,
            printer_number,
            printer_serial: String::from(printer_serial),
            printer_access_code: String::from(printer_access_code),
            configured_printer_ip: *printer_ip,
            configured_printer_name: printer_config_name.clone(),
            inner_printer_name: printer_config_name.clone().unwrap_or(default_printer_name()),
            printer_selector_name,
            auto_restore_k,
            track_print_consume,
            fetch_3mf,
            ignore_certificates,
            printer_mode,
            use_ams_scan,
            printer_ip: printer_ip.unwrap_or(Ipv4Address::new(0, 0, 0, 0)),
            printer_connectivity_ok: None,
            protocol_state: ProtocolState::new(),
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
            tray_tar: [255, 255],       // canonical format: 0..23 (AMS/AMS-HT), 254 (external), 255 (none)
            tray_now: [255, 255],
            tray_pre: [255, 255],
            locked_mode: match printer_mode {
                PrinterMode::Auto => None,
                PrinterMode::DevOrOldFirmware => Some(false),
                PrinterMode::Cloud => Some(true),
            },
            runtime_persistence_request_channel,
            ams_info: {
                let mut ams_info = alloc::vec![AmsInfo::default();14]; // 0..3: standard ams, 4..11: 128..135 (HT), 12: 254, 13: 255
                ams_info[12] = AmsInfo::external(1); // 254 is left, extruder 1
                ams_info[13] = AmsInfo::external(0); // 255 is right, extruder 0
                ams_info
            },
            fila_switch: BambuFilaSwitchState::default(),
            gcode_state: GcodeState::Unknown,
            layer_num: None,
            total_layer_num: None,
            mc_percent: None,
            mc_remaining_time: None,
            print_error: None,
            gcode_file_prepare_percent: None,
            subtask_name: None,
            stg_cur: None,
            hms: None,
            snapshot_state: None,
        }
    }

    pub fn set_snapshot_state(&mut self, snapshot_state: PrinterSnapshotState) {
        self.snapshot_state = Some(snapshot_state);
    }

    pub fn snapshot_state(&self) -> Option<PrinterSnapshotState> {
        self.snapshot_state.clone()
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
            "093" => PrinterModel::H2S,
            "20P" => PrinterModel::X2D,
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
            PrinterModel::H2S => PrinterModelSeries::H2,
            PrinterModel::H2C => PrinterModelSeries::H2,
            PrinterModel::X2D => PrinterModelSeries::X2,
        }
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

    fn get_active_extruder_from_extruder_state(extruder_state: &Option<i32>) -> Option<usize> {
        let extruder_index = (extruder_state.unwrap_or_default() >> 4 & 0xF) as usize;
        if extruder_index <= 1 { Some(extruder_index) } else { None }
    }

    fn get_active_extruder(&self) -> Option<usize> {
        Self::get_active_extruder_from_extruder_state(self.extruder_state())
    }

    pub fn notify_printer_connect_status(&mut self, status: bool) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_printer_connect_status(self, status);
        }
    }

    pub fn notify_tag_scanned(&self, tray_id: i32, tag_id: &str, only_spool_id: bool) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_scanned(tray_id, tag_id, only_spool_id);
        }
    }

    pub fn notify_slot_consumption_reported(&mut self, tray_id: i32, grams: f32) {
        let Some(tray_id) = canonical_tray_id(tray_id) else {
            error!("[{}] Ignoring consumption for unsupported Bambu tray id {tray_id}", self.printer_number);
            return;
        };
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_slot_consumption_reported(tray_id, grams);
        }
    }

    pub fn update_ams_trays_done(&mut self, prev_trays_bits: &TrayBits, new_trays_bits: &TrayBits, removed_tags: &HashMap<usize, SpoolId>) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_trays_update(self, prev_trays_bits, new_trays_bits, removed_tags);
        }
    }

    pub fn get_unique_extruder_id_for_tray(&self, tray_id: i32) -> Option<u32> {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        // In real Bambu FTS setups, internal AMS groups are ambiguous and external trays are uniquely bound.
        // The exact-one rule also handles defensive protocol cases that are not expected normal configurations.
        let ams_info_index = self.get_ams_info_index_for_tray(tray_id).ok()?;
        let bound_extruders = &self.ams_info[ams_info_index].bound_extruders;
        if bound_extruders.len() == 1 { Some(bound_extruders[0]) } else { None }
    }

    pub fn get_extruder_id_for_tray(&self, tray_id: i32) -> Result<u32, String> {
        // Display-only fallback for K value/name lookup. Automatic PA commands must use
        // get_unique_extruder_id_for_tray() so ambiguous FTS AMS groups are never configured as extruder 0.
        let ams_info_index = self.get_ams_info_index_for_tray(tray_id)?;
        self.ams_info[ams_info_index].bound_extruders.first().copied().ok_or_else(|| {
            format!(
                "[{}] Slot {tray_id} does not resolve to a Bambu extruder",
                self.printer_number
            )
        })
    }

    pub fn get_extruder_for_tray(&self, tray_id: i32) -> Result<&Extruder, String> {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        Ok(self.get_extruder(self.get_extruder_id_for_tray(tray_id)?))
    }

    pub fn fila_switch_installed(&self) -> bool {
        self.fila_switch.installed
    }

    pub fn queue_runtime_persistence_request(&self, kind: PrinterRuntimePersistenceRequestKind) {
        channel_send(
            &self.runtime_persistence_request_channel,
            PrinterRuntimePersistenceRequest {
                printer_id: BambuPrinterConfig::printer_id_for_serial(&self.printer_serial),
                kind,
            },
        );
    }
}

pub type SpoolId = String;

fn default_printer_name() -> String {
    String::from("Unknown")
}

#[allow(clippy::too_many_arguments)]
pub fn init(
    framework: Rc<RefCell<Framework>>,
    printer_number: usize, // number of printer in user's configuration,
    printer_config_name: &Option<String>,
    printer_config: &BambuPrinterConfig,
    app_config: Rc<RefCell<AppConfig>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
    runtime_persistence_request_channel: Rc<PrinterRuntimePersistenceRequestChannel>,
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

    let printer_ip = printer_config.ip;
    let log_filter = if let Some(log_filter) = &printer_config.log_filter {
        *log_filter
    } else {
        log::LevelFilter::Warn
    };
    let auto_restore_k = printer_config.auto_restore_k;
    let track_print_consume = printer_config.track_print_consume;
    let fetch_3mf = printer_config.fetch_3mf;
    let ignore_certificates = printer_config.ignore_certificates;
    let printer_mode = printer_config.printer_mode;
    let use_ams_scan = printer_config.use_ams_scan;

    // == Setup MQTT ==================================================================
    let write_packets = Rc::new(WritePacketsChannel::new());

    let read_packets = Rc::new(ReadPacketsPubSub::new());

    let restart_printer = Rc::new(embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>::new());

    let bambu_printer = BambuPrinter::new(
        printer_number,
        &printer_serial,
        &printer_access_code,
        printer_config_name,
        &printer_ip,
        auto_restore_k,
        track_print_consume,
        fetch_3mf,
        ignore_certificates,
        printer_mode,
        use_ams_scan,
        write_packets.clone(),
        app_config.clone(),
        restart_printer.clone(),
        log_filter,
        runtime_persistence_request_channel,
    );

    spawner
        .spawn_heap(restartable_mqtt_task(
            framework,
            MQTT_TCP_RX_BUFFER_BYTES,
            MQTT_TCP_TX_BUFFER_BYTES,
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
    let printer_number = bambu_printer.borrow().printer_number;

    BambuPrinter::init_protocol(&bambu_printer).await;

    BambuPrinter::request_version_info_async(&bambu_printer).await;

    // fetch first setting for all nozzles, need that in advance before getting filaments
    let nozzle_diameters = ["0.2", "0.6", "0.8", "0.4"];
    for nozzle_diameter in nozzle_diameters {
        debug!("[{printer_number}] Request calibration information for nozzle {nozzle_diameter}");
        BambuPrinter::fetch_filament_calibrations_async(&bambu_printer, nozzle_diameter).await;
        Timer::after_millis(200).await;
    }

    // Now request full update, and wait until data is processed and have the nozzle diameter at hand for next request
    BambuPrinter::request_full_update_async(&bambu_printer).await;
    while bambu_printer.borrow().nozzle_diameter(0).is_none() {
        Timer::after_millis(100).await;
    }

    // Get again the filaments for current nozzle size,
    // that's because in slicer they don't check if data received from printer it's current nozzle or not
    // it's a bug there, can even be reproduced in the slicer by switching in the manage results to another nozzle diameter
    let curr_nozzle_diameter = bambu_printer.borrow().nozzle_diameter(0).as_ref().unwrap().clone();
    BambuPrinter::fetch_filament_calibrations_async(&bambu_printer, &curr_nozzle_diameter).await;
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
    H2S,
    X2D,
}

pub enum PrinterModelSeries {
    Unknown,
    X1,
    P1,
    A1,
    H2,
    P2,
    X2,
}

#[derive(Clone, Debug)]
pub enum PrinterConnectMode {
    Unknown,
    Cloud,
    Lan,
}
