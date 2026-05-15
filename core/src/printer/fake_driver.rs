use alloc::{
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use crate::app_config::FakePrinterConfig;
use crate::store::Store;
use framework::debug;

use super::{
    DriverData, ExtruderSnapshot, GenericPrinterPersistentState, GenericSlotPersistentState, MaterialSlotSnapshot, PressureAdvanceCapability,
    PrintSnapshot, PrintState, PrinterCapabilities, PrinterChange, PrinterCommand, PrinterDiagnostic, PrinterDriver, PrinterDriverKind,
    PrinterError, PrinterFilament, PrinterFilamentInfo, PrinterId, PrinterObserver, PrinterPersistentStatePayload, PrinterResult, PrinterSnapshot,
    SlotAssignMode, SlotGroupKind, SlotGroupSnapshot, SlotId, SlotState,
};

pub struct FakePrinterDriver {
    id: PrinterId,
    name: String,
    state_path: String,
    state_dirty: bool,
    state_dirty_reasons: Vec<String>,
    slots: Vec<MaterialSlotSnapshot>,
    observers: Vec<Weak<RefCell<dyn PrinterObserver>>>,
}

impl FakePrinterDriver {
    pub fn new(name: Option<String>, config: &FakePrinterConfig) -> Self {
        let id = config
            .printer_id()
            .unwrap_or_else(|_| FakePrinterConfig::printer_id_for_unique_id("invalid"));
        let name = name.unwrap_or_else(|| format!("Fake Printer {}", config.unique_id));
        let state_path = Self::state_path_for_printer_id(&id);
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
            state_path,
            state_dirty: false,
            state_dirty_reasons: Vec::new(),
            slots,
            observers: Vec::new(),
        }
    }

    fn state_path_for_printer_id(printer_id: &PrinterId) -> String {
        format!("/state/{}.fak/startup.jsn", Self::short_state_basename(printer_id.as_str()))
    }

    fn short_state_basename(input: &str) -> String {
        const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut hash = 0xcbf29ce484222325u64;
        for byte in input.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }

        let mut name = String::new();
        for shift in (0..8).rev() {
            let index = ((hash >> (shift * 5)) & 0x1f) as usize;
            name.push(ALPHABET[index] as char);
        }
        name
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

    fn mark_state_dirty(&mut self, reason: impl ToString) {
        let reason = reason.to_string();
        self.state_dirty = true;
        if !self.state_dirty_reasons.iter().any(|existing| existing == &reason) {
            self.state_dirty_reasons.push(reason);
        }
    }

    fn persistent_slot_state(slot: &MaterialSlotSnapshot) -> GenericSlotPersistentState {
        GenericSlotPersistentState {
            slot_id: slot.id.clone(),
            state: slot.state,
            filament: slot.filament.clone(),
            spool_id: slot.spool_id.clone(),
            consumed_since_load_g: slot.consumed_since_load_g,
            consumed_since_weight_g: slot.consumed_since_weight_g,
            used_in_print: slot.used_in_print,
        }
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
            persistent_slot_state: true,
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
                self.mark_state_dirty(format!("assign slot {}", slot_id.as_str()));
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
                self.mark_state_dirty(format!("clear slot {}", slot_id.as_str()));
                self.notify_slots_changed();
                Ok(())
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                self.slot_mut(&slot_id)?.spool_id = None;
                self.mark_state_dirty(format!("unassign spool from slot {}", slot_id.as_str()));
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

    fn persistent_state_path(&self) -> Option<String> {
        Some(self.state_path.clone())
    }

    fn load_persistent_state(&mut self, state_json: &str, store: &Rc<Store>) -> Result<(), String> {
        let state = serde_json::from_str::<GenericPrinterPersistentState>(state_json)
            .map_err(|err| format!("Failed to parse fake printer state: {err}"))?;
        if state.version != 1 {
            return Err(format!("Unsupported fake printer state version {}", state.version));
        }
        if state.printer_id != self.id {
            return Err(format!(
                "State file belongs to {}, not {}",
                state.printer_id.as_str(),
                self.id.as_str()
            ));
        }
        if state.driver_kind != PrinterDriverKind::Fake {
            return Err(format!("State file has unexpected driver kind {:?}", state.driver_kind));
        }

        for stored_slot in state.slots {
            let missing_spool = stored_slot
                .spool_id
                .as_ref()
                .is_some_and(|spool_id| store.get_spool_by_id(spool_id).is_none());
            let Ok(slot) = self.slot_mut(&stored_slot.slot_id) else {
                continue;
            };
            slot.state = stored_slot.state;
            slot.filament = stored_slot.filament;
            slot.spool_id = if missing_spool { None } else { stored_slot.spool_id };
            slot.consumed_since_load_g = stored_slot.consumed_since_load_g;
            slot.consumed_since_weight_g = stored_slot.consumed_since_weight_g;
            slot.used_in_print = stored_slot.used_in_print;
            if missing_spool {
                self.mark_state_dirty(format!("removed missing spool from slot {}", stored_slot.slot_id.as_str()));
            }
        }
        Ok(())
    }

    fn prepare_persistent_state_store(&mut self) -> Result<Option<PrinterPersistentStatePayload>, String> {
        if !self.state_dirty {
            return Ok(None);
        }

        debug!(
            "[{}] Dirty status: Fake slots({}), Reasons({})",
            self.name,
            self.state_dirty,
            if self.state_dirty_reasons.is_empty() {
                "unknown".to_string()
            } else {
                self.state_dirty_reasons.join(", ")
            }
        );

        let state = GenericPrinterPersistentState {
            version: 1,
            printer_id: self.id.clone(),
            driver_kind: PrinterDriverKind::Fake,
            slots: self.slots.iter().map(Self::persistent_slot_state).collect(),
        };
        let contents = serde_json::to_string(&state).map_err(|err| format!("Failed to serialize fake printer state: {err}"))?;
        self.state_dirty = false;
        self.state_dirty_reasons.clear();
        Ok(Some(PrinterPersistentStatePayload {
            path: self.state_path.clone(),
            contents,
        }))
    }

    fn restore_persistent_state_after_failed_store(&mut self) {
        self.mark_state_dirty("previous store failed");
    }
}
