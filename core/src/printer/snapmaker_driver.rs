use alloc::{
    rc::{Rc, Weak},
    string::{String, ToString},
};
use core::cell::RefCell;

use crate::app_config::SnapmakerU1PrinterConfig;

use super::{
    PrinterCapabilities, PrinterCommand, PrinterDriver, PrinterDriverKind, PrinterError, PrinterId, PrinterObserver, PrinterResult,
    PrinterSnapshot, PrinterSnapshotState, PrinterSnapshotStateInner,
};

pub struct SnapmakerU1PrinterDriver {
    id: PrinterId,
    state: PrinterSnapshotState,
}

impl SnapmakerU1PrinterDriver {
    pub fn new(name: Option<String>, config: &SnapmakerU1PrinterConfig) -> PrinterResult<Self> {
        let id = config.printer_id().map_err(PrinterError::InvalidCommand)?;
        let name = name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Snapmaker U1")
            .to_string();
        let snapshot = PrinterSnapshot {
            id: id.clone(),
            kind: PrinterDriverKind::SnapmakerU1,
            native_id: config.serial.as_deref().unwrap_or_default().trim().to_string(),
            name,
            connected: false,
            slot_groups_known: false,
            ..Default::default()
        };
        Ok(Self {
            id,
            state: Rc::new(PrinterSnapshotStateInner::new(snapshot)),
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
        self.state.with_snapshot(|snapshot| snapshot.name.clone())
    }

    fn capabilities(&self) -> PrinterCapabilities {
        PrinterCapabilities::default()
    }

    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.state.clone()
    }

    fn snapshot(&self) -> PrinterSnapshot {
        self.state.clone_snapshot()
    }

    fn dispatch(&mut self, _command: PrinterCommand) -> PrinterResult<()> {
        Err(PrinterError::PrinterUnavailable("Snapmaker U1 is disconnected".to_string()))
    }

    fn subscribe(&mut self, _observer: Weak<RefCell<dyn PrinterObserver>>) {}
}
