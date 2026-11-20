use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use framework::{debug, info, prelude::Framework, utils::SpawnerHeapExt, warn};
use serde_json::{Value, json};

use super::snapmaker_comm::snapmaker_u1_comm_task;
use super::snapmaker_state::{U1Message, U1Runtime, U1SavedState, snapmaker_u1_messages_task};
use super::{
    MaterialSlotSnapshot, PrintControlCommand, PrinterCapabilities, PrinterCommand, PrinterDriver, PrinterDriverKind, PrinterError, PrinterFilament,
    PrinterId, PrinterObserver, PrinterPersistentStateLoadError, PrinterResult, PrinterSnapshot, PrinterSnapshotState, PrinterSnapshotStateInner,
    SlotAssignMode, SlotGroupKind, SlotGroupSnapshot, SlotId, slot_in_snapshot_mut,
};
use crate::app_config::SnapmakerU1PrinterConfig;
use crate::store::Store;

pub(super) type U1Channel = Channel<NoopRawMutex, U1Message, 16>;
pub(super) type U1CommandChannel = Channel<NoopRawMutex, Box<String>, 8>;

pub struct SnapmakerU1PrinterDriver {
    id: PrinterId,
    runtime: Rc<U1Runtime>,
    config: SnapmakerU1PrinterConfig,
    printer_number: usize,
    task_started: bool,
    commands: Rc<U1CommandChannel>,
}

impl SnapmakerU1PrinterDriver {
    pub fn new(name: Option<String>, config: &SnapmakerU1PrinterConfig, printer_number: usize) -> PrinterResult<Self> {
        let id = config.printer_id().map_err(PrinterError::InvalidCommand)?;
        let name = name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Snapmaker U1")
            .to_string();
        let slots = (0..4)
            .map(|index| MaterialSlotSnapshot {
                id: SlotId::new(format!("snapmaker_u1:{index}")),
                native_id: Some(index.to_string()),
                display_name: format!("Slot {}", index + 1),
                short_name: format!("Slot {}", index + 1),
                ..Default::default()
            })
            .collect();
        let snapshot = PrinterSnapshot {
            id: id.clone(),
            kind: PrinterDriverKind::SnapmakerU1,
            native_id: config.serial.as_deref().unwrap_or_default().trim().to_string(),
            name,
            connected: false,
            num_extruders: 4,
            slot_groups_known: true,
            slot_groups: vec![SlotGroupSnapshot {
                id: "snapmaker_u1:slots".into(),
                name: "Snapmaker U1".into(),
                short_name: "U1".into(),
                kind: SlotGroupKind::Mms,
                slots,
                ..Default::default()
            }],
            ..Default::default()
        };
        Ok(Self {
            id: id.clone(),
            runtime: Rc::new(U1Runtime {
                id,
                number: printer_number,
                state: Rc::new(PrinterSnapshotStateInner::new(snapshot)),
                observers: RefCell::new(Vec::new()),
                saved: RefCell::new(U1SavedState::default()),
                saved_dirty: Cell::new(false),
                pending_dirty: Cell::new(false),
                restore_finished: Default::default(),
            }),
            config: config.clone(),
            printer_number,
            task_started: false,
            commands: Rc::new(U1CommandChannel::new()),
        })
    }

    fn state_path(&self) -> String {
        const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut hash = 0xcbf29ce484222325u64;
        for byte in self.id.as_str().bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
        let name: String = (0..8)
            .rev()
            .map(|shift| ALPHABET[((hash >> (shift * 5)) & 31) as usize] as char)
            .collect();
        format!("/state/{name}.u1/startup.jsn")
    }

