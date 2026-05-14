use alloc::{
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use crate::bambu::{
    BambuPrinter, NozzleType,
    bambu_api::{GcodeState, PrintCommand as BambuPrintCommand},
    filament::Filament as BambuFilament,
    tray::{Tray, TrayState as BambuTrayState},
};

use super::{
    DiagnosticSeverity, DriverData, DriverDataField, ExtruderSnapshot, FilamentTemps, MaterialSlotSnapshot, PressureAdvanceCapability,
    PrintControlCommand, PrintSnapshot, PrintState, PrinterCapabilities, PrinterCommand, PrinterDiagnostic, PrinterDriver, PrinterDriverKind,
    PrinterError, PrinterFilament, PrinterFilamentInfo, PrinterId, PrinterObserver, PrinterResult, PrinterSnapshot, SlotAssignMode, SlotGroupKind,
    SlotGroupSnapshot, SlotId, SlotState,
};

pub struct BambuPrinterDriver {
    id: PrinterId,
    printer: Rc<RefCell<BambuPrinter>>,
    observers: Vec<Weak<RefCell<dyn PrinterObserver>>>,
}

impl BambuPrinterDriver {
    pub fn new(printer: Rc<RefCell<BambuPrinter>>) -> Self {
        let id = PrinterId::new(format!("bambu:{}", printer.borrow().printer_serial));
        Self {
            id,
            printer,
            observers: Vec::new(),
        }
    }

    pub fn printer(&self) -> Rc<RefCell<BambuPrinter>> {
        self.printer.clone()
    }

    pub fn snapshot_from_printer(printer: &BambuPrinter) -> PrinterSnapshot {
        PrinterSnapshot {
            id: PrinterId::new(format!("bambu:{}", printer.printer_serial)),
            kind: PrinterDriverKind::Bambu,
            name: printer.printer_name().clone(),
            connected: printer.printer_connectivity_ok.unwrap_or_default(),
            capabilities: Self::capabilities_from_printer(printer),
            extruders: Self::extruders_from_printer(printer),
            slot_groups: Self::slot_groups_from_printer(printer),
            print: Self::print_from_printer(printer),
            diagnostics: Self::diagnostics_from_printer(printer),
        }
    }

    fn capabilities_from_printer(printer: &BambuPrinter) -> PrinterCapabilities {
        PrinterCapabilities {
            material_slot_read: true,
            material_slot_write: true,
            print_status_read: true,
            print_control: true,
            consumption_tracking: printer.track_print_consume,
            printer_tag_scan: true,
            print_file_fetch: true,
            persistent_slot_state: true,
            pressure_advance: PressureAdvanceCapability::DriverManaged,
        }
    }

    fn extruders_from_printer(printer: &BambuPrinter) -> Vec<ExtruderSnapshot> {
        let active_extruder = Self::active_extruder(printer);
        let active_slot_id = printer.get_tray_active().map(Self::slot_id_from_tray_id);
        let mut extruders = Vec::new();

        for extruder_id in 0..printer.num_extruders().min(2) {
            let extruder = printer.get_extruder(extruder_id);
            let active = active_extruder == Some(extruder_id as usize);
            extruders.push(ExtruderSnapshot {
                id: extruder.id,
                name: format!("Extruder {}", extruder.id + 1),
                active,
                loaded_slot_id: if active { active_slot_id.clone() } else { None },
                nozzle_diameter_mm: extruder.diameter.as_ref().and_then(|diameter| diameter.parse::<f32>().ok()),
                nozzle_type: extruder.nozzle_type_code().map(Self::nozzle_type_name),
                temperature_c: None,
                target_temperature_c: None,
            });
        }

        extruders
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
            name: printer.ams_name(tray_id as usize),
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

    fn slot_from_tray(printer: &BambuPrinter, tray_id: i32, tray: &Tray) -> MaterialSlotSnapshot {
        MaterialSlotSnapshot {
            id: Self::slot_id_from_tray_id(tray_id),
            display_name: printer.full_slot_description(tray_id),
            state: Self::slot_state_from_bambu(tray.state),
            filament: Self::filament_from_bambu(&tray.filament),
            spool_id: tray.meta_info.spool_id.clone(),
            consumed_since_load_g: tray.meta_info.consumed_since_load,
            consumed_since_weight_g: tray.meta_info.consumed_since_weight,
            used_in_print: tray.meta_info.used_in_print,
            driver_data: Self::driver_data_for_slot(printer, tray_id, tray),
        }
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
            active_slot_id: printer.get_tray_active().map(Self::slot_id_from_tray_id),
        }
    }

    fn diagnostics_from_printer(printer: &BambuPrinter) -> Vec<PrinterDiagnostic> {
        let mut diagnostics = Vec::new();

        if let Some(print_error) = printer.print_error {
            diagnostics.push(PrinterDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: Some(format!("print_error:{print_error}")),
                message: format!("Bambu print error {print_error}"),
            });
        }

        if let Some(hms_errors) = &printer.hms {
            for hms in hms_errors {
                let attr = hms.attr.unwrap_or_default();
                let code = hms.code.unwrap_or_default();
                diagnostics.push(PrinterDiagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: Some(format!("hms:{attr}:{code}")),
                    message: format!("Bambu HMS error {attr}:{code}"),
                });
            }
        }

        diagnostics
    }

    fn active_extruder(printer: &BambuPrinter) -> Option<usize> {
        let extruder_state = printer.extruder_state().as_ref().copied().unwrap_or_default();
        let extruder_index = (extruder_state >> 4 & 0xF) as usize;
        if extruder_index <= 1 { Some(extruder_index) } else { None }
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

    fn nozzle_type_name(nozzle_type: NozzleType) -> String {
        match nozzle_type {
            NozzleType::Standard => "standard".to_string(),
            NozzleType::HighFlow => "high_flow".to_string(),
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

    fn driver_data_for_slot(printer: &BambuPrinter, tray_id: i32, tray: &Tray) -> DriverData {
        let mut fields = Vec::new();
        fields.push(DriverDataField {
            key: "bambu_tray_id".to_string(),
            value: tray_id.to_string(),
        });
        fields.push(DriverDataField {
            key: "bambu_resolved_k".to_string(),
            value: printer.get_tray_resolved_k_value(tray, tray_id),
        });
        DriverData { fields }
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

    fn snapshot(&self) -> PrinterSnapshot {
        Self::snapshot_from_printer(&self.printer.borrow())
    }

    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        match command {
            PrinterCommand::Refresh => {
                self.printer.borrow_mut().request_full_update_sync();
                Ok(())
            }
            PrinterCommand::PrintControl(command) => {
                self.printer.borrow_mut().request_printer_command_sync(match command {
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
                        self.printer.borrow_mut().set_tray_spool_rec(tray_id as usize, &spool.spool_rec);
                        Ok(())
                    }
                    SlotAssignMode::WritePrinterMaterial => self
                        .printer
                        .borrow_mut()
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
                self.printer.borrow_mut().reset_tray(tray_id);
                Ok(())
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                let tray_id = Self::tray_id_from_slot_id(&slot_id)?;
                self.printer.borrow_mut().update_any_tray(tray_id as usize, |tray| {
                    tray.meta_info.spool_id = None;
                });
                Ok(())
            }
            PrinterCommand::AddPressureAdvance(_profile) => Err(PrinterError::UnsupportedCommand("add_pressure_advance".to_string())),
            PrinterCommand::DriverSpecific(command) => Err(PrinterError::UnsupportedCommand(command.name)),
        }
    }

    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>) {
        self.observers.push(observer);
    }
}
