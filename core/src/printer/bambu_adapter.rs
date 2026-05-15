use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;
use framework::error;
use hashbrown::HashMap;
use serde_json::Value;

use crate::app_config::BambuPrinterConfig;
use crate::bambu::{
    BambuPrinter, BambuPrinterObserver, SpoolId,
    bambu_api::{GcodeState, PrintCommand as BambuPrintCommand},
    bambu_print::PrintProject,
    filament::Filament as BambuFilament,
    printer_state::BambuPersistentDirtyState,
    tray::{Tray, TrayBits, TrayState as BambuTrayState},
};
use crate::store::Store;

use super::{
    FilamentTemps, MaterialSlotPresenceChange, MaterialSlotPresenceChangeKind, MaterialSlotSnapshot, PressureAdvanceCapability, PrintControlCommand,
    PrintSnapshot, PrintState, PrinterCapabilities, PrinterChange, PrinterCommand, PrinterDriver, PrinterDriverKind, PrinterError, PrinterEvent,
    PrinterEventKind, PrinterFilament, PrinterFilamentInfo, PrinterId, PrinterObserver, PrinterResult, PrinterSnapshot, PrinterSnapshotState,
    PrinterSnapshotStateInner, SlotAssignMode, SlotGroupKind, SlotGroupSnapshot, SlotId, SlotState, slot_in_snapshot_mut,
};

type PrinterObserverList = Rc<RefCell<Vec<Weak<RefCell<dyn PrinterObserver>>>>>;

pub struct BambuPrinterDriver {
    id: PrinterId,
    printer: Rc<RefCell<BambuPrinter>>,
    snapshot_state: PrinterSnapshotState,
    observers: PrinterObserverList,
    bridge_observer: Option<Rc<RefCell<dyn BambuPrinterObserver>>>,
    pending_dirty_state: Option<BambuPersistentDirtyState>,
}

struct BambuPrinterEventBridge {
    printer_id: PrinterId,
    snapshot_state: PrinterSnapshotState,
    observers: PrinterObserverList,
}

impl BambuPrinterDriver {
    pub fn new(printer: Rc<RefCell<BambuPrinter>>) -> Self {
        let mut printer_borrow = printer.borrow_mut();
        let id = BambuPrinterConfig::printer_id_for_serial(&printer_borrow.printer_serial);
        let snapshot_state = Rc::new(PrinterSnapshotStateInner::new(Self::raw_snapshot_from_printer(&printer_borrow)));
        printer_borrow.set_snapshot_state(snapshot_state.clone());
        drop(printer_borrow);
        Self {
            id,
            printer,
            snapshot_state,
            observers: Rc::new(RefCell::new(Vec::new())),
            bridge_observer: None,
            pending_dirty_state: None,
        }
    }

    pub fn printer(&self) -> Rc<RefCell<BambuPrinter>> {
        self.printer.clone()
    }

    pub fn snapshot_from_printer(printer: &BambuPrinter) -> PrinterSnapshot {
        if let Some(snapshot_state) = printer.snapshot_state() {
            return Self::sync_snapshot_state_from_printer(printer, &snapshot_state);
        }

        Self::raw_snapshot_from_printer(printer)
    }

    fn raw_snapshot_from_printer(printer: &BambuPrinter) -> PrinterSnapshot {
        PrinterSnapshot {
            id: BambuPrinterConfig::printer_id_for_serial(&printer.printer_serial),
            kind: PrinterDriverKind::Bambu,
            name: printer.printer_name().clone(),
            connected: printer.printer_connectivity_ok.unwrap_or_default(),
            num_extruders: printer.num_extruders(),
            slot_groups: Self::slot_groups_from_printer(printer),
            print: Self::print_from_printer(printer),
        }
    }

    fn sync_snapshot_state_from_printer(printer: &BambuPrinter, snapshot_state: &PrinterSnapshotState) -> PrinterSnapshot {
        let mut snapshot = Self::raw_snapshot_from_printer(printer);
        Self::overlay_generic_fields_from_state(&mut snapshot, &snapshot_state.clone_snapshot());
        snapshot_state.replace(snapshot.clone(), false);
        snapshot
    }

