use alloc::{
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::cell::RefCell;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use framework::{prelude::Framework, utils::SpawnerHeapExt, warn};

use super::snapmaker_comm::snapmaker_u1_comm_task;
use super::snapmaker_state::{U1Message, U1Runtime, snapmaker_u1_messages_task};
use super::{
    MaterialSlotSnapshot, PrinterCapabilities, PrinterCommand, PrinterDriver, PrinterDriverKind, PrinterError, PrinterId, PrinterObserver,
    PrinterResult, PrinterSnapshot, PrinterSnapshotState, PrinterSnapshotStateInner, SlotGroupKind, SlotGroupSnapshot, SlotId,
};
use crate::app_config::SnapmakerU1PrinterConfig;

pub(super) type U1Channel = Channel<NoopRawMutex, U1Message, 16>;

pub struct SnapmakerU1PrinterDriver {
    id: PrinterId,
    runtime: Rc<U1Runtime>,
    config: SnapmakerU1PrinterConfig,
    printer_number: usize,
    task_started: bool,
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
                state: Rc::new(PrinterSnapshotStateInner::new(snapshot)),
                observers: RefCell::new(Vec::new()),
            }),
            config: config.clone(),
            printer_number,
            task_started: false,
        })
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
            print_status_read: true,
            ..Default::default()
        }
    }
    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.runtime.state.clone()
    }
    fn snapshot(&self) -> PrinterSnapshot {
        self.runtime.state.clone_snapshot()
    }
    fn dispatch(&mut self, _command: PrinterCommand) -> PrinterResult<()> {
        Err(PrinterError::UnsupportedCommand("Snapmaker U1 command".into()))
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
        if let Err(err) = spawner.spawn_heap(snapmaker_u1_messages_task(self.runtime.clone(), channel.clone())) {
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
        )) {
            warn!(
                "[{}] Snapmaker U1 {:?}: failed to start communication task: {:?}",
                self.printer_number, name, err
            );
        }
    }
}
