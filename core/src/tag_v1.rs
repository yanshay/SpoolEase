use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use framework::error;
use hashbrown::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    bambu::{
        calibration::{Calibration, formatted_k_value},
        filament::FilamentInfo,
    },
    spool_record::SpoolRecord,
    tag_standards::SPOOLEASE_V1_TAG_TYPE,
};

const FILAMENT_URL_PREFIX: &str = "https://info.filament3d.org/";
const FILAMENT_URL_PREFIX_V1_TAG: &str = "https://info.filament3d.org/V1";

#[derive(Debug)]
pub enum Error {
    ParseError,
    MissingFields,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct OldTagCalibration {
    pub filament_id: String,
    pub k_value: String,
    // n_coef: f32,
    pub setting_id: Option<String>,
    pub name: String,
    pub cali_idx: i32,
}

impl OldTagCalibration {
    pub fn new_minimal(k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            k_value: formatted_k_value(k_value),
            filament_id: String::from(filament_id),
            setting_id: if setting_id.is_empty() { None } else { Some(setting_id.to_string()) },
            name: String::from(name),
            cali_idx,
        }
    }
}

impl From<&Calibration> for OldTagCalibration {
    fn from(v: &Calibration) -> Self {
        // this "Filament" in bambu_api is really calibrations, bambulab naming ...
        Self {
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: v.k_value.clone(),
            // n_coef: f32::from_str(&v.n_coef).unwrap_or(-1.0),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct TagInformationV1 {
    pub id: Option<String>,
    pub tag_id: Option<Vec<u8>>,
    pub filament: Option<FilamentInfo>,
    pub weight_advertised: Option<i32>,
    pub weight_core: Option<i32>,
    pub weight_new: Option<i32>,
    pub brand: Option<String>,
    pub filament_subtype: Option<String>,
    pub color_name: Option<String>,
    pub note: Option<String>,
    pub encode_time: Option<i32>,
    // for old tags support (where K was on tag)
    pub calibrations: HashMap<String, OldTagCalibration>,
    pub calibrations_printer_name: String, // has value only if calibrations has any value
    pub calibrations_printer_uuid: String, // has value only if calibrations has any value
}

impl TagInformationV1 {
    pub fn _from(v: &SpoolRecord, min_max_temp: (u32, u32)) -> Self {
        // TODO: need to deal with case of no data or partial data for filament_info?
        let filament_info = {
            FilamentInfo {
                tray_info_idx: v.slicer_filament.clone(),
                tray_type: v.material_type.clone(),
                tray_color: v.color_code.clone(),
                nozzle_temp_max: min_max_temp.1,
                nozzle_temp_min: min_max_temp.0,
            }
        };
        Self {
            id: Some(v.id.clone()),
            tag_id: if v.tag_id.is_empty() {
                None
            } else {
                hex::decode(v.tag_id.as_bytes()).ok()
            },
            filament: Some(filament_info),
            weight_advertised: v.weight_advertised,
            weight_core: v.weight_core,
            weight_new: v.weight_new,
            brand: if v.brand.is_empty() { None } else { Some(v.brand.clone()) },
            filament_subtype: if v.material_subtype.is_empty() {
                None
            } else {
                Some(v.material_subtype.clone())
            },
            color_name: if v.color_name.is_empty() { None } else { Some(v.color_name.clone()) },
            note: if v.note.is_empty() { None } else { Some(v.note.clone()) },
            encode_time: v.encode_time,
            calibrations: HashMap::new(),
            calibrations_printer_name: String::new(),
            calibrations_printer_uuid: String::new(),
        }
    }
    pub fn to_spool_rec(&self) -> SpoolRecord {
        let empty = &String::new();
        // let empty_filament = &FilamentInfo::default(),
        let calibration_filament_id = if let Some(key) = self.calibrations.keys().next() {
            self.calibrations.get(key).map(|c| c.filament_id.clone())
        } else {
            None
        };
        SpoolRecord {
            id: self.id.as_ref().unwrap_or(empty).clone(),
            tag_id: self.tag_id.as_ref().map(hex::encode_upper).unwrap_or_default(),
            material_type: self.filament.as_ref().map(|f| f.tray_type.clone()).unwrap_or_default(),
            material_subtype: self.filament_subtype.as_ref().unwrap_or(empty).clone(),
            color_name: self.color_name.as_ref().unwrap_or(empty).clone(),
            color_code: self.filament.as_ref().map(|f| f.tray_color.clone()).unwrap_or_default(),
            note: self.note.as_ref().unwrap_or(empty).clone(),
            brand: self.brand.as_ref().unwrap_or(empty).clone(),
            weight_advertised: self.weight_advertised,
            weight_core: self.weight_core,
            weight_new: self.weight_new,
            weight_current: None,
            slicer_filament: calibration_filament_id.unwrap_or_default(),
            added_time: None,
            encode_time: self.encode_time,
            added_full: self.weight_new.map(|_| true), // Some(true) if weight_new exists
            consumed_since_add: 0.0,
            consumed_since_weight: 0.0,
            ext_has_k: false, // this means if in the store, so need to be set to true when saving store
            data_origin: SPOOLEASE_V1_TAG_TYPE.to_string(),
            tag_type: String::new(),
            assigned_location: String::new(),
            actual_location: String::new(),
            spools_count: 1,
        }
    }
}

impl TagInformationV1 {
    // pub fn _to_v1_descriptor(&self, _printer_name: Option<&str>, _printer_uuid: Option<&str>) -> Option<String> {
    //     let brand_part = self
    //         .brand
    //         .as_ref()
    //         .map(|s| format!("&B={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let filament_subtype_part = self
    //         .filament_subtype
    //         .as_ref()
    //         .map(|s| format!("&MS={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let color_name_part = self
    //         .color_name
    //         .as_ref()
    //         .map(|s| format!("&CN={}", my_encode_to_url_part(s)))
    //         .unwrap_or_default();
    //     let note_part = self.note.as_ref().map(|s| format!("&N={}", my_encode_to_url_part(s))).unwrap_or_default();
    //
    //     let mut material_part = String::new();
    //     let mut color_part = String::new();
    //     let mut nozzle_temp_min_part = String::new();
    //     let mut nozzle_temp_max_part = String::new();
    //     let mut tray_info_idx_part = String::new();
    //
    //     if let Some(filament) = &self.filament {
    //         material_part = if filament.tray_type.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&M={}", filament.tray_type.trim()) // changed due to a bug in inventory that got CR into material
    //         };
    //         color_part = if filament.tray_color.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&C={}", filament.tray_color)
    //         };
    //         nozzle_temp_min_part = if filament.nozzle_temp_min == 0 {
    //             String::new()
    //         } else {
    //             format!("&NN={}", filament.nozzle_temp_min)
    //         };
    //         nozzle_temp_max_part = if filament.nozzle_temp_max == 0 {
    //             String::new()
    //         } else {
    //             format!("&NX={}", filament.nozzle_temp_max)
    //         };
    //         tray_info_idx_part = if filament.tray_info_idx.is_empty() {
    //             String::new()
    //         } else {
    //             format!("&FI={}", filament.tray_info_idx)
    //         };
    //     }
    //     let advertised_weight_part = self.weight_advertised.map(|v| format!("&WA={}", v)).unwrap_or_default();
    //     let weight_core_part = self.weight_core.map(|v| format!("&WC={}", v)).unwrap_or_default();
    //     let weight_new_part = self.weight_new.map(|v| format!("&WN={}", v)).unwrap_or_default();
    //     let encode_time_part = self.encode_time.map(|v| format!("&DE={}", v)).unwrap_or_default();
    //
    //     Some(format!("{FILAMENT_URL_PREFIX}V1?ID={TAG_PLACEHOLDER}{encode_time_part}{material_part}{filament_subtype_part}{color_part}{color_name_part}{brand_part}{advertised_weight_part}{weight_core_part}{weight_new_part}{nozzle_temp_min_part}{nozzle_temp_max_part}{note_part}{tray_info_idx_part}"))
    // }

    // TODO: remove all the printer parts, should only parse, the rest of the matching thould go elsewhere

    pub fn from_v1_descriptor(descriptor: &str) -> Result<Self, Error> {
        let mut filament_info_result = FilamentInfo::new();
        let mut calibrations_result = HashMap::new();
        let mut weight_advertised = None;
        let mut weight_core = None;
        let mut weight_new = None;
        let mut brand = None;
        let mut filament_subtype = None;
        let mut color_name = None;
        let mut note = None;
        let mut tag_id = None;
        let mut encode_time = None;

        if !(descriptor.starts_with(FILAMENT_URL_PREFIX_V1_TAG)) {
            // below the code should still use the base FILAMENT_URL_PREFIX
            return Err(Error::ParseError);
        }
        // let descriptor = descriptor.trim_start_matches(FILAMENT_URL_PREFIX);

        let mut id = false;
        let mut v = false;
        let mut m = false;
        let mut _fi = false;
        let mut c = false;
        let mut _nn = false;
        let mut _nx = false;
        for param in descriptor.strip_prefix(FILAMENT_URL_PREFIX).unwrap_or(descriptor).split(['&', '/', '?']) {
            if param == "V1" {
                v = true;
                continue;
            }
            if let Some((param_name, param_value)) = param.split_once("=") {
                // note that this process only values of name=value. Others are currently not processed here (like V1, and TagId)
                match param_name {
                    // Tag ID
                    "ID" => {
                        id = true;
                        if let Ok(tag_id_bytes) = URL_SAFE_NO_PAD.decode(param_value) {
                            tag_id = Some(tag_id_bytes);
                        } else {
                            error!("Error decoding tag id from tag descriptor {descriptor}");
                            return Err(Error::ParseError);
                        }
                    }
                    // Material / Tray Type (material code in some other form)
                    "M" => {
                        filament_info_result.tray_type = String::from(param_value.trim()); // trimmed due to a bug in inventory that added CR, hope won't make issues
                        m = true;
                    }
                    // Color / Tray Color
                    "C" => {
                        filament_info_result.tray_color = String::from(param_value);
                        c = true;
                    }
                    // Nozzle miN Temp
                    "NN" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_min = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        _nn = true;
                    }
                    // Nozzle maX Temp
                    "NX" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_max = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        _nx = true;
                    }
                    // "K4" | "K2" | "K6" | "K8" => (),
                    // // Filament Id/ Tray Index (material code in some form) - looks like Bambu specific
                    "FI" => {
                        filament_info_result.tray_info_idx = String::from(param_value);
                        _fi = true;
                    }
                    "WA" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_advertised = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    "WC" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_core = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    "WN" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            weight_new = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    "B" => {
                        brand = Some(my_decode_from_url_part(param_value));
                    }
                    "MS" => {
                        // Material Subtype
                        filament_subtype = Some(my_decode_from_url_part(param_value));
                    }
                    "CN" => {
                        color_name = Some(my_decode_from_url_part(param_value));
                    }
                    "N" => {
                        note = Some(my_decode_from_url_part(param_value));
                    }
                    "DE" => {
                        if let Ok(ret_val) = param_value.parse::<i32>() {
                            encode_time = Some(ret_val);
                        } else {
                            return Err(Error::ParseError);
                        }
                    }
                    _ => (), //return Err(Error::ParseError), TODO: verify match to pattern, or even run what's coming next inside here
                }
            }
        }

        // Processing of K Factor //////////////////
        // TODO: IMPORTANT: This assumes a single printer info, the printer name is thrown away.
        // Therefore, scanning/encoding to/from the staging at this point probably change information to current printer, which is not good in case of multiple printers
        // An easy solution is to store also copy of original string in staging and just encode it directly

        // First just collect data from tag

        let mut calibrations_printer_name = "";
        let mut calibrations_printer_uuid = "";
        // Second pass on parts that need to be processed after the first
        let re = Regex::new(r"^(.*)\((K.*)\)$").unwrap();
        for param in descriptor.split(&['/', '&', '?']) {
            let mut param = param;
            if let Some(captures) = re.captures(param) {
                // to get k data use match 2
                if let Some(param_match) = captures.get(2) {
                    param = param_match.as_str();
                }
                if let Some(param_match) = captures.get(1) {
                    let printer_name_and_uuid = param_match.as_str();
                    (calibrations_printer_name, calibrations_printer_uuid) =
                        printer_name_and_uuid.split_once('~').unwrap_or((printer_name_and_uuid, ""));
                }
                // to get the printer name (formatted as name~serial , use match 1 and don't forget to my_decode_from_url_part the data
                // currently not used, could compare to current printer name and ignore
            }

            // this is just calibrations loaded from the filament, without any matching, all with cali_idx = -1
            if let Some((param_name, param_value)) = param.split_once("=") {
                match param_name {
                    // K - Pressure Advance Factor for Nozzle Diameter 0.4, 0.2, 0.6, 0.8
                    "K4" | "K2" | "K6" | "K8" => {
                        //TODO: Currently we set the filament calibration only if it is found in the printer tables
                        // In the future consider adding the calibarion to the printer if it's not available
                        let nozzle_diameter_digit = param_name.chars().nth(1).unwrap();
                        let nozzle_diameter = format!("0.{}", nozzle_diameter_digit);

                        let mut k_parts = param_value.splitn(3, '~');

                        let k_value = k_parts.next().ok_or(Error::ParseError)?.trim_end_matches("0");
                        let setting_id = k_parts.next().ok_or(Error::ParseError)?;
                        let name = k_parts.next().ok_or(Error::ParseError)?;
                        let name = my_decode_from_url_part(name);
                        let calibration = OldTagCalibration::new_minimal(k_value, &filament_info_result.tray_info_idx, setting_id, &name, -1);
                        calibrations_result.insert(nozzle_diameter, calibration);
                    }
                    _ => (), // previous run already identified unrecognized parameters, here we skip also those that were ok so can't error
                }
            }
        }

        if v && id && m && c {
            Ok(Self {
                id: None,
                // origin_descriptor: descriptor.to_string(),
                tag_id,
                filament: Some(filament_info_result),
                weight_advertised,
                weight_core,
                weight_new,
                brand,
                filament_subtype,
                color_name,
                note,
                encode_time,

                // for old k calibration handling
                calibrations: calibrations_result,
                calibrations_printer_name: my_decode_from_url_part(calibrations_printer_name),
                calibrations_printer_uuid: calibrations_printer_uuid.to_string(),
            })
        } else {
            Err(Error::MissingFields)
        }
    }
}