    fn overlay_generic_fields_from_state(snapshot: &mut PrinterSnapshot, state: &PrinterSnapshot) {
        for slot in snapshot.slot_groups.iter_mut().flat_map(|group| group.slots.iter_mut()) {
            let Some(state_slot) = state
                .slot_groups
                .iter()
                .flat_map(|group| group.slots.iter())
                .find(|state_slot| state_slot.id == slot.id)
            else {
                continue;
            };
            slot.spool_id = state_slot.spool_id.clone();
            slot.consumed_since_load_g = state_slot.consumed_since_load_g;
            slot.consumed_since_load_saved_g = state_slot.consumed_since_load_saved_g;
            slot.consumed_since_weight_g = state_slot.consumed_since_weight_g;
            slot.used_in_print = state_slot.used_in_print;
        }
    }

    pub fn dispatch_to_printer(printer: &mut BambuPrinter, command: PrinterCommand) -> PrinterResult<()> {
        match command {
            PrinterCommand::Refresh => {
                printer.request_full_update_sync();
                Ok(())
            }
            PrinterCommand::PrintControl(command) => {
                printer.request_printer_command_sync(match command {
                    PrintControlCommand::Pause => BambuPrintCommand::Pause,
                    PrintControlCommand::Resume => BambuPrintCommand::Resume,
                    PrintControlCommand::Stop => BambuPrintCommand::Stop,
                });
                Ok(())
            }
            PrinterCommand::AssignMaterialToSlot { slot_id, spool, temps, mode } => {
                let tray_id = Self::tray_id_from_slot_id(&slot_id)?;
                match mode {
                    SlotAssignMode::SpoolIdOnly => {
                        printer.set_tray_spool_rec(tray_id as usize, &spool.spool_rec);
                        Ok(())
                    }
                    SlotAssignMode::WritePrinterMaterial => printer
                        .set_tray_filament(
                            tray_id,
                            &spool,
                            temps.nozzle_min_c.unwrap_or_default(),
                            temps.nozzle_max_c.unwrap_or_default(),
                        )
                        .map_err(PrinterError::DriverError),
                }
            }
            PrinterCommand::ClearSlot { slot_id } => {
                let tray_id = Self::tray_id_from_slot_id(&slot_id)?;
                printer.reset_tray(tray_id);
                printer.clear_snapshot_slot_consumption(tray_id as usize);
                Ok(())
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                let tray_id = Self::tray_id_from_slot_id(&slot_id)?;
                printer.unassign_snapshot_slot_spool(tray_id as usize);
                Ok(())
            }
            PrinterCommand::AddPressureAdvance(_profile) => Err(PrinterError::UnsupportedCommand("add_pressure_advance".to_string())),
            PrinterCommand::DriverSpecific(command) => Err(PrinterError::UnsupportedCommand(command.name)),
        }
    }

    fn capabilities_from_printer(printer: &BambuPrinter) -> PrinterCapabilities {
        PrinterCapabilities {
            material_slot_read: true,
            material_slot_write: true,
            material_slot_assign: true,
            material_slot_set_spool_id: true,
            material_slot_clear: true,
            material_slot_unassign_spool: true,
            material_slot_presence_notify: true,
            print_status_read: true,
            print_control: true,
            consumption_tracking: printer.track_print_consume,
            printer_tag_scan: true,
            print_file_fetch: true,
            persistent_slot_state: true,
            pressure_advance: PressureAdvanceCapability::DriverManaged,
        }
    }

