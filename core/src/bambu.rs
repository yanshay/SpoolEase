// TODO:
// Deal with when to clear tag information, when we know spool taken out
// Deal with when to copy tag information between trays if only some data change but we know the spool is there

pub mod bambu_api;
pub mod bambu_print;
pub mod bambu_ssdp;
pub mod calibration;
pub mod filament;
pub mod mqtt;
pub mod protocol;
pub mod outgoing;
pub mod printer_state;
pub mod process_incoming;
pub mod tray;

use crate::bambu::bambu_api::{GcodeState, PrintDeviceNozzleInfo};
use crate::bambu::calibration::Calibration;
use crate::bambu::filament::{Filament, FilamentInfo};
use crate::bambu::mqtt::{ReadPacketsPubSub, WritePacketsChannel, restartable_mqtt_task};
use crate::bambu::process_incoming::incoming_messages_task;
use crate::bambu::protocol::ProtocolState;
use crate::bambu::tray::{Tray, TrayBits};
use crate::view_model::StoreStateRequestChannel;
use crate::{app_config::PrinterConfig, ssdp::SSDPPubSubChannel};
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use bambu_print::PrintProject;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use core::cell::RefCell;
use embassy_net::Ipv4Address;
use embassy_time::Timer;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::Fetch3mf;

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

#[derive(Default, Debug, Clone)]
pub struct AmsInfo {
    pub extruder: u32,
}

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

    pub fn dummy_printer(&self) -> bool {
        self.printer_serial == "000000000000000"
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

    pub fn update_ams_trays_done(&mut self, prev_trays_bits: &TrayBits, new_trays_bits: &TrayBits, removed_tags: &HashMap<usize, SpoolId>) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_trays_update(self, prev_trays_bits, new_trays_bits, removed_tags);
        }
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
}

pub type SpoolId = String;

fn default_printer_name() -> String {
    String::from("Unknown")
}

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
