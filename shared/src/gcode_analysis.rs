use crate::utils::{deserialize_f32_base64, serialize_f32_base64};
use core::f32::consts::PI;

use alloc::vec::Vec;
use hashbrown::HashMap;
use serde::Deserialize;
use snafu::prelude::*;
type Result<T, E = snafu::Whatever> = core::result::Result<T, E>;
use serde::Serialize;

#[allow(dead_code)]
#[derive(Deserialize)]
struct BblInfo {
    flow_cali: bool,
    #[serde(rename = "use ams")]
    use_ams: bool,
    #[serde(rename = "ams mapping")]
    ams_mapping: Vec<i32>,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Copy)]
pub struct FilamentUsageEntry {
    // pub len_mm: f32,
    pub layer: i32,
    pub gcode_filament_id: i32,
    #[serde(
        default,
        serialize_with = "serialize_f32_base64",
        deserialize_with = "deserialize_f32_base64"
    )]
    pub weight_g: f32,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct GcodeFilamentCalc {
    pub ams_slots: Vec<i32>,
    filament_density: Vec<f32>,
    filament_diameter: Vec<f32>,
    gcode_filament_id_to_slicer_filament_index: HashMap<u8, usize>, // maps filament_id to 0 base number (to access for example density vec)

    pub total_extruded: f32, //TODO: verify this is correct
    pub layers_extruded: Vec<FilamentUsageEntry>,
    pub filament_swaps: i32,

    gcode_buffer: Vec<u8>,
    curr_gcode_line: usize,
    pub curr_extrusion_position: f32,
    curr_filament_id: Option<i32>,
    curr_layer: i32,
    curr_extrude_len: f32,
}

#[allow(dead_code)]
impl GcodeFilamentCalc {
    pub fn new() -> Self {
        Self {
            gcode_buffer: Vec::new(),
            filament_density: Vec::new(),
            filament_diameter: Vec::new(),
            gcode_filament_id_to_slicer_filament_index: HashMap::new(),
            curr_gcode_line: 0,
            curr_extrusion_position: 0.0,
            total_extruded: 0.0,
            layers_extruded: Vec::new(),
            ams_slots: Vec::new(),
            curr_layer: 0,
            curr_filament_id: None,
            curr_extrude_len: 0.0,
            filament_swaps: 0,
        }
    }

    #[allow(dead_code)]
    pub fn set_bbl_info(&mut self, buffer: &[u8]) -> Result<()> {
        let bbl_file = serde_json::from_slice::<BblInfo>(buffer)
            .whatever_context("Failed to deserialize bbl file")?;
        for slot in bbl_file.ams_mapping {
            if slot != -1 {
                self.ams_slots.push(slot);
            }
        }

        Ok(())
    }

    pub fn add_buffer(&mut self, buffer: &[u8]) -> Result<()> {
        self.gcode_buffer.extend_from_slice(buffer);
        self.process_available_buffer()
    }