    fn slot_groups_from_printer(printer: &BambuPrinter) -> Vec<SlotGroupSnapshot> {
        let mut groups = Vec::new();

        if let Some(ams_exist_bits) = *printer.ams_exist_bits() {
            for ams_index in Self::ams_list(ams_exist_bits) {
                let ams_info = printer.ams_info.get(ams_index as usize);
                let mut slots = Vec::new();
                let (slots_offset, num_slots) = if ams_index <= 3 {
                    (ams_index as usize * 4, 4)
                } else {
                    (16 + (ams_index - 4) as usize, 1)
                };

                for tray_id in slots_offset..slots_offset + num_slots {
                    if let Some(tray) = printer.ams_trays().get(tray_id) {
                        slots.push(Self::slot_from_tray(printer, tray_id as i32, tray));
                    }
                }

                groups.push(SlotGroupSnapshot {
                    id: format!("bambu:group:{ams_index}"),
                    name: printer.ams_name(ams_index as usize),
                    short_name: printer.ams_name(ams_index as usize),
                    kind: SlotGroupKind::InternalChanger,
                    extruder: ams_info.map(|info| info.extruder),
                    temperature_c: ams_info.and_then(|info| info.temp),
                    humidity_percent: ams_info.and_then(|info| info.humidity),
                    slots,
                });
            }
        }

        groups.push(Self::external_group(printer, 255, 0));
        if printer.num_extruders() == 2 {
            groups.push(Self::external_group(printer, 254, 1));
        }

        groups
    }

    fn external_group(printer: &BambuPrinter, tray_id: i32, extruder: u32) -> SlotGroupSnapshot {
        SlotGroupSnapshot {
            id: format!("bambu:external:{tray_id}"),
            name: Self::external_group_name(printer, extruder),
            short_name: Self::external_group_short_name(printer, extruder),
            kind: SlotGroupKind::External,
            extruder: Some(extruder),
            temperature_c: None,
            humidity_percent: None,
            slots: {
                let mut slots = Vec::new();
                slots.push(Self::slot_from_tray(printer, tray_id, printer.get_any_tray(tray_id as usize)));
                slots
            },
        }
    }

    fn external_group_name(printer: &BambuPrinter, extruder: u32) -> String {
        if printer.num_extruders() == 1 {
            "External".into()
        } else if extruder == 1 {
            "Ext Left".into()
        } else {
            "Ext Right".into()
        }
    }

    fn external_group_short_name(printer: &BambuPrinter, extruder: u32) -> String {
        if printer.num_extruders() == 1 {
            "Ext".into()
        } else if extruder == 1 {
            "Ext-L".into()
        } else {
            "Ext-R".into()
        }
    }

    fn slot_from_tray(printer: &BambuPrinter, tray_id: i32, tray: &Tray) -> MaterialSlotSnapshot {
        let (pressure_advance_value, pressure_advance_meta) = Self::pressure_advance_from_tray(printer, tray_id, tray);
        MaterialSlotSnapshot {
            id: Self::slot_id_from_tray_id(tray_id),
            display_name: printer.full_slot_description(tray_id),
            short_name: Self::slot_short_name_from_tray_id(tray_id),
            state: Self::slot_state_from_bambu(tray.state),
            filament: Self::filament_from_bambu(&tray.filament),
            spool_id: None,
            consumed_since_load_g: 0.0,
            consumed_since_load_saved_g: 0.0,
            consumed_since_weight_g: 0.0,
            used_in_print: false,
            pressure_advance_value,
            pressure_advance_meta,
        }
    }

    fn slot_short_name_from_tray_id(tray_id: i32) -> String {
        match tray_id {
            255 => "Ext-R".to_string(),
            254 => "Ext-L".to_string(),
            0..=15 => {
                let ams_letter = (b'A' + (tray_id / 4) as u8) as char;
                format!("{ams_letter}{}", tray_id % 4 + 1)
            }
            16..=23 => format!("HT-{}", tray_id - 15),
            _ => tray_id.to_string(),
        }
    }

    fn pressure_advance_from_tray(printer: &BambuPrinter, tray_id: i32, tray: &Tray) -> (String, String) {
        let value = printer.get_tray_resolved_k_value(tray, tray_id);
        let meta = Self::pressure_advance_meta_from_tray(printer, tray_id, tray).unwrap_or_default();
        (value, meta)
    }

    fn pressure_advance_meta_from_tray(printer: &BambuPrinter, tray_id: i32, tray: &Tray) -> Option<String> {
        let cali_idx = tray.cali_idx?;
        if cali_idx == -1 || cali_idx == 0 {
            return None;
        }
        let extruder = printer.get_extruder_for_tray(tray_id).ok()?;
        let nozzle_diameter = extruder.diameter.as_deref()?;
        let nozzle_type = extruder.nozzle_type_code()?;

        printer
            .calibrations
            .iter()
            .find(|calibration| {
                calibration.extruder == extruder.id as i32
                    && calibration.diameter == nozzle_diameter
                    && calibration.nozzle_type_code() == nozzle_type
                    && calibration.cali_idx == cali_idx
            })
            .map(|calibration| calibration.name.clone())
    }

