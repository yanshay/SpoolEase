use crate::utils::{deserialize_string_array, serialize_string_array};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use serde::{Deserialize, Serialize};

use crate::{
    app_config::MATERIALS,
    bambu::{BambuPrinter, bambu_api},
};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub enum Filament {
    #[default]
    Unknown,
    Known(FilamentInfo),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FilamentInfo {
    pub tray_info_idx: String, // e.g. "GFL99"
    pub tray_type: String,     // e.g. "PLA"
    #[serde(serialize_with = "serialize_string_array", deserialize_with = "deserialize_string_array")]
    pub tray_color: Vec<String>, // e.g. "2323F7FF"
    pub nozzle_temp_max: u32,  // e.g. 250
    pub nozzle_temp_min: u32,  // w.g. 190
}

impl FilamentInfo {
    pub fn new() -> Self {
        Self {
            tray_info_idx: String::from(""),
            tray_type: String::from(""),
            tray_color: alloc::vec![String::from("")],
            nozzle_temp_max: 0,
            nozzle_temp_min: 0,
        }
    }
    pub fn primary_color(&self) -> String {
        self.tray_color.first().cloned().unwrap_or_default()
    }
}

impl From<bambu_api::PrintTray> for FilamentInfo {
    fn from(v: bambu_api::PrintTray) -> Self {
        Self {
            tray_color: v.tray_colors(),
            tray_info_idx: v.tray_info_idx.unwrap_or_default(),
            tray_type: v.tray_type.unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
        }
    }
}

impl From<&bambu_api::PrintTray> for FilamentInfo {
    fn from(v: &bambu_api::PrintTray) -> Self {
        Self {
            tray_color: v.tray_colors(),
            tray_info_idx: v.tray_info_idx.as_ref().cloned().unwrap_or_default(),
            tray_type: v.tray_type.as_ref().cloned().unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
        }
    }
}

impl BambuPrinter {
    pub fn fill_filament_defaults_if_needed(&self, filament: &mut FilamentInfo) -> bool {
        // fill in temps based on material only type only (and then replace the tray_info_idx/slicer-material to the base material one) if not available
        // returns false if can't send to printer due to lack of information
        if filament.tray_type.is_empty() {
            return false;
        }
        let mut res = true;
        if filament.nozzle_temp_min == 0 || filament.nozzle_temp_max == 0 || filament.tray_info_idx.is_empty() {
            res = false;
            for (line_index, material_line) in MATERIALS.lines().enumerate() {
                if line_index == 0 {
                    continue;
                } // skip title line
                let mut split = material_line.split(',');
                if let Some(material) = split.next()
                    && material == filament.tray_type
                    && let (Some(filament_id), Some(nozzle_temp_low), Some(nozzle_temp_high)) = (split.next(), split.next(), split.next())
                    && let (Ok(nozzle_temp_low), Ok(nozzle_temp_high)) = (nozzle_temp_low.parse::<u32>(), nozzle_temp_high.parse::<u32>())
                {
                    filament.tray_info_idx = filament_id.to_string();
                    filament.nozzle_temp_min = nozzle_temp_low;
                    filament.nozzle_temp_max = nozzle_temp_high;
                    res = true;
                }
            }
        }
        res
    }
}
