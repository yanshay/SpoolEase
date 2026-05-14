#![allow(dead_code)]

pub mod bambu_adapter;
pub mod manager;

use alloc::{
    rc::Weak,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use serde::{Deserialize, Serialize};

use crate::spool_record::FullSpoolRecord;

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
    Moonraker,
    Prusa,
    Snapmaker,
    Simulator,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrinterCapabilities {
    pub material_slot_read: bool,
    pub material_slot_write: bool,
    pub print_status_read: bool,
    pub print_control: bool,
    pub consumption_tracking: bool,
    pub printer_tag_scan: bool,
    pub print_file_fetch: bool,
    pub persistent_slot_state: bool,
    pub pressure_advance: PressureAdvanceCapability,
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
    pub capabilities: PrinterCapabilities,
    pub extruders: Vec<ExtruderSnapshot>,
    pub slot_groups: Vec<SlotGroupSnapshot>,
    pub print: PrintSnapshot,
    pub diagnostics: Vec<PrinterDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExtruderSnapshot {
    pub id: u32,
    pub name: String,
    pub active: bool,
    pub loaded_slot_id: Option<SlotId>,
    pub nozzle_diameter_mm: Option<f32>,
    pub nozzle_type: Option<String>,
    pub temperature_c: Option<f32>,
    pub target_temperature_c: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlotGroupSnapshot {
    pub id: String,
    pub name: String,
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
    pub state: SlotState,
    pub filament: PrinterFilament,
    pub spool_id: Option<String>,
    pub consumed_since_load_g: f32,
    pub consumed_since_weight_g: f32,
    pub used_in_print: bool,
    pub driver_data: DriverData,
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
    pub active_slot_id: Option<SlotId>,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrinterDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    #[default]
    Info,
    Warning,
    Error,
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
    fn snapshot(&self) -> PrinterSnapshot;
    fn dispatch(&mut self, command: PrinterCommand) -> PrinterResult<()>;
    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>);
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
pub enum PrinterEvent {
    ConnectivityChanged {
        printer_id: PrinterId,
        connected: bool,
    },
    SnapshotChanged {
        printer_id: PrinterId,
        change: PrinterChange,
    },
    SlotTagScanned {
        printer_id: PrinterId,
        slot_id: SlotId,
        tag_id: String,
        only_spool_id: bool,
    },
    SlotConsumptionReported {
        printer_id: PrinterId,
        slot_id: SlotId,
        grams: f32,
        source: ConsumptionSource,
    },
    PrintFileAnalysisRequested {
        printer_id: PrinterId,
        request: PrintFileAnalysisRequest,
    },
    PrintFileAnalysisCanceled {
        printer_id: PrinterId,
        job_number: i32,
    },
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