    fn print_from_printer(printer: &BambuPrinter) -> PrintSnapshot {
        PrintSnapshot {
            state: Self::print_state_from_bambu(printer.gcode_state),
            job_name: printer.subtask_name.clone(),
            progress_percent: match printer.gcode_state {
                GcodeState::PREPARE => Self::percent(printer.gcode_file_prepare_percent),
                GcodeState::RUNNING | GcodeState::PAUSE => Self::percent(printer.mc_percent),
                _ => None,
            },
            remaining_minutes: Self::non_negative_u32(printer.mc_remaining_time),
            current_layer: Self::non_negative_u32(printer.layer_num),
            total_layers: Self::non_negative_u32(printer.total_layer_num),
        }
    }

    fn ams_list(mut ams_exist_bits: u32) -> Vec<i32> {
        let mut ams_ids = Vec::new();
        for ams_id in 0..=11 {
            if ams_exist_bits & 1 != 0 {
                ams_ids.push(ams_id);
            }
            ams_exist_bits >>= 1;
        }
        ams_ids
    }

    fn slot_id_from_tray_id(tray_id: i32) -> SlotId {
        SlotId::new(format!("bambu:{tray_id}"))
    }

    fn tray_id_from_slot_id(slot_id: &SlotId) -> PrinterResult<i32> {
        let raw_slot_id = slot_id.as_str().strip_prefix("bambu:").unwrap_or(slot_id.as_str());
        let tray_id = raw_slot_id
            .parse::<i32>()
            .map_err(|_| PrinterError::InvalidCommand(format!("Invalid Bambu slot id: {}", slot_id.as_str())))?;

        if (0..=23).contains(&tray_id) || tray_id == 254 || tray_id == 255 {
            Ok(tray_id)
        } else {
            Err(PrinterError::SlotNotFound(slot_id.clone()))
        }
    }

    fn slot_state_from_bambu(state: BambuTrayState) -> SlotState {
        match state {
            BambuTrayState::Unknown => SlotState::Unknown,
            BambuTrayState::Empty => SlotState::Empty,
            BambuTrayState::Spool => SlotState::Occupied,
            BambuTrayState::Reading => SlotState::Reading,
            BambuTrayState::Ready => SlotState::Ready,
            BambuTrayState::Loading => SlotState::Loading,
            BambuTrayState::Unloading => SlotState::Unloading,
            BambuTrayState::Loaded => SlotState::Loaded,
        }
    }

    fn filament_from_bambu(filament: &BambuFilament) -> PrinterFilament {
        match filament {
            BambuFilament::Unknown => PrinterFilament::Unknown,
            BambuFilament::Known(filament_info) => PrinterFilament::Known(PrinterFilamentInfo {
                material_type: filament_info.tray_type.clone(),
                material_subtype: String::new(),
                brand: String::new(),
                color_name: String::new(),
                color_codes: filament_info.tray_color.clone(),
                slicer_filament: filament_info.tray_info_idx.clone(),
                temps: FilamentTemps {
                    nozzle_min_c: Self::non_zero_u32(filament_info.nozzle_temp_min),
                    nozzle_max_c: Self::non_zero_u32(filament_info.nozzle_temp_max),
                },
            }),
        }
    }

    fn print_state_from_bambu(state: GcodeState) -> PrintState {
        match state {
            GcodeState::Unknown | GcodeState::Unsupported => PrintState::Unknown,
            GcodeState::IDLE => PrintState::Idle,
            GcodeState::SLICING => PrintState::Slicing,
            GcodeState::PREPARE => PrintState::Preparing,
            GcodeState::RUNNING => PrintState::Printing,
            GcodeState::FINISH => PrintState::Finished,
            GcodeState::FAILED => PrintState::Failed,
            GcodeState::PAUSE => PrintState::Paused,
        }
    }

