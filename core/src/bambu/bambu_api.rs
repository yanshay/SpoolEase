use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ==========================================================================

// https://github.com/markhaehnel/bambulab/blob/main/src/message.rs

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    Print(Print),
    Info(Info),
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Print {
    pub print: PrintData,
}

impl Print {
    // #[allow(dead_code)]
    // pub fn find_print_tray_by_id(&self, target_id: u32) -> Option<&PrintTray> {
    //     self.print
    //         .ams
    //         .as_ref()? // Get reference to PrintAms if it exists, otherwise return None
    //         .ams
    //         .as_ref()? // Get reference to Vec<PrintAmsData> if it exists, otherwise return None
    //         .iter() // Create an iterator over Vec<PrintAmsData>
    //         .flat_map(|ams_data| &ams_data.tray) // Flatten all trays into a single iterator
    //         .find(|tray| tray.id == target_id) // Find the tray with the matching id
    // }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filament {
    // this is really calibrations, not filaments
    pub filament_id: String,
    pub name: String,
    pub k_value: String,
    // pub n_coef: String, // doen't exist in H2D
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting_id: Option<String>,
    // pub tray_id: Option<i32>, // ??? why is it here? In extrusion_cali_set it can exist (case when adding new calibration)
    pub cali_idx: i32, // Need to switch to optional since in extrusion_cali_set it is missing at least sometimes (case when adding new calibration)
    pub nozzle_id: Option<String>,
    pub extruder_id: Option<i32>,
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
    pub mc_percent: Option<i32>,
    pub mc_remaining_time: Option<i32>,
    // pub ams_status: Option<i64>,
    // pub ams_rfid_status: Option<i64>,
    // pub hw_switch_state: Option<i64>,
    // pub spd_mag: Option<i64>,
    // pub spd_lvl: Option<i64>,
    pub print_error: Option<i32>,
    // pub lifecycle: Option<String>,
    // pub wifi_signal: Option<String>,
    pub gcode_state: Option<GcodeState>,
    #[serde(default, serialize_with = "option_i32_as_str_se", deserialize_with = "option_i32_as_str_de")]
    pub gcode_file_prepare_percent: Option<i32>,
    // pub queue_number: Option<i64>,
    // pub queue_total: Option<i64>,
    // pub queue_est: Option<i64>,
    // pub queue_sts: Option<i64>,
    pub project_id: Option<String>,
    // pub profile_id: Option<String>,
    // pub task_id: Option<String>,
    // pub subtask_id: Option<String>,
    pub subtask_name: Option<String>,
    pub ams_mapping: Option<Vec<i32>>,
    pub ams_mapping2: Option<Vec<AmsMapping2Entry>>,
    pub gcode_file: Option<String>,
    // pub stg: Option<Vec<Value>>,
    pub stg_cur: Option<i32>,
    // pub print_type: Option<String>,
    // pub home_flag: Option<i64>,
    // pub mc_print_line_number: Option<String>,
    // pub mc_print_sub_stage: Option<i64>,
    // pub sdcard: Option<bool>,
    // pub force_upgrade: Option<bool>,
    // pub mess_production_state: Option<String>,
    pub layer_num: Option<i32>,
    pub total_layer_num: Option<i32>,
    // pub s_obj: Option<Vec<Value>>,
    // pub fan_gear: Option<i64>,
    pub hms: Option<Vec<Hms>>,
    // pub online: Option<PrintOnline>,
    pub ams: Option<PrintAms>,
    // pub ipcam: Option<PrintIpcam>,
    pub vt_tray: Option<PrintTray>, // was PrintVtTray
    pub vir_slot: Option<Vec<PrintTray>>,
    // pub lights_report: Option<Vec<PrintLightsReport>>,
    // pub upgrade_state: Option<PrintUpgradeState>,
    pub command: Option<String>,
    pub param: Option<String>, // "Metadata/plate_1.gcode"
    pub url: Option<String>,   // "https://or-cloud-upload-prod.s3.dualstack.us-west-2.amazonaws.com/users/1728685496/models/20250..."
    pub use_ams: Option<bool>,
    // #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    // If field needed, need to support it both as integer and as a string
    // pub plate_idx: Option<u32>, // !!!!! "1", or 1 - on X1C in cloud mode it is 1, in P1S it is "1",
    // pub msg: Option<i64>,
    pub sequence_id: Option<String>,