    fn slot_index(slot_id: &SlotId) -> PrinterResult<usize> {
        slot_id
            .as_str()
            .strip_prefix("snapmaker_u1:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < 4)
            .ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))
    }

    fn configuration(&self, index: usize, spool: &crate::spool_record::SpoolRecord) -> PrinterResult<Option<String>> {
        for text in [&spool.brand, &spool.material_type, &spool.material_subtype] {
            if text.chars().any(|ch| matches!(ch, '"' | '#' | ';' | '*' | '\r' | '\n')) {
                return Err(PrinterError::InvalidCommand(
                    "Filament information contains characters not supported by the printer".into(),
                ));
            }
        }
        if spool.material_type.trim().is_empty() {
            return Err(PrinterError::InvalidCommand("Filament material is missing".into()));
        }
        let colors: Vec<String> = spool
            .color_code
            .iter()
            .flat_map(|value| value.split(';'))
            .filter(|value| !value.is_empty())
            .map(|value| value.trim().trim_start_matches('#').to_ascii_uppercase())
            .collect();
        if colors.is_empty()
            || colors.len() > 5
            || colors
                .iter()
                .any(|color| !matches!(color.len(), 6 | 8) || !color.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(PrinterError::InvalidCommand("U1 requires one to five valid filament colors".into()));
        }
        let alpha = if colors[0].len() == 8 {
            u8::from_str_radix(&colors[0][6..8], 16).unwrap_or(255)
        } else {
            255
        };
        let rgb: Vec<&str> = colors.iter().map(|color| &color[..6]).collect();
        let matches = self.runtime.state.with_snapshot(|snapshot| {
            let Some(slot) = snapshot.slot_groups.first().and_then(|group| group.slots.get(index)) else {
                return false;
            };
            let PrinterFilament::Known(filament) = &slot.filament else {
                return false;
            };
            let wanted: Vec<String> = rgb.iter().map(|color| format!("{color}{alpha:02X}")).collect();
            filament.material_type == spool.material_type
                && filament.material_subtype == spool.material_subtype
                && filament.brand == spool.brand
                && filament.color_codes.len() == wanted.len()
                && filament.color_codes.iter().zip(&wanted).all(|(a, b)| a.eq_ignore_ascii_case(b))
        });
        if matches {
            return Ok(None);
        }
        let script = format!(
            "SET_PRINT_FILAMENT_CONFIG CONFIG_EXTRUDER={index} VENDOR=\"{}\" FILAMENT_TYPE=\"{}\" FILAMENT_SUBTYPE=\"{}\" COLOR_NUMS={} COLORS={} ALPHA={alpha} MULTI_MODE=0 FORCE=1",
            spool.brand,
            spool.material_type,
            spool.material_subtype,
            rgb.len(),
            rgb.join(",")
        );
        Ok(Some(
            json!({"jsonrpc":"2.0", "id":3, "method":"printer.gcode.script", "params":{"script":script}}).to_string(),
        ))
    }
}

