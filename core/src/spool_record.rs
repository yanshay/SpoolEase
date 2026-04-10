use crate::utils::{deserialize_string_array, serialize_string_array};
use crate::{
    bambu::{
        NozzleType,
        calibration::{KInfo, KNozzleId},
    },
    csvdb::CsvDbId,
    tag_standards::{BambuLabTag, OpenPrintTagTag},
    types::FilamentSupInfo,
};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use derivative::Derivative;
use serde::{Deserialize, Deserializer, Serialize};
use shared::spool_tag::TAG_PLACEHOLDER;
use shared::utils::{
    deserialize_bool_yn_empty_n, deserialize_f32_base64, deserialize_optional, deserialize_optional_bool_yn, serialize_bool_yn, serialize_f32_base64,
    serialize_optional_bool_yn,
};

// TODO: think if to change it to get the spoolRecord from store (and it will hold Rc to store)
#[derive(Debug, Clone, Default)]
pub struct FullSpoolRecord {
    pub spool_rec: SpoolRecord,
    pub spool_rec_ext: SpoolRecordExt,
}

#[derive(Derivative)]
#[derivative(Default)]
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct SpoolRecord {
    pub id: String,
    #[serde(serialize_with = "serialize_string_array", deserialize_with = "deserialize_string_array")]
    pub tag_id: Vec<String>, // currently primary tag is first item
    pub material_type: String,    // 10
    pub material_subtype: String, // 10
    pub color_name: String,       // 10
    #[serde(serialize_with = "serialize_string_array", deserialize_with = "deserialize_string_array")]
    pub color_code: Vec<String>, // 8
    pub note: String,             // 40
    pub brand: String,            // 30
    #[serde(deserialize_with = "deserialize_optional")]
    pub weight_advertised: Option<i32>, // 4
    #[serde(deserialize_with = "deserialize_optional")]
    pub weight_core: Option<i32>, // 4
    #[serde(deserialize_with = "deserialize_optional")]
    pub weight_new: Option<i32>, // 4
    #[serde(deserialize_with = "deserialize_optional")]
    pub weight_current: Option<i32>, // 4
    #[serde(default)]
    pub slicer_filament: String,
    #[serde(default, deserialize_with = "deserialize_optional")]
    pub added_time: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    pub encode_time: Option<i32>,
    #[serde(default, serialize_with = "serialize_optional_bool_yn", deserialize_with = "deserialize_optional_bool_yn")]
    pub added_full: Option<bool>,
    #[serde(default, serialize_with = "serialize_f32_base64", deserialize_with = "deserialize_f32_base64")]
    pub consumed_since_add: f32,
    #[serde(default, serialize_with = "serialize_f32_base64", deserialize_with = "deserialize_f32_base64")]
    pub consumed_since_weight: f32,
    #[serde(default, serialize_with = "serialize_bool_yn", deserialize_with = "deserialize_bool_yn_empty_n")]
    pub ext_has_k: bool,
    #[serde(default)]
    pub data_origin: String,
    #[serde(default)]
    pub tag_type: String,
    #[serde(default)]
    pub assigned_location: String,
    #[serde(default)]
    pub actual_location: String,
    #[serde(default = "default_one")]
    #[serde(deserialize_with = "one_if_empty")]
    #[derivative(Default(value = "1"))]
    pub spools_count: i32,
    // !!! Don't Forget to set default for any field!
    // pub update_time
    // pub update_tag_fields_time
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub price: Option<()>,
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub grade/quality: Option<()>,
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub diameter: Option<()>,
    //
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub location: Option<()>,
    //
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub purchased: Option<()>,
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub opened: Option<()>,
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub dried: Option<()>,
    // #[serde(default,deserialize_with = "deserialize_optional_unit")]
    // pub last_used: Option<()>,
}

fn default_one() -> i32 {
    1
}

fn one_if_empty<'de, D>(d: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(d)?;
    Ok(match s.as_deref() {
        None | Some("") => 1,
        Some(v) => v.parse().map_err(serde::de::Error::custom)?,
    })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum OriginData {
    SpoolEaseV1 { uid: String, url: String },
    BambuLabTag { bambulab_tag: BambuLabTag },
    OpenPrintTagTag { openprinttag_tag: OpenPrintTagTag },
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SpoolRecordExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k_info: Option<KInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_data: Option<OriginData>,
}

impl SpoolRecordExt {
    pub fn get_calibration(&self, printer: &str, extruder: i32, diameter: &str, nozzle_type_code: NozzleType) -> Option<&KNozzleId> {
        // Note that this search for nozzle_type, not specific nozzle_id (so HS00 and not HS00-0.4)
        let calibration_nozzle_type_str = format!("H{}00-{}", if nozzle_type_code == NozzleType::HighFlow { 'H' } else { 'S' }, diameter);
        let nozzles = &self
            .k_info
            .as_ref()?
            .printers
            .get(printer)?
            .extruders
            .get(&extruder)?
            .diameters
            .get(diameter)?
            .nozzles;

        nozzles.get(&calibration_nozzle_type_str).or_else(|| {
            if nozzle_type_code == NozzleType::Standard {
                nozzles.get("")
            } else {
                None
            }
        })
    }
}