const ENCODING_TABLE: [(char, &str); 9] = [
    ('%', "%25"),
    ('/', "%2F"),
    ('&', "%26"),
    ('?', "%3F"),
    (' ', "%20"),
    ('#', "%23"),
    ('(', "%28"),
    (')', "%29"),
    ('~', "%7E"),
];

// static ENCODING_MAP: Lazy<Mutex<CriticalSectionRawMutex, HashMap<char, &str>>> = Lazy::new(|| {
//     let char_hashmap: HashMap<char, &str> = ENCODING_TABLE.into_iter().collect();
//     Mutex::new(char_hashmap)
// });

fn my_decode_from_url_part(text: &str) -> String {
    // % must be last (because some originated from encodings and will need to be replaced first)
    // let name = name.replace("%7E", "/").replace("%2F", "/").replace("%28", "(").replace("%29", ")").replace("%26", "&").replace("%3F", "?").replace("%20", " ").replace("%25", "%");
    efficient_decode(text, &ENCODING_TABLE)
}

// fn my_encode_to_url_part(text: &str) -> String {
//     // % must be first (because later added)
//     // let name = name.replace("%", "%25").replace("/", "%2F").replace("&", "%26").replace("?", "%3F").replace(" ", "%20").replace("(", "%28").replace(")", "%29").replace( "~","%7E");
//     ENCODING_MAP.lock(|encoding_map| efficient_encode(text, encoding_map))
// }

