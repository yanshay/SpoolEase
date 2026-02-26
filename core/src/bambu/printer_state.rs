use core::cell::RefCell;

use crate::bambu::calibration::Calibration;
use crate::bambu::{Extruder, default_printer_name};
use alloc::borrow::Cow;
use alloc::{format, rc::Rc, string::String};
use embassy_time::Timer;
use framework::{debug, error, info, term_info};
use framework::{prelude::Framework, term_error};
use serde::{Deserialize, Serialize};

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

impl BambuPrinter {
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
            if self.get_any_tray(tray_id).meta_info.consumed_since_weight == 0.0
                && let Some(spool_id) = self.get_any_tray(tray_id).meta_info.spool_id.as_ref()
            {
                let spool_record = store.get_spool_by_id(spool_id.as_str());
                if let Some(spool_record) = spool_record {
                    self.update_any_tray(tray_id, |tray| tray.meta_info.consumed_since_weight = spool_record.consumed_since_weight);
                } else {
                    self.update_any_tray(tray_id, |tray| tray.meta_info.spool_id = None);
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
}
