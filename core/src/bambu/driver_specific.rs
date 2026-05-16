use alloc::{string::String, vec::Vec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BambuDriverCommand {
    AddPressureAdvance(BambuAddPressureAdvance),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BambuAddPressureAdvance {
    pub extruder: i32,
    pub diameter: String,
    pub nozzle_id: String,
    pub filament_id: String,
    pub setting_id: String,
    pub k_value: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BambuDriverQuery {
    PressureAdvanceEntries { filament_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BambuDriverQueryResult {
    PressureAdvanceEntries(Vec<BambuPressureAdvanceEntry>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BambuPressureAdvanceEntry {
    pub extruder: i32,
    pub diameter: String,
    pub nozzle_id: String,
    pub name: String,
    pub k_value: String,
    pub cali_idx: i32,
    pub setting_id: Option<String>,
}
