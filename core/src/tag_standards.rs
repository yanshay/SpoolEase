use core::cmp::{max, min};

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use hashbrown::HashMap;
use minicbor::Decoder;
use minicbor::{Decode, Encode};
use ndef_rs::NdefMessage;
use serde::{Deserialize, Serialize};

use crate::{
    app_config::{BAMBU_COLOR_NAMES, BASE_FILAMENTS, MATERIALS},
    spool_record::SpoolRecord,
};

use serde_with::hex::Hex;
use serde_with::serde_as;

pub const SPOOLEASE_V1_TAG_TYPE: &str = "SpoolEaseV1";
pub const BAMBULAB_TAG_TYPE: &str = "Bambu Lab";
pub const OPENPRINTTAG_TAG_TYPE: &str = "OpenPrintTag";

// -----------------------------------------------------------------------------------------------------------
// -------- Bambu Lab Tag ------------------------------------------------------------------------------------
// -----------------------------------------------------------------------------------------------------------

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BambuLabTag {
    tag_id: String,
    #[serde_as(as = "HashMap<_, Hex>")]
    blocks: HashMap<i32, Vec<u8>>,
    // if adding fields, make sure to add skip not to persist them
}

// color_name
// https://raw.githubusercontent.com/bambulab/BambuStudio/refs/heads/master/resources/profiles/BBL/filament/filaments_color_codes.json
impl BambuLabTag {
    pub fn _material_variant_id(&self) -> String {
        // A00-G1 (Block 1, index 0..=7)
        self.get_block_cstr(1, 0, 8)
    }
    pub fn material_id(&self) -> String {
        // GFA00 (Block 1a (Block 1, index 8..=15)
        self.get_block_cstr(1, 8, 8)
    }
    pub fn _filament_type(&self) -> String {
        // PLA (Block 2) (Block 2, all)
        self.get_block_cstr(2, 0, 16)
    }
    pub fn _detailed_filament_type(&self) -> String {
        // PLA Basic (Block 4, all)
        self.get_block_cstr(4, 0, 16)
    }
    pub fn colors_rgba(&self) -> Vec<String> {
        let mut colors = Vec::<String>::new();
        // (Block 5, index 0..=3)
        if let Some(block) = self.blocks.get(&5) {
            let rgba = &block.as_slice()[0..=3];
            colors.push(hex::encode_upper(rgba));
        };
        // (Block 16, index 4, reversed), Block 16, index 2 two bytes show how many colors
        if let Some(block) = self.blocks.get(&16) {
            let block = block.as_slice();
            let num_colors = max(i16::from_le_bytes([block[2], block[3]]) as usize - 1, 0);
            for i in 0..min(num_colors, 3) {
                let offset = i * 4;
                let rgba = [block[7 + offset], block[6 + offset], block[5 + offset], block[4 + offset]];
                colors.push(hex::encode_upper(rgba));
            }
        }
        colors
    }
    pub fn spool_weight(&self) -> i32 {
        // 250g  (Block 5, index 4..=5)
        if let Some(block) = self.blocks.get(&5) {
            i16::from_le_bytes([block.as_slice()[4], block.as_slice()[5]]) as i32
        } else {
            0
        }
    }

    pub fn new(tag_id_hex: &str, blocks: &HashMap<i32, Vec<u8>>) -> Self {
        BambuLabTag {
            tag_id: tag_id_hex.to_string(),
            blocks: blocks.clone(),
        }
    }
    fn get_block_cstr(&self, block: i32, start: usize, len: usize) -> String {
        if let Some(block) = self.blocks.get(&block) {
            Self::get_cstr(&block.as_slice()[start..start + len]).unwrap_or_default()
        } else {
            String::new()
        }
    }

    fn get_cstr(bytes: &[u8]) -> Result<String, core::str::Utf8Error> {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        core::str::from_utf8(&bytes[..len]).map(|s| s.to_string())
    }