impl PrinterDriver for SnapmakerU1PrinterDriver {
    fn id(&self) -> &PrinterId {
        &self.id
    }
    fn kind(&self) -> PrinterDriverKind {
        PrinterDriverKind::SnapmakerU1
    }
    fn display_name(&self) -> String {
        self.runtime.state.with_snapshot(|snapshot| snapshot.name.clone())
    }
    fn capabilities(&self) -> PrinterCapabilities {
        PrinterCapabilities {
            material_slot_assign: true,
            material_slot_set_spool_id: true,
            material_slot_unassign_spool: true,
            print_status_read: true,
            print_control: true,
            ..Default::default()
        }
    }
    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.runtime.state.clone()
    }
    fn snapshot(&self) -> PrinterSnapshot {
        self.runtime.state.clone_snapshot()
    }
    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        match command {
            PrinterCommand::PrintControl(command) => {
                if !self.runtime.state.with_snapshot(|snapshot| snapshot.connected) {
                    return Err(PrinterError::PrinterUnavailable("Snapmaker U1 disconnected".into()));
                }
                let method = match command {
                    PrintControlCommand::Pause => "printer.print.pause",
                    PrintControlCommand::Resume => "printer.print.resume",
                    PrintControlCommand::Stop => "printer.print.cancel",
                };
                let request = json!({"jsonrpc":"2.0", "id":5, "method":method, "params":{}}).to_string();
                self.commands
                    .try_send(Box::new(request))
                    .map_err(|err| PrinterError::DriverError(format!("Failed to queue U1 print command: {err:?}")))?;
                info!("[{}] Snapmaker U1 print command {} queued", self.printer_number, method);
                Ok(())
            }
            PrinterCommand::AssignMaterialToSlot { slot_id, spool, mode, .. } => {
                let index = Self::slot_index(&slot_id)?;
                if !self.runtime.state.with_snapshot(|snapshot| snapshot.connected) {
                    return Err(PrinterError::PrinterUnavailable("Snapmaker U1 disconnected".into()));
                }
                if mode == SlotAssignMode::WritePrinterMaterial {
                    if let Some(command) = self.configuration(index, &spool.spool_rec)? {
                        self.commands
                            .try_send(Box::new(command))
                            .map_err(|err| PrinterError::DriverError(format!("Failed to queue U1 configuration: {err:?}")))?;
                        info!(
                            "[{}] Snapmaker U1 slot {} configuring material for spool {}",
                            self.printer_number,
                            index + 1,
                            spool.spool_rec.id
                        );
                    } else {
                        info!(
                            "[{}] Snapmaker U1 slot {} material already matches spool {}",
                            self.printer_number,
                            index + 1,
                            spool.spool_rec.id
                        );
                    }
                }
                self.runtime.state.try_update(true, |snapshot| {
                    let slot = slot_in_snapshot_mut(snapshot, &slot_id).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))?;
                    slot.assign_spool_id(spool.spool_rec.id.clone());
                    slot.consumed_since_load_g = 0.0;
                    slot.consumed_since_load_saved_g = 0.0;
                    slot.consumed_since_weight_g = spool.spool_rec.consumed_since_weight;
                    Ok(())
                })?;
                info!(
                    "[{}] Snapmaker U1 slot {} spool ID set to {}",
                    self.printer_number,
                    index + 1,
                    spool.spool_rec.id
                );
                Ok(())
            }
            PrinterCommand::UnassignSpoolFromSlot { slot_id } => {
                let index = Self::slot_index(&slot_id)?;
                self.runtime.state.try_update(true, |snapshot| {
                    let slot = slot_in_snapshot_mut(snapshot, &slot_id).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))?;
                    slot.clear_spool_id();
                    Ok(())
                })?;
                info!("[{}] Snapmaker U1 slot {} spool ID cleared", self.printer_number, index + 1);
                Ok(())
            }
            _ => Err(PrinterError::UnsupportedCommand("Snapmaker U1 command".into())),
        }
    }
    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>) {
        self.runtime.observers.borrow_mut().push(observer);
    }
    fn start(&mut self, framework: Rc<RefCell<Framework>>) {
        if self.task_started {
            return;
        }
        let channel = Rc::new(U1Channel::new());
        let spawner = framework.borrow().spawner;
        let name = self.display_name();
        if let Err(err) = spawner.spawn_heap(snapmaker_u1_messages_task(self.runtime.clone(), channel.clone(), self.commands.clone())) {
            warn!(
                "[{}] Snapmaker U1 {:?}: failed to start message task: {:?}",
                self.printer_number, name, err
            );
            return;
        }
        self.task_started = true;
        if let Err(err) = spawner.spawn_heap(snapmaker_u1_comm_task(
            framework,
            self.config.clone(),
            self.printer_number,
            name.clone(),
            channel,
            self.commands.clone(),
        )) {
            warn!(
                "[{}] Snapmaker U1 {:?}: failed to start communication task: {:?}",
                self.printer_number, name, err
            );
        }
    }

    fn persistent_state_path(&self) -> Option<String> {
        Some(self.state_path())
    }
    fn persistent_state_restore_finished(&mut self) {
        self.runtime.restore_finished.signal(());
    }
    fn adjust_loaded_snapshot(&self, snapshot: &mut PrinterSnapshot) {
        snapshot.connected = false;
        snapshot.name = self.display_name();
    }
    fn private_state_dirty(&self) -> bool {
        self.runtime.saved_dirty.get()
    }
    fn load_private_state(&mut self, state: Option<Value>, _store: &Rc<Store>) -> Result<(), PrinterPersistentStateLoadError> {
        if let Some(state) = state {
            *self.runtime.saved.borrow_mut() =
                serde_json::from_value(state).map_err(|err| PrinterPersistentStateLoadError::Parse(format!("Invalid U1 printer state: {err}")))?;
        }
        Ok(())
    }
    fn prepare_private_state_store(&mut self) -> Result<Option<Value>, String> {
        let state = serde_json::to_value(&*self.runtime.saved.borrow()).map_err(|err| format!("Failed to serialize U1 state: {err}"))?;
        let dirty = self.runtime.saved_dirty.replace(false);
        if dirty {
            debug!(
                "[{}] Snapmaker U1 dirty status: feeder/toolhead loaded state changed",
                self.printer_number
            );
        }
        if self.runtime.state.is_dirty() {
            debug!("[{}] Snapmaker U1 dirty status: slot spool information changed", self.printer_number);
        }
        self.runtime.pending_dirty.set(self.runtime.pending_dirty.get() || dirty);
        Ok(Some(state))
    }
    fn private_state_store_succeeded(&mut self) {
        self.runtime.pending_dirty.set(false);
    }
    fn restore_private_state_after_failed_store(&mut self) {
        if self.runtime.pending_dirty.replace(false) {
            self.runtime.saved_dirty.set(true);
        }
    }
}
