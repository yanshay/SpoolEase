use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BambuSlotGroupInfo {
    pub ams_type: BambuAmsType,
    pub bound_extruders: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BambuAmsType {
    ExternalSpool,
    #[default]
    Ams,
    AmsLite,
    Ams2Pro,
    AmsHt,
    Unknown(u8),
}

impl BambuAmsType {
    pub fn from_protocol(value: u8) -> Self {
        match value {
            0 => Self::ExternalSpool,
            1 => Self::Ams,
            2 => Self::AmsLite,
            3 => Self::Ams2Pro,
            4 => Self::AmsHt,
            _ => Self::Unknown(value),
        }
    }
}

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