///// Encodes specific characters in a string based on a provided mapping.
///// Minimizes allocations while still returning a String.
/////
///// # Arguments
///// * `input` - The string to encode
///// * `char_map` - A mapping of characters to their encoded string representation
/////
///// # Returns
///// The encoded string
//pub fn efficient_encode(input: &str, char_map: &HashMap<char, &str>) -> String {
//    // Pre-calculate output size to avoid reallocations
//    let mut capacity = 0;
//    for c in input.chars() {
//        capacity += match char_map.get(&c) {
//            Some(replacement) => replacement.len(),
//            None => c.len_utf8(),
//        };
//    }
//
//    // Pre-allocate output string with exact capacity needed
//    let mut result = String::with_capacity(capacity);
//
//    // Process each character
//    for c in input.chars() {
//        match char_map.get(&c) {
//            Some(replacement) => result.push_str(replacement),
//            None => result.push(c),
//        }
//    }
//
//    result
//}

/// Decodes a string by replacing encoded sequences with their original characters.
/// Minimizes allocations while still returning a String.
///
/// # Arguments
/// * `input` - The string to decode
/// * `char_map` - A mapping of characters to their encoded string representation
///
/// # Returns
/// The decoded string
pub fn efficient_decode(input: &str, char_table: &[(char, &str)]) -> String {
    // Pre-allocate with input size (likely sufficient since decoding usually results in shorter strings)
    let mut result = String::with_capacity(input.len());

    // Use slice for efficient substring comparison
    let input_bytes = input.as_bytes();
    let mut i = 0;

    while i < input_bytes.len() {
        let mut found = false;

        // Try to match each encoded sequence at current position
        for (original, encoded) in char_table {
            let encoded_bytes = encoded.as_bytes();

            if i + encoded_bytes.len() <= input_bytes.len() && &input_bytes[i..i + encoded_bytes.len()] == encoded_bytes {
                result.push(*original);
                i += encoded_bytes.len();
                found = true;
                break;
            }
        }

        // If no encoded sequence matches, copy original character
        if !found {
            // Get one complete UTF-8 character
            let char_len = if (input_bytes[i] & 0x80) == 0 {
                1 // ASCII
            } else if (input_bytes[i] & 0xE0) == 0xC0 {
                2 // 2-byte UTF-8
            } else if (input_bytes[i] & 0xF0) == 0xE0 {
                3 // 3-byte UTF-8
            } else {
                4 // 4-byte UTF-8
            };

            // Safe because we're checking bounds and copying valid UTF-8 sequences
            if i + char_len <= input_bytes.len() {
                result.push_str(core::str::from_utf8(&input_bytes[i..i + char_len]).unwrap());
                i += char_len;
            } else {
                // Handle truncated UTF-8 at end of string (shouldn't happen with valid UTF-8)
                i += 1;
            }
        }
    }

    result
}
