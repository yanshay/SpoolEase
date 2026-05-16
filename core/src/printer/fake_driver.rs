use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::Timer;

use crate::app_config::FakePrinterConfig;
use framework::{error, framework::Framework, info, prelude::*};

use super::{
    MaterialSlotSnapshot, PressureAdvanceCapability, PrintSnapshot, PrintState, PrinterCapabilities, PrinterChange, PrinterCommand, PrinterDriver,
    PrinterDriverKind, PrinterError, PrinterEvent, PrinterEventKind, PrinterFilament, PrinterFilamentInfo, PrinterId, PrinterObserver, PrinterResult,
    PrinterSnapshot, PrinterSnapshotState, PrinterSnapshotStateInner, SlotAssignMode, SlotGroupKind, SlotGroupSnapshot, SlotId, SlotState,
    slot_in_snapshot_mut,
};

type FakePrinterCommandChannel = Channel<NoopRawMutex, PrinterCommand, 5>;

pub struct FakePrinterDriver {
    id: PrinterId,
    runtime: Rc<RefCell<FakePrinterRuntime>>,
    command_channel: Rc<FakePrinterCommandChannel>,
    task_started: bool,
}

struct FakePrinterRuntime {
    id: PrinterId,
    printer_number: usize,
    state_path: String,
    state: PrinterSnapshotState,
    observers: Vec<Weak<RefCell<dyn PrinterObserver>>>,
}

impl FakePrinterRuntime {
    fn new(name: Option<String>, config: &FakePrinterConfig, printer_number: usize) -> Self {
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
                short_name: format!("Slot {}", index + 1),
                state: SlotState::Empty,
                filament: PrinterFilament::Unknown,
                spool_id: None,
                consumed_since_load_g: 0.0,
                consumed_since_load_saved_g: 0.0,
                consumed_since_weight_g: 0.0,
                used_in_print: false,
                pressure_advance_value: String::new(),
                pressure_advance_meta: String::new(),
            })
            .collect();

        let snapshot = PrinterSnapshot {
            id: id.clone(),
            kind: PrinterDriverKind::Fake,
            identifier: config.unique_id.clone(),
            name,
            connected: true,
            num_extruders: 1,
            print_error_code: None,
            system_error_codes: Vec::new(),
            slot_groups: alloc::vec![SlotGroupSnapshot {
                id: "fake:slots".into(),
                name: "Fake Slots".into(),
                short_name: "Fake".into(),
                kind: SlotGroupKind::Virtual,
                extruder: Some(0),
                temperature_c: None,
                humidity_percent: None,
                slots,
            }],
            print: PrintSnapshot {
                state: PrintState::Idle,
                ..PrintSnapshot::default()
            },
        };

        Self {
            id,
            printer_number,
            state_path,
            state: Rc::new(PrinterSnapshotStateInner::new(snapshot)),
            observers: Vec::new(),
        }
    }

    fn capabilities() -> PrinterCapabilities {
        PrinterCapabilities {
            material_slot_read: true,
            material_slot_write: true,
            material_slot_assign: true,
            material_slot_set_spool_id: true,
            material_slot_clear: true,
            material_slot_unassign_spool: true,
            material_slot_presence_notify: false,
            print_status_read: true,
            print_control: false,
            consumption_tracking: false,
            printer_tag_scan: false,
            print_file_fetch: false,
            persistent_slot_state: true,
            pressure_advance: PressureAdvanceCapability::Unsupported,
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

    fn slot_index(&self, slot_id: &SlotId) -> PrinterResult<usize> {
        let raw_slot_id = slot_id.as_str().strip_prefix("fake:").unwrap_or(slot_id.as_str());
        let slot_index = raw_slot_id.parse::<usize>().map_err(|_| PrinterError::SlotNotFound(slot_id.clone()))?;
        let slot_count = self
            .state
            .clone_snapshot()
            .slot_groups
            .iter()
            .flat_map(|group| group.slots.iter())
            .count();
        if slot_index < slot_count {
            Ok(slot_index)
        } else {
            Err(PrinterError::SlotNotFound(slot_id.clone()))
        }
    }

    fn update_slot<F>(&self, slot_id: &SlotId, f: F) -> PrinterResult<()>
    where
        F: FnOnce(&mut MaterialSlotSnapshot),
    {
        self.state.try_update(true, |state| {
            let slot = slot_in_snapshot_mut(state, slot_id).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))?;
            f(slot);
            Ok(())
        })
    }

    fn validate_command(&self, command: &PrinterCommand) -> PrinterResult<()> {
        match command {
            PrinterCommand::Refresh => Ok(()),
            PrinterCommand::AssignMaterialToSlot { slot_id, .. }
            | PrinterCommand::ClearSlot { slot_id }
            | PrinterCommand::UnassignSpoolFromSlot { slot_id } => self.slot_index(slot_id).map(|_| ()),
            PrinterCommand::PrintControl(_) => Err(PrinterError::UnsupportedCommand("print_control".to_string())),
            PrinterCommand::DriverSpecific(command) => Err(PrinterError::UnsupportedCommand(format!("{command:?}"))),
        }
    }

    fn snapshot(&self) -> PrinterSnapshot {
        self.state.clone_snapshot()
    }

    fn dispatch_command(&mut self, command: PrinterCommand) -> PrinterResult<Option<PrinterEvent>> {
        match command {
            PrinterCommand::Refresh => Ok(Some(self.snapshot_changed(PrinterChange::All))),
            PrinterCommand::AssignMaterialToSlot { slot_id, spool, temps, mode } => {
                self.update_slot(&slot_id, |slot| {
                    slot.spool_id = Some(spool.spool_rec.id.clone());
                    slot.consumed_since_load_g = 0.0;
                    slot.consumed_since_load_saved_g = 0.0;
                    slot.consumed_since_weight_g = spool.spool_rec.consumed_since_weight;
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
                })?;
                Ok(Some(self.snapshot_changed(PrinterChange::Slot(slot_id))))
            }
            PrinterCommand::ClearSlot { slot_id } => {
                self.update_slot(&slot_id, |slot| {
                    slot.state = SlotState::Empty;
                    slot.spool_id = None;
                    slot.filament = PrinterFilament::Unknown;
                    slot.consumed_since_load_g = 0.0;
                    slot.consumed_since_load_saved_g = 0.0;
                    slot.consumed_since_weight_g = 0.0;
                    slot.used_in_print = false;
                })?;
                Ok(Some(self.snapshot_changed(PrinterChange::Slot(slot_id))))
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                self.update_slot(&slot_id, |slot| {
                    slot.spool_id = None;
                })?;
                Ok(Some(self.snapshot_changed(PrinterChange::Slot(slot_id))))
            }
            PrinterCommand::PrintControl(_) => Err(PrinterError::UnsupportedCommand("print_control".to_string())),
            PrinterCommand::DriverSpecific(command) => Err(PrinterError::UnsupportedCommand(format!("{command:?}"))),
        }
    }

    fn snapshot_changed(&self, change: PrinterChange) -> PrinterEvent {
        PrinterEvent {
            printer_id: self.id.clone(),
            kind: PrinterEventKind::SnapshotChanged {
                change,
                snapshot: Box::new(self.snapshot()),
            },
        }
    }
}

