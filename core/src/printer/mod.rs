#![allow(dead_code)]

pub mod bambu_adapter;
pub mod fake_driver;
pub mod manager;

use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{spool_record::FullSpoolRecord, store::Store};
use framework::framework::Framework;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PrinterId(pub String);

impl PrinterId {
    pub fn new(id: impl ToString) -> Self {
        Self(id.to_string())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SlotId(pub String);

impl SlotId {
    pub fn new(id: impl ToString) -> Self {
        Self(id.to_string())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrinterDriverKind {
    #[default]
    Unknown,
    Bambu,
    Fake,
    Moonraker,
    Prusa,
    Snapmaker,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrinterCapabilities {
    pub material_slot_read: bool,                    // Driver can report material slot groups, slots, and their state.
    pub material_slot_write: bool,                   // Driver supports at least one material slot mutation command.
    pub material_slot_assign: bool,                  // Driver can write material/profile data to a slot.
    pub material_slot_set_spool_id: bool,            // Driver or SpoolEase state can associate a spool ID with a slot.
    pub material_slot_clear: bool,                   // Driver can clear/reset slot material and spool state.
    pub material_slot_unassign_spool: bool,          // Driver can remove only the spool association from a slot.
    pub material_slot_presence_notify: bool,         // Driver can report physical spool insertion/removal per slot.
    pub print_status_read: bool,                     // Driver can report print/job state and progress.
    pub print_control: bool,                         // Driver can accept print control commands such as pause/resume/stop.
    pub consumption_tracking: bool,                  // Driver can report consumed filament by slot or spool.
    pub printer_tag_scan: bool,                      // Driver can report tag scans that happen inside the printer.
    pub print_file_fetch: bool,                      // Driver can fetch or provide print files for analysis.
    pub persistent_slot_state: bool,                 // Slot state survives restart or can be restored by the driver.
    pub pressure_advance: PressureAdvanceCapability, // Driver supports pressure advance/K-factor management.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PressureAdvanceCapability {
    #[default]
    Unsupported,
    DriverManaged,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrinterSnapshot {
    pub id: PrinterId,
    pub kind: PrinterDriverKind,
    #[serde(default)]
    pub identifier: String,
    pub name: String,
    pub connected: bool,
    pub num_extruders: u32,
    #[serde(default)]
    pub print_error_code: Option<i32>,
    #[serde(default)]
    pub system_error_codes: Vec<(i32, i32)>,
    pub slot_groups: Vec<SlotGroupSnapshot>,
    pub print: PrintSnapshot,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlotGroupSnapshot {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub kind: SlotGroupKind,
    pub extruder: Option<u32>,
    pub temperature_c: Option<f32>,
    pub humidity_percent: Option<i32>,
    pub slots: Vec<MaterialSlotSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SlotGroupKind {
    #[default]
    Other,
    InternalChanger,
    External,
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MaterialSlotSnapshot {
    pub id: SlotId,
    pub display_name: String,
    pub short_name: String,
    pub state: SlotState,
    pub filament: PrinterFilament,
    pub spool_id: Option<String>,
    pub consumed_since_load_g: f32,
    #[serde(default)]
    pub consumed_since_load_saved_g: f32,
    pub consumed_since_weight_g: f32,
    pub used_in_print: bool,
    pub pressure_advance_value: String,
    pub pressure_advance_meta: String,
}

pub type PrinterSnapshotState = Rc<PrinterSnapshotStateInner>;

#[derive(Debug)]
pub struct PrinterSnapshotStateInner {
    snapshot: RefCell<PrinterSnapshot>,
    dirty: Cell<bool>,
    pending_store_dirty: Cell<bool>,
}

impl PrinterSnapshotStateInner {
    pub fn new(snapshot: PrinterSnapshot) -> Self {
        Self {
            snapshot: RefCell::new(snapshot),
            dirty: Cell::new(false),
            pending_store_dirty: Cell::new(false),
        }
    }

    pub fn clone_snapshot(&self) -> PrinterSnapshot {
        self.snapshot.borrow().clone()
    }

    pub fn replace(&self, snapshot: PrinterSnapshot, mark_dirty: bool) {
        let mut current = self.snapshot.borrow_mut();
        if *current != snapshot {
            *current = snapshot;
            if mark_dirty {
                self.dirty.set(true);
            }
        }
    }

    pub fn replace_loaded(&self, mut snapshot: PrinterSnapshot) {
        sanitize_loaded_snapshot(&mut snapshot);
        self.replace_loaded_sanitized(snapshot);
    }

    pub fn replace_loaded_sanitized(&self, snapshot: PrinterSnapshot) {
        *self.snapshot.borrow_mut() = snapshot;
        self.dirty.set(false);
        self.pending_store_dirty.set(false);
    }

    pub fn update<F>(&self, mark_dirty: bool, f: F)
    where
        F: FnOnce(&mut PrinterSnapshot),
    {
        f(&mut self.snapshot.borrow_mut());
        if mark_dirty {
            self.dirty.set(true);
        }
    }

    pub fn try_update<E, F>(&self, mark_dirty: bool, f: F) -> Result<(), E>
    where
        F: FnOnce(&mut PrinterSnapshot) -> Result<(), E>,
    {
        f(&mut self.snapshot.borrow_mut())?;
        if mark_dirty {
            self.dirty.set(true);
        }
        Ok(())
    }

    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    pub fn begin_store(&self) -> bool {
        let was_dirty = self.dirty.replace(false);
        if was_dirty {
            self.pending_store_dirty.set(true);
        }
        was_dirty
    }

    pub fn store_succeeded(&self) {
        self.pending_store_dirty.set(false);
    }

    pub fn store_failed(&self) {
        if self.pending_store_dirty.replace(false) {
            self.dirty.set(true);
        }
    }
}

pub fn sanitize_loaded_snapshot(snapshot: &mut PrinterSnapshot) {
    snapshot.connected = false;
    snapshot.print.stage_code = None;
    snapshot.print.remaining_minutes = None;
    snapshot.print_error_code = None;
    snapshot.system_error_codes.clear();
    for group in &mut snapshot.slot_groups {
        group.temperature_c = None;
        group.humidity_percent = None;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterStateFile {
    pub version: u32,
    pub printer_id: PrinterId,
    pub driver_kind: PrinterDriverKind,
    pub generic: PrinterSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_private: Option<Value>,
}

pub const PRINTER_STATE_FILE_VERSION: u32 = 1;

pub fn slot_in_snapshot_mut<'a>(snapshot: &'a mut PrinterSnapshot, slot_id: &SlotId) -> Option<&'a mut MaterialSlotSnapshot> {
    snapshot
        .slot_groups
        .iter_mut()
        .flat_map(|group| group.slots.iter_mut())
        .find(|slot| slot.id == *slot_id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrinterPersistentStatePayload {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SlotState {
    #[default]
    Unknown,
    Empty,
    Occupied,
    Reading,
    Ready,
    Loading,
    Unloading,
    Loaded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PrinterFilament {
    #[default]
    Unknown,
    Known(PrinterFilamentInfo),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrinterFilamentInfo {
    pub material_type: String,
    pub material_subtype: String,
    pub brand: String,
    pub color_name: String,
    pub color_codes: Vec<String>,
    pub slicer_filament: String,
    pub temps: FilamentTemps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FilamentTemps {
    pub nozzle_min_c: Option<u32>,
    pub nozzle_max_c: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrintSnapshot {
    pub state: PrintState,
    pub job_name: Option<String>,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub current_layer: Option<u32>,
    pub total_layers: Option<u32>,
    #[serde(default)]
    pub stage_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrintState {
    #[default]
    Unknown,
    Idle,
    Slicing,
    Preparing,
    Printing,
    Paused,
    Finished,
    Failed,
    Canceled,
}

#[derive(Debug, Clone)]
pub enum PrinterCommand {
    Refresh,
    PrintControl(PrintControlCommand),
    AssignMaterialToSlot {
        slot_id: SlotId,
        spool: FullSpoolRecord,
        temps: FilamentTemps,
        mode: SlotAssignMode,
    },
    ClearSlot {
        slot_id: SlotId,
    },
    UnassignSpoolFromSlot {
        slot_id: SlotId,
    },
    DriverSpecific(DriverSpecificCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrintControlCommand {
    Pause,
    Resume,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotAssignMode {
    SpoolIdOnly,
    WritePrinterMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverSpecificCommand {
    Bambu(crate::bambu::driver_specific::BambuDriverCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverSpecificQuery {
    Bambu(crate::bambu::driver_specific::BambuDriverQuery),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverSpecificQueryResult {
    Bambu(crate::bambu::driver_specific::BambuDriverQueryResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterRuntimePersistenceRequest {
    pub printer_id: PrinterId,
    pub kind: PrinterRuntimePersistenceRequestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterRuntimePersistenceRequestKind {
    StorePrintProject,
    StoreConsumeIndex { consume_store_counter: i32 },
    DeletePrintProject,
}

pub type PrinterRuntimePersistenceRequestChannel = Channel<NoopRawMutex, PrinterRuntimePersistenceRequest, 5>;
pub type PrinterRuntimePersistenceFuture = Pin<Box<dyn Future<Output = Result<(), String>>>>;

pub trait PrinterObserver {
    fn on_printer_event(&mut self, event: PrinterEvent);
}

pub trait PrinterDriver {
    fn id(&self) -> &PrinterId;
    fn kind(&self) -> PrinterDriverKind;
    fn display_name(&self) -> String;
    fn capabilities(&self) -> PrinterCapabilities;
    fn snapshot_state(&self) -> PrinterSnapshotState;
    fn snapshot(&self) -> PrinterSnapshot;
    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()>;
    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>);
    fn start(&mut self, _framework: Rc<RefCell<Framework>>) {}
    fn acknowledge_slot_consumption_saved(&mut self, slot_id: &SlotId, consumed_since_load_saved_g: f32) -> PrinterResult<()> {
        let snapshot_state = self.snapshot_state();
        snapshot_state.try_update(true, |snapshot| {
            let slot = slot_in_snapshot_mut(snapshot, slot_id).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))?;
            slot.consumed_since_load_saved_g = consumed_since_load_saved_g;
            Ok(())
        })
    }
    fn query_driver_specific(&self, query: DriverSpecificQuery) -> PrinterResult<DriverSpecificQueryResult> {
        Err(PrinterError::UnsupportedCommand(format!("{query:?}")))
    }
    fn persistent_state_path(&self) -> Option<String> {
        None
    }
    fn private_state_dirty(&self) -> bool {
        false
    }
    fn load_private_state(&mut self, _state: Option<Value>, _store: &Rc<Store>) -> Result<(), String> {
        Ok(())
    }
    fn adjust_loaded_snapshot(&self, _snapshot: &mut PrinterSnapshot) {}
    fn prepare_private_state_store(&mut self) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn private_state_store_succeeded(&mut self) {}
    fn restore_private_state_after_failed_store(&mut self) {}
    fn restore_runtime_state(&mut self, _framework: Rc<RefCell<Framework>>) -> Option<PrinterRuntimePersistenceFuture> {
        None
    }
    fn handle_runtime_persistence_request(
        &mut self,
        _framework: Rc<RefCell<Framework>>,
        _request: PrinterRuntimePersistenceRequestKind,
    ) -> Option<PrinterRuntimePersistenceFuture> {
        None
    }
}

pub type PrinterResult<T> = Result<T, PrinterError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrinterError {
    UnsupportedCommand(String),
    PrinterUnavailable(String),
    SlotNotFound(SlotId),
    InvalidCommand(String),
    DriverError(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterEvent {
    pub printer_id: PrinterId,
    pub kind: PrinterEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrinterEventKind {
    ConnectivityChanged { connected: bool },
    SnapshotChanged { change: PrinterChange, snapshot: Box<PrinterSnapshot> },
    SlotTagScanned { slot_id: SlotId, tag_id: String, only_spool_id: bool },
    MaterialSlotPresenceChanged { changes: Vec<MaterialSlotPresenceChange> },
    PrintFileAnalysisRequested { request: PrintFileAnalysisRequest },
    PrintFileAnalysisCanceled { job_number: i32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialSlotPresenceChange {
    pub slot_id: SlotId,
    pub change: MaterialSlotPresenceChangeKind,
    pub spool_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialSlotPresenceChangeKind {
    Inserted,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrinterChange {
    All,
    Slots,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrintFileAnalysisRequest {
    pub job_number: i32,
    pub job_name: String,
    pub file_name: String,
}
