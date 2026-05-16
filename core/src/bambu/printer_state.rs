use core::cell::RefCell;

use crate::bambu::calibration::Calibration;
use crate::bambu::{Extruder, default_printer_name};
use alloc::borrow::Cow;
use alloc::{format, rc::Rc, string::String};
use embassy_time::Timer;
use framework::{debug, error, info, term_info};
use framework::{prelude::Framework, term_error};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    bambu::{BambuPrinter, tray::Tray},
    store::Store,
};

#[derive(Serialize, Deserialize, Debug)]
pub struct PrinterPersistentState<'a> {
    pub ams_trays: Cow<'a, [Tray]>,
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
    pub calibrations: Cow<'a, [Calibration]>,
    #[serde(default = "default_printer_name")]
    pub printer_name: String,
    #[serde(default)]
    pub extruders: Option<Cow<'a, [Extruder; 2]>>,
    #[serde(default)]
    pub extruder_state: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
pub struct BambuPersistentDirtyState {
    virt_trays_dirty: bool,
    ams_trays_dirty: [bool; 24],
    extruders_dirty: bool,
    ams_exist_bits_dirty: bool,
    tray_exist_bits_dirty: bool,
    tray_read_done_bits_dirty: bool,
    calibrations_dirty: bool,
    printer_name_dirty: bool,
    relevant_extruder_state_dirty: bool,
}

