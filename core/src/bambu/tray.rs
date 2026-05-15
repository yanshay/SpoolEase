use core::mem::swap;

use alloc::{format, string::String, vec::Vec};
use derivative::Derivative;
use framework::error;
use serde::{Deserialize, Serialize};

use crate::{
    bambu::{BambuPrinter, SpoolId, filament::Filament},
    printer::{MaterialSlotSnapshot, SlotId, slot_in_snapshot_mut},
    spool_record::SpoolRecord,
    tag_v1::TagInformationV1,
};

#[allow(dead_code)]
#[derive(Debug)]
pub struct TrayBits {
    pub tray_exist_bits: Option<u32>,
    pub tray_read_done_bits: Option<u32>,
    pub tray_reading_bits: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, Derivative)]
#[derivative(PartialEq)]
// IMPORTANT: Don't change names, will hurt persistence
//            When adding new fields include serde(default)
pub struct TrayMetaInfo {
    #[serde(rename = "tag_info", skip_serializing)]
    pub old_tag_info: Option<TagInformationV1>, // calibration for nozzles
    #[derivative(PartialEq = "ignore")]
    #[serde(skip)]
    #[serde(default)]
    pub waiting_for_tag_uid: bool,
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

impl BambuPrinter {
    fn slot_id_from_tray_index(index: usize) -> SlotId {
        SlotId::new(format!("bambu:{index}"))
    }

    fn update_snapshot_slot<F>(&self, index: usize, f: F) -> bool
    where
        F: FnOnce(&mut MaterialSlotSnapshot),
    {
        let Some(snapshot_state) = self.snapshot_state() else {
            return false;
        };
        let slot_id = Self::slot_id_from_tray_index(index);
        snapshot_state
            .try_update(true, |snapshot| {
                let Some(slot) = slot_in_snapshot_mut(snapshot, &slot_id) else {
                    return Err(());
                };
                f(slot);
                Ok(())
            })
            .is_ok()
    }

    pub fn snapshot_slot_spool_id(&self, index: usize) -> Option<SpoolId> {
        let snapshot_state = self.snapshot_state()?;
        let slot_id = Self::slot_id_from_tray_index(index);
        snapshot_state
            .clone_snapshot()
            .slot_groups
            .iter()
            .flat_map(|group| group.slots.iter())
            .find(|slot| slot.id == slot_id)
            .and_then(|slot| slot.spool_id.clone())
    }

    pub fn clear_snapshot_slot_consumption(&mut self, index: usize) {
        self.update_snapshot_slot(index, |slot| {
            slot.spool_id = None;
            slot.consumed_since_load_g = 0.0;
            slot.consumed_since_load_saved_g = 0.0;
            slot.consumed_since_weight_g = 0.0;
        });
    }

    pub fn unassign_snapshot_slot_spool(&mut self, index: usize) {
        self.update_snapshot_slot(index, |slot| {
            slot.spool_id = None;
        });
    }

    pub fn acknowledge_snapshot_slot_consumption_saved(&mut self, index: usize, consumed_since_load_saved_g: f32) {
        self.update_snapshot_slot(index, |slot| {
            slot.consumed_since_load_saved_g = consumed_since_load_saved_g;
        });
    }

    pub fn clear_snapshot_slots_used_in_print(&mut self) {
        let Some(snapshot_state) = self.snapshot_state() else {
            return;
        };
        snapshot_state.update(true, |snapshot| {
            for slot in snapshot.slot_groups.iter_mut().flat_map(|group| group.slots.iter_mut()) {
                slot.used_in_print = false;
            }
        });
    }

    pub fn set_snapshot_slot_used_in_print(&mut self, index: usize, used_in_print: bool) {
        self.update_snapshot_slot(index, |slot| {
            slot.used_in_print = used_in_print;
        });
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
            self.ams_trays_dirty[index] = true;
            // extra test because meta is excluded from partialeq for Tray
            if self.inner_ams_trays[index].meta_info != tray.meta_info {
                self.ams_trays_dirty[index] = true;
            }
        }
        // always swap, only dirty is conditional
        swap(&mut self.inner_ams_trays[index], tray);
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
        if tray != self.inner_virt_trays[extruder_id as usize] || tray.meta_info != self.inner_virt_trays[extruder_id as usize].meta_info {
            self.virt_trays_dirty = true;
        }
        // always set, condition above should be only for dirty test
        self.inner_virt_trays[extruder_id as usize] = tray;
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

    // returns 0..15, 128.. for AMS-HT, 254/255
    pub(crate) fn get_common_tray_active(active_extruder: usize, extruder_tray_now: i32) -> Option<i32> {
        if extruder_tray_now == 255 {
            None
        } else if extruder_tray_now == 254 {
            if active_extruder == 0 { Some(255) } else { Some(254) }
        } else {
            Some(extruder_tray_now)
        }
    }

