#![allow(dead_code)]

pub mod bambu_adapter;
pub mod fake_driver;
pub mod manager;

use alloc::{
    boxed::Box,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use serde::{Deserialize, Serialize};

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
    pub name: String,
    pub connected: bool,
    pub num_extruders: u32,
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
    Toolhead,
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

pub type PrinterSnapshotState = Rc<RefCell<PrinterSnapshot>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenericPrinterPersistentState {
    pub version: u32,
    pub printer_id: PrinterId,
    pub driver_kind: PrinterDriverKind,
    pub slots: Vec<GenericSlotPersistentState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenericSlotPersistentState {
    pub slot_id: SlotId,
    pub state: SlotState,
    pub filament: PrinterFilament,
    pub spool_id: Option<String>,
    pub consumed_since_load_g: f32,
    #[serde(default)]
    pub consumed_since_load_saved_g: f32,
    pub consumed_since_weight_g: f32,
    pub used_in_print: bool,
}

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
    AddPressureAdvance(PressureAdvanceProfile),
    DriverSpecific(DriverCommand),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PressureAdvanceProfile {
    pub id: Option<String>,
    pub name: String,
    pub filament: PrinterFilamentInfo,
    pub value: f32,
    pub driver_data: DriverData,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DriverCommand {
    pub name: String,
    pub data: DriverData,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DriverData {
    pub fields: Vec<DriverDataField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverDataField {
    pub key: String,
    pub value: String,
}

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
        let mut snapshot = snapshot_state.borrow_mut();
        let slot = slot_in_snapshot_mut(&mut snapshot, slot_id).ok_or_else(|| PrinterError::SlotNotFound(slot_id.clone()))?;
        slot.consumed_since_load_saved_g = consumed_since_load_saved_g;
        Ok(())
    }
    fn persistent_state_path(&self) -> Option<String> {
        None
    }
    fn load_persistent_state(&mut self, _state_json: &str, _store: &Rc<Store>) -> Result<(), String> {
        Ok(())
    }
    fn prepare_persistent_state_store(&mut self) -> Result<Option<PrinterPersistentStatePayload>, String> {
        Ok(None)
    }
    fn persistent_state_store_succeeded(&mut self) {}
    fn restore_persistent_state_after_failed_store(&mut self) {}
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
    ConnectivityChanged {
        connected: bool,
    },
    SnapshotChanged {
        change: PrinterChange,
        snapshot: Box<PrinterSnapshot>,
    },
    SlotTagScanned {
        slot_id: SlotId,
        tag_id: String,
        only_spool_id: bool,
    },
    MaterialSlotPresenceChanged {
        changes: Vec<MaterialSlotPresenceChange>,
    },
    SlotConsumptionReported {
        slot_id: SlotId,
        grams: f32,
        source: ConsumptionSource,
    },
    PrintFileAnalysisRequested {
        request: PrintFileAnalysisRequest,
    },
    PrintFileAnalysisCanceled {
        job_number: i32,
    },
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
    Connectivity,
    Slots,
    Slot(SlotId),
    Print,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsumptionSource {
    DriverTelemetry,
    PrintFileAnalysis,
    Manual,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrintFileAnalysisRequest {
    pub job_number: i32,
    pub job_name: String,
    pub file_name: String,
    pub driver_data: DriverData,
}