impl BambuPrinter {
    pub fn load_printer_private_state_value(&mut self, state: Value, store: &Rc<Store>) -> Result<(), String> {
        match serde_json::from_value::<PrinterPersistentState<'static>>(state) {
            Ok(printer_state) => {
                self.init_printer_persistent_state(printer_state, store);
                Ok(())
            }
            Err(err) => {
                error!("[{}] Failed to parse printer private state: {}", self.printer_number, err);
                Err(format!(
                    "[{}] Failed to Parse Printer Private State (Check Terminal for More Info)",
                    self.printer_number
                ))
            }
        }
    }

    pub fn printer_persistent_state_store_blocked(&self) -> bool {
        self.auto_restore_k && self.pending_k_restore_sequence
    }

    pub fn printer_persistent_state_dirty(&self) -> bool {
        self.ams_trays_dirty.iter().any(|&v| v)
            || self.virt_trays_dirty
            || self.extruders_dirty
            || self.ams_exist_bits_dirty
            || self.tray_exist_bits_dirty
            || self.tray_read_done_bits_dirty
            || self.calibrations_dirty
            || self.printer_name_dirty
            || self.force_store_state
            || self.relevant_extruder_state_dirty
    }

    fn printer_persistent_state(&self) -> PrinterPersistentState<'_> {
        PrinterPersistentState {
            ams_trays: Cow::Borrowed(self.ams_trays()),
            virt_tray: None,
            virt_trays: Some(Cow::Borrowed(self.virt_trays())),
            nozzle_diameter: None, // for backwards compatibility before dual extruder printer
            ams_exist_bits: self.inner_ams_exist_bits,
            tray_exist_bits: self.inner_tray_exist_bits,
            tray_read_done_bits: self.inner_tray_read_done_bits,
            calibrations: Cow::Borrowed(&self.calibrations),
            printer_name: self.inner_printer_name.clone(),
            extruders: Some(Cow::Borrowed(&self.inner_extruders)),
            extruder_state: self.inner_extruder_state,
        }
    }

    pub fn prepare_printer_private_state_store(&mut self) -> Result<Option<(Value, Option<BambuPersistentDirtyState>)>, String> {
        if self.printer_persistent_state_store_blocked() {
            return Ok(None);
        }

        let private_dirty = self.printer_persistent_state_dirty();
        let state =
            serde_json::to_value(self.printer_persistent_state()).map_err(|err| format!("Failed to serialize Bambu private printer state: {err}"))?;
        let dirty_state = if private_dirty {
            debug!(
                "[{}] Dirty status: AMS slots({}), Ext slots({}), Extruders({}), AmsExists: ({}), Tray Exists: ({}), Try Read Done ({}), Calibrations ({}), Printer Name ({}), Relevant Extruder State ({}), Forced Store ({})",
                self.printer_number,
                self.ams_trays_dirty.iter().any(|&v| v),
                self.virt_trays_dirty,
                self.extruders_dirty,
                self.ams_exist_bits_dirty,
                self.tray_exist_bits_dirty,
                self.tray_read_done_bits_dirty,
                self.calibrations_dirty,
                self.printer_name_dirty,
                self.relevant_extruder_state_dirty,
                self.force_store_state,
            );
            let dirty_state = self.printer_persistent_dirty_state();
            self.clear_printer_persistent_dirty_state();
            Some(dirty_state)
        } else {
            None
        };

        Ok(Some((state, dirty_state)))
    }

    pub fn restore_printer_persistent_dirty_state(&mut self, dirty_state: BambuPersistentDirtyState) {
        self.virt_trays_dirty |= dirty_state.virt_trays_dirty;
        for (current, previous) in self.ams_trays_dirty.iter_mut().zip(&dirty_state.ams_trays_dirty) {
            *current |= *previous;
        }
        self.extruders_dirty |= dirty_state.extruders_dirty;
        self.ams_exist_bits_dirty |= dirty_state.ams_exist_bits_dirty;
        self.tray_exist_bits_dirty |= dirty_state.tray_exist_bits_dirty;
        self.tray_read_done_bits_dirty |= dirty_state.tray_read_done_bits_dirty;
        self.calibrations_dirty |= dirty_state.calibrations_dirty;
        self.printer_name_dirty |= dirty_state.printer_name_dirty;
        self.relevant_extruder_state_dirty |= dirty_state.relevant_extruder_state_dirty;
        self.force_store_state = true; // be conservative in case a dirty source was missed
    }

    fn printer_persistent_dirty_state(&self) -> BambuPersistentDirtyState {
        BambuPersistentDirtyState {
            virt_trays_dirty: self.virt_trays_dirty,
            ams_trays_dirty: self.ams_trays_dirty,
            extruders_dirty: self.extruders_dirty,
            ams_exist_bits_dirty: self.ams_exist_bits_dirty,
            tray_exist_bits_dirty: self.tray_exist_bits_dirty,
            tray_read_done_bits_dirty: self.tray_read_done_bits_dirty,
            calibrations_dirty: self.calibrations_dirty,
            printer_name_dirty: self.printer_name_dirty,
            relevant_extruder_state_dirty: self.relevant_extruder_state_dirty,
        }
    }

    fn clear_printer_persistent_dirty_state(&mut self) {
        self.ams_trays_dirty.fill(false);
        self.virt_trays_dirty = false;
        self.extruders_dirty = false;
        self.ams_exist_bits_dirty = false;
        self.tray_exist_bits_dirty = false;
        self.tray_read_done_bits_dirty = false;
        self.calibrations_dirty = false;
        self.printer_name_dirty = false;
        self.force_store_state = false;
        self.relevant_extruder_state_dirty = false;
    }

    #[allow(dead_code)]
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
        if printer.borrow().dummy_printer() {
            return Ok(());
        }
        for trial in 1..=3 {
            // in separate section for file_store to be release for later load
            let file_store = framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            match file_store.read_file_str(&path).await {
                Ok(state_str) => {
                    if state_str.trim().is_empty() {
                        err_str = format!("[{printer_number}] Loaded empty state file {path} in trial {trial}");
                        term_error!("{}", err_str); // will retry after timeout below
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
    #[allow(dead_code)]
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
                    printer_borrow.printer_number,
                    ams_trays_dirty,
                    printer_borrow.virt_trays_dirty,
                    printer_borrow.extruders_dirty,
                    printer_borrow.ams_exist_bits_dirty,
                    printer_borrow.tray_exist_bits_dirty,
                    printer_borrow.tray_read_done_bits_dirty,
                    printer_borrow.calibrations_dirty,
                    printer_borrow.printer_name_dirty,
                    printer_borrow.relevant_extruder_state_dirty,
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
                    &format!("[{}] Printer State to Store is Empty", printer.borrow().printer_number),
                    "Please report on Github/Discord !!!",
                    crate::app::StatusType::Error,
                    0,
                );
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

    pub fn init_printer_persistent_state(&mut self, mut state: PrinterPersistentState, _store: &Rc<Store>) {
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
    }
}