    // returns 0..15, 128.. for AMS-HT, 254/255
    pub(crate) fn get_tray_active(&self) -> Option<i32> {
        let active_extruder = self.get_active_extruder()?;
        Self::get_common_tray_active(active_extruder, self.tray_now[active_extruder])
    }

    pub(crate) fn get_tray_detailed_ready_state(&self, tray_id: i32) -> TrayState {
        // tray_id: 0..15, 16.., 254, 255
        // To know if a tray is active is different between AMS and external tray.
        // For AMS - Loaded is when it is actually the active extruder, Ready is when filament available in AMS.
        //           We don't have a state for Loaded into extruder but not Printing, maybe should add such.
        //           So Loaded is when extruder is active and the tray is in the extruder
        //           Ready is when filament is available in AMS whether loaded in INACTIVE extruder or not.
        //
        // For external - there is no such thing as in AMS.
        //           In single extruder if the tray_now is 254 then it means it is loaded and ready, since once it is loaded it is the printing one, no other option
        //           In dual extruder if the relevant extruder snow is 254 then it is ready, if the extruder is active it is loaded
        //

        if tray_id != 254 && tray_id != 255 {
            if let Some(mut active_tray_id) = self.get_tray_active() {
                // tray_active: tray_xxx format (0..16, 128..135, 254, 255)
                if active_tray_id >= 128 {
                    // AMS-HT case
                    active_tray_id = active_tray_id - 128 + 16;
                }
                if active_tray_id == tray_id {
                    TrayState::Loaded
                } else {
                    TrayState::Ready
                }
            } else {
                TrayState::Ready
            }
        } else {
            // get external slot number of the relevant tray (each external slot means a different extruder)
            let extruder_id = self.get_extruder_id_for_tray(tray_id).unwrap() as usize;

            // get the tray_now of that extruder
            if self.tray_now[extruder_id] == 254 {
                // if that tray_now is 254, then it is at least ready (second option)
                if self.get_active_extruder() == Some(extruder_id) {
                    // if it is ready and the extruder is active then it is also loaded
                    TrayState::Loaded
                } else {
                    TrayState::Ready
                }
            } else {
                TrayState::Empty
            }
        }
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

    pub(crate) fn normalized_h2d_tray_xxx(h2d_tray_xxx: i32) -> i32 {
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

    pub(crate) fn update_h2d_tray_xxx(curr_tray_xxx: &mut i32, message_sxxx: &i32, changed: &mut bool) {
        let new_tray_xxx = Self::normalized_h2d_tray_xxx(*message_sxxx);
        if new_tray_xxx != *curr_tray_xxx {
            *curr_tray_xxx = new_tray_xxx;
            *changed = true;
        }
    }

    pub(crate) fn update_std_tray_xxx(curr_tray_xxx: &mut i32, message_tray_xxx: &Option<i32>, changed: &mut bool) {
        if let Some(new_tray_xxx) = message_tray_xxx
            && new_tray_xxx != curr_tray_xxx
        {
            *curr_tray_xxx = *new_tray_xxx;
            *changed = true;
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

    pub fn set_tray_spool_rec(&mut self, tray_id: usize, spool_rec: &SpoolRecord) {
        let spool_id = spool_rec.id.clone();
        let consumed_since_weight = spool_rec.consumed_since_weight;
        self.update_snapshot_slot(tray_id, |slot| {
            slot.spool_id = Some(spool_id.clone());
            slot.consumed_since_load_g = 0.0;
            slot.consumed_since_load_saved_g = 0.0;
            slot.consumed_since_weight_g = consumed_since_weight;
        });
    }

    pub fn full_slot_description(&self, tray_id: i32) -> String {
        let (ams_id, slot_in_ams) = Self::get_ams_and_slot_id(tray_id as usize);
        if ams_id <= 3 {
            format!("{} Slot {}", self.ams_name(ams_id), slot_in_ams + 1)
        } else {
            self.ams_name(ams_id)
        }
    }

    pub fn ams_name(&self, mut ams_id: usize) -> String {
        if (4..4 + 8).contains(&ams_id) {
            // deal with case of AMS_HT as index in ams list vs. bambu values of 128..
            ams_id = ams_id - 4 + 128;
        }
        if ams_id <= 3 {
            format!("AMS-{}", (b'A' + ams_id as u8) as char)
        } else if (128..128 + 8).contains(&ams_id) {
            format!("HT-{}", (b'A' + (ams_id - 128) as u8) as char)
        } else if ams_id == 255 {
            if self.num_extruders() == 1 {
                "External Spool".into()
            } else {
                "Right External Spool".into()
            }
        } else if ams_id == 254 {
            "Left External Spool".into()
        } else {
            format!("AMS-#{ams_id}?")
        }
    }
}
