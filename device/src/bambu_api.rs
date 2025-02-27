use alloc::{format, string::String, vec::Vec};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ==========================================================================

// https://github.com/markhaehnel/bambulab/blob/main/src/message.rs

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Print {
    pub print: PrintData,
}

impl Print {
    #[allow(dead_code)]
    pub fn find_print_tray_by_id(&self, target_id: u32) -> Option<&PrintTray> {
        self.print
            .ams
            .as_ref()? // Get reference to PrintAms if it exists, otherwise return None
            .ams
            .as_ref()? // Get reference to Vec<PrintAmsData> if it exists, otherwise return None
            .iter() // Create an iterator over Vec<PrintAmsData>
            .flat_map(|ams_data| &ams_data.tray) // Flatten all trays into a single iterator
            .find(|tray| tray.id == target_id) // Find the tray with the matching id
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filament {
    pub filament_id: String,
    pub name: String,
    pub k_value: String,
    pub n_coef: String,
    pub setting_id: String,
    // pub tray_id: Option<i32>, // ??? why is it here? In extrusion_cali_set it can exist (case when adding new calibration)
    pub cali_idx: i32, // Need to switch to optional since in extrusion_cali_set it is missing at least sometimes (case when adding new calibration)
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintData {
    // AMS Section
    // pub upload: Option<PrintUpload>,
    // pub nozzle_temper: Option<f64>,
    // pub nozzle_target_temper: Option<i64>,
    // pub bed_temper: Option<f64>,
    // pub bed_target_temper: Option<i64>,
    // pub chamber_temper: Option<i64>,
    // pub mc_print_stage: Option<String>,
    // pub heatbreak_fan_speed: Option<String>,
    // pub cooling_fan_speed: Option<String>,
    // pub big_fan1_speed: Option<String>,
    // pub big_fan2_speed: Option<String>,
    // pub mc_percent: Option<i64>,
    // pub mc_remaining_time: Option<i64>,
    // pub ams_status: Option<i64>,
    // pub ams_rfid_status: Option<i64>,
    // pub hw_switch_state: Option<i64>,
    // pub spd_mag: Option<i64>,
    // pub spd_lvl: Option<i64>,
    // pub print_error: Option<i64>,
    // pub lifecycle: Option<String>,
    // pub wifi_signal: Option<String>,
    // pub gcode_state: Option<String>,
    // pub gcode_file_prepare_percent: Option<String>,
    // pub queue_number: Option<i64>,
    // pub queue_total: Option<i64>,
    // pub queue_est: Option<i64>,
    // pub queue_sts: Option<i64>,
    // pub project_id: Option<String>,
    // pub profile_id: Option<String>,
    // pub task_id: Option<String>,
    // pub subtask_id: Option<String>,
    // pub subtask_name: Option<String>,
    // pub gcode_file: Option<String>,
    // pub stg: Option<Vec<Value>>,
    // pub stg_cur: Option<i64>,
    // pub print_type: Option<String>,
    // pub home_flag: Option<i64>,
    // pub mc_print_line_number: Option<String>,
    // pub mc_print_sub_stage: Option<i64>,
    // pub sdcard: Option<bool>,
    // pub force_upgrade: Option<bool>,
    // pub mess_production_state: Option<String>,
    // pub layer_num: Option<i64>,
    // pub total_layer_num: Option<i64>,
    // pub s_obj: Option<Vec<Value>>,
    // pub fan_gear: Option<i64>,
    // pub hms: Option<Vec<Value>>,
    // pub online: Option<PrintOnline>,
    pub ams: Option<PrintAms>,
    // pub ipcam: Option<PrintIpcam>,
    pub vt_tray: Option<PrintTray>, // was PrintVtTray
    // pub lights_report: Option<Vec<PrintLightsReport>>,
    // pub upgrade_state: Option<PrintUpgradeState>,
    pub command: Option<String>,
    // pub msg: Option<i64>,
    pub sequence_id: Option<String>,

    // Added by me , were missing in original structure definition, from announcement on filament change following a change from slicer.
    // It not neccesarily arrive in a larger message so need to process it.
    pub nozzle_temp_max: Option<u32>,
    pub nozzle_temp_min: Option<u32>,
    // pub setting_id: Option<String>, // contains something similar to tray_info_idx, see below, so for now ignoring it
    pub tray_color: Option<String>,
    pub tray_id: Option<i32>,
    pub ams_id: Option<i32>,
    pub cali_idx: Option<i32>,
    pub tray_info_idx: Option<String>,
    pub tray_type: Option<String>,
    pub reason: Option<String>,
    pub result: Option<String>,

    pub nozzle_diameter: Option<String>, // sometimes received, required so to be sent in extruder_cali commane as below after filament setting (like slicer)
    pub filament_id: Option<String>,
    pub filaments: Option<Vec<Filament>>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintAms {
    // Several AMS's - AMS as a System
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ams: Option<Vec<PrintAmsData>>, // Vector of AMS's
    pub ams_exist_bits: Option<String>,
    pub tray_exist_bits: Option<String>,
    pub tray_is_bbl_bits: Option<String>,
    // pub tray_tar: Option<String>,
    // pub tray_now: Option<String>,
    // pub tray_pre: Option<String>,
    pub tray_read_done_bits: Option<String>,
    pub tray_reading_bits: Option<String>,
    // pub version: Option<i64>,
    // pub insert_flag: Option<bool>,
    // pub power_on_flag: Option<bool>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintAmsData {
    // A Specific AMS
    pub id: String,
    pub humidity: String,
    // pub temp: String,
    pub tray: Vec<PrintTray>, // Vector of Trays
}

// An AMS Tray
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintTray {
    #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub id: u32, // Tray Id
    #[serde(skip_serializing)]
    pub k: Option<f32>,
    #[serde(skip_serializing)]
    pub cali_idx: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tray_info_idx: Option<String>, // e.g. "GFL99"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tray_type: Option<String>, // e.g. "PLA"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tray_color: Option<String>, // e.g. "2323F7FF"
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    pub nozzle_temp_max: Option<u32>, // e.g. 250
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    pub nozzle_temp_min: Option<u32>, // w.g. 190
                                      // pub remain: Option<i64>,
                                      // pub n: Option<f64>,
                                      // pub tag_uid: Option<String>,
                                      // pub tray_id_name: Option<String>,
                                      // pub tray_sub_brands: Option<String>,
                                      // pub tray_weight: Option<String>,
                                      // pub tray_diameter: Option<String>,
                                      // pub tray_temp: Option<String>,
                                      // pub tray_time: Option<String>,
                                      // pub bed_temp_type: Option<String>,
                                      // pub bed_temp: Option<String>,
                                      // pub xcam_info: Option<String>,
                                      // pub tray_uuid: Option<String>,
}
// TODO: check if can consolidate the two types of trays to a single one(only difference is optional items for serde?)
// External Tray - One per printer
// #[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
// pub struct PrintVtTray {
//     #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
//     pub id: u32,
//     #[serde(skip_serializing)]
//     pub k: Option<f32>,
//     #[serde(skip_serializing)]
//     pub cali_idx: Option<i32>,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     pub tray_info_idx: Option<String>,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     pub tray_type: Option<String>,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     pub tray_color: Option<String>,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
//     pub nozzle_temp_max: Option<u32>,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
//     pub nozzle_temp_min: Option<u32>,
//     // pub tray_sub_brands: String,
//     // pub tag_uid: String,
//     // pub tray_id_name: String,
//     // pub tray_weight: String,
//     // pub tray_diameter: String,
//     // pub tray_temp: String,
//     // pub tray_time: String,
//     // pub bed_temp_type: String,
//     // pub bed_temp: String,
//     // pub xcam_info: String,
//     // pub tray_uuid: String,
//     // pub remain: i64,
//     // pub n: i64,
// }

// Commands

////////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushAllCommand {
    pub pushing: PushAll, // ams_filament_setting
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushAll {
    pub command: String, // ams_filament_setting
}

impl PushAllCommand {
    pub fn new() -> Self {
        Self {
            pushing: PushAll {
                command: String::from("pushall"),
            },
        }
    }
}

///////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmsFilamentSettingCommand {
    print: AmsFilamentSetting,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmsFilamentSetting {
    pub command: String, // ams_filament_setting
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub ams_id: u32,
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub tray_id: i32,
    pub tray_info_idx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting_id: Option<String>,
    pub tray_color: String,
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub nozzle_temp_min: u32,
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub nozzle_temp_max: u32,
    pub tray_type: String,
    pub sequence_id: String,
}

impl AmsFilamentSettingCommand {
    pub fn new(
        ams_id: u32,
        tray_id: i32,
        tray_info_idx: &str,
        setting_id: Option<&str>,
        tray_type: &str,
        tray_color: &str,
        nozzle_temp_min: u32,
        nozzle_temp_max: u32,
    ) -> Self {
        Self {
            print: AmsFilamentSetting {
                command: String::from("ams_filament_setting"),
                ams_id,
                tray_id,
                tray_info_idx: String::from(tray_info_idx),
                setting_id: setting_id.map(|v| String::from(v)),
                tray_color: String::from(tray_color),
                nozzle_temp_min,
                nozzle_temp_max,
                tray_type: String::from(tray_type),
                sequence_id: String::from("1"),
            },
        }
    }
}

fn u32_as_str_se<S>(x: &u32, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

fn u32_as_str_de<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let s: &str = Deserialize::deserialize(deserializer)?;
    s.parse::<u32>().map_err(serde::de::Error::custom)
}

fn option_u32_as_str_se<S>(value: &Option<u32>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(v) => u32_as_str_se(v, serializer),
        None => serializer.serialize_none(),
    }
}

fn option_u32_as_str_de<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let option: Option<String> = Option::deserialize(deserializer)?;
    option.as_deref().map(|s| s.parse::<u32>().map_err(serde::de::Error::custom)).transpose()
}

// "print": {
//     "command": "ams_filament_setting",
//     "ams_id": 0,
//     "tray_id": 0,
//     "tray_info_idx": "GFL99",
//     "tray_color": "FF0000FF",
//     "nozzle_temp_min": 190,
//     "nozzle_temp_max": 250,
//     "tray_type": "PLA"
// }
//  }"#;

////////////////////////////////////////////////////////////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliGetCommand {
    print: ExtrusionCaliGet,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliGet {
    pub command: String,     // extrusion_cali_get
    pub filament_id: String, // always empty
    pub nozzle_diameter: String,
    pub sequence_id: String,
}

impl ExtrusionCaliGetCommand {
    pub fn new(nozzle_diameter: &str) -> Self {
        Self {
            print: ExtrusionCaliGet {
                command: String::from("extrusion_cali_get"),
                filament_id: String::from(""),
                nozzle_diameter: String::from(nozzle_diameter),
                sequence_id: String::from("1"),
            },
        }
    }
}

// {
//   "print": {
//     "command": "extrusion_cali_get",
//     "filament_id": "",
//     "nozzle_diameter": "0.4"
//   }
// }
///////////////////////////////////////////////////////////////////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliSelCommand {
    print: ExtrusionCaliSel,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliSel {
    pub command: String, // extrusion_cali_sel
    pub cali_idx: i32,
    pub filament_id: String, // always empty
    pub nozzle_diameter: String,
    pub tray_id: i32,
    pub sequence_id: String,
}

impl ExtrusionCaliSelCommand {
    pub fn new(nozzle_diameter: &str, tray_id: i32, filament_id: &str, cali_idx: Option<i32>) -> Self {
        Self {
            print: ExtrusionCaliSel {
                command: String::from("extrusion_cali_sel"),
                cali_idx: cali_idx.unwrap_or(-1),
                filament_id: String::from(filament_id),
                nozzle_diameter: String::from(nozzle_diameter),
                tray_id,
                sequence_id: String::from("1"),
            },
        }
    }
}

// {
//   "print": {
//     "cali_idx": -1,
//     "command": "extrusion_cali_sel",
//     "filament_id": "GFL03",
//     "nozzle_diameter": "0.4",
//     "sequence_id": "21266",
//     "tray_id": 254,
//     "reason": "success",
//     "result": "success"
//   }
// }
