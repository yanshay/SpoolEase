use crate::{
    bambu::{
        BambuPrinter, Extruder, Filament, NozzleType,
        bambu_api::{self, ExtrusionCaliSelCommand},
        tray::Tray,
    },
    spool_record::FullSpoolRecord,
};
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::RefCell, str::FromStr};
use embassy_time::Timer;
use framework::{info, term_info};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Calibration {
    pub extruder: i32,
    pub diameter: String,
    pub nozzle_id: String, // this is "" for old printers, and HS01-0.4 for example for new (so not nozzle_type), nozzle_type is a function below
    pub filament_id: String,
    pub k_value: String,
    // n_coef: f32,
    pub setting_id: Option<String>,
    pub name: String,
    pub cali_idx: i32,
}

impl Calibration {
    pub fn nozzle_type_code(&self) -> NozzleType {
        if self.nozzle_id.len() < 8 || self.nozzle_id.as_bytes()[1] != b'H' {
            NozzleType::Standard
        } else {
            NozzleType::HighFlow
        }
    }
    pub fn from(v: &bambu_api::Filament, diameter: &str) -> Self {
        // this "Filament" in bambu_api is really calibrations, bambulab naming ...
        Self {
            extruder: v.extruder_id.unwrap_or_default(),
            nozzle_id: v.nozzle_id.clone().unwrap_or_default(),
            diameter: diameter.to_string(),
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: formatted_k_value(&v.k_value),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }

    pub fn _new_minimal(diameter: &str, k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            extruder: 0,
            nozzle_id: String::new(),
            diameter: diameter.to_string(),
            k_value: formatted_k_value(k_value),
            filament_id: String::from(filament_id),
            setting_id: if setting_id.is_empty() { None } else { Some(setting_id.to_string()) },
            name: String::from(name),
            cali_idx,
        }
    }
}

pub(crate) fn formatted_k_value(k: &str) -> String {
    if k.is_empty() {
        return "".to_string();
    }
    if k.starts_with("(") {
        let k = k.trim_matches(['(', ')']);
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("({:.3})", k_value)
    } else {
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("{:.3}", k_value)
    }
}

impl BambuPrinter {
    fn get_calibration(&self, extruder: &Extruder, cali_idx: i32) -> Option<&Calibration> {
        let extruder_id = extruder.id as i32;
        let nozzle_diameter = extruder.diameter.as_deref()?;
        let nozzle_type_code = extruder.nozzle_type_code()?;

        self.calibrations.iter().find(|cal| {
            cal.extruder == extruder_id && cal.diameter == nozzle_diameter && cal.nozzle_type_code() == nozzle_type_code && cal.cali_idx == cali_idx
        })
    }

    fn get_cali_k_value(&self, extruder: &Extruder, cali_idx: i32) -> Option<String> {
        self.get_calibration(extruder, cali_idx).map(|calibration| calibration.k_value.clone())
    }

    pub fn get_tray_resolved_k_value(&self, tray: &Tray, tray_id: i32) -> String {
        // tray_id: 0..15 (4xAMS), 16..23 (8 AMS-HT), 254, 255
        let mut k_result = "(0.020)".to_string();
        if let Some(k_from_tray) = &tray.k_from_tray {
            k_result = format!("({k_from_tray:.3})");
        }

        if let Ok(extruder) = self.get_extruder_for_tray(tray_id)
            && let Some(cali_idx) = tray.cali_idx
            && let Some(k_value) = self.get_cali_k_value(extruder, cali_idx)
        {
            let k_float = f32::from_str(&k_value).unwrap_or_default();
            k_result = format!("{:.3}", k_float);
        }
        k_result
    }