    pub fn to_spool_rec(&self) -> SpoolRecord {
        let material_id = self.material_id();
        let colors_rgba = self.colors_rgba();

        let (full_material, material_type) = BASE_FILAMENTS
            .lines()
            .find_map(|line| {
                let mut s = line.split(',');
                if s.next()? == material_id {
                    let full_material = s.next()?;
                    s.next()?;
                    s.next()?;
                    let material_type = s.next()?;
                    Some((full_material, material_type.to_string()))
                } else {
                    None
                }
            })
            .unwrap_or(("", String::new()));

        let color_name = BAMBU_COLOR_NAMES
            .lines()
            .find_map(|line| {
                let mut s = line.split(',');
                let id = s.next()?;
                if id != material_id {
                    return None;
                }

                let colors = s.next()?;
                // make sure lists are exactly the same (except for order)
                let mut color_name_colors: Vec<&str> = colors.split('/').collect();
                color_name_colors.sort();

                let mut colors_rgba_for_compare = colors_rgba.clone();
                colors_rgba_for_compare.sort();
                let colors_rgba_for_compare: Vec<&str> = colors_rgba_for_compare.iter().map(|s| s.as_str()).collect();

                let full_match = colors_rgba_for_compare == color_name_colors;

                if !full_match {
                    return None;
                }

                let name = s.next()?;
                let code = s.next()?;
                Some(format!("{name} ({code})"))
            })
            .unwrap_or_else(|| "(Fill Color-Name Manually)".to_string());

        let subtype_prefix = format!("Bambu {material_type} ");
        let material_subtype = if let Some(after_prefix) = full_material.strip_prefix(&subtype_prefix) {
            after_prefix.to_string()
        } else {
            String::new()
        };
        let spool_weight = self.spool_weight();

        SpoolRecord {
            material_type,
            material_subtype,
            color_name,
            color_code: colors_rgba,
            brand: "Bambu".to_string(),
            weight_advertised: if spool_weight != 0 { Some(spool_weight) } else { None },
            weight_core: None,
            slicer_filament: material_id,
            data_origin: BAMBULAB_TAG_TYPE.to_string(),
            ..Default::default()
        }
    }
}

// --------------------------------------------------------------------------------------------------------------
// -------- OpenPrintTag Tag ------------------------------------------------------------------------------------
// --------------------------------------------------------------------------------------------------------------

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenPrintTagTag {
    tag_id: String,
    #[serde_as(as = "Hex")]
    ndef_bytes: Vec<u8>,
    // if adding fields, make sure to add skip not to persist them
}

impl OpenPrintTagTag {
    pub fn new(tag_id_hex: &str, ndef_message: &[u8]) -> Self {
        Self {
            tag_id: tag_id_hex.to_string(),
            ndef_bytes: ndef_message.to_vec(),
        }
    }