    fn non_zero_u32(value: u32) -> Option<u32> {
        if value == 0 { None } else { Some(value) }
    }

    fn non_negative_u32(value: Option<i32>) -> Option<u32> {
        value.and_then(|value| if value >= 0 { Some(value as u32) } else { None })
    }

    fn percent(value: Option<i32>) -> Option<u8> {
        value.and_then(|value| if (0..=100).contains(&value) { Some(value as u8) } else { None })
    }

    fn notify_observers(observers: &PrinterObserverList, event: PrinterEvent) {
        let observer_list = observers.borrow().clone();
        let mut has_dead_observer = false;

        for weak_observer in observer_list {
            if let Some(observer) = weak_observer.upgrade() {
                observer.borrow_mut().on_printer_event(event.clone());
            } else {
                has_dead_observer = true;
            }
        }

        if has_dead_observer {
            observers.borrow_mut().retain(|observer| observer.upgrade().is_some());
        }
    }
}

impl BambuPrinterEventBridge {
    fn notify(&self, kind: PrinterEventKind) {
        BambuPrinterDriver::notify_observers(
            &self.observers,
            PrinterEvent {
                printer_id: self.printer_id.clone(),
                kind,
            },
        );
    }
}

impl BambuPrinterObserver for BambuPrinterEventBridge {
    fn on_trays_update(
        &mut self,
        bambu_printer: &mut BambuPrinter,
        prev_tray_bits: &TrayBits,
        new_tray_bits: &TrayBits,
        removed_tags: &HashMap<usize, SpoolId>,
    ) {
        self.notify(PrinterEventKind::SnapshotChanged {
            change: PrinterChange::Slots,
            snapshot: Box::new(BambuPrinterDriver::sync_snapshot_state_from_printer(bambu_printer, &self.snapshot_state)),
        });

        let mut changes = Vec::new();
        if let Some(new_tray_exist_bits) = new_tray_bits.tray_exist_bits {
            let prev_tray_exist_bits = prev_tray_bits.tray_exist_bits.unwrap_or_default();
            for tray_id in 0..bambu_printer.ams_trays().len() {
                let prev_exists = ((prev_tray_exist_bits >> tray_id) & 0x01) != 0;
                let new_exists = ((new_tray_exist_bits >> tray_id) & 0x01) != 0;
                if !prev_exists && new_exists {
                    changes.push(MaterialSlotPresenceChange {
                        slot_id: BambuPrinterDriver::slot_id_from_tray_id(tray_id as i32),
                        change: MaterialSlotPresenceChangeKind::Inserted,
                        spool_id: bambu_printer.snapshot_slot_spool_id(tray_id),
                    });
                }
            }
        }

        for (tray_id, spool_id) in removed_tags {
            changes.push(MaterialSlotPresenceChange {
                slot_id: BambuPrinterDriver::slot_id_from_tray_id(*tray_id as i32),
                change: MaterialSlotPresenceChangeKind::Removed,
                spool_id: Some(spool_id.clone()),
            });
        }

        if !changes.is_empty() {
            self.notify(PrinterEventKind::MaterialSlotPresenceChanged { changes });
        }
    }

    fn on_printer_connect_status(&self, _bambu_printer: &mut BambuPrinter, status: bool) {
        self.notify(PrinterEventKind::ConnectivityChanged { connected: status });
    }

    fn on_request_gcode_analysis(&mut self, _bambu_printer: &mut BambuPrinter, _print_project: &PrintProject) -> i32 {
        0
    }

    fn on_cancel_gcode_analysis(&mut self, _job_number: i32) {}

    fn on_tag_scanned(&self, _printer_index: usize, tray_id: i32, tag_id: &str, only_spool_id: bool) {
        self.notify(PrinterEventKind::SlotTagScanned {
            slot_id: SlotId::new(format!("bambu:{tray_id}")),
            tag_id: tag_id.to_string(),
            only_spool_id,
        });
    }

