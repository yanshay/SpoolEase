#![allow(dead_code)]
use core::cell::RefCell;

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use embassy_time::Instant;
use framework::{debug, error, info, prelude::Framework, warn};
use serde::{Deserialize, Serialize};
use shared::{
    gcode_analysis::FilamentUsageEntry,
    gcode_analysis_task::{Fetch3mf, FilamentUsage},
};

use crate::{
    bambu::{
        BambuPrinter,
        bambu_api::{AmsMapping2Entry, GcodeState, PrintData},
    },
    printer::PrinterRuntimePersistenceRequestKind,
};

const EXTRA_DEBUG: bool = false;

macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

mod instant_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(v: &Instant, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_u64(v.as_ticks())
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Instant, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ticks = u64::deserialize(d)?;
        Ok(Instant::from_ticks(ticks))
    }
}

#[derive(Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum GcodeAnalysis {
    #[default]
    WaitingForPrinter,
    Requested {
        #[serde(with = "instant_serde")]
        at: Instant,
        job_number: i32,
    },
    Received {
        #[serde(with = "instant_serde")]
        at: Instant,
        job_number: i32,
        #[serde(skip)]
        usage: FilamentUsage,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConsumeIndexState {
    pub rev: i32,
    pub value: i32,
}
impl Default for ConsumeIndexState {
    fn default() -> Self {
        Self { rev: -1, value: -1 }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PrintProject {
    pub project_id: String,
    pub subtask_name: String,
    pub threemf_url: String,           // url field
    pub gcode_filename_in_3mf: String, // param field, the gcode file inside the 3mf
    pub(super) ams_mapping: Vec<i32>,
    pub(super) ams_mapping2: Option<Vec<AmsMapping2Entry>>,
    pub(super) use_ams: Option<bool>,
    pub gcode_analysis: GcodeAnalysis,

    // track printer state fields
    pub(super) total_layer_num: i32,

    //
    pub(super) need_consume: bool,
    pub(super) inner_consume_index: i32,
    pub(super) consume_store_counter: i32,
}

impl PrintProject {
    pub(super) fn new(
        project_id: &str,
        subtask_name: &str,
        threemf_url: &str,
        gcode_filename_in_3mf: &str,
        ams_mapping: &[i32],
        ams_mapping2: Option<Vec<AmsMapping2Entry>>,
        use_ams: Option<bool>,
    ) -> Self {
        Self {
            project_id: project_id.to_string(),
            subtask_name: subtask_name.to_string(),
            ams_mapping: ams_mapping.to_vec(),
            ams_mapping2,
            gcode_analysis: GcodeAnalysis::WaitingForPrinter,
            total_layer_num: -1,
            need_consume: false,
            inner_consume_index: -1,
            threemf_url: threemf_url.to_string(),
            gcode_filename_in_3mf: gcode_filename_in_3mf.to_string(),
            use_ams,
            consume_store_counter: 0,
        }
    }
    pub(super) fn set_not_store_consume_index(&mut self, value: i32) {
        self.inner_consume_index = value;
    }
    pub(super) fn store_consume_index(&mut self, printer: &BambuPrinter) {
        self.consume_store_counter += 1;
        printer.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::StoreConsumeIndex {
            consume_store_counter: self.consume_store_counter,
        });
    }
    pub(super) fn consume_index(&self) -> i32 {
        self.inner_consume_index
    }

    pub(super) fn get_ams_mapping_tray_id(&self, filament_id: i32) -> Option<i32> {
        // This function prioritize old firmware API (ams_mapping) and resorts to new (ams_mapping2) in case it's not there
        if filament_id >= 0 {
            let ams_slot = self.ams_mapping.get(filament_id as usize).copied();
            // Deal with multiextruder, old and new mappings
            // Single extruded (so no_ams means external spool, and not always there is ams_mapping2,
            // Sometimes even ams_mapping is empty in case of external spool (Orca on A1)
            if ams_slot.is_none() || ams_slot == Some(-1) {
                if let Some(ams_mapping2) = &self.ams_mapping2 {
                    if let Some(ams2_info) = ams_mapping2.get(filament_id as usize) {
                        if ams2_info.ams_id == 255 && ams2_info.slot_id == 0 {
                            return Some(255);
                        } else if ams2_info.ams_id == 254 && ams2_info.slot_id == 0 {
                            return Some(254);
                        }
                    }
                } else if self.use_ams == Some(false) {
                    return Some(255);
                }
            }
            ams_slot
        } else {
            None
        }
    }
    pub(super) fn curr_usage_entry(&self) -> Option<&FilamentUsageEntry> {
        if self.consume_index() < 0 {
            return None;
        }
        if let GcodeAnalysis::Received { at: _, job_number: _, usage } = &self.gcode_analysis {
            usage.data.get(self.consume_index() as usize)
        } else {
            None
        }
    }
}

impl BambuPrinter {
    #[allow(non_snake_case)]
    pub fn process_print_message__project_file(&mut self, print: &PrintData) -> bool {
        self.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::DeletePrintProject);

        let mut changed = false;
        let printer_log_id = self.printer_number;
        if !self.track_print_consume {
            info!("[{printer_log_id}] Print project started but configured not to track print filament usage");
            return false;
        }
        // TODO: theoretically all are options so could 'take' instead of clone
        if let (Some(subtask_name), Some(ams_mapping), Some(url), Some(param)) = (&print.subtask_name, &print.ams_mapping, &print.url, &print.param) {
            let ams_mapping2 = print.ams_mapping2.clone();
            let use_ams = print.use_ams;
            let project_id = if let Some(project_id) = &print.project_id {
                project_id.to_string()
            } else {
                format!("display_print_{:?}", print.sequence_id)
            };
            info!("[{printer_log_id}] Print project started: name: '{subtask_name}', using ams slots: {ams_mapping:?}, {ams_mapping2:?}");
            let mut curr_print_project = PrintProject::new(&project_id, subtask_name, url, param, ams_mapping, ams_mapping2, use_ams);

            // in case of http can already fetch now and not wait for printer to download first
            if self.fetch_3mf == Fetch3mf::CloudHttp
                || curr_print_project.threemf_url.starts_with("ftp://")
                || curr_print_project.threemf_url.starts_with("file://")
                || curr_print_project.threemf_url.starts_with("brtc://")
            {
                let job_number = self.notify_request_gcode_analysis(&curr_print_project);
                curr_print_project.gcode_analysis = GcodeAnalysis::Requested {
                    at: Instant::now(),
                    job_number,
                };
            }

            self.update_trays_from_print_job(&curr_print_project);
            changed = true;

            self.curr_print_project = Some(curr_print_project);
        }

        changed
    }

    pub fn update_trays_from_print_job(&mut self, print: &PrintProject) {
        // set trays used in print, but first clear all
        self.clear_snapshot_slots_used_in_print();

        for tray_id in &print.ams_mapping {
            let tray_id = *tray_id as usize;
            if (0..self.ams_trays().len()).contains(&tray_id) {
                self.set_snapshot_slot_used_in_print(tray_id, true);
            }
        }
        if let Some(ams_mapping2) = &print.ams_mapping2 {
            for ams2_info in ams_mapping2 {
                if ams2_info.ams_id == 255 && ams2_info.slot_id == 0 {
                    self.set_snapshot_slot_used_in_print(255, true);
                } else if ams2_info.ams_id == 254 && ams2_info.slot_id == 0 {
                    self.set_snapshot_slot_used_in_print(254, true);
                }
                // TODO: not filling standard trays from ams_mapping2, maybe should for future removal of ams_mapping?
            }
        } else if print.use_ams == Some(false) {
            self.set_snapshot_slot_used_in_print(255, true);
        }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__print_project_logic(&mut self, print: &PrintData) -> bool {
        let mut changed = false;
        let mut gcode_state_change = false;
        let mut new_gcode_state = self.gcode_state;
        // Order when printing is: tray_pre, then tray_tar, then tray_now
        let printer_log_id = self.printer_number;

        let mut curr_print_project = self.curr_print_project.take();

        if let Some(curr_print_project) = &mut curr_print_project {
            debugex!(">>>>> In print_project_logic - curr_print_project available");
            let mut tray_tar_change_from_tray_now = false; // plan to switch filament
            let mut tray_active_change = false; // new filament is loaded
            // let mut tray_pre_change_to_tray_now = false; // meaning, starting to pull out filament

            let mut layer_num_change = false;
            let mut new_layer_num = self.layer_num.unwrap_or(-1);

            // Update print project state
            if let Some(gcode_state) = print.gcode_state {
                if gcode_state == GcodeState::Unsupported {
                    error!("[{}] Unsupported gcode state", self.printer_number);
                } else if gcode_state != self.gcode_state {
                    gcode_state_change = true;
                    new_gcode_state = gcode_state;
                }
            }

            if gcode_state_change
                && new_gcode_state == GcodeState::PREPARE
                && let Some(subtak_name) = &print.subtask_name
            {
                // This fix special characters (in the prepare the printer notifies of the real file name after special chars fix)
                // This is important for FTP access
                curr_print_project.subtask_name = subtak_name.clone();
            }

            if let Some(total_layer_num) = print.total_layer_num {
                curr_print_project.total_layer_num = total_layer_num;
            }

            if let Some(layer_num) = print.layer_num
                && layer_num != self.layer_num.unwrap_or(-1)
            {
                layer_num_change = true;
                new_layer_num = layer_num;
            }

            let curr_tray_active = self.get_tray_active();
            let new_tray_active = Self::get_print_data_tray_active(print, curr_tray_active);
            if new_tray_active != curr_tray_active {
                tray_active_change = true;
            }

            #[allow(clippy::collapsible_if)]
            if let Some(_extruder) = print.device.as_ref().and_then(|d| d.extruder.as_ref()) {
                // in case of multiextruder consuming on change of tray_tar is too sensitive to exact timing and potential to change in the future
                // So not doing it for now
                // if update_tray_tar != self.tray_tar && update_tray_tar != self.tray_now {
                //     // if any extrudeer tray_tar changed, it means tray_tar for our purpose of consume changed
                //     tray_tar_change_from_tray_now = true;
                // }
            } else if let Some(ams) = &print.ams {
                if let Some(update_tray_tar) = ams.tray_tar {
                    if update_tray_tar != self.tray_tar[0] && update_tray_tar != self.tray_now[0] {
                        tray_tar_change_from_tray_now = true;
                    }
                }

                // This was generalized into tray_active
                // if let Some(update_tray_now) = ams.tray_now {
                //     if update_tray_now != self.tray_now[0] {
                //         tray_now_change = true;
                //     }
                //     new_tray_now = update_tray_now;
                // }

                // This is old - not required, leaving for now
                // if let Some(update_tray_pre) = ams.tray_pre {
                //     if update_tray_pre != self.tray_pre && update_tray_pre == self.tray_now {
                //         tray_pre_change_to_tray_now = true;
                //     }
                // }
            }

            // Non modifying state (except consume) actions based on current state and changes
            // Notes: verifying match to layer number and color since several changes in sequence could
            //   want to consume the same usage because they come one after another and don't want
            //   to rely on order (e.g. layer change and filament change which could happen or not after)
            // if tray_pre_change_to_tray_now {
            //     // debugex!(">>>>> tray_pre_change_to_tray_now && need_consume");
            //     changed |= self.try_consume(curr_print_project);
            // }

            if tray_tar_change_from_tray_now && curr_print_project.consume_index() != 0 {
                debugex!(">>>>> tray_tar_change_from_tray_now && && not first consume");
                changed |= self.try_consume(curr_print_project, ConsumeType::FilamentSwitch);
            }

            if tray_active_change {
                debugex!(">>>>> tray_active change (from {:?} to {:?})", curr_tray_active, new_tray_active);
                changed |= self.try_consume(curr_print_project, ConsumeType::FilamentSwitch);
            }

            if layer_num_change && new_layer_num != 0 {
                // important that it comes last
                debugex!(">>>>> layer_num_change (from {:?} to {})", self.layer_num, new_layer_num);
                changed |= self.try_consume(curr_print_project, ConsumeType::LayerChange(new_layer_num));
                // do some validations here:
                //    verify that the new entry is for the next layer
                //    if not consumed verify that previous color is different from next layer (so color change caused a consume)
            }

            if gcode_state_change && new_gcode_state == GcodeState::FINISH {
                // if there is one to consume consume it.
                // !!! verify it is the last one
                self.try_consume(curr_print_project, ConsumeType::Finish);
                info!("[{printer_log_id}] Print project finished successfuly");
                if let GcodeAnalysis::Received { at: _, job_number: _, usage } = &curr_print_project.gcode_analysis {
                    if curr_print_project.consume_index() != usage.data.len() as i32 {
                        error!(
                            "[{}] Print project filament consumption tracking didn't finish well, reached index {} (0 based), while usage data contain {} records",
                            printer_log_id,
                            curr_print_project.consume_index(),
                            usage.data.iter().len()
                        );
                    } else {
                        info!("[{printer_log_id}] All consumption entries used as expected");
                    }
                } else {
                    error!("[{printer_log_id}] Something is wrong tracking print project, at FINISH no gcode_analysis data available");
                }
            }

            if gcode_state_change && new_gcode_state == GcodeState::FAILED {
                info!("[{printer_log_id}] Print project failed");
            }

            // Modifying state actions

            if layer_num_change && new_gcode_state == GcodeState::RUNNING {
                // here we don't test for gcode_state_change becasue it's the ongoing state that counts
                // debugex!(">>>>> layer_num_change, set need_consume true");
                curr_print_project.need_consume = true;
            }

            if tray_active_change && new_tray_active.is_some() {
                // debugex!(">>>>> tray_now_change, set need_consume true");
                curr_print_project.need_consume = true;
            }

            // all the gcode_state, layer_num, tray_tar/pre/now will be updated by process_ams since it is not only for print case

            // curr_gcode_state = curr_print_project.gcode_state;

            // // debugex!(">>>> {:?}, {:?}, {}", curr_print_project.gcode_analysis, curr_print_project.gcode_state, curr_print_project.layer_num);
            // if curr_print_project.gcode_analysis == GcodeAnalysis::WaitingForPrinter
            //     && curr_print_project.gcode_state == GcodeState::RUNNING
            //     && curr_print_project.total_layer_num != -1
            // {
            //     // curr_print_project.need_consume = true; // changed to set it only when state CHANGED to running
            // }

            // actions post updates,
            // be aware that still can't rely on updated self.tray_tar/now/pre which will be updated later

            // debug!(">>>>> gcode_state_change {gcode_state_change}, new_gcode_state {new_gcode_state:?}, curr_print_project.gcode_analysis {:?}", curr_print_project.gcode_analysis);
            if gcode_state_change && new_gcode_state == GcodeState::RUNNING {
                // if not requested earlier, request scale to fetch gcode from printer and analyze it
                // In case of ftp it will be requested here, if http already earlier when project_file arrived
                if curr_print_project.gcode_analysis == GcodeAnalysis::WaitingForPrinter {
                    let job_number = self.notify_request_gcode_analysis(curr_print_project);
                    curr_print_project.gcode_analysis = GcodeAnalysis::Requested {
                        at: Instant::now(),
                        job_number,
                    };
                }
                curr_print_project.need_consume = true;
            }

            if gcode_state_change && new_gcode_state == GcodeState::FAILED && curr_print_project.gcode_analysis != GcodeAnalysis::WaitingForPrinter {
                match curr_print_project.gcode_analysis {
                    GcodeAnalysis::WaitingForPrinter => unreachable!(),
                    GcodeAnalysis::Requested { at: _, job_number } | GcodeAnalysis::Received { at: _, job_number, usage: _ } => {
                        self.notify_cancel_gcode_analysis(job_number);
                    }
                }
            }
        } else {
            debugex!(">>>>> In print_project_logic - curr_print_project NOT available");
        }

        self.curr_print_project = curr_print_project;

        // need to do it here because above we used 'take()' to get curr_print_project and only in here in previous line we gave it back
        if gcode_state_change && [GcodeState::FAILED, GcodeState::FINISH].contains(&new_gcode_state) {
            self.curr_print_project = None;
            self.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::DeletePrintProject);

            self.clear_snapshot_slots_used_in_print();
        }

        changed
    }

    fn try_consume(&mut self, print_project: &mut PrintProject, consume_type: ConsumeType) -> bool {
        let consume_index_at_start = print_project.consume_index();
        debugex!(">>>>>>> Trying to consume");
        let printer_log_id = self.printer_number;
        if !print_project.need_consume {
            return false;
        }
        let mut consumed = false;
        debugex!(">>>> need consume = true");
        match consume_type {
            ConsumeType::LayerChange { .. } | ConsumeType::Finish => {
                let up_to_layer_num = match consume_type {
                    ConsumeType::LayerChange(v) => v,
                    ConsumeType::Finish => -1,
                    ConsumeType::FilamentSwitch => unreachable!(),
                };
                debugex!(">>>>> Layer change consume");
                loop {
                    debugex!(">>>>> Consume loop");
                    if let Some(usage_entry) = print_project.curr_usage_entry() {
                        debugex!(">>>>>> Checking curr usage entry {usage_entry:?}");
                        if usage_entry.layer < up_to_layer_num || up_to_layer_num == -1 {
                            // comparing with previous layer - to consume all previous layers in case of skip
                            if let Some(usage_entry_tray_id) = print_project.get_ams_mapping_tray_id(usage_entry.gcode_filament_id) {
                                self.notify_slot_consumption_reported(usage_entry_tray_id, usage_entry.weight_g);
                                debug!(
                                    "[{}] Print project consumed entry {} on layer change : {:.2}g, from filament at slot {}",
                                    printer_log_id,
                                    print_project.consume_index(),
                                    usage_entry.weight_g,
                                    usage_entry_tray_id,
                                );
                            } else {
                                error!(
                                    "[{}] Internal Error? No AMS slot for gcode filament id {}",
                                    printer_log_id, usage_entry.gcode_filament_id
                                );
                            }
                            print_project.set_not_store_consume_index(print_project.consume_index() + 1);
                            consumed = true;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                print_project.need_consume = false;
            }
            ConsumeType::FilamentSwitch => {
                if let Some(usage_entry) = print_project.curr_usage_entry() {
                    debugex!(">>>>>> Checking curr usage entry {usage_entry:?}");
                    if let Some(usage_entry_tray_id) = print_project.get_ams_mapping_tray_id(usage_entry.gcode_filament_id) {
                        debugex!(">>>>> self.layer_num = {:?}", self.layer_num);
                        debugex!(">>>>> self.get_tray_active() = {:?}", self.get_tray_active());
                        debugex!(">>>>> usage_entry_tray_id = {usage_entry_tray_id}");
                        if self.layer_num.unwrap_or(-1) == usage_entry.layer
                            && self.get_tray_active() == Some(usage_entry_tray_id)
                            && (0..self.ams_trays().len() as i32).contains(&usage_entry_tray_id)
                        {
                            self.notify_slot_consumption_reported(usage_entry_tray_id, usage_entry.weight_g);
                            debug!(
                                "[{}] Print project consumed entry {} on filament change : {:.2}g, from filament at slot {}",
                                printer_log_id,
                                print_project.consume_index(),
                                usage_entry.weight_g,
                                usage_entry_tray_id,
                            );
                            print_project.set_not_store_consume_index(print_project.consume_index() + 1);
                            consumed = true;
                            print_project.need_consume = false;
                        } else {
                            // No matching data to consume, this is ok
                        }
                    } else {
                        error!("[{printer_log_id}] No matching AMS slot for usage information {:?}", usage_entry);
                    }
                } else {
                    // Could happen in the last entry
                }
            }
        }
        if print_project.consume_index() != consume_index_at_start {
            info!(
                "[{}] Consumed indexes {} to {}",
                printer_log_id,
                consume_index_at_start,
                print_project.consume_index()
            );
            print_project.store_consume_index(self);
        }
        consumed
    }

    pub fn notify_cancel_gcode_analysis(&mut self, job_number: i32) {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_cancel_gcode_analysis(job_number);
        }
    }

    pub fn notify_request_gcode_analysis(&mut self, print_project: &PrintProject) -> i32 {
        let mut observers = self.observers.clone(); // to avoid two references - can probably optimize in various ways
        let mut job_number = 0;
        for weak_observer in observers.iter_mut() {
            let observer = weak_observer.upgrade().unwrap();
            let job_number_update = observer.borrow_mut().on_request_gcode_analysis(self, print_project);
            if job_number_update != 0 {
                if job_number == 0 {
                    job_number = job_number_update;
                } else {
                    error!(
                        "[{}] Internal software error, two gcode analysis requests listeners with with job_number, only one possible",
                        self.printer_number
                    );
                }
            }
        }
        job_number
    }

    pub fn set_gcode_analysis(&mut self, job_number: i32, filament_usage: FilamentUsage) {
        let printer_log_id = self.printer_number;
        info!("[{}] Setting gcode analysis with {} entries", printer_log_id, filament_usage.data.len());
        if let Some(curr_print_project) = &mut self.curr_print_project {
            // TODO: turn to a function on print_project or on printer
            match &curr_print_project.gcode_analysis {
                GcodeAnalysis::WaitingForPrinter => {
                    warn!("[{}>] Print monitoring awaiting printer, ignoring gcode analysis", printer_log_id);
                    return;
                }
                GcodeAnalysis::Requested {
                    at: _,
                    job_number: awaited_job_number,
                }
                | GcodeAnalysis::Received {
                    at: _,
                    job_number: awaited_job_number,
                    usage: _,
                } => {
                    if *awaited_job_number != job_number {
                        warn!(
                            "[{}] Print monitoring awaiting job number {}, received a different job number {}, ignoring gcode analysis",
                            printer_log_id, awaited_job_number, job_number
                        );
                        return;
                    }
                }
            }
            curr_print_project.gcode_analysis = GcodeAnalysis::Received {
                at: Instant::now(),
                job_number,
                usage: filament_usage,
            };
            if curr_print_project.consume_index() == -1 {
                curr_print_project.set_not_store_consume_index(0);
            }
            self.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::StorePrintProject);
        } else {
            error!("[{}] Internal Error setting gcode analysis", self.printer_number);
        }
    }

    pub async fn load_print_project_state(framework: &Rc<RefCell<Framework>>, printer: &Rc<RefCell<BambuPrinter>>) -> Result<bool, String> {
        let printer_log_id = printer.borrow().printer_number;
        let print_project_path = printer.borrow().printer_state_path_for_file("print.jsn");
        let filament_usage_path = printer.borrow().printer_state_path_for_file("print.csv");
        let file_store = framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let filament_usage = match file_store.read_file_str(&filament_usage_path).await {
            Ok(filament_usage_str) => match FilamentUsage::from_csv(&filament_usage_str) {
                Ok(v) => v,
                Err(err) => {
                    let err_str = format!("[{}] Error Parsing Filament Usage File", printer_log_id);
                    error!("{err_str} : {err}");
                    return Err(err_str);
                }
            },
            Err(_) => return Ok(false),
        };
        let mut print_project = match file_store.read_file_str(&print_project_path).await {
            Ok(print_project_str) => match serde_json::from_str::<PrintProject>(&print_project_str) {
                Ok(print_project) => print_project,
                Err(err) => {
                    let err_str = format!("[{}] Error Parsing Print Project File", printer_log_id);
                    error!("{err_str} : {err}");
                    return Err(err_str);
                }
            },
            Err(err) => {
                let err_str = format!("[{}] Missing/Partial Print Project State (1)", printer_log_id);
                error!("{err_str} : {err}");
                return Err(err_str);
            }
        };
        let (consume_index, consume_store_counter) = {
            let mut consume_index_state = ConsumeIndexState::default();
            let mut at_least_one_loaded = false;
            let mut at_least_one_parsed = false;
            for i in 0..=1 {
                let consume_index_path = printer.borrow().printer_state_path_for_file(&format!("print.ci{i}"));
                info!("Trying to read file {consume_index_path}");
                #[allow(clippy::single_match)]
                match file_store.read_file_str(&consume_index_path).await {
                    Ok(consume_index_str) => {
                        at_least_one_loaded = true;
                        match serde_json::from_str::<ConsumeIndexState>(&consume_index_str) {
                            Ok(v) => {
                                at_least_one_parsed = true;
                                if v.rev > consume_index_state.rev {
                                    consume_index_state = v;
                                }
                            }
                            Err(err) => {
                                error!(
                                    "Error parsing consume index file {consume_index_path}{}, error : {err}",
                                    if i == 0 { ", will try second one" } else { "" }
                                );
                                error!("File {consume_index_path} contains '{consume_index_str}'");
                            }
                        }
                    }
                    Err(_) => (),
                }
            }
            if !at_least_one_loaded {
                let err_str = format!("[{}] Missing/Partial Print Project State (2)", printer_log_id);
                error!("{err_str}");
                return Err(err_str);
            }
            if !at_least_one_parsed {
                let err_str = format!("[{}] Error Parsing Consume Index File", printer_log_id);
                error!("{err_str}");
                return Err(err_str);
            }
            (consume_index_state.value, consume_index_state.rev)
        };

        if let GcodeAnalysis::Received { ref mut usage, .. } = print_project.gcode_analysis {
            *usage = filament_usage;
        } else {
            let err_str = format!("[{}] Internal: Print Project Stored With Wrong GCodeAnalysis State", printer_log_id);
            error!("{err_str}");
            return Err(err_str);
        }
        print_project.set_not_store_consume_index(consume_index);
        print_project.consume_store_counter = consume_store_counter; // don't increase by 1 here, will be increased before next save

        printer.borrow_mut().loaded_print_project = Some(print_project);

        info!("[{}] Loaded print project resume state", printer.borrow().printer_number);
        Ok(true)
    }

    // TODO: make error handling consistent, should not issue messages, only view_mnodel should
    pub async fn store_print_project_state(framework: &Rc<RefCell<Framework>>, printer: &Rc<RefCell<BambuPrinter>>) -> Result<(), String> {
        BambuPrinter::delete_print_project_state(framework, printer).await;
        let printer_log_id = printer.borrow().printer_number;
        info!("[{}] Storing print project resume state", printer.borrow().printer_number);
        let (curr_print_project_str, filament_usage_csv, consume_index_str, consume_store_counter) = {
            let printer_borrow = printer.borrow();
            if let Some(curr_print_project) = &printer_borrow.curr_print_project {
                if let GcodeAnalysis::Received {
                    at: _,
                    job_number: _,
                    usage: filament_uage,
                } = &curr_print_project.gcode_analysis
                {
                    let inner_curr_print_project_str = serde_json::to_string(curr_print_project).unwrap();
                    let inner_filament_usage_csv = filament_uage.to_csv().unwrap();
                    let consume_index_str = serde_json::to_string(&ConsumeIndexState {
                        rev: curr_print_project.consume_store_counter,
                        value: curr_print_project.consume_index(),
                    })
                    .unwrap();
                    (
                        inner_curr_print_project_str,
                        inner_filament_usage_csv,
                        consume_index_str,
                        curr_print_project.consume_store_counter,
                    )
                } else {
                    let err_str = format!("[{}] Internal Error: store_print_project_state called at wrong state", printer_log_id);
                    error!("{err_str}");
                    return Err(err_str);
                }
            } else {
                let err_str = format!(
                    "[{}] Internal Error: store_print_project_state called without curr_print_project",
                    printer_log_id
                );
                error!("{err_str}");
                return Err(err_str);
            }
        };
        let file_store = framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let print_project_path = printer.borrow().printer_state_path_for_file("print.jsn");
        let filament_usage_path = printer.borrow().printer_state_path_for_file("print.csv");
        let consume_index_path = printer
            .borrow()
            .printer_state_path_for_file(&format!("print.ci{}", consume_store_counter % 2));
        if let Err(err) = file_store.create_write_file_str(&print_project_path, &curr_print_project_str).await {
            let err_str = format!("[{}] Error Writing Print Project File", printer_log_id);
            error!("{err_str} : {err:?}");
            return Err(err_str);
            // view_model.borrow().message_box("Print Tracking Notice", &err_str, "Spoolease Will Not be Able to Resume Tracking if Restarted", crate::app::StatusType::Error, 0);
        }
        if let Err(err) = file_store.create_write_file_str(&filament_usage_path, &filament_usage_csv).await {
            let err_str = format!("[{}] Error Writing Print Filament Usage File", printer_log_id);
            error!("{err_str} : {err:?}");
            return Err(err_str);
            // view_model.borrow().message_box("Print Tracking Notice", "Error Writing Print Filament Usage File", "Spoolease Will Not be Able to Resume Tracking if Restarted", crate::app::StatusType::Error, 0);
        }
        if let Err(err) = file_store.create_write_file_str(&consume_index_path, &consume_index_str).await {
            let err_str = format!("[{}] Error Writing Consume Index File", printer_log_id);
            error!("{err_str} : {err:?}");
            return Err(err_str);
            // view_model.borrow().message_box("Print Tracking Notice", "Error Writing Consume Index File", "SpoolEase Will Not be Able to Resume Tracking if Restarted", crate::app::StatusType::Error, 0);
        }
        Ok(())
    }

    // TODO: take care of error handling as well
    pub async fn store_consume_index_state(
        framework: &Rc<RefCell<Framework>>,
        printer: &Rc<RefCell<BambuPrinter>>,
        consume_store_counter: i32,
    ) -> Result<(), String> {
        info!(
            "[{}] Storing consume index resume state print.ci{}",
            printer.borrow().printer_number,
            consume_store_counter % 2
        );
        let printer_log_id = printer.borrow().printer_number;
        let consume_index_state_str = if let Some(curr_print_project) = &printer.borrow().curr_print_project {
            serde_json::to_string(&ConsumeIndexState {
                rev: consume_store_counter,
                value: curr_print_project.consume_index(),
            })
            .unwrap()
        } else {
            // project ended by now
            // let err_str = format!("[{printer_log_id}] Internal Error: store_consume_index_state called without curr_print_project");
            // error!("{err_str}");
            return Ok(());
        };

        let consume_index_path = printer
            .borrow()
            .printer_state_path_for_file(&format!("print.ci{}", consume_store_counter % 2));
        let file_store = framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        if let Err(err) = file_store.create_write_file_str(&consume_index_path, &consume_index_state_str).await {
            let err_str = format!("[{}] Error Writing Consume Index File", printer_log_id);
            error!("{err_str} : {err:?}");
            return Err(err_str);
        }
        Ok(())
    }

    pub async fn delete_print_project_state(framework: &Rc<RefCell<Framework>>, printer: &Rc<RefCell<BambuPrinter>>) {
        info!(
            "[{}] Erasing stored print project resume state (if exists)",
            printer.borrow().printer_number
        );
        let print_project_path = printer.borrow().printer_state_path_for_file("print.jsn");
        let filament_usage_path = printer.borrow().printer_state_path_for_file("print.csv");
        let consume_index_path0 = printer.borrow().printer_state_path_for_file("print.ci0");
        let consume_index_path1 = printer.borrow().printer_state_path_for_file("print.ci1");
        let file_store = framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let _ = file_store.delete_file(&consume_index_path0).await;
        let _ = file_store.delete_file(&consume_index_path1).await;
        let _ = file_store.delete_file(&filament_usage_path).await;
        let _ = file_store.delete_file(&print_project_path).await;
    }

    fn get_print_data_tray_active(print: &PrintData, current_tray_active: Option<i32>) -> Option<i32> {
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
}

#[derive(PartialEq)]
enum ConsumeType {
    LayerChange(i32),
    FilamentSwitch,
    Finish,
}
