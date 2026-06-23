use core::cell::RefCell;

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use embassy_time::{Duration, with_timeout};
use framework::{debug, error, info, term_info, trace, utils::SpawnerHeapExt, warn};
use hashbrown::HashMap;

use crate::{
    app_config::UseAmsScan,
    bambu::{
        BambuFilaSwitchPos, BambuFilaSwitchSlot, BambuPrinter, Filament, FilamentInfo, PrinterMode, ReadPacketsPubSub, SpoolId,
        bambu_api::{GcodeState, Message, PrintAms, PrintData, PrintTray},
        calibration::{Calibration, fix_k_on_restart},
        driver_specific::BambuAmsType,
        fetch_initial_info,
        protocol::clean_message_bytes_to_log,
        tray::{Tray, TrayBits, TrayMetaInfo, TrayState, canonical_tray_id, canonical_tray_id_from_ams_slot},
    },
    printer::PrinterRuntimePersistenceRequestKind,
    settings::MAX_NUM_PRINTERS,
};

impl BambuPrinter {
    fn parse_aux_fila_switch_installed(aux: &str) -> Option<bool> {
        u64::from_str_radix(aux, 16).ok().map(|info_bits| ((info_bits >> 29) & 1) == 1)
    }

    fn parse_fila_switch_slot(value: i32) -> Option<BambuFilaSwitchSlot> {
        if value < 0 {
            return None;
        }
        Some(BambuFilaSwitchSlot {
            ams_id: value >> 8,
            slot_id: value & 0xFF,
        })
    }