    fn on_slot_consumption_reported(&mut self, _printer_index: usize, tray_id: i32, grams: f32) {
        if grams == 0.0 {
            return;
        }
        if grams < 0.0 || !grams.is_finite() {
            error!("Invalid consumption amount from Bambu tray {tray_id}: {grams}");
            return;
        }

        let slot_id = BambuPrinterDriver::slot_id_from_tray_id(tray_id);
        if self
            .snapshot_state
            .try_update(true, |snapshot| {
                let Some(slot) = slot_in_snapshot_mut(snapshot, &slot_id) else {
                    return Err(());
                };
                slot.consumed_since_load_g += grams;
                slot.consumed_since_weight_g += grams;
                Ok(())
            })
            .is_err()
        {
            error!("Missing snapshot slot for consumed Bambu tray {tray_id}");
        }
    }
}

impl PrinterDriver for BambuPrinterDriver {
    fn id(&self) -> &PrinterId {
        &self.id
    }

    fn kind(&self) -> PrinterDriverKind {
        PrinterDriverKind::Bambu
    }

    fn display_name(&self) -> String {
        self.printer.borrow().printer_name().clone()
    }

    fn capabilities(&self) -> PrinterCapabilities {
        Self::capabilities_from_printer(&self.printer.borrow())
    }

    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.snapshot_state.clone()
    }

    fn snapshot(&self) -> PrinterSnapshot {
        Self::sync_snapshot_state_from_printer(&self.printer.borrow(), &self.snapshot_state)
    }

    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        let mut printer = self.printer.borrow_mut();
        Self::dispatch_to_printer(&mut printer, command)
    }

    fn acknowledge_slot_consumption_saved(&mut self, slot_id: &SlotId, consumed_since_load_saved_g: f32) -> PrinterResult<()> {
        let tray_id = Self::tray_id_from_slot_id(slot_id)?;
        self.printer
            .borrow_mut()
            .acknowledge_snapshot_slot_consumption_saved(tray_id as usize, consumed_since_load_saved_g);
        Ok(())
    }

    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>) {
        self.observers.borrow_mut().push(observer);
    }

    fn start(&mut self, _framework: Rc<RefCell<framework::framework::Framework>>) {
        if self.bridge_observer.is_some() {
            return;
        }

        let bridge_observer: Rc<RefCell<dyn BambuPrinterObserver>> = Rc::new(RefCell::new(BambuPrinterEventBridge {
            printer_id: self.id.clone(),
            snapshot_state: self.snapshot_state.clone(),
            observers: self.observers.clone(),
        }));
        let bridge_observer_weak = Rc::downgrade(&bridge_observer);
        self.printer.borrow_mut().subscribe(bridge_observer_weak);
        self.bridge_observer = Some(bridge_observer);
    }

    fn persistent_state_path(&self) -> Option<String> {
        let printer = self.printer.borrow();
        if printer.dummy_printer() {
            None
        } else if printer.printer_name().to_lowercase() == "simulator" {
            // None
            Some(BambuPrinter::printer_state_file_path(&printer.printer_serial))
        } else {
            Some(BambuPrinter::printer_state_file_path(&printer.printer_serial))
        }
    }

    fn private_state_dirty(&self) -> bool {
        let printer = self.printer.borrow();
        printer.printer_persistent_state_store_blocked() || printer.printer_persistent_state_dirty()
    }

    fn load_private_state(&mut self, state: Option<Value>, store: &Rc<Store>) -> Result<(), String> {
        let Some(state) = state else {
            return Ok(());
        };
        self.printer.borrow_mut().load_printer_private_state_value(state, store)
    }

    fn prepare_private_state_store(&mut self) -> Result<Option<Value>, String> {
        let mut printer = self.printer.borrow_mut();

        if printer.dummy_printer() {
            return Ok(None);
        } else if printer.printer_name().to_lowercase() == "simulator" {
            // return Ok(None);
        }
        let Some((state, dirty_state)) = printer.prepare_printer_private_state_store()? else {
            return Ok(None);
        };
        self.pending_dirty_state = dirty_state;
        Ok(Some(state))
    }

    fn private_state_store_succeeded(&mut self) {
        self.pending_dirty_state = None;
    }

    fn restore_private_state_after_failed_store(&mut self) {
        if let Some(dirty_state) = self.pending_dirty_state.take() {
            self.printer.borrow_mut().restore_printer_persistent_dirty_state(dirty_state);
        }
    }
}