    pub fn to_spool_rec(&self) -> Result<SpoolRecord, String> {
        if let Ok(ndef_message) = NdefMessage::decode(&self.ndef_bytes) {
            for record in ndef_message.records() {
                if core::str::from_utf8(record.record_type()) == Ok("application/vnd.openprinttag") {
                    let spool_info_bytes = record.payload().to_vec();

                    let mut decoder = Decoder::new(&spool_info_bytes);

                    let meta = decoder.decode::<Meta>();
                    let mut main_region_offset = decoder.position();
                    if let Ok(meta) = meta
                        && let Some(meta_main_region_offset) = meta.main_region_offset
                    {
                        main_region_offset = meta_main_region_offset
                    }

                    decoder.set_position(main_region_offset);
                    let main = decoder.decode::<MainRegion>();

                    if let Ok(info) = main {
                        let mut material_type_str = String::new();
                        let mut color_name = String::new();
                        let mut color_code = Vec::<String>::new();
                        let mut note = String::new();
                        let mut brand = String::new();

                        if let (Some(material_name), Some(material_type)) = (info.material_name, info.material_type) {
                            material_type_str = format!("{material_type:?}");
                            color_name = remove_words_case_insensitive(&material_name, &[&material_type_str.to_lowercase()]);
                        }

                        let slicer_filament = MATERIALS
                            .lines()
                            .find_map(|line| {
                                let mut s = line.split(',');
                                if s.next()?.to_lowercase() == material_type_str.to_lowercase() {
                                    let slicer_code = s.next()?;
                                    Some(slicer_code.to_string())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(String::new());

                        add_color(&mut color_code, &info.primary_color);
                        add_color(&mut color_code, &info.secondary_color_0);
                        add_color(&mut color_code, &info.secondary_color_1);
                        add_color(&mut color_code, &info.secondary_color_2);
                        add_color(&mut color_code, &info.secondary_color_3);
                        add_color(&mut color_code, &info.secondary_color_4);

                        let weight_advertised = info.nominal_netto_full_weight.map(|v| v as i32);

                        let weight_core = info.empty_container_weight.map(|v| v as i32);

                        if let Some(brand_name) = info.brand_name {
                            brand = brand_name.clone();
                        }

                        // Fill in the note field with what's missing

                        if material_type_str.is_empty() {
                            note.push_str("Material,");
                        }
                        if slicer_filament.is_empty() {
                            note.push_str("Slicer Filament,");
                        }
                        if color_name.is_empty() {
                            note.push_str("Color Name,");
                        }
                        if color_code.is_empty() {
                            note.push_str("RGBA Color,");
                        }
                        if brand.is_empty() {
                            note.push_str("Brand,")
                        }
                        if weight_advertised.is_none() {
                            note.push_str("Label Weight,");
                        }
                        if weight_core.is_none() {
                            note.push_str("Empty Weight,");
                        }

                        if !note.is_empty() {
                            note.insert_str(0, "Missing:");
                            note.pop(); // remove the last ","
                        }

                        return Ok(SpoolRecord {
                            material_type: material_type_str,
                            material_subtype: String::new(),
                            color_name,
                            color_code,
                            note,
                            brand,
                            weight_advertised,
                            weight_core,
                            slicer_filament,
                            data_origin: OPENPRINTTAG_TAG_TYPE.to_string(),
                            ..Default::default()
                        });
                    }
                }
            }
            return Err("Not OpenPrintTag tag".to_string());
        }

        Err("Failed to parse tag".to_string())
    }
}

fn add_color(color_codes: &mut Vec<String>, color: &Option<Vec<u8>>) {
    if let Some(color_bytes) = color {
        let color_code = match color_bytes.len() {
            3 => format!("{}FF", hex::encode_upper(color_bytes)),
            4 => hex::encode_upper(color_bytes),
            _ => String::new(),
        };
        if !color_code.is_empty() {
            color_codes.push(color_code);
        }
    }
}

pub fn remove_words_case_insensitive(sentence: &str, words: &[&str]) -> String {
    let mut result = String::with_capacity(sentence.len());
    let mut first = true;

    for w in sentence.split_whitespace() {
        if !words.iter().any(|word| word.eq_ignore_ascii_case(w)) {
            if !first {
                result.push(' ');
            }
            first = false;
            result.push_str(w);
        }
    }

    result
}

#[derive(Debug, Encode, Decode)]
#[cbor(map)]
struct Meta {
    #[n(0)]
    main_region_offset: Option<usize>,
    #[n(1)]
    main_region_size: Option<usize>,
    #[n(2)]
    aux_region_offset: Option<usize>,
    #[n(3)]
    aux_region_size: Option<usize>,
}

#[derive(Debug, Encode, Decode)]
#[cbor(map)]
struct MainRegion {
    #[n(10)]
    material_name: Option<String>, // Coloe Name?
    #[n(11)]
    brand_name: Option<String>,
    #[n(52)]
    material_abbreviation: Option<String>,
    #[n(9)]
    material_type: Option<MaterialType>,
    #[n(16)]
    nominal_netto_full_weight: Option<usize>, // label
    #[n(17)]
    actual_netto_full_weight: Option<usize>, // real weight at start
    #[n(18)]
    empty_container_weight: Option<usize>, // empty / core weight
    #[cbor(n(19), with = "minicbor::bytes")]
    primary_color: Option<Vec<u8>>,
    #[cbor(n(20), with = "minicbor::bytes")]
    secondary_color_0: Option<Vec<u8>>,
    #[cbor(n(21), with = "minicbor::bytes")]
    secondary_color_1: Option<Vec<u8>>,
    #[cbor(n(22), with = "minicbor::bytes")]
    secondary_color_2: Option<Vec<u8>>,
    #[cbor(n(23), with = "minicbor::bytes")]
    secondary_color_3: Option<Vec<u8>>,
    #[cbor(n(24), with = "minicbor::bytes")]
    secondary_color_4: Option<Vec<u8>>,
}

#[derive(Debug, Encode, Decode)]
#[cbor(index_only)]
#[allow(clippy::upper_case_acronyms)]
pub enum MaterialType {
    #[n(0)]
    PLA,
    #[n(1)]
    PETG,
    #[n(2)]
    TPU,
    #[n(3)]
    ABS,
    #[n(4)]
    ASA,
    #[n(5)]
    PC,
    #[n(6)]
    PCTG,
    #[n(7)]
    PP,
    #[n(8)]
    PA6,
    #[n(9)]
    PA11,
    #[n(10)]
    PA12,
    #[n(11)]
    PA66,
    #[n(12)]
    CPE,
    #[n(13)]
    TPE,
    #[n(14)]
    HIPS,
    #[n(15)]
    PHA,
    #[n(16)]
    PET,
    #[n(17)]
    PEI,
    #[n(18)]
    PBT,
    #[n(19)]
    PVB,
    #[n(20)]
    PVA,
    #[n(21)]
    PEKK,
    #[n(22)]
    PEEK,
    #[n(23)]
    BVOH,
    #[n(24)]
    TPC,
    #[n(25)]
    PPS,
    #[n(26)]
    PPSU,
    #[n(27)]
    PVC,
    #[n(28)]
    PEBA,
    #[n(29)]
    PVDF,
    #[n(30)]
    PPA,
    #[n(31)]
    PCL,
    #[n(32)]
    PES,
    #[n(33)]
    PMMA,
    #[n(34)]
    POM,
    #[n(35)]
    PPE,
    #[n(36)]
    PS,
    #[n(37)]
    PSU,
    #[n(38)]
    TPI,
    #[n(39)]
    SBS,
}
