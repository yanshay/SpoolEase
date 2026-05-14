use alloc::{
    format,
    rc::Weak,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use crate::app_config::FakePrinterConfig;

use super::{
    DriverData, ExtruderSnapshot, MaterialSlotSnapshot, PressureAdvanceCapability, PrintSnapshot, PrintState, PrinterCapabilities, PrinterChange,
    PrinterCommand, PrinterDiagnostic, PrinterDriver, PrinterDriverKind, PrinterError, PrinterFilament, PrinterFilamentInfo, PrinterId,
    PrinterObserver, PrinterResult, PrinterSnapshot, SlotAssignMode, SlotGroupKind, SlotGroupSnapshot, SlotId, SlotState,
};

pub struct FakePrinterDriver {
    id: PrinterId,
    name: String,
    slots: Vec<MaterialSlotSnapshot>,
    observers: Vec<Weak<RefCell<dyn PrinterObserver>>>,
}

impl FakePrinterDriver {
    pub fn new(name: Option<String>, config: &FakePrinterConfig) -> Self {
        let id = config
            .printer_id()
            .unwrap_or_else(|_| FakePrinterConfig::printer_id_for_unique_id("invalid"));
        let name = name.unwrap_or_else(|| format!("Fake Printer {}", config.unique_id));
        let slot_count = config.slot_count.clamp(1, 64);
        let slots = (0..slot_count)
            .map(|index| MaterialSlotSnapshot {
                id: SlotId::new(format!("fake:{index}")),
                display_name: format!("Slot {}", index + 1),
                state: SlotState::Empty,
                filament: PrinterFilament::Unknown,
                spool_id: None,
                consumed_since_load_g: 0.0,
                consumed_since_weight_g: 0.0,
                used_in_print: false,
                driver_data: DriverData::default(),
            })
            .collect();

        Self {
            id,
            name,
            slots,
            observers: Vec::new(),
        }
    }

    fn slot_mut(&mut self, slot_id: &SlotId) -> PrinterResult<&mut MaterialSlotSnapshot> {
        let raw_slot_id = slot_id.as_str().strip_prefix("fake:").unwrap_or(slot_id.as_str());
        let slot_index = raw_slot_id.parse::<usize>().map_err(|_| PrinterError::SlotNotFound(slot_id.clone()))?;
        self.slots.get_mut(slot_index).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))
    }

    fn notify_slots_changed(&mut self) {
        let event = super::PrinterEvent::SnapshotChanged {
            printer_id: self.id.clone(),
            change: PrinterChange::Slots,
        };
        self.observers.retain(|observer| {
            if let Some(observer) = observer.upgrade() {
                observer.borrow_mut().on_printer_event(event.clone());
                true
            } else {
                false
            }
        });
    }
}

impl PrinterDriver for FakePrinterDriver {
    fn id(&self) -> &PrinterId {
        &self.id
    }

    fn kind(&self) -> PrinterDriverKind {
        PrinterDriverKind::Fake
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    fn capabilities(&self) -> PrinterCapabilities {
        PrinterCapabilities {
            material_slot_read: true,
            material_slot_write: true,
            material_slot_assign: true,
            material_slot_set_spool_id: true,
            material_slot_clear: true,
            material_slot_unassign_spool: true,
            print_status_read: true,
            print_control: false,
            consumption_tracking: false,
            printer_tag_scan: false,
            print_file_fetch: false,
            persistent_slot_state: false,
            pressure_advance: PressureAdvanceCapability::Unsupported,
        }
    }

    fn snapshot(&self) -> PrinterSnapshot {
        PrinterSnapshot {
            id: self.id.clone(),
            kind: PrinterDriverKind::Fake,
            name: self.name.clone(),
            connected: true,
            capabilities: self.capabilities(),
            extruders: alloc::vec![ExtruderSnapshot {
                id: 0,
                name: "Toolhead".into(),
                active: true,
                loaded_slot_id: None,
                nozzle_diameter_mm: None,
                nozzle_type: None,
                temperature_c: None,
                target_temperature_c: None,
            }],
            slot_groups: alloc::vec![SlotGroupSnapshot {
                id: "fake:slots".into(),
                name: "Fake Slots".into(),
                kind: SlotGroupKind::Virtual,
                extruder: Some(0),
                temperature_c: None,
                humidity_percent: None,
                slots: self.slots.clone(),
            }],
            print: PrintSnapshot {
                state: PrintState::Idle,
                ..PrintSnapshot::default()
            },
            diagnostics: Vec::<PrinterDiagnostic>::new(),
        }
    }

    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        match command {
            PrinterCommand::Refresh => Ok(()),
            PrinterCommand::AssignMaterialToSlot { slot_id, spool, temps, mode } => {
                let slot = self.slot_mut(&slot_id)?;
                slot.spool_id = Some(spool.spool_rec.id.clone());
                if mode == SlotAssignMode::WritePrinterMaterial {
                    slot.state = SlotState::Ready;
                    slot.filament = PrinterFilament::Known(PrinterFilamentInfo {
                        material_type: spool.spool_rec.material_type.clone(),
                        material_subtype: spool.spool_rec.material_subtype.clone(),
                        brand: spool.spool_rec.brand.clone(),
                        color_name: spool.spool_rec.color_name.clone(),
                        color_codes: spool.spool_rec.color_code.clone(),
                        slicer_filament: spool.spool_rec.slicer_filament.clone(),
                        temps,
                    });
                }
                self.notify_slots_changed();
                Ok(())
            }
            PrinterCommand::ClearSlot { slot_id } => {
                let slot = self.slot_mut(&slot_id)?;
                slot.state = SlotState::Empty;
                slot.spool_id = None;
                slot.filament = PrinterFilament::Unknown;
                slot.consumed_since_load_g = 0.0;
                slot.consumed_since_weight_g = 0.0;
                slot.used_in_print = false;
                self.notify_slots_changed();
                Ok(())
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                self.slot_mut(&slot_id)?.spool_id = None;
                self.notify_slots_changed();
                Ok(())
            }
            PrinterCommand::PrintControl(_) => Err(PrinterError::UnsupportedCommand("print_control".to_string())),
            PrinterCommand::AddPressureAdvance(_) => Err(PrinterError::UnsupportedCommand("add_pressure_advance".to_string())),
            PrinterCommand::DriverSpecific(command) => Err(PrinterError::UnsupportedCommand(command.name)),
        }
    }

    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>) {
        self.observers.push(observer);
    }
}