    fn parse_fila_switch_out_extruder(value: i32) -> Option<u32> {
        if value < 0 || value == 0x0E { None } else { Some(value as u32) }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__fila_switch(&mut self, print: &PrintData) -> bool {
        let mut new_state = self.fila_switch.clone();

        if let Some(aux) = &print.aux {
            if let Some(installed) = Self::parse_aux_fila_switch_installed(aux) {
                new_state.installed = installed;
            } else {
                warn!("[{}] Could not parse Bambu aux bits: {aux}", self.printer_number);
            }
        }

        if let Some(fila_switch) = print.device.as_ref().and_then(|device| device.fila_switch.as_ref()) {
            if let Some(in_slots) = &fila_switch.in_slots {
                new_state.in_slots = [
                    in_slots.first().and_then(|value| Self::parse_fila_switch_slot(*value)),
                    in_slots.get(1).and_then(|value| Self::parse_fila_switch_slot(*value)),
                ];
            }
            if let Some(out) = &fila_switch.out {
                new_state.out_extruders = [
                    out.first().and_then(|value| Self::parse_fila_switch_out_extruder(*value)),
                    out.get(1).and_then(|value| Self::parse_fila_switch_out_extruder(*value)),
                ];
            }
            if fila_switch.stat.is_some() {
                new_state.stat = fila_switch.stat;
            }
            if fila_switch.info.is_some() {
                new_state.info = fila_switch.info;
            }
        }

        if new_state != self.fila_switch {
            self.fila_switch = new_state;
            true
        } else {
            false
        }
    }

    fn ams_info_from_protocol(&self, ams_info: u32) -> (BambuAmsType, Vec<u32>, Option<BambuFilaSwitchPos>) {
        let ams_type = BambuAmsType::from_protocol((ams_info & 0x0F) as u8);
        let extruder_id = (ams_info >> 8) & 0x0F;
        let switcher_pos = match (ams_info >> 24) & 0x0F {
            0 => Some(BambuFilaSwitchPos::InB),
            1 => Some(BambuFilaSwitchPos::InA),
            _ => None,
        };

        let bound_extruders = if extruder_id == 0x0E {
            if self.fila_switch.installed && switcher_pos.is_some() {
                alloc::vec![0, 1]
            } else {
                Vec::new()
            }
        } else {
            alloc::vec![extruder_id]
        };

        (ams_type, bound_extruders, switcher_pos)
    }

    /// Converts AMS existence bits to the tray-bit mask covered by those AMS units.
    fn tray_mask_for_ams_exist_bits(ams_bits: u32) -> u32 {
        let mut tray_mask = 0u32;
        for ams_index in 0..=11 {
            if ((ams_bits >> ams_index) & 0x01) == 0 {
                continue;
            }

            if ams_index <= 3 {
                tray_mask |= 0x0f << (ams_index * 4);
            } else {
                tray_mask |= 1 << (16 + (ams_index - 4));
            }
        }
        tray_mask
    }

    /// During startup, keeps known AMS bits while accepting newly reported AMS bits.
    fn effective_startup_ams_exist_bits(&self, incoming_ams_exist_bits: u32) -> (u32, u32) {
        if !self.startup_ams_guard_active() {
            return (incoming_ams_exist_bits, 0);
        }

        let current_ams_exist_bits = (*self.ams_exist_bits()).unwrap_or_default();
        let protected_ams_bits = current_ams_exist_bits & !incoming_ams_exist_bits;
        let effective_ams_exist_bits = current_ams_exist_bits | incoming_ams_exist_bits;
        (effective_ams_exist_bits, Self::tray_mask_for_ams_exist_bits(protected_ams_bits))
    }

    /// Preserves previous tray bits for AMS slots protected by the startup guard.
    fn protect_startup_ams_guard_tray_bits(incoming_bits: u32, previous_bits: Option<u32>, protected_tray_mask: u32) -> u32 {
        if protected_tray_mask == 0 {
            return incoming_bits;
        }

        let Some(previous_bits) = previous_bits else {
            return incoming_bits;
        };

        (incoming_bits & !protected_tray_mask) | (previous_bits & protected_tray_mask)
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__vt_tray(&mut self, extruder_id: u32, v_tray: &PrintTray) -> (bool, Option<SpoolId>) {
        let old_tray = self.virt_trays()[extruder_id as usize].clone();
        let external_tray_id = if extruder_id == 0 { 255 } else { 254 };
        let new_tray = self.get_updated_tray(Some(v_tray), external_tray_id);
        if let Some(new_tray) = new_tray {
            let removed_tag = if old_tray.state != TrayState::Empty && new_tray.state == TrayState::Empty {
                self.snapshot_slot_spool_id(external_tray_id as usize)
            } else {
                None
            };
            self.set_virt_tray(extruder_id, new_tray);
            if removed_tag.is_some() {
                self.clear_snapshot_slot_consumption(external_tray_id as usize);
            }
            return (true, removed_tag);
        }
        (false, None)
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__vir_slots(&mut self, vir_slot: &[PrintTray]) -> (bool, HashMap<usize, SpoolId>) {
        let mut changed_res = false;
        let mut removed_tag_res = HashMap::new();
        for vt_slot in vir_slot {
            if let Some(external_slot_id) = vt_slot.id {
                let extruder_id = if external_slot_id == 254 { 1 } else { 0 };
                let (changed, removed_tag) = self.process_print_message__vt_tray(extruder_id, vt_slot);
                changed_res |= changed;
                if let Some(removed_tag) = removed_tag {
                    removed_tag_res.insert(external_slot_id as usize, removed_tag);
                }
            }
        }
        (changed_res, removed_tag_res)
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__ams_filament_setting(&mut self, print: &PrintData) -> bool {
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
                    tray_color: alloc::vec![print.tray_color.as_ref().cloned().unwrap_or_default()],
                    nozzle_temp_max: print.nozzle_temp_max.unwrap_or(250),
                    nozzle_temp_min: print.nozzle_temp_min.unwrap_or(190),
                })
            };
            // tray_id == 254 in the response message is for old firmwares
            // in new firmwares the response message arrives with ams_id==255 && tray_id==Some(0) (not like the command which is tray 254 and slot_id 0)
            // check first the use of ams_id, if not available then switch to tray_index 254
            let clearing_filament = new_filament == Filament::Unknown;
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
                if clearing_filament {
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
            if clearing_filament {
                self.clear_snapshot_slot_consumption(tray_index);
            }
            change_made = true;
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_sel(&mut self, print: &PrintData) -> bool {
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
    pub fn process_print_message__extrusion_cali_get(&mut self, print: &PrintData) -> bool {
        let mut change_made = false;
        // ignore if filament_id isn't ""
        if let Some(nozzle_diameter) = &print.nozzle_diameter
            && print.filament_id.as_deref() == Some("")
            && let Some(ref filaments) = print.filaments
        {
            // filaments is really calibrations
            let mut updated_calibrations = Vec::new();
            for filament in filaments {
                let Some(calibration) = Calibration::from(filament, nozzle_diameter) else {
                    warn!(
                        "[{}] Ignoring extrusion_cali_get for nozzle {nozzle_diameter}: missing cali_idx",
                        self.printer_number
                    );
                    return false;
                };
                updated_calibrations.push(calibration);
            }

            change_made = true;
            self.calibrations.retain(|cal| &cal.diameter != nozzle_diameter);
            self.calibrations.extend(updated_calibrations);
            self.calibrations_dirty = true;
        }

        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__common(&mut self, print: &PrintData) -> (bool, HashMap<usize, SpoolId>) {
        let mut removed_tags = HashMap::<usize, String>::new();
        // let command = print.command.unwrap_or_default();
        let prev_lock_mode = self.locked_mode;
        if self.printer_mode == PrinterMode::Auto
            && let Some(fun) = &print.fun
            && let Ok(fun) = u64::from_str_radix(fun, 16)
        {
            if fun & 0x20000000 != 0 {
                // locked mode
                self.locked_mode = Some(true)
            } else {
                // dev mode
                self.locked_mode = Some(false)
            }
        }
        if self.locked_mode != prev_lock_mode {
            info!(
                "[{}] Printer locked mode changed from {:?} to {:?}: printer_mode={:?} fun={:?}",
                self.printer_number, prev_lock_mode, self.locked_mode, self.printer_mode, print.fun
            );
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

        if print.layer_num.is_some() {
            self.layer_num = print.layer_num;
        }

        if print.total_layer_num.is_some() {
            self.total_layer_num = print.total_layer_num;
        }

        if print.mc_percent.is_some() {
            self.mc_percent = print.mc_percent;
        }

        if print.mc_remaining_time.is_some() {
            self.mc_remaining_time = print.mc_remaining_time;
        }

        if print.print_error.is_some() {
            self.print_error = print.print_error;
        }

        if print.gcode_file_prepare_percent.is_some() {
            self.gcode_file_prepare_percent = print.gcode_file_prepare_percent;
        }

        if print.subtask_name.is_some() {
            self.subtask_name = print.subtask_name.clone();
        }

        if print.stg_cur.is_some() {
            self.stg_cur = print.stg_cur;
        }

        if print.hms.is_some() {
            self.hms = print.hms.clone();
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
        if let Some(vir_slot) = &print.vir_slot {
            // this is hd2 version of external slotS
            let removed_tag;
            (vt_tray_change_made, removed_tag) = self.process_print_message__vir_slots(vir_slot);
            removed_tags.extend(removed_tag);
        } else if let Some(v_tray) = &print.vt_tray {
            // this is older printers external slot
            let removed_tag;
            (vt_tray_change_made, removed_tag) = self.process_print_message__vt_tray(0, v_tray);
            if let Some(removed_tag) = removed_tag {
                removed_tags.insert(255, removed_tag);
            }
        } else if tray_xxx_change_made {
            // I believe this situation can happen only in pre H2D printers ( in H2D always seen vir_slot)
            // Still, let's handle it just in case
            // documenting retroactively: seems like external tray state can change w/o vt_tray in message
            // then the state should be deduced from the tray_xxx
            // and with that also the removed tags
            for (extruder_id, external_tray_id) in [(0u32, 255), (1u32, 254)] {
                let new_vt_tray_detailed_ready_state = self.get_tray_detailed_ready_state(external_tray_id);
                let curr_vt_tray_detailed_ready_state = self.virt_trays()[extruder_id as usize].state;
                self.update_virt_tray(extruder_id, |tray| tray.state = new_vt_tray_detailed_ready_state);

                if curr_vt_tray_detailed_ready_state != TrayState::Empty && new_vt_tray_detailed_ready_state == TrayState::Empty {
                    let mut vt_tray = self.virt_trays()[extruder_id as usize].clone();
                    let spool_id = self.snapshot_slot_spool_id(external_tray_id as usize);
                    vt_tray.meta_info = TrayMetaInfo::default();
                    self.set_virt_tray(extruder_id, vt_tray);
                    self.clear_snapshot_slot_consumption(external_tray_id as usize);
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
            if let Some(prev_state) = prev_state
                && (self.ams_trays()[..] != prev_state.0 || self.virt_trays()[0] != prev_state.1)
            {
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

    #[allow(non_snake_case)]
    pub fn process_print_message__tray_xxx(&mut self, print: &PrintData) -> bool {
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
        let mut protected_tray_mask = 0u32;

        // first check which ams's exist
        if let Some(ams_exist_bits) = &ams.ams_exist_bits {
            let incoming_ams_exist_bits = u32::from_str_radix(ams_exist_bits, 16);
            if let Ok(incoming_ams_exist_bits) = incoming_ams_exist_bits {
                let (ams_exist_bits, protected_mask) = self.effective_startup_ams_exist_bits(incoming_ams_exist_bits);
                protected_tray_mask = protected_mask;
                if protected_tray_mask != 0 {
                    info!(
                        "[{}] Startup AMS guard preserving AMS topology: incoming={incoming_ams_exist_bits:#x}, effective={ams_exist_bits:#x}",
                        self.printer_number
                    );
                }
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
        if let Some(tray_exist_bits) = &ams.tray_exist_bits
            && let Ok(tray_exist_bits) = u32::from_str_radix(tray_exist_bits, 16)
        {
            let tray_exist_bits = Self::protect_startup_ams_guard_tray_bits(tray_exist_bits, prev_tray_exist_bits, protected_tray_mask);
            if *self.tray_exist_bits() != Some(tray_exist_bits) {
                self.set_tray_exist_bits(Some(tray_exist_bits));
                change_made = true;
            }
        }
        // tray_read_done - which trays (from those that exist) that have been "read" (meaning ready from ams perspective)
        if let Some(tray_read_done_bits) = &ams.tray_read_done_bits
            && let Ok(tray_read_done_bits) = u32::from_str_radix(tray_read_done_bits, 16)
        {
            let tray_read_done_bits =
                Self::protect_startup_ams_guard_tray_bits(tray_read_done_bits, *self.tray_read_done_bits(), protected_tray_mask);
            if *self.tray_read_done_bits() != Some(tray_read_done_bits) {
                self.set_tray_read_done_bits(Some(tray_read_done_bits));
                change_made = true;
            }
        }
        // tray_reading - which trays (from those that exist) that are currently being "read" (meaning ams is rotating them to get them ready)
        if let Some(tray_reading_bits) = &ams.tray_reading_bits
            && let Ok(tray_reading_bits) = u32::from_str_radix(tray_reading_bits, 16)
            && self.tray_reading_bits != Some(tray_reading_bits)
        {
            self.tray_reading_bits = Some(tray_reading_bits);
            change_made = true;
        }

        // IPORTANT Note: For now doesn't seem relevatnt to change_made, nor for persistent state
        if let Some(ams_units) = &ams.ams {
            for ams in ams_units {
                let ams_id = ams.id;
                let ams_index = match ams_id {
                    0..=3 => ams_id,
                    128..=135 => ams_id - 128 + 4,
                    254 | 255 => ams_id - 254 + 12,
                    _ => {
                        error!("[{}] Bad ams_id encountered: {ams_id}", self.printer_number);
                        continue;
                    }
                } as usize;
                if let Some(humidity) = ams.humidity {
                    let mut new_info = self.ams_info[ams_index].clone();
                    new_info.humidity = Some(-humidity);
                    if new_info != self.ams_info[ams_index] {
                        self.ams_info[ams_index] = new_info;
                        change_made = true;
                    }
                }
                if let Some(humidity_raw) = ams.humidity_raw {
                    let mut new_info = self.ams_info[ams_index].clone();
                    new_info.humidity = Some(humidity_raw);
                    if new_info != self.ams_info[ams_index] {
                        self.ams_info[ams_index] = new_info;
                        change_made = true;
                    }
                }
                if let Some(temp) = ams.temp {
                    let mut new_info = self.ams_info[ams_index].clone();
                    new_info.temp = Some(temp);
                    if new_info != self.ams_info[ams_index] {
                        self.ams_info[ams_index] = new_info;
                        change_made = true;
                    }
                }
                if let Some(ams_info) = ams.info {
                    let (ams_type, bound_extruders, bound_switcher_pos) = self.ams_info_from_protocol(ams_info);
                    let mut new_info = self.ams_info[ams_index].clone();
                    new_info.ams_type = ams_type;
                    new_info.bound_extruders = bound_extruders;
                    new_info.bound_switcher_pos = bound_switcher_pos;
                    if new_info != self.ams_info[ams_index] {
                        self.ams_info[ams_index] = new_info;
                        change_made = true;
                    }
                }
            }
            // The two external trays are fixed by Bambu extruder side, not by FTS inlet.
            for (index, extruder_id) in [(12, 1), (13, 0)] {
                let new_info = crate::bambu::AmsInfo::external(extruder_id);
                if self.ams_info[index] != new_info {
                    self.ams_info[index] = new_info;
                    change_made = true;
                }
            }
        }

        let mut removed_tags: HashMap<usize, SpoolId> = HashMap::new();

        let mut _derived_ams_exist_bits = 0u32;
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
                    let ams_bit_index = match ams_id {
                        0..=3 => ams_id,
                        128..=135 => ams_id - 128 + 4,
                        _ => continue,
                    };
                    _derived_ams_exist_bits |= 1u32 << ams_bit_index;
                    ams_data.tray.iter().find(|v| v.id == Some(ams_tray_id as u32))
                } else {
                    None
                }
            } else {
                None
            };

            if ((protected_tray_mask >> tray_id) & 0x01) != 0 && source_tray.is_none() {
                continue;
            }

            let new_tray = self.get_updated_tray(source_tray, tray_id as i32);
            if let Some(mut new_tray) = new_tray {
                change_made = true;
                self.swap_ams_tray(tray_id, &mut new_tray);

                if spool_removed {
                    let prev_spool_id = self.snapshot_slot_spool_id(tray_id);
                    self.clear_snapshot_slot_consumption(tray_id);
                    if let Some(prev_spool_id) = prev_spool_id {
                        // Before there was a tag and spool removed, add it to the list
                        removed_tags.insert(tray_id, prev_spool_id);
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

    pub fn process_print_message(&mut self, print: &PrintData) -> (bool, HashMap<usize, SpoolId>) {
        if self.startup_ams_guard_active() {
            self.maybe_release_startup_ams_guard(print.command.as_deref());
        }

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

        let mut change_made = self.process_print_message__fila_switch(print);
        let mut removed_tags = HashMap::new();
        let mut processed_specific_command = false;
        if let Some(command) = &print.command {
            processed_specific_command = true;
            if command == "ams_filament_setting" {
                change_made |= self.process_print_message__ams_filament_setting(print)
            } else if command == "extrusion_cali_set" || command == "extrusion_cali_del" {
                // trigger request command for cali_get (request, not response)
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    self.fetch_filament_calibrations(nozzle_diameter);
                }
                change_made = true;
            } else if command == "extrusion_cali_sel" {
                // update the tray with the new k factor
                change_made |= self.process_print_message__extrusion_cali_sel(print)
            } else if command == "extrusion_cali_get" {
                // TODO: Check: distinguish between command that was sent and the result, which are structured the same
                // here we want to process only the results (the one that includes the list of filaments )
                change_made |= self.process_print_message__extrusion_cali_get(print);
            } else if command == "project_file" {
                change_made |= self.process_print_message__project_file(print);
            } else {
                processed_specific_command = false;
            }
            if self.log_filter >= log::Level::Debug {
                debug!("[{}]    {command} message", &self.printer_number);
            }
        }
        if !processed_specific_command {
            let (common_change_made, common_removed_tags) = self.process_print_message__common(print);
            removed_tags = common_removed_tags;
            change_made |= common_change_made;
            if self.loaded_print_project.is_some()
                && let Some(gcode_state) = print.gcode_state
            {
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
                            self.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::DeletePrintProject);
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
                    self.queue_runtime_persistence_request(PrinterRuntimePersistenceRequestKind::DeletePrintProject);
                }
            }
        }
        (change_made, removed_tags)
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
    pub fn get_updated_tray(&mut self, tray_update: Option<&PrintTray>, tray_id: i32) -> Option<Tray> {
        let old_tray = self.get_any_tray(tray_id as usize);
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
                        if !matches!(self.use_ams_scan, UseAmsScan::Disabled) {
                            new_tray.meta_info.waiting_for_tag_uid = true;
                        }
                    }
                    if tray_read_done {
                        new_tray.state = self.get_tray_detailed_ready_state(tray_id);

                        #[allow(clippy::collapsible_if)]
                        if !matches!(self.use_ams_scan, UseAmsScan::Disabled) {
                            if new_tray.meta_info.waiting_for_tag_uid
                                && let Some(tray_update) = tray_update
                                && let Some(tag_uid) = &tray_update.tag_uid
                                && tag_uid.len() >= 8
                                && !tag_uid.starts_with("00000000")
                            {
                                let scanned_tag = &tag_uid[..8];
                                info!(
                                    "[{}] Tag {scanned_tag} scanned by {}",
                                    self.printer_number,
                                    self.full_slot_description(tray_id)
                                );
                                self.notify_tag_scanned(tray_id, scanned_tag, matches!(self.use_ams_scan, UseAmsScan::SpoolId));
                                new_tray.meta_info.waiting_for_tag_uid = false;
                            }
                        }
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
                    new_tray.meta_info = TrayMetaInfo::default(); // if spool is removed, reset Bambu-private tray metadata
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
                            // TODO: Need to think about the edge case of filament unknown but tag available (can for example be if removing filament information after loading tag)
                            new_tray.state = TrayState::Empty;
                            new_tray.meta_info = TrayMetaInfo::default();
                        } else {
                            new_tray.state = self.get_tray_detailed_ready_state(tray_id);
                            if old_tray.state != TrayState::Empty && new_tray.state == TrayState::Empty {
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

    // Done: TODO: External (2)
    #[allow(clippy::manual_map)]
    pub fn get_tray_index_from_print_msg(ams_id: Option<i32>, tray_id: Option<i32>, _slot_id: Option<i32>) -> Option<usize> {
        // returns either index into the ams_trays or 254/255 for external trays (other functions may depend on this)
        if let (Some(ams_id), Some(tray_id)) = (ams_id, tray_id) {
            canonical_tray_id_from_ams_slot(ams_id, tray_id).map(|tray_id| tray_id as usize)
        } else if let Some(tray_id) = tray_id {
            if tray_id == 254 {
                Some(255)
            } else {
                canonical_tray_id(tray_id).map(|tray_id| tray_id as usize)
            }
        } else {
            None
        }
    }
}

#[embassy_executor::task(pool_size = MAX_NUM_PRINTERS)]
pub async fn incoming_messages_task(read_packets: Rc<ReadPacketsPubSub>, bambu_printer: Rc<RefCell<BambuPrinter>>) {
    let mut subscriber = read_packets.subscriber().unwrap();
    const KEEP_ALIVE_SEC: u32 = 20;
    let printer_log_id = bambu_printer.borrow().printer_number;
    let log_level = bambu_printer.borrow().log_filter;

    let mut printer_known_to_be_up = false;
    let mut connectivity_probe_pending = false;
    loop {
        let wait_res = with_timeout(Duration::from_secs(KEEP_ALIVE_SEC as u64), subscriber.next_message_pure()).await;
        match wait_res {
            Ok(packet) => {
                printer_known_to_be_up = true;
                connectivity_probe_pending = false;
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
                            let parse_res = Box::new(serde_json::from_slice::<Message>(payload));

                            if log_level >= log::Level::Trace {
                                let cleaned_log = clean_message_bytes_to_log(payload);
                                if !cleaned_log.is_empty() {
                                    trace!("[{printer_log_id}] [Q:{}] [SIM] {cleaned_log}", subscriber.len());
                                }
                            }

                            if let Ok(message) = *parse_res {
                                if log_level >= log::Level::Trace {
                                    trace!("[{}] {:?}", printer_log_id, message);
                                }

                                match message {
                                    Message::Print(print) => {
                                        if log_level < log::Level::Trace && print.print.command.as_deref() == Some("project_file") {
                                            let cleaned_log = clean_message_bytes_to_log(payload);
                                            if !cleaned_log.is_empty() {
                                                trace!("[{printer_log_id}] [Q:{}] [SIM] {cleaned_log}", subscriber.len());
                                            }
                                        }
                                        let mut skip = false;
                                        if let Some(print_result) = &print.print.result
                                            && print_result == "fail"
                                        {
                                            if log_level >= log::Level::Warn {
                                                warn!("[{}] Printer reported an error message, ignoring message", printer_log_id);
                                                warn!("[{}] {:?}", printer_log_id, print);
                                            }
                                            skip = true;
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
                                    Message::Info(_info) => {}
                                }
                            } else if log_level >= log::Level::Debug {
                                if log_level >= log::Level::Trace {
                                    debug!("[{printer_log_id}] Previous message couldn't be parsed {parse_res:?}");
                                } else {
                                    let cleaned_log = clean_message_bytes_to_log(payload);
                                    if !cleaned_log.is_empty() {
                                        debug!("[{printer_log_id}] Unprocessed message {parse_res:?} : {cleaned_log:?}");
                                    }
                                }
                            }
                        }
                        mqttrust::Packet::Suback(mqttrust::encoding::v4::Suback { pid: _, return_codes: _ }) => {
                            // Subscribed, now time to request for update
                            let spawner = unsafe { embassy_executor::Spawner::for_current_executor().await };
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
                if connectivity_probe_pending {
                    printer_known_to_be_up = false;
                    connectivity_probe_pending = false;
                    if bambu_printer.borrow().printer_connectivity_ok == Some(false) {
                        continue;
                    }
                    if log_level >= log::Level::Warn {
                        warn!("[{}] Printer connectivity issues confirmed, reconnecting", printer_log_id);
                    }
                    let restart_printer = bambu_printer.borrow().restart_printer.clone();
                    bambu_printer.borrow_mut().report_printer_connectivity(false);
                    restart_printer.signal(0);
                } else if printer_known_to_be_up {
                    if log_level >= log::Level::Warn {
                        warn!("[{}] Printer connectivity issues suspected (uncertain), checking", printer_log_id);
                    }
                    BambuPrinter::request_full_update_async(&bambu_printer).await;
                    connectivity_probe_pending = true;
                    printer_known_to_be_up = false;
                }
            }
        }
    }
}