    // Added by me , were missing in original structure definition, from announcement on filament change following a change from slicer.
    // It not neccesarily arrive in a larger message so need to process it.
    pub nozzle_temp_max: Option<u32>,
    pub nozzle_temp_min: Option<u32>,
    // pub setting_id: Option<String>, // contains something similar to tray_info_idx, see below, so for now ignoring it
    pub tray_color: Option<String>,
    pub tray_id: Option<i32>,
    pub slot_id: Option<i32>,
    pub ams_id: Option<i32>,
    pub cali_idx: Option<i32>,
    pub tray_info_idx: Option<String>,
    pub tray_type: Option<String>,
    pub reason: Option<String>,
    pub result: Option<String>,

    pub nozzle_diameter: Option<String>, // sometimes received, required so to be sent in extruder_cali commane as below after filament setting (like slicer)
    pub filament_id: Option<String>,
    pub filaments: Option<Vec<Filament>>,
    pub fun: Option<String>,
    pub device: Option<PrintDevice>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hms {
    pub attr: Option<i32>,
    pub code: Option<i32>,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDevice {
    pub extruder: Option<PrintDeviceExtruder>,
    // ignore failure to parse due to X1C fw 01.08.02.00 (last before authorization)
    // that has nozzle but completely different than H2D and not in use for X1C
    #[serde(default, deserialize_with = "ignore_errors")]
    pub nozzle: Option<PrintDeviceNozzle>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDeviceExtruder {
    pub info: Vec<PrintDeviceExtruderInfo>,
    pub state: Option<i32>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDeviceExtruderInfo {
    pub id: i32,
    pub snow: i32,
    pub spre: i32,
    pub star: i32,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDeviceNozzle {
    pub info: Vec<PrintDeviceNozzleInfo>,
    pub exist: Option<i32>,
    pub state: Option<i32>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDeviceNozzleInfo {
    pub id: i32,
    pub diameter: f32,
    #[serde(rename = "type")] // type is reserved in Rust
    pub nozzle_type: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintAms {
    // Several AMS's - AMS as a System
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ams: Option<Vec<PrintAmsData>>, // Vector of AMS's
    pub ams_exist_bits: Option<String>,
    pub tray_exist_bits: Option<String>,
    pub tray_is_bbl_bits: Option<String>,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub tray_tar: Option<i32>,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub tray_now: Option<i32>,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub tray_pre: Option<i32>,
    pub tray_read_done_bits: Option<String>,
    pub tray_reading_bits: Option<String>,
    // pub version: Option<i64>,
    // pub insert_flag: Option<bool>,
    // pub power_on_flag: Option<bool>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintAmsData {
    // A Specific AMS
    #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub id: u32,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub humidity: Option<i32>,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub humidity_raw: Option<i32>,
    #[serde(default, serialize_with = "option_as_str_se", deserialize_with = "option_as_str_de")]
    pub temp: Option<f32>,
    pub tray: Vec<PrintTray>, // Vector of Trays
    #[serde(default, serialize_with = "option_u32_as_str_hex_se", deserialize_with = "option_u32_as_str_hex_de")]
    pub info: Option<u32>,
}

// An AMS Tray
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintTray {
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    pub id: Option<u32>, // Tray Id
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
    pub cols: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    pub nozzle_temp_max: Option<u32>, // e.g. 250
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, serialize_with = "option_u32_as_str_se", deserialize_with = "option_u32_as_str_de")]
    pub nozzle_temp_min: Option<u32>, // w.g. 190
    pub tag_uid: Option<String>,
    // pub remain: Option<i64>,
    // pub n: Option<f64>,
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

impl PrintTray {
    pub fn tray_colors(&self) -> Vec<String> {
        if let Some(cols) = &self.cols {
            if !cols.is_empty() {
                cols.clone()
            } else {
                self.tray_color
                    .clone()
                    .map(|c| alloc::vec![c])
                    .unwrap_or_else(|| alloc::vec![String::new()])
            }
        } else {
            self.tray_color
                .clone()
                .map(|c| alloc::vec![c])
                .unwrap_or_else(|| alloc::vec![String::new()])
        }
    }
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

////////////////////////////////////////////////////////////
// Commands
////////////////////////////////////////////////////////////

pub trait MqttCommand {
    fn set_sequence_id(&mut self, sequence_id: String);
}

macro_rules! impl_print_mqtt_command {
    ($ty:ty) => {
        impl MqttCommand for $ty {
            fn set_sequence_id(&mut self, sequence_id: String) {
                self.print.sequence_id = sequence_id;
            }
        }
    };
}

////////////////////////////////////////////////////////////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushAllCommand {
    pub pushing: PushAll, // ams_filament_setting
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushAll {
    pub command: String, // ams_filament_setting
    pub sequence_id: String,
}

impl PushAllCommand {
    pub fn new() -> Self {
        Self {
            pushing: PushAll {
                command: String::from("pushall"),
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl MqttCommand for PushAllCommand {
    fn set_sequence_id(&mut self, sequence_id: String) {
        self.pushing.sequence_id = sequence_id;
    }
}

////////////////////////////////////////////////////////////

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmsFilamentSettingCommand {
    print: AmsFilamentSetting,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmsFilamentSetting {
    pub command: String, // ams_filament_setting
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub ams_id: i32,
    // #[serde(serialize_with = "u32_as_str_se", deserialize_with = "u32_as_str_de")]
    pub tray_id: i32,
    pub slot_id: i32,
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

#[allow(clippy::too_many_arguments)]
impl AmsFilamentSettingCommand {
    pub fn new(
        ams_id: i32,
        tray_id: i32,
        slot_id: i32,
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
                slot_id,
                tray_info_idx: String::from(tray_info_idx),
                setting_id: setting_id.map(String::from),
                tray_color: String::from(tray_color),
                nozzle_temp_min,
                nozzle_temp_max,
                tray_type: String::from(tray_type),
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl_print_mqtt_command!(AmsFilamentSettingCommand);

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
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl_print_mqtt_command!(ExtrusionCaliGetCommand);

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
    pub ams_id: i32,
    pub tray_id: i32,
    pub slot_id: i32,
    pub sequence_id: String,
}

impl ExtrusionCaliSelCommand {
    pub fn new(nozzle_diameter: &str, ams_id: i32, tray_id: i32, slot_id: i32, filament_id: &str, cali_idx: Option<i32>) -> Self {
        Self {
            print: ExtrusionCaliSel {
                command: String::from("extrusion_cali_sel"),
                cali_idx: cali_idx.unwrap_or(-1),
                filament_id: String::from(filament_id),
                nozzle_diameter: String::from(nozzle_diameter),
                ams_id,
                tray_id,
                slot_id,
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl_print_mqtt_command!(ExtrusionCaliSelCommand);

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

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliSetFilament {
    // this is really calibrations, not filaments
    pub ams_id: i32,
    pub extruder_id: i32,
    pub filament_id: String,
    pub k_value: String,
    pub n_coef: String, // doen't exist in H2D
    pub name: String,
    pub nozzle_diameter: String,
    pub nozzle_id: String,
    pub setting_id: String,
    pub slot_id: i32,
    pub tray_id: i32, // ??? why is it here? In extrusion_cali_set it can exist (case when adding new calibration)
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliSetCommand {
    print: ExtrusionCaliSet,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrusionCaliSet {
    pub command: String, // extrusion_cali_sel
    pub filaments: Vec<ExtrusionCaliSetFilament>,
    pub nozzle_diameter: String,
    pub sequence_id: String,
}

impl ExtrusionCaliSetCommand {
    pub fn new(extruder_id: i32, nozzle_diameter: &str, nozzle_id: &str, filament_id: &str, setting_id: &str, k_value: &str, name: &str) -> Self {
        let filaments = alloc::vec![ExtrusionCaliSetFilament {
            ams_id: 0,
            extruder_id,
            filament_id: filament_id.to_string(),
            k_value: k_value.to_string(),
            n_coef: "0.000000".to_string(),
            name: name.to_string(),
            nozzle_diameter: nozzle_diameter.to_string(),
            nozzle_id: nozzle_id.to_string(),
            setting_id: setting_id.to_string(),
            slot_id: 0,
            tray_id: -1,
        }];
        Self {
            print: ExtrusionCaliSet {
                command: String::from("extrusion_cali_set"),
                filaments,
                nozzle_diameter: nozzle_diameter.to_string(),
                sequence_id: "1".to_string(),
            },
        }
    }
}

impl_print_mqtt_command!(ExtrusionCaliSetCommand);

// {
//     "print":{
//         "command":"extrusion_cali_set",
//         "filaments":[
//             {
//                 "ams_id":0,
//                 "extruder_id":0,
//                 "filament_id":"Pb79127b",
//                 "k_value":"0.123000",
//                 "n_coef":"0.000000",
//                 "name":"setting-name",
//                 "nozzle_diameter":"0.4",
//                 "nozzle_id":"HS00-0.4",
//                 "setting_id":"PFUSced7c16e6d1066",
//                 "slot_id":0,
//                 "tray_id":-1}
//         ],
//         "nozzle_diameter":"0.4",
//         "sequence_id":"21930",
//     }
// }

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct AmsMapping2Entry {
    pub ams_id: i32,
    pub slot_id: i32,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Deserialize, Serialize, Debug, PartialEq, Clone, Copy)]
pub enum GcodeState {
    Unknown,
    IDLE,
    SLICING,
    PREPARE,
    RUNNING,
    FINISH,
    FAILED,
    PAUSE,
    #[serde(other)]
    Unsupported,
}

// {
//     "info": {
//         "sequence_id": "0",
//         "command": "get_version"
//     }
// }

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetVersionCommand {
    pub info: GetVersion, // ams_filament_setting
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetVersion {
    pub command: String, // ams_filament_setting
    pub sequence_id: String,
}

impl GetVersionCommand {
    pub fn new() -> Self {
        Self {
            info: GetVersion {
                command: String::from("get_version"),
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl MqttCommand for GetVersionCommand {
    fn set_sequence_id(&mut self, sequence_id: String) {
        self.info.sequence_id = sequence_id;
    }
}

// {
//  "pushing": {
//        "command": "pushall",
//        "sequence_id": "1"
//  }
// }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrintCommand {
    Stop,
    Pause,
    Resume,
}

impl PrintCommand {
    pub fn get_command(&self) -> &str {
        match self {
            PrintCommand::Stop => "stop",
            PrintCommand::Pause => "pause",
            PrintCommand::Resume => "resume",
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterCommand {
    print: Printer,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Printer {
    pub command: String, // extrusion_cali_get
    pub param: String,
    pub sequence_id: String,
}

impl PrinterCommand {
    pub fn new(printer_command: PrintCommand) -> Self {
        Self {
            print: Printer {
                command: printer_command.get_command().to_string(),
                param: String::from(""),
                sequence_id: String::from("10001"),
            },
        }
    }
}

impl_print_mqtt_command!(PrinterCommand);

//////////////////////////////////////////////////////////////////////////////
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub info: InfoData,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoData {
    pub command: String,
    pub sequence_id: String,
    pub module: Vec<InfoModule>,
    pub result: Option<String>,
    pub reason: Option<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoModule {
    pub name: String,
    pub project_name: Option<String>,
    pub product_name: Option<String>,
    pub sw_ver: String,
    pub hw_ver: String,
    pub sn: String,
    pub flag: Option<i32>,
    pub loader_ver: Option<String>,
    pub ota_ver: Option<String>,
}

// { "info": { "command": "get_version", "module": [
//     { "flag": 3, "hw_ver": "N/A", "name": "ota", "sn": "N/A", "sw_ver": "01.08.02.00" },
//     { "flag": 0, "hw_ver": "AMS08", "name": "ams/0", "sn": "00600A452223458", "sw_ver": "00.00.06.44" },
//     { "flag": 0, "hw_ver": "MC07", "name": "mc", "sn": "00206A442501491", "sw_ver": "00.00.27.26" },
//     { "flag": 0, "hw_ver": "SMC01", "name": "sm", "sn": "N/A", "sw_ver": "00.00.27.26" },
//     { "flag": 0, "hw_ver": "TH09", "name": "th", "sn": "00306D441004413", "sw_ver": "00.00.07.12" },
//     { "flag": 0, "hw_ver": "AP05", "name": "ap", "sn": "00M09D460801484", "sw_ver": "00.00.32.39" } ],
//     "sequence_id": "20006" } }[0m

// {"info":{"command":"get_version","sequence_id":"47663","module":[
//     {"name":"ota","sw_ver":"01.08.01.00","hw_ver":"OTA","loader_ver":"00.00.00.00","sn":"01P00A3A2900822","product_name":"Bambu Lab P1S","visible":true,"new_ver":"01.08.02.00","flag":15},
//     {"name":"esp32","sw_ver":"01.11.35.47","hw_ver":"AP04","loader_ver":"00.00.00.00","sn":"01P00A3A2900822","product_name":"","visible":false,"flag":0},
//     {"name":"mc","sw_ver":"00.01.32.85","hw_ver":"MC07","loader_ver":"00.00.00.28","sn":"01D06B3A0901973","product_name":"","visible":false,"flag":0},
//     {"name":"th","sw_ver":"00.00.09.95","hw_ver":"TH09","loader_ver":"00.00.00.14","sn":"01E06B382801461","product_name":"","visible":false,"flag":0},
//     {"name":"ams/0","sw_ver":"00.01.06.62","hw_ver":"AMS08","loader_ver":"00.00.00.00","sn":"00600A3A1903180","product_name":"AMS (1)","visible":true,"flag":0},
//     {"name":"ams/1","sw_ver":"00.01.06.62","hw_ver":"AMS08","loader_ver":"00.00.00.00","sn":"00600A482719744","product_name":"AMS (2)","visible":true,"flag":0}],"result":"success","reason":""}}[0m

// { "info": { "command": "get_version", "module": [
//     { "flag": 3, "hw_ver": "N/A", "loader_ver": "00.00.00.00", "name": "ota", "product_name": "Bambu Lab X1-Carbon", "sn": "00M09D492100781", "sw_ver": "01.10.00.00", "visible": true },
//     { "flag": 0, "hw_ver": "N3F05", "loader_ver": "00.00.00.00", "name": "n3f/0", "product_name": "AMS 2 Pro (1)", "sn": "19C06A4B2408067", "sw_ver": "02.00.19.68", "visible": true },
//     { "flag": 0, "hw_ver": "MC07", "loader_ver": "00.00.00.28", "name": "mc", "product_name": "", "sn": "00206A482312627", "sw_ver": "00.00.32.96", "visible": false },
//     { "flag": 0, "hw_ver": "N/A", "loader_ver": "00.00.00.00", "name": "mc-sub", "product_name": "", "sn": "N/A", "sw_ver": "00.00.32.96", "visible": false },
//     { "flag": 0, "hw_ver": "TH09", "loader_ver": "00.00.00.14", "name": "th", "product_name": "", "sn": "00306D483105759", "sw_ver": "00.00.07.12", "visible": false },
//     { "flag": 0, "hw_ver": "AP05", "loader_ver": "00.00.01.08", "name": "ap", "product_name": "", "sn": "00M09D492100781", "sw_ver": "00.00.51.09", "visible": false } ], "sequence_id": "20034" } }

// Helpers ///////////////////////////////////////////////////////////

fn u32_as_str_se<S>(x: &u32, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

fn i32_as_str_se<S>(x: &i32, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

#[allow(dead_code)]
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

fn option_i32_as_str_se<S>(value: &Option<i32>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(v) => i32_as_str_se(v, serializer),
        None => serializer.serialize_none(),
    }
}

fn option_i32_as_str_de<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    let option: Option<String> = Option::deserialize(deserializer)?;
    option.as_deref().map(|s| s.parse::<i32>().map_err(serde::de::Error::custom)).transpose()
}

fn as_str_se<T, S>(x: &T, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: core::fmt::Display,
{
    s.serialize_str(&format!("{}", x))
}

#[allow(dead_code)]
fn as_str_de<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: core::str::FromStr,
    <T as core::str::FromStr>::Err: core::fmt::Display,
{
    let s: &str = Deserialize::deserialize(deserializer)?;
    s.parse::<T>().map_err(serde::de::Error::custom)
}

fn option_as_str_se<T, S>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: core::fmt::Display,
{
    match value {
        Some(v) => as_str_se::<T, S>(v, serializer),
        None => serializer.serialize_none(),
    }
}

fn option_as_str_de<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: core::str::FromStr,
    <T as core::str::FromStr>::Err: core::fmt::Display,
{
    let option: Option<String> = Option::deserialize(deserializer)?;
    option.as_deref().map(|s| s.parse::<T>().map_err(serde::de::Error::custom)).transpose()
}

fn u32_as_str_hex_se<S>(x: &u32, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{:x}", x))
}

#[allow(dead_code)]
fn u32_as_str_hex_de<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let s: &str = Deserialize::deserialize(deserializer)?;
    u32::from_str_radix(s, 16).map_err(serde::de::Error::custom)
}

fn option_u32_as_str_hex_se<S>(x: &Option<u32>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match x {
        Some(v) => u32_as_str_hex_se(v, s),
        None => s.serialize_none(),
    }
}

fn option_u32_as_str_hex_de<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    opt.as_deref()
        .map(|s| u32::from_str_radix(s, 16).map_err(serde::de::Error::custom))
        .transpose()
}

fn ignore_errors<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(T::deserialize(deserializer).ok())
}