    pub fn get_matching_printer_calibration_for_extruder(&self, full_spool_rec: &FullSpoolRecord, extruder_id: u32) -> Option<Calibration> {
        // cali_idx, setting_id
        // Now process it

        // Now we have a list of calibrations from the filament.
        // We need to select for each nozzle size in the printer (even if no value in filament settings), a matching calibration from the printer, if possible.
        // We can either match a perfect match or we can deduce of no perfect match
        // We can deduce for a certain nozzle also based on information we have on other nozzle diameters in the filaments calibrations

        // within the same nozzle/printer-type setting_id & filament_id will be the same
        // setting_id differs across nozzles/printer-types
        // filament_id is the same across nozzles/printer-types

        // Go through nozzle sizes 0.2, 0.4, 0.6 and 0.8
        //    Go through printer calibrations of the iterated-nozzle-size (if there are any) with the same filamentm_id and:
        //    First, look at the calibration for that nozzle size in the filament calibrations.
        // Same printer-type/nozzle (so same setting-id)
        //      A1- check if any printer calibration match to the setting_id & pa-profile-name (uncleaned)- if it is there's an exact match
        //      A2- check if any printer calibration has the same setting_id && setting-name (cleaned) - if it is there's a match (similar match)
        //      A3- check if any printer calibration same setting-id && same k value - if it is there's a match (not exact)
        //      Afuture: 4- check if any printer calibration has a similar name & close k - if it is there's a match (similar match)
        //    Next, go through calibrations of other nozzle sizes in the filament calibrations
        //      B1- check if any printer calibration has only the same setting-name exactly (ignore setting-id)
        //      B2- check if any printer calibration has only the same setting-name cleaned (ignore setting-id)
        //      B3- check if any printer calibration has a similar name - if it is then there's a match
        //    If all failed, then no match
        //

        fn clean_compare(a: &str, b: &str) -> bool {
            // Create filtered iterators that:
            // 1. Skip whitespace
            // 2. Skip chars_to_ignore
            // 3. Convert to lowercase for case-insensitive comparison
            let chars_to_ignore = &['.', '-', ','];
            let iter_a = a
                .chars()
                .filter(|&c| !c.is_whitespace() && !chars_to_ignore.contains(&c))
                .flat_map(|c| c.to_lowercase());

            let iter_b = b
                .chars()
                .filter(|&c| !c.is_whitespace() && !chars_to_ignore.contains(&c))
                .flat_map(|c| c.to_lowercase());

            // Compare the filtered iterators
            iter_a.eq(iter_b)
        }

        fn similar_compare(_s1: &str, _s2: &str) -> bool {
            // TODO: implement Metaphone Double
            false
        }

        let printer_nozzle_diameter = self.nozzle_diameter(extruder_id).as_ref()?;
        let extruder = self.get_extruder(extruder_id);
        let relevant_printer_calibrations = self.calibrations.iter().filter(|cal| {
            cal.extruder as u32 == extruder_id
                && cal.diameter == *printer_nozzle_diameter
                && cal.nozzle_type_code() == extruder.nozzle_type_code().unwrap_or(NozzleType::Standard)
        });
        let filament_id = &full_spool_rec.spool_rec.slicer_filament;
        // Using the new K from SpoolRecordExt

        // If there is filament calibration for that nozzle size (assumption there can be only one, which makes sense)
        if let Some(nozzle_k) = full_spool_rec.spool_rec_ext.get_calibration(
            &self.printer_serial,
            extruder_id as i32,
            self.nozzle_diameter(extruder_id).as_ref()?,
            extruder.nozzle_type_code().unwrap_or(NozzleType::Standard),
        ) {
            // here need to test not against printer nozzle but also consider the AMS tray which is the target, meaning, can't set/show it in staging
            // is tag_info modified when loaded into staging based on current printer? Or only displayed?
            // This also means, that we probably want the nozzle diameter when encoded to be the kay (so K4~0~HH-0.4) where 0 is extruder and after that comes the nozzle type
            // Need to check what does it mean that there isn't really a printer nozzle diameter

            // there could be several tht match filament_id, setting_id (even common)
            // Important Note: On A1,P1,X1 - calibration includes setting_id, so if we have it we encode it and then when scanning will compare it.
            //                 On H2D - Setting_id is not included - therefore the second part of below filter will compare None (on calibration) to filament setting_id
            //                 This is ok, because if there is no match it means it is not exact match from this printer type, it was encoded on pritner with setting_id and loaded to h2d
            let same_filament_id_nozzle_printer_type_calibrations = relevant_printer_calibrations.filter(|&c| c.filament_id == *filament_id);
            // A1
            if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone() // only cloning the iterator, not the data
                .find(|printer_calibration| printer_calibration.name == nozzle_k.name)
            {
                return Some(calibration_match.clone());
            // Starting here, we can improve by finding several that match and select the closest
            // A2
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| clean_compare(&printer_calibration.name, &nozzle_k.name))
            {
                return Some(calibration_match.clone());
            // A3
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| printer_calibration.k_value == nozzle_k.k_value)
            // because we are on same printer-type/nozzle this should be ok
            {
                return Some(calibration_match.clone());
            // A4 : TODO: use metaphone double to compare strings
            } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
                .clone()
                .find(|printer_calibration| similar_compare(&printer_calibration.name, &nozzle_k.name))
            {
                return Some(calibration_match.clone());
            }
        }

        // // If there is filament calibration for that nozzle size (assumption there can be only one, which makes sense)
        // if let Some(filament_calibration) = tag_info.calibrations.get(printer_nozzle) {
        //     // here need to test not against printer nozzle but also consider the AMS tray which is the target, meaning, can't set/show it in staging
        //     // is tag_info modified when loaded into staging based on current printer? Or only displayed?
        //     // This also means, that we probably want the nozzle diameter when encoded to be the kay (so K4~0~HH-0.4) where 0 is extruder and after that comes the nozzle type
        //     // Need to check what does it mean that there isn't really a printer nozzle diameter
        //
        //     // there could be several tht match filament_id, setting_id (even common)
        //     // Important Note: On A1,P1,X1 - calibration includes setting_id, so if we have it we encode it and then when scanning will compare it.
        //     //                 On H2D - Setting_id is not included - therefore the second part of below filter will compare None (on calibration) to filament setting_id
        //     //                 This is ok, because if there is no match it means it is not exact match from this printer type, it was encoded on pritner with setting_id and loaded to h2d
        //     let same_filament_id_nozzle_printer_type_calibrations = printer_calibrations
        //         .iter()
        //         .filter(|&c| c.1.filament_id == *tag_filament_id && c.1.setting_id == filament_calibration.setting_id);
        //
        //     // A1
        //     if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
        //     {
        //         return Some(calibration_match.1.clone());
        //     // Starting here, we can improve by finding several that match and select the closest
        //     // A2
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     // A3
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.k_value == filament_calibration.k_value)
        //     // because we are on same printer-type/nozzle this should be ok
        //     {
        //         return Some(calibration_match.1.clone());
        //     // A4 : TODO: use metaphone double to compare strings
        //     } else if let Some(calibration_match) = same_filament_id_nozzle_printer_type_calibrations
        //         .clone()
        //         .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        // };
        //
        // // This never takes place because tag_info contains only one clibration
        // for (_, filament_calibration) in &tag_info.calibrations {
        //     // TODO: When tag has several calibrations for different nozzles, here we can iterate over them as well
        //     // (so compare man to many) since name from another nozzle diameter could help finding for another nozzle
        //     // size, it's just name mathing
        //     let same_filament_id_printer_calibrations = printer_calibrations.iter().filter(|&c| c.1.filament_id == *tag_filament_id);
        //     // B1
        //     if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| printer_calibration.1.name == filament_calibration.name)
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        //     // Starting here, we can improve by finding several that match and select the closest
        //     // B2
        //     else if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| clean_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     // B3
        //     } else if let Some(calibration_match) = same_filament_id_printer_calibrations
        //         .clone()
        //         .find(|printer_calibration| similar_compare(&printer_calibration.1.name, &filament_calibration.name))
        //     {
        //         return Some(calibration_match.1.clone());
        //     }
        // }