impl FakePrinterDriver {
    pub fn new(name: Option<String>, config: &FakePrinterConfig, printer_number: usize) -> Self {
        let runtime = Rc::new(RefCell::new(FakePrinterRuntime::new(name, config, printer_number)));
        let id = runtime.borrow().id.clone();
        Self {
            id,
            runtime,
            command_channel: Rc::new(FakePrinterCommandChannel::new()),
            task_started: false,
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
        self.runtime.borrow().state.clone_snapshot().name
    }

    fn capabilities(&self) -> PrinterCapabilities {
        FakePrinterRuntime::capabilities()
    }

    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.runtime.borrow().state.clone()
    }

    fn snapshot(&self) -> PrinterSnapshot {
        self.runtime.borrow().snapshot()
    }

    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        self.runtime.borrow().validate_command(&command)?;
        self.command_channel
            .try_send(command)
            .map_err(|err| PrinterError::DriverError(format!("Failed to queue fake printer command: {err:?}")))
    }

    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>) {
        self.runtime.borrow_mut().observers.push(observer);
    }

    fn start(&mut self, framework: Rc<RefCell<Framework>>) {
        if self.task_started {
            return;
        }
        self.task_started = true;
        framework
            .borrow()
            .spawner
            .spawn_heap(fake_printer_task(self.runtime.clone(), self.command_channel.clone()))
            .ok();
    }

    fn persistent_state_path(&self) -> Option<String> {
        Some(self.runtime.borrow().state_path.clone())
    }

    fn adjust_loaded_snapshot(&self, snapshot: &mut PrinterSnapshot) {
        snapshot.connected = true;
    }
}

fn notify_runtime_observers(runtime: &Rc<RefCell<FakePrinterRuntime>>, event: PrinterEvent) {
    let observers = runtime.borrow().observers.clone();
    let mut has_dead_observer = false;

    for weak_observer in observers {
        if let Some(observer) = weak_observer.upgrade() {
            observer.borrow_mut().on_printer_event(event.clone());
        } else {
            has_dead_observer = true;
        }
    }

    if has_dead_observer {
        runtime.borrow_mut().observers.retain(|observer| observer.upgrade().is_some());
    }
}

async fn fake_printer_task(runtime: Rc<RefCell<FakePrinterRuntime>>, command_channel: Rc<FakePrinterCommandChannel>) {
    let printer_number = runtime.borrow().printer_number;
    info!("[{printer_number}] Fake printer runtime task started");
    let receiver = command_channel.receiver();

    loop {
        let command = receiver.receive().await;
        Timer::after_millis(500).await;
        let event = {
            let mut runtime = runtime.borrow_mut();
            runtime.dispatch_command(command)
        };

        match event {
            Ok(Some(event)) => notify_runtime_observers(&runtime, event),
            Ok(None) => {}
            Err(err) => error!("[{printer_number}] Fake printer command failed: {err:?}"),
        }
    }
}