    fn process_available_buffer(&mut self) -> Result<()> {
        let mut taken_buffer = core::mem::take(&mut self.gcode_buffer);
        if let Some(last_cr) = taken_buffer.iter().rposition(|&b| b == b'\n') {
            let buffer_to_process = &taken_buffer[..=last_cr];
            let string_to_process = core::str::from_utf8(buffer_to_process)
                .whatever_context("gcode isn't valid utf8")?;
            for full_line in string_to_process.lines() {
                let line = full_line.trim();
                // Remove comments after gcode (like ; )
                let line = if !line.starts_with(';') {
                    line.split_once(';').map(|(a, _)| a).unwrap_or(full_line)
                } else {
                    line
                };
                self.curr_gcode_line += 1;
                // println!("// {}, {}", self.gcode_buffer_line, line);
                if line.starts_with("; CHANGE_LAYER") {
                    // println!(
                    //     "Ended layer {} with extrusion_position {}",
                    //     self.curr_layer, self.extrusion_position
                    // );
                    self.store_curr_extrusion_info();

                    // println!(
                    //     "{} : Switching to layer {}",
                    //     self.gcode_buffer_line, self.curr_layer
                    // );
                } else if line.starts_with("M620 S") {
                    for part in line.split(' ') {
                        if part.starts_with("S") & part.ends_with("A") {
                            if let Ok(filament_id) = part[1..part.len() - 1].parse::<i32>() {
                                // println!(
                                //     "{} : Switch to filament_id {ams_id} // {}",
                                //     self.gcode_buffer_line, line
                                // );
                                if self.curr_extrude_len > 0.0 {
                                    // println!(
                                    //     "Switching ams_id {} with extrusion_position {}",
                                    //     self.curr_ams_id.unwrap(),
                                    //     self.extrusion_position
                                    // );
                                    // This is the case of filament switch
                                    // Need to store  the extrusion but w/o layer increase
                                    if let Some(curr_filament_id) = self.curr_filament_id {
                                        let extrude_grams = self
                                            .extrude_gram(self.curr_extrude_len, curr_filament_id);
                                        self.layers_extruded.push(FilamentUsageEntry {
                                            // len_mm: self.curr_extrude_len,
                                            weight_g: extrude_grams,
                                            layer: self.curr_layer,
                                            gcode_filament_id: curr_filament_id,
                                        });
                                    }
                                }
                                self.curr_extrude_len = 0.0;

                                if self.curr_filament_id != Some(filament_id) {
                                    self.filament_swaps += 1;
                                    // if filament change it means exrtusion position is zero again
                                    self.curr_extrusion_position = 0.0; //-6.2;
                                }
                                self.curr_filament_id = Some(filament_id);
                            } else {
                                panic!("filament_id larger than u8 in '{full_line}'");
                            }
                        }
                    }
                } else if line.starts_with("G") || line.starts_with("M620.11") {
                    // G0, G1, G2, maybe other G
                    // M620.11 S1 I0 E-18 F523
                    for part in line.split(' ') {
                        if let Some(after_e) = part.strip_prefix("E") {
                            if let Ok(extrusion_length) = after_e.parse::<f32>() {
                                if line.starts_with("M620.11") {
                                    // This seems to be part of a sequenct of filament switch and pulling back  (retracting) filament that was beyond the 0 point to cut and pull into AMS
                                    // There is a parellel positive one that seems to need to be ignored, not clear why
                                    // And both aren't considered in the extruder position.
                                    // But could also both be considered in the extruder position and it would work just as well
                                    // Or at leaset, this worked to get data consistent with bambustudio
                                    if extrusion_length > 0.0 {
                                        self.total_extruded -= extrusion_length;
                                        // self.curr_extrude_len -= extrusion_length;
                                        self.curr_extrusion_position -= extrusion_length;
                                    }
                                } else if self.curr_filament_id.is_some() {
                                    // on A1 there is an extrusion before filament is selected, so we test here there's filament
                                    self.curr_extrusion_position += extrusion_length;
                                }
                                if self.curr_extrusion_position > 0.0 {
                                    self.total_extruded += self.curr_extrusion_position;
                                    self.curr_extrude_len += self.curr_extrusion_position;
                                    self.curr_extrusion_position = 0.0;
                                }
                            } else {
                                panic!("can't parse E in '{full_line}'")
                            }
                        }
                    }
                } else if let Some(number_of_layers_str) =
                    line.strip_prefix("; total layer number: ")
                {
                    // ; total layer number: 1250
                    if let Ok(num_of_layers) = number_of_layers_str.parse::<usize>() {
                        self.layers_extruded
                            .reserve(num_of_layers - self.layers_extruded.len());
                    }
                } else if let Some(filament_density_str) = line.strip_prefix("; filament_density: ")
                {
                    //  ; filament_density: 1.24,1.25,1.25,1.04
                    for density_str in filament_density_str.split(',') {
                        self.filament_density.push(
                            density_str
                                .parse::<f32>()
                                .whatever_context("Failed to parse filaments density")?,
                        );
                    }
                } else if let Some(filament_diameter_str) =
                    line.strip_prefix("; filament_diameter: ")
                {
                    //  ; filament_diameter: 1.24,1.25,1.25,1.04
                    for diameter_str in filament_diameter_str.split(',') {
                        self.filament_diameter.push(
                            diameter_str
                                .parse::<f32>()
                                .whatever_context("Failed to parse filaments diameter")?,
                        );
                    }
                } else if let Some(filament_str) = line.strip_prefix("; filament: ") {
                    for (filament_index, filament_id_str) in filament_str.split(',').enumerate() {
                        self.gcode_filament_id_to_slicer_filament_index.insert(
                            filament_id_str
                                .parse::<u8>()
                                .whatever_context("Failed to parse filament ids ")?
                                - 1, // -1 because M620 SxA - x is 0 based
                            filament_index,
                        );
                    }
                }
            }
            taken_buffer.drain(..=last_cr);
        }
        self.gcode_buffer = taken_buffer;
        Ok(())
    }

    fn store_curr_extrusion_info(&mut self) {
        // println!(
        //     "Ended layer {} with extrusion_position {}",
        //     self.curr_layer, self.curr_extrusion_position
        // );
        if let Some(curr_filament_id) = self.curr_filament_id {
            let extrude_gram = self.extrude_gram(self.curr_extrude_len, curr_filament_id);
            self.layers_extruded.push(FilamentUsageEntry {
                // len_mm: self.curr_extrude_len,
                weight_g: extrude_gram,
                layer: self.curr_layer,
                gcode_filament_id: curr_filament_id,
            });
            self.curr_layer += 1;
        }
        self.curr_extrude_len = 0.0;
    }

    pub fn done(&mut self) {
        self.store_curr_extrusion_info();

        // println!(
        //     "Ended layer {} with extrusion_position {}",
        //     self.curr_layer, self.curr_extrusion_position
        // );
    }

    pub fn processed_line_count(&self) -> usize {
        self.curr_gcode_line
    }

    pub fn completed_layer_count(&self) -> i32 {
        self.curr_layer
    }

    pub fn declared_filament_count(&self) -> usize {
        self.gcode_filament_id_to_slicer_filament_index.len()
    }

    pub fn filament_selection_count(&self) -> i32 {
        self.filament_swaps
    }

    fn extrude_gram(&mut self, extrude_len: f32, filament_id: i32) -> f32 {
        let diameter = self.filament_diameter[(filament_id) as usize];
        let density = self.filament_density[(filament_id) as usize];
        gram_from_length(extrude_len, diameter, density)
    }
}

pub fn gram_from_length(length_mm: f32, diameter_mm: f32, density: f32) -> f32 {
    let area_cm2 = PI * diameter_mm * diameter_mm / 100.0 / 4.0; // 100: from mm3 to cm3, 4: diameter->radius
    let volume = length_mm * area_cm2 / 10.0; // 10: length mm to cm
    volume * density
}