        None
    }
}

pub async fn fix_k_on_restart(
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    prev_ams_trays: Vec<Tray>,
    prev_virt_tray: Tray,
    prev_nozzle: Option<String>,
) {
    Timer::after_secs(1).await;
    let printer_number = bambu_printer.borrow().printer_number;
    term_info!("[{}] Checking pressure advance (k) at printer startup", printer_number);
    if prev_nozzle != *bambu_printer.borrow().nozzle_diameter(0) {
        term_info!(
            "[{}] Nozzle diameter changed ({:?}->{:?}), K restore not relevant",
            printer_number,
            prev_nozzle,
            *bambu_printer.borrow().nozzle_diameter(0)
        );
        bambu_printer.borrow_mut().pending_k_restore_sequence = false;
        return;
    }
    let mut set_tray_cali_idx: [Option<i32>; 24] = [None; 24];
    let mut set_virt_cali_idx: Option<i32> = None;

    {
        // block start, so borrow will be dropped
        let bambu_borrow = bambu_printer.borrow();
        for (id, prev_tray) in prev_ams_trays
            .iter()
            .enumerate()
            .chain(core::iter::once(&prev_virt_tray).map(|v| (254, v)))
        {
            let curr_tray = if id == 254 {
                &bambu_borrow.virt_trays()[0]
            } else {
                &bambu_borrow.ams_trays()[id]
            };
            let set_tray = if id == 254 { &mut set_virt_cali_idx } else { &mut set_tray_cali_idx[id] };
            if let Filament::Known(curr_filament_info) = &curr_tray.filament
                && let Filament::Known(prev_filament_info) = &prev_tray.filament
                && curr_filament_info == prev_filament_info
            {
                // Turn both Some(-1) and None to Some(-1)
                let prev_cali_idx_normalized = prev_tray.cali_idx.or(Some(-1));
                let curr_cali_idx_normalized = curr_tray.cali_idx.or(Some(-1));

                // if curr idx isn't set and previously it was set, return it to previous state
                if curr_cali_idx_normalized == Some(-1) && prev_cali_idx_normalized != Some(-1) {
                    // set_tray_cali_idx[id] = prev_cali_idx_normalized; // -1 means to set -1, value means set to that cali_idx
                    *set_tray = prev_cali_idx_normalized; // -1 means to set -1, value means set to that cali_idx
                } else {
                    // set_tray_cali_idx[id] = None; // None means not do anything
                    *set_tray = None; // None means not do anything
                }
            }
        }
    }

    for (id, prev_tray) in prev_ams_trays
        .iter()
        .enumerate()
        .chain(core::iter::once(&prev_virt_tray).map(|v| (255, v)))
    {
        {
            let set_tray = if id == 255 { &set_virt_cali_idx } else { &set_tray_cali_idx[id] };
            if set_tray.is_some()
                && let Filament::Known(filament_info) = &prev_tray.filament
            {
                let tray_id = id as i32;
                let Some(extruder_id) = bambu_printer.borrow().get_unique_extruder_id_for_tray(tray_id) else {
                    // Defensive support only: real FTS internal AMS groups are ambiguous, while non-FTS groups should be uniquely bound.
                    info!("[{}] Skipping K restore for tray {tray_id}: no unique extruder", printer_number);
                    continue;
                };
                let nozzle_diameter = bambu_printer.borrow().nozzle_diameter(extruder_id).clone().unwrap_or_default();
                let original_tray_id = if id == 255 { 254 } else { id };
                let (ams_id, slot_id) = BambuPrinter::get_ams_and_slot_id(original_tray_id);
                // TODO: (if change) check ams_id against 255
                if ams_id != 255 && ams_id != 254 {
                    info!("[{}] Updating pressure advance of AMS {} slot {}", printer_number, ams_id, slot_id);
                } else {
                    info!("[{}] Updating pressure advance of external slot", printer_number);
                }
                let mut cmd = ExtrusionCaliSelCommand::new(
                    &nozzle_diameter,
                    ams_id as i32,
                    original_tray_id as i32, // here we need the original tray_id
                    slot_id as i32,
                    &filament_info.tray_info_idx, // tray_info_idx is filament_id in this command
                    *set_tray,
                );

                if !bambu_printer.borrow().is_locked() {
                    let payload = bambu_printer.borrow_mut().printer_message(&mut cmd);
                    BambuPrinter::publish_payload_async(&bambu_printer, payload).await;
                }
                Timer::after_millis(250).await;
            }
        }
    }

    Timer::after_millis(500).await; // wait until last K change is absorbed by the printer
    bambu_printer.borrow_mut().pending_k_restore_sequence = false;
    term_info!("[{}] Completed K restore where required", printer_number);
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KInfo {
    pub printers: HashMap<String, KPrinter>,
}

impl KInfo {
    fn _get_filament_k_for(&self, printer: &str, extruder: i32, diameter: &str, nozzle_id: &str) -> Option<&KNozzleId> {
        self.printers
            .get(printer)?
            .extruders
            .get(&extruder)?
            .diameters
            .get(diameter)?
            .nozzles
            .get(nozzle_id)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KPrinter {
    pub extruders: HashMap<i32, KExtruder>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KExtruder {
    pub diameters: HashMap<String, KNozzleDiameter>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KNozzleDiameter {
    pub nozzles: HashMap<String, KNozzleId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KNozzleId {
    pub name: String,
    pub k_value: String,
    pub cali_idx: i32,
    pub setting_id: Option<String>,
}

// k_info.printers["01P..."][0]["0.4"]["HH00"].name / .k