impl CsvDbId for SpoolRecord {
    fn id(&self) -> &String {
        &self.id
    }
}

const _TAG_URL_PREFIX_V2: &str = "https://info.filament3d.org/V2/"; // in 0.5.x
const TAG_URL_PREFIX_S1: &str = "https://tag.spoolease.io/S1/"; // starting 0.6.0-b.24
// Some(format!("{FILAMENT_URL_PREFIX}V1?ID={TAG_PLACEHOLDER}{encode_time_part}{material_part}
// {filament_subtype_part}{color_part}{color_name_part}{brand_part}{advertised_weight_part}{weight_core_part}{weight_new_part}{nozzle_temp_min_part}{nozzle_temp_max_part}{note_part}{tray_info_idx_part}"))
// TODO:
// 1. Add slicer_filament_name - derive from slicer,mfilament_code or from material_type if slicer not filled in, use get_filament_info for that
// 2. X Add temperatures - use get_filament_info for that
// 3. Add note - and fully url encode it
// 4. ? Add added time
// 5. {note_part}{tray_info_idx_part}"))
// 6. note (N)
// 8. slicner name (SN)
// 9. DA
impl SpoolRecord {
    pub fn linked_tag_ids(&self) -> impl Iterator<Item = &str> {
        self.tag_id.iter().map(String::as_str).filter(|tag_id| !tag_id.is_empty())
    }

    pub fn to_tag_descriptor_s1(&self, filament_sup_info: &Option<FilamentSupInfo>) -> Option<String> {
        let color_code = self
            .color_code
            .iter()
            .map(String::as_str)
            .filter(|color_code| !color_code.is_empty())
            .collect::<Vec<_>>()
            .join(";");
        if color_code.is_empty() {
            return None;
        }
        if self.id.is_empty() || self.material_type.is_empty() {
            return None;
        }
        let encode_time_part = part_opt("DE", &self.encode_time);
        let added_time_part = part_opt("DA", &self.added_time);
        let tag_id_part = format!("TG={TAG_PLACEHOLDER}");
        let id_part = part_val("ID", &self.id);
        let material_part = part_val("M", &self.material_type);
        let material_subtype_part = part_val("MS", &self.material_subtype);
        let brand_part = part_val("B", &self.brand);
        let color_code_part = part_val("CC", &color_code);
        let color_name_part = part_val("CN", &self.color_name);
        let weight_advertised_part = part_opt("WL", &self.weight_advertised);
        let weight_core_part = part_opt("WE", &self.weight_core);
        let weight_new_part = part_opt("WF", &self.weight_new);
        let slicer_filament_code_part = part_val("SC", &self.slicer_filament);
        let note_part = part_val("N", &self.note);
        let slicer_filament_name = filament_sup_info.as_ref().map_or("", |fsi| &fsi.slicer_name);
        let slicer_filament_name_part = part_val("SN", &slicer_filament_name.to_string());

        let nozzle_temp_min = filament_sup_info.as_ref().map(|fsi| fsi.nozzle_temp_low);
        let nozzle_temp_min_part = part_opt("NN", &nozzle_temp_min);

        let nozzle_temp_max = filament_sup_info.as_ref().map(|fsi| fsi.nozzle_temp_high);
        let nozzle_temp_max_part = part_opt("NX", &nozzle_temp_max);

        Some(format!(
            "{TAG_URL_PREFIX_S1}?{tag_id_part}{id_part}{encode_time_part}{added_time_part}{material_part}{material_subtype_part}{color_code_part}{color_name_part}{brand_part}{weight_advertised_part}{weight_core_part}{weight_new_part}{slicer_filament_code_part}{slicer_filament_name_part}{nozzle_temp_min_part}{nozzle_temp_max_part}{note_part}"
        ))
    }
    pub fn has_valid_tag_id(&self) -> bool {
        !self.tag_id.is_empty()
    }
}

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
pub fn part_opt<T: Default + PartialEq + core::fmt::Display>(prefix: &str, opt: &Option<T>) -> String {
    match opt {
        Some(v) => part_val(prefix, v),
        None => String::new(),
    }
}

const ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b';');

pub fn part_val<T: Default + PartialEq + core::fmt::Display>(prefix: &str, val: &T) -> String {
    if *val != T::default() {
        let value = val.to_string();
        let url_encoded_value = utf8_percent_encode(&value, ENCODE_SET).to_string();
        format!("&{prefix}={url_encoded_value}")
    } else {
        "".to_string()
    }
}
