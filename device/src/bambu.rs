use crate::spool_tag::TAG_PLACEHOLDER;
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::RefCell, str::FromStr};
use embassy_futures::select::{select, Either};
use embassy_net::{Ipv4Address, Stack};
use embassy_sync::{
    blocking_mutex::{
        raw::{CriticalSectionRawMutex, NoopRawMutex},
        Mutex,
    },
    channel::Channel,
    pubsub::PubSubChannel,
};
use embassy_time::{with_deadline, with_timeout, Duration, Instant, Timer};
use esp_mbedtls::TlsReference;
use hashbrown::HashMap;
use mqttrust::QoS;
use once_cell::sync::Lazy;
use regex::Regex;

use framework::prelude::*;

use crate::{
    app_config::AppConfig,
    bambu_api::{self, PrintAms, PrintTray},
    my_mqtt::BufferedMqttPacket,
};

const FILAMENT_URL_PREFIX: &str = "https://info.filament3d.org/";

pub struct BambuPrinter {
    pub nozzle_diameter: Option<String>,
    pub ams_trays: [Tray; 16],
    pub virt_tray: Tray,
    pub calibrations: HashMap<String, HashMap<i32, Calibration>>,
    write_packets: &'static embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn BambuPrinterObserver>>>,
    app_config: Rc<RefCell<AppConfig>>,
    tray_exist_bits: Option<u32>,
    tray_read_done_bits: Option<u32>,
    tray_reading_bits: Option<u32>,
    pub ams_exist_bits: Option<u32>,
}

pub trait BambuPrinterObserver {
    fn on_trays_update(&self, bambu_printer: &BambuPrinter, prev_tray_reading_bits: Option<u32>, new_tray_reading_bits: Option<u32>);
}

impl BambuPrinter {
    pub fn new(
        write_packets: &'static embassy_sync::channel::Channel<
            embassy_sync::blocking_mutex::raw::NoopRawMutex,
            crate::my_mqtt::BufferedMqttPacket,
            3,
        >,
        app_config: Rc<RefCell<AppConfig>>,
    ) -> Self {
        let unknown = Tray {
            state: TrayState::Unknown,
            filament: Filament::Unknown,
            k: None,
            cali_idx: None,
        };
        Self {
            nozzle_diameter: None,
            ams_trays: [
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
                unknown.clone(),
            ], //, unknown, unknown, unknown],
            virt_tray: unknown,
            calibrations: HashMap::new(),
            write_packets,
            observers: Vec::new(),
            app_config,
            tray_exist_bits: None,
            tray_read_done_bits: None,
            tray_reading_bits: None,
            ams_exist_bits: None,
        }
    }
    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn BambuPrinterObserver>>) {
        self.observers.push(observer);
    }

    pub fn get_filament_calibration_for_current_nozzle<'a>(&self, filament_info: &'a FilamentInfo) -> Option<&'a Calibration> {
        if let Some(filament_calibration) = filament_info.calibrations.get(self.nozzle_diameter.as_ref().unwrap()) {
            return Some(filament_calibration);
        }
        None
    }

    pub fn get_filament_k_for_current_nozzle(&self, filament_info: &FilamentInfo) -> String {
        if let Some(filament_calibration) = self.get_filament_calibration_for_current_nozzle(filament_info) {
            return filament_calibration.k_value.clone();
        }
        "".to_string()
    }

    fn get_cali_k_value(&self, nozzle_diameter: &str, cali_idx: i32) -> Option<String> {
        let nozzle_calibrations = match self.calibrations.get(nozzle_diameter) {
            Some(calibrations) => calibrations,
            None => return None,
        };
        let calibration = match nozzle_calibrations.get(&cali_idx) {
            Some(calibration) => calibration,
            None => return None,
        };

        Some(calibration.k_value.clone())
    }

    fn get_tray_cali_k_value(&self, tray: &Tray) -> Option<String> {
        let cali_idx = match tray.cali_idx {
            Some(cali_idx) => cali_idx,
            None => {
                return tray.k.clone();
            }
        };
        let nozzle_diameter = match &self.nozzle_diameter {
            Some(nozzle_diameter) => nozzle_diameter,
            None => {
                return tray.k.clone();
            }
        };
        self.get_cali_k_value(nozzle_diameter, cali_idx).or_else(|| tray.k.clone())
    }

    fn tray_from_update(&self, tray_update: &PrintTray) -> Result<Option<Tray>, String> {
        if let (Some(tray_type_update), Some(tray_info_idx_update), Some(tray_color_update)) =
            (&tray_update.tray_type, &tray_update.tray_info_idx, &tray_update.tray_color)
        {
            // Remember: tray_type is the material(PLA, PETG, etc), tray_info_idx is the filament_id (some code)
            // when there is data in the tray data then
            let mut new_tray = Tray::default(); // Everything is unknown at start
                                                // when adding filament to a tray when the printer doesn't know what is inside, tray_info_idx and tray_type
                                                // will arrive as empty, so this is a fine condition. In the past I thought it couldn't be.
                                                // I'm still unclear when filament settings are cleared form tray.

            // Sometimes the tray arrives with tray_type, tray_info_idx, color filled with 00000000 (also last two are 00),  which may be an error, not sure
            // if strange issues seem to appear, check that out and maybe deal with that case
            if tray_type_update.ends_with("00") {
                error!("??????????????????????? tray_type with 00 suffix");
                debug!("{:?}", tray_update);
                return Err("tray_type junk".to_string());
            }
            if tray_color_update.ends_with("00") {
                error!("??????????????????????? tray_color with 00 suffix");
                debug!("{:?}", tray_update);
                return Err("tray_color junk".to_string());
            }
            if tray_info_idx_update.starts_with("00") {
                // might end with 00, so checking if starts with 00
                error!("??????????????????????? tray_info_idx with 00 suffix");
                debug!("{:?}", tray_update);
                return Err("tray_info_idx junk".to_string());
            }

            new_tray.filament = if tray_type_update.is_empty() {
                Filament::Unknown
            } else {
                Filament::Known(FilamentInfo::from(tray_update))
            };

            // TODO: This snippet is in two places, fix that
            new_tray.cali_idx = tray_update.cali_idx;
            // start by assigning the tray 'k', then override with calibration if exist
            new_tray.k = tray_update.k.map(|k| format!("({k:.3})"));
            new_tray.k = self.get_tray_cali_k_value(&new_tray);

            // add the cali_idx and its name into the filament for the specific nozzle
            if let (Some(nozzle_diameter), Some(cali_idx)) = (self.nozzle_diameter.as_ref(), tray_update.cali_idx.as_ref()) {
                if let Some(calibrations) = self.calibrations.get(nozzle_diameter) {
                    if let Some(calibration) = calibrations.get(cali_idx) {
                        if let Filament::Known(ref mut filament_info) = new_tray.filament {
                            filament_info.calibrations.insert(nozzle_diameter.clone(), calibration.clone());
                        }
                    }
                }
            }
            Ok(Some(new_tray))
        } else {
            Ok(None)
        }
    }

    // Arguments:
    //   old_tray is the tray as known prior to this update
    //   tray_update is the tray information received from the printer
    //   tray_id is the tray_id in case of AMS or None in case of External spool
    // Return value:
    //   if tray not changed from old_tray, or something wrong with tray, returns None
    pub fn get_updated_tray(&self, old_tray: &Tray, tray_update: Option<&PrintTray>, tray_id: Option<usize>) -> Option<Tray> {
        if let Some(tray_id) = tray_id {
            // AMS tray
            if let Some(tray_exist_bits) = self.tray_exist_bits {
                let tray_exist = ((tray_exist_bits >> tray_id) & 0x01) != 0;

                if tray_exist {
                    let tray_reading = self.tray_reading_bits.map_or(false, |x| ((x >> tray_id) & 0x01) != 0);
                    let tray_read_done = self.tray_read_done_bits.map_or(false, |x| ((x >> tray_id) & 0x01) != 0);

                    let mut new_tray = if let Some(tray_update) = tray_update {
                        if let Ok(tray_update) = self.tray_from_update(tray_update) {
                            // TODO: in case I a tray w/o any information (but with exist bit) then I just copy old, is it ok?
                            let tray_based_on_update = tray_update.unwrap_or_else(|| {
                                let mut new_tray = old_tray.clone();
                                new_tray.state = TrayState::Empty;
                                new_tray
                            });
                            tray_based_on_update
                        } else {
                            // Update is bad so ignoring it
                            return None;
                        }
                    } else {
                        // If no update data for try (but tray exist) copy previous tray
                        let mut new_tray = old_tray.clone();
                        new_tray.state = TrayState::Empty;
                        new_tray
                    };
                    new_tray.state = TrayState::Spool;

                    if tray_reading {
                        new_tray.state = TrayState::Reading;
                    }
                    if tray_read_done {
                        new_tray.state = TrayState::Ready;
                    }
                    return Some(new_tray);
                } else {
                    // In case the tray is empty (so no ready bits), we still want to keep the filamen-info of the tray, but set it as empty
                    // special case handling (different than Bambustudio).
                    // we remember historical color, K, etc (which the printer also remembers, just doesn't report)
                    let mut new_tray = old_tray.clone();
                    new_tray.state = TrayState::Empty;
                    Some(new_tray)
                }
            } else {
                //  if tray_exist_bits not available yet, then tray should be unknown
                Some(Tray::unknown())
            }
        } else {
            // External Tray
            if let Some(tray_update) = tray_update {
                if let Ok(tray_update) = self.tray_from_update(tray_update) {
                    if let Some(mut new_tray) = tray_update {
                        // External tray with data is always considered Ready
                        if matches!(new_tray.filament, Filament::Unknown) {
                            new_tray.state = TrayState::Empty;
                        } else {
                            new_tray.state = TrayState::Ready;
                        }
                        return Some(new_tray);
                    } else {
                        // Empty tray data means tray empty in case of external
                        return Some(Tray::unknown());
                    }
                } else {
                    // Error in tray information, don't change anything
                    None
                }
            } else {
                // No new information, don't change anything
                None
            }
        }
    }

    pub fn _old_get_updated_tray(&self, old_tray: &Tray, tray_update: Option<&PrintTray>, tray_id: Option<usize>) -> Option<Tray> {
        let mut new_tray = if let Some(tray_exist_bits) = self.tray_exist_bits {
            let tray_exist = match tray_id {
                Some(tray_id) => ((tray_exist_bits >> tray_id) & 0x01) != 0,
                None => {
                    // external tray
                    if let Some(v_tray) = tray_update {
                        !v_tray.tray_type.as_ref().is_none_or(|v| v.is_empty())
                    } else {
                        false
                    }
                }
            };
            let tray_reading = if let Some(tray_reading_bits) = self.tray_reading_bits {
                match tray_id {
                    Some(tray_id) => ((tray_reading_bits >> tray_id) & 0x01) != 0,
                    None => false,
                }
            } else {
                false
            };

            let tray_read_done = if let Some(tray_read_done_bits) = self.tray_read_done_bits {
                match tray_id {
                    Some(tray_id) => ((tray_read_done_bits >> tray_id) & 0x01) != 0,
                    None => false,
                }
            } else {
                tray_exist
            };

            if tray_exist {
                if let Some(tray_update) = tray_update {
                    if let (Some(tray_type_update), Some(_tray_info_idx_update), Some(_tray_color_update)) =
                        (&tray_update.tray_type, &tray_update.tray_info_idx, &tray_update.tray_color)
                    {
                        // Remember: tray_type is the material(PLA, PETG, etc), tray_info_idx is the filament_id (some code)
                        // when there is data in the tray data then
                        let new_tray: Option<Tray> = {
                            let mut new_tray = Tray::default(); // Everything is unknown at start
                                                                // when adding filament to a tray when the printer doesn't know what is inside, tray_info_idx and tray_type
                                                                // will arrive as empty, so this is a fine condition. In the past I thought it couldn't be.
                                                                // I'm still unclear when filament settings are cleared form tray.

                            // Sometimes the tray arrives with tray_type, tray_info_idx, color filled with 00000000 (also last two are 00),  which may be an error, not sure
                            // if strange issues seem to appear, check that out and maybe deal with that case
                            if tray_type_update.ends_with("00") {
                                error!("??????????????????????? tray_type with 00 suffix");
                                debug!("{:?}", tray_update);
                                return None;
                            }
                            if _tray_color_update.ends_with("00") {
                                error!("??????????????????????? tray_color with 00 suffix");
                                debug!("{:?}", tray_update);
                                return None;
                            }
                            if _tray_info_idx_update.ends_with("00") {
                                error!("??????????????????????? tray_info_idx with 00 suffix");
                                debug!("{:?}", tray_update);
                                return None;
                            }

                            // For AMS - begin with Spool, for External - Ready since don't really know anything about it
                            new_tray.state = if tray_id.is_some() { TrayState::Spool } else { TrayState::Ready };
                            if tray_reading {
                                new_tray.state = TrayState::Reading;
                            }
                            if tray_read_done {
                                new_tray.state = TrayState::Ready;
                            }

                            new_tray.filament = if tray_type_update.is_empty() {
                                Filament::Unknown
                            } else {
                                Filament::Known(FilamentInfo::from(tray_update))
                            };
                            // TODO: This snippet is in two places, fix that
                            new_tray.cali_idx = tray_update.cali_idx;
                            // start by assigning the tray 'k', then override with calibration if exist
                            new_tray.k = tray_update.k.map(|k| format!("({k:.3})"));
                            new_tray.k = self.get_tray_cali_k_value(&new_tray);

                            // add the cali_idx and its name into the filament for the specific nozzle
                            if let (Some(nozzle_diameter), Some(cali_idx)) = (self.nozzle_diameter.as_ref(), tray_update.cali_idx.as_ref()) {
                                if let Some(calibrations) = self.calibrations.get(nozzle_diameter) {
                                    if let Some(calibration) = calibrations.get(cali_idx) {
                                        if let Filament::Known(ref mut filament_info) = new_tray.filament {
                                            filament_info.calibrations.insert(nozzle_diameter.clone(), calibration.clone());
                                        }
                                    }
                                }
                            }

                            Some(new_tray)
                        };
                        new_tray
                    } else if tray_id.is_none() {
                        // if case of External Tray
                        // No data in external tray means it is empty?
                        // No is only in case of explicit reset fom Studio, so no need to 'remember' previous filament like in ams trays
                        Some(Tray {
                            state: TrayState::Empty,
                            filament: Filament::Unknown,
                            cali_idx: None,
                            k: None,
                        })
                    } else {
                        // No data in ams tray and tray exist, don't change a thing for this tray
                        None
                    }
                } else {
                    None // if no source_tray (in data) then don't change anything for this tray by returning None
                }
            } else {
                // In case the tray is empty (so no ready bits), we still want to keep the filamen-info of the tray, but set it as empty
                // special case handling (different than Bambustudio).
                // we remember historical color (which the printer also remembers, just doesn't report)
                let mut new_tray = old_tray.clone();
                new_tray.state = TrayState::Empty;
                Some(new_tray)
            }
        } else {
            Some(Tray::unknown()) //  if exist_bits not available yet, then tray should be unknown
        };

        //
        // Handle tray_reading_bits
        //
        // TODO: duplication of above, organize this entire function better
        // Separate external and internal path
        // Remove duplications of work (reading)
        if tray_id.is_some() {
            // ams tray
            let tray_reading = if let Some(tray_reading_bits) = self.tray_reading_bits {
                match tray_id {
                    Some(tray_id) => ((tray_reading_bits >> tray_id) & 0x01) != 0,
                    None => false,
                }
            } else {
                false
            };
            if tray_reading {
                match new_tray {
                    Some(ref mut new_tray) => {
                        new_tray.state = TrayState::Reading;
                    }
                    None => {
                        let mut alt_new_tray = old_tray.clone();
                        alt_new_tray.state = TrayState::Reading;
                        new_tray = Some(alt_new_tray);
                    }
                }
            }
        }

        if new_tray.is_some() && *old_tray != *new_tray.as_ref().unwrap() {
            new_tray
        } else {
            None
        }
    }

    pub fn get_ams_and_tray_id(tray_id: usize) -> (usize, usize) {
        if tray_id < 254 {
            let ams_id = tray_id / 4;
            let ams_tray_id = tray_id - ams_id * 4;
            (ams_id, ams_tray_id)
        } else {
            (254, tray_id)
        }
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__push_status__ams(&mut self, ams: &PrintAms) -> bool {
        let mut change_made = false;

        // first check which ams's exist
        if let Some(ams_exist_bits) = &ams.ams_exist_bits {
            let ams_exist_bits = ams_exist_bits.parse::<u32>();
            if let Ok(ams_exist_bits) = ams_exist_bits {
                if self.ams_exist_bits.is_none() || self.ams_exist_bits.unwrap() != ams_exist_bits {
                    self.ams_exist_bits = Some(ams_exist_bits);
                    change_made = true;
                }
            }
        }

        // tray_exist_bits seem to be bits for all ams systems (due to where it is in the struct hierrchy)
        // and the lowest most bits seem to be the first ams trays bits
        // for now handle only the first ams
        // if tray_exist_bits are specified it means they may have changed, so update them
        // the stored value is the one we'll reference later

        // tray_exist_bits - which trays contain a spool
        if let Some(tray_exist_bits) = &ams.tray_exist_bits {
            if let Ok(tray_exist_bits) = u32::from_str_radix(tray_exist_bits, 16) {
                if self.tray_exist_bits != Some(tray_exist_bits) {
                    self.tray_exist_bits = Some(tray_exist_bits);
                    change_made = true;
                }
            }
        }
        // tray_read_done - which trays (from those that exist) that have been "read" (meaning ready from ams perspective)
        if let Some(tray_read_done_bits) = &ams.tray_read_done_bits {
            if let Ok(tray_read_done_bits) = u32::from_str_radix(tray_read_done_bits, 16) {
                if self.tray_read_done_bits != Some(tray_read_done_bits) {
                    self.tray_read_done_bits = Some(tray_read_done_bits);
                    change_made = true;
                }
            }
        }
        // tray_reading - which trays (from those that exist) that are currently being "read" (meaning ams is rotating them to get them ready)
        if let Some(tray_reading_bits) = &ams.tray_reading_bits {
            if let Ok(tray_reading_bits) = u32::from_str_radix(tray_reading_bits, 16) {
                if self.tray_reading_bits != Some(tray_reading_bits) {
                    self.tray_reading_bits = Some(tray_reading_bits);
                    change_made = true;
                }
            }
        }

        for tray_id in 0..self.ams_trays.len() {
            let (ams_id, ams_tray_id) = BambuPrinter::get_ams_and_tray_id(tray_id);
            let ams_id_str = format!("{ams_id}");
            let source_tray = if let Some(amss) = &ams.ams {
                let ams = amss.iter().find(|v| v.id == ams_id_str);
                if let Some(ams_data) = ams {
                    ams_data.tray.iter().find(|v| v.id as usize == ams_tray_id)
                } else {
                    None
                }
            } else {
                None
            };
            let old_tray = &self.ams_trays[tray_id];
            let new_tray = self.get_updated_tray(old_tray, source_tray, Some(tray_id));
            if let Some(new_tray) = new_tray {
                change_made = true;
                self.ams_trays[tray_id] = new_tray;
            }
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__push_status__vt_tray(&mut self, v_tray: &PrintTray) -> bool {
        let old_tray = self.virt_tray.clone();
        let new_tray = self.get_updated_tray(&old_tray, Some(v_tray), None);
        if let Some(new_tray) = new_tray {
            self.virt_tray = new_tray;
            return true;
        }
        false
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__ams_filament_setting(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;

        // updating ONLY filament and not state for the theoretical case when filament is set externally when there isn't a spool
        // theoretically possible if want to supssport that in this app using nfc as a source for example
        if let Some(tray_id) = print.tray_id {
            let tray_info_idx = print.tray_info_idx.as_ref().cloned().unwrap_or_default();
            let new_filament = if tray_info_idx.is_empty() {
                Filament::Unknown
            } else {
                Filament::Known(FilamentInfo {
                    tray_info_idx,
                    tray_type: print.tray_type.as_ref().cloned().unwrap_or_default(),
                    tray_color: print.tray_color.as_ref().cloned().unwrap_or_default(),
                    nozzle_temp_max: print.nozzle_temp_max.unwrap_or(250),
                    nozzle_temp_min: print.nozzle_temp_min.unwrap_or(190),
                    calibrations: HashMap::new(),
                })
            };
            if tray_id == 254 {
                // Handle external tray
                if new_filament == Filament::Unknown {
                    self.virt_tray.state = TrayState::Empty;
                } else {
                    self.virt_tray.state = TrayState::Ready;
                }
                self.virt_tray.filament = new_filament;
                self.virt_tray.k = None; // Is this correct to do?
            } else {
                // Handle AMS tray
                if let Some(ams_id) = print.ams_id {
                    // no change to tray state in case of AMS
                    let ams_id = usize::try_from(ams_id).unwrap();
                    self.ams_trays[ams_id * 4 + usize::try_from(tray_id).unwrap()].filament = new_filament;
                    self.ams_trays[ams_id * 4 + usize::try_from(tray_id).unwrap()].k = None;
                    // Is this correct to do?
                }
            }
            change_made = true;
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_sel(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        if let (Some(nozzle_diameter), Some(tray_id), Some(cali_idx)) = (&print.nozzle_diameter, &print.tray_id, &print.cali_idx) {
            if *tray_id >= 0 {
                let tray_id: usize = (*tray_id).try_into().unwrap();
                let k = self.get_cali_k_value(nozzle_diameter, *cali_idx);
                let tray = if tray_id == 254 {
                    &mut self.virt_tray
                } else {
                    &mut self.ams_trays[tray_id]
                };
                // TODO: This snippet is in two places, fix that
                tray.cali_idx = if *cali_idx == -1 { None } else { Some(*cali_idx) };
                tray.k = k.or(Some(format!("({:.3})", 0.02))); // TODO: where to bring default from, all materials 0.020?
                if let (Some(nozzle_diameter), Some(cali_idx)) = (self.nozzle_diameter.as_ref(), tray.cali_idx.as_ref()) {
                    if let Some(calibrations) = self.calibrations.get(nozzle_diameter) {
                        if let Some(calibration) = calibrations.get(cali_idx) {
                            if let Filament::Known(ref mut filament_info) = tray.filament {
                                filament_info.calibrations.insert(nozzle_diameter.clone(), calibration.clone());
                            }
                        }
                    }
                }
                change_made = true;
            }
        }
        change_made
    }

    #[allow(non_snake_case)]
    pub fn process_print_message__extrusion_cali_get(&mut self, print: &bambu_api::PrintData) -> bool {
        let mut change_made = false;
        let nozzle_diameter = match &print.nozzle_diameter {
            Some(nozzle_diameter) => nozzle_diameter,
            None => return false,
        };
        // filament_id either empty string (so entire list) or something
        let filament_id = match &print.filament_id {
            Some(filament_id) => filament_id,
            None => return false,
        };

        if let Some(ref filaments) = print.filaments {
            change_made = true;
            let nozzle_calibrations = self.calibrations.entry_ref(nozzle_diameter).or_default(); //insert(HashMap::new()) let calibration = Calibration::from(filament);
            if filament_id.is_empty() {
                nozzle_calibrations.clear();
            } else {
                nozzle_calibrations.retain(|_k, v| &v.filament_id != filament_id);
            }
            for filament in filaments {
                let calibration = Calibration::from(filament);
                nozzle_calibrations.insert(filament.cali_idx, calibration);
            }
            for i in 0..self.ams_trays.len() {
                self.ams_trays[i].k = self.get_tray_cali_k_value(&self.ams_trays[i]);
            }
            self.virt_tray.k = self.get_tray_cali_k_value(&self.virt_tray);
        }

        change_made
    }

    pub fn process_print_message(&mut self, print: &bambu_api::PrintData) -> bool {
        if let Some(sequence_id) = &print.sequence_id {
            dbgt!("-> Message ", sequence_id);
        } else {
            warn!("-> Message with No sequence_id ?");
        }
        // important: Can't issue event from here because this method is called with a mut reference (even if behind RefCell)
        // Therefore, to issue an event need to call update_ams_trays_done afterwards through a non mut reference (so not borrow_mut if refcell)
        //   in order to issue the event on observers
        let mut change_made = false;
        if let Some(command) = &print.command {
            if command == "push_status" {
                debug!("             {command} message");
                let mut nozzle_diameter_change_made = false;
                let mut ams_change_made = false;
                let mut vt_tray_change_made = false;
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    let old_nozzle_diameter = self.nozzle_diameter.clone();
                    self.nozzle_diameter = Some(nozzle_diameter.clone());
                    nozzle_diameter_change_made = old_nozzle_diameter != self.nozzle_diameter;
                }
                if let Some(ams) = &print.ams {
                    ams_change_made = self.process_print_message__push_status__ams(ams);
                }
                if let Some(v_tray) = &print.vt_tray {
                    vt_tray_change_made = self.process_print_message__push_status__vt_tray(v_tray);
                }
                change_made = nozzle_diameter_change_made || ams_change_made || vt_tray_change_made;
            } else if command == "ams_filament_setting" {
                change_made = self.process_print_message__ams_filament_setting(print)
            } else if command == "extrusion_cali_set" {
                // trigger request command for cali_get (request, not response)
                debug!("             {command} message");
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    self.fetch_filament_calibrations(nozzle_diameter);
                }
                change_made = true;
            } else if command == "extrusion_cali_del" {
                // trigger request command for cali_get (request, not response)
                debug!("             {command} message");
                if let Some(nozzle_diameter) = &print.nozzle_diameter {
                    self.fetch_filament_calibrations(nozzle_diameter);
                }
                change_made = true;
            } else if command == "extrusion_cali_sel" {
                // update the tray with the new k factor
                debug!("             {command} message");
                change_made = self.process_print_message__extrusion_cali_sel(print)
            } else if command == "extrusion_cali_get" {
                // TODO: Check: distinguish between command that was sent and the result, which are structured the same
                // here we want to process only the results (the one that includes the list of filaments )
                debug!("             {command} message");
                change_made = self.process_print_message__extrusion_cali_get(print);
            }
        }
        change_made
    }

    pub fn update_ams_trays_done(&self, prev_trays_reading_bits: Option<u32>, new_trays_reading_bits: Option<u32>) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer
                .borrow_mut()
                .on_trays_update(self, prev_trays_reading_bits, new_trays_reading_bits);
        }
    }

    // TODO: Unify sending messages, no need for two functions

    pub fn publish_payload(&self, payload: String) {
        debug!("MQTT Publish: {}", payload);

        let topic_name = format!("device/{}/request", self.app_config.borrow().printer_serial.as_ref().unwrap());
        let topic_name = topic_name.as_str();

        let packet = mqttrust::Packet::Publish(mqttrust::Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            pid: Some(mqttrust::encoding::v4::Pid::new()),
            retain: false,
            topic_name,
            payload: payload.as_bytes(),
        });
        let message = BufferedMqttPacket::try_from(packet).unwrap();
        let _ = self.write_packets.try_send(message);
    }

    // TODO: Unify sending messages, no need for two functions

    pub async fn publish_payload_async(
        printer_serial: &String,
        write_packets: &'static embassy_sync::channel::Channel<
            embassy_sync::blocking_mutex::raw::NoopRawMutex,
            crate::my_mqtt::BufferedMqttPacket,
            3,
        >,
        payload: String,
    ) {
        debug!("MQTT Publish: {}", payload);
        let topic_name = format!("device/{}/request", printer_serial);
        let topic_name = topic_name.as_str();

        let packet = mqttrust::Packet::Publish(mqttrust::Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            pid: Some(mqttrust::encoding::v4::Pid::new()),
            retain: false,
            topic_name,
            payload: payload.as_bytes(),
        });
        let message = BufferedMqttPacket::try_from(packet).unwrap();
        write_packets.send(message).await;
    }

    pub async fn request_full_update(
        printer_serial: &String,
        write_packets: &'static embassy_sync::channel::Channel<
            embassy_sync::blocking_mutex::raw::NoopRawMutex,
            crate::my_mqtt::BufferedMqttPacket,
            3,
        >,
    ) {
        let cmd = crate::bambu_api::PushAllCommand::new();
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, write_packets, payload).await;
    }

    pub fn fetch_filament_calibrations(&self, nozzle_diameter: &str) {
        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);
    }

    pub async fn fetch_filament_calibrations_async(
        printer_serial: &String,
        write_packets: &'static embassy_sync::channel::Channel<
            embassy_sync::blocking_mutex::raw::NoopRawMutex,
            crate::my_mqtt::BufferedMqttPacket,
            3,
        >,
        nozzle_diameter: &str,
    ) {
        let cmd = crate::bambu_api::ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        BambuPrinter::publish_payload_async(printer_serial, write_packets, payload).await;
    }

    pub fn set_tray_filament(&self, tray_id: i32, filament: &FilamentInfo) {
        let ams_id: u32;
        let ams_tray_id;

        if tray_id == 254 {
            ams_id = 255;
            ams_tray_id = 254
        } else {
            ams_id = u32::try_from(tray_id).unwrap() / 4;
            ams_tray_id = tray_id % 4;
        }

        let setting_id = if let Some(calibration) = self.get_filament_calibration_for_current_nozzle(filament) {
            Some(calibration.setting_id.as_str())
        } else {
            None
        };

        let cmd = crate::bambu_api::AmsFilamentSettingCommand::new(
            ams_id,
            ams_tray_id, // here we need the tray_id within the specific ams
            &filament.tray_info_idx,
            setting_id,
            &filament.tray_type,
            &filament.tray_color,
            filament.nozzle_temp_min,
            filament.nozzle_temp_max,
        );
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);

        let mut cali_idx = -1;

        // If the filament info contains calibration for the current nozzle and the printer calibrations contain that calibration-idx for that nozzle diameter then send that, otherwise send -1 (so no calibration)
        if let Some(filament_calibration) = filament.calibrations.get(self.nozzle_diameter.as_ref().unwrap()) {
            if let Some(printer_calibrations) = self.calibrations.get(self.nozzle_diameter.as_ref().unwrap()) {
                if printer_calibrations.contains_key(&filament_calibration.cali_idx) {
                    cali_idx = filament_calibration.cali_idx;
                }
            }
        }

        let cmd = crate::bambu_api::ExtrusionCaliSelCommand::new(
            &self.nozzle_diameter.clone().unwrap_or_default(),
            tray_id,                 // here we need the original tray_id
            &filament.tray_info_idx, // tray_info_idx is filament_id in this command
            Some(cali_idx),
        );
        let payload = serde_json::to_string_pretty(&cmd).unwrap();
        self.publish_payload(payload);
    }
}

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tray {
    pub state: TrayState,
    pub filament: Filament,
    pub k: Option<String>,
    pub cali_idx: Option<i32>,
}

impl Tray {
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            state: TrayState::Empty,
            ..Default::default()
        }
    }
    pub fn unknown() -> Self {
        Self {
            state: TrayState::Unknown,
            ..Default::default()
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TrayState {
    #[default]
    Unknown,
    Empty,     // Empty - known to be empty
    Spool,     // When a spool is placed into the slot
    Reading,   // Reading - during the process of inserting spool into AMS
    Ready,     // Ready - there is a spool, it is not loaded to the extruder now
    Loading,   // Loading - during the process of loading into the extruder
    Unloading, // Unloading - during the process of unloading from the extruder
    Loaded,    // Loaded - in the extruder
}

////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
pub enum Error {
    ParseError,
    MissingFields,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Filament {
    #[default]
    Unknown,
    Known(FilamentInfo),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilamentInfo {
    pub tray_info_idx: String,                      // e.g. "GFL99"
    pub tray_type: String,                          // e.g. "PLA"
    pub tray_color: String,                         // e.g. "2323F7FF"
    pub nozzle_temp_max: u32,                       // e.g. 250
    pub nozzle_temp_min: u32,                       // w.g. 190
    pub calibrations: HashMap<String, Calibration>, // calibration for nozzles
}

impl FilamentInfo {
    pub fn to_descriptor(&self, printer_name: &Option<String>) -> String {
        let mut inner_calibrations_part = String::new();
        let printer_name = printer_name.as_ref();

        let empty = "".to_string();
        let k_prefix = printer_name.unwrap_or(&empty);
        let k_prefix = if !k_prefix.is_empty() {
            format!("&{}(", my_encode_to_url_part(k_prefix))
        } else {
            "&".to_string()
        };
        let k_postfix = if !k_prefix.is_empty() { ")" } else { "" };

        for calibration_kv in self.calibrations.iter() {
            if let Some(cal_nozzle_diameter_char) = calibration_kv.0.chars().nth(2) {
                let calibration = calibration_kv.1;
                inner_calibrations_part += &format!(
                    "K{}={}~{}~{}",
                    cal_nozzle_diameter_char,
                    calibration.k_value.trim_end_matches('0'),
                    &calibration.setting_id,
                    &my_encode_to_url_part(&calibration.name)
                );
            }
        }
        let calibrations_part = if inner_calibrations_part.is_empty() {
            inner_calibrations_part
        } else {
            format!("{k_prefix}{inner_calibrations_part}{k_postfix}")
        };
        format!(
            "{FILAMENT_URL_PREFIX}V1?ID={TAG_PLACEHOLDER}&M={}&C={}&NN={}&NX={}{}&FI={}",
            self.tray_type, self.tray_color, self.nozzle_temp_min, self.nozzle_temp_max, calibrations_part, self.tray_info_idx
        )
    }

    pub fn new() -> Self {
        Self {
            tray_info_idx: String::from(""),
            tray_type: String::from(""),
            tray_color: String::from(""),
            nozzle_temp_max: 0,
            nozzle_temp_min: 0,
            calibrations: HashMap::new(),
        }
    }

    pub fn from_descriptor(descriptor: &str, bambu_printer: &BambuPrinter) -> Result<Self, Error> {
        let mut filament_info_result = FilamentInfo::new();
        if !(descriptor.starts_with(FILAMENT_URL_PREFIX)) {
            return Err(Error::ParseError);
        }
        let descriptor = descriptor.trim_start_matches(FILAMENT_URL_PREFIX);

        let mut id = false;
        let mut v = false;
        let mut m = false;
        let mut fi = false;
        let mut c = false;
        let mut nn = false;
        let mut nx = false;
        for param in descriptor.split(['&', '/', '?']) {
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
                    }
                    // Material / Tray Type (material code in some other form)
                    "M" => {
                        filament_info_result.tray_type = String::from(param_value);
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
                        nn = true;
                    }
                    // Nozzle maX Temp
                    "NX" => {
                        if let Ok(ret_val) = param_value.parse::<u32>() {
                            filament_info_result.nozzle_temp_max = ret_val;
                        } else {
                            return Err(Error::ParseError);
                        }
                        nx = true;
                    }
                    // "K4" | "K2" | "K6" | "K8" => (),
                    // // Filament Id/ Tray Index (material code in some form) - looks like Bambu specific
                    "FI" => {
                        filament_info_result.tray_info_idx = String::from(param_value);
                        fi = true;
                    }
                    _ => (), //return Err(Error::ParseError), TODO: verify match to pattern, or even run what's coming next inside here
                }
            }
        }

        // Second pass on parts that need to be processed after the first
        for param in descriptor.split(&['/', '&', '?']) {
            let mut param = param;
            let re = Regex::new(r"^(.*)\((K.*)\)$").unwrap();
            if let Some(captures) = re.captures(param) {
                // to get k data use match 2
                if let Some(param_match) = captures.get(2) {
                    param = param_match.as_str();
                }
                // to get the printer name use match 1 and don't forget to my_decode_from_url_part the data
                // currently not used, could compare to current printer name and ignore
            }
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

                        // Here there is room for flexibility.
                        // We have K, filament_id (from filament info as tray_info_idx), setting_id and name
                        // And current nozzle diameter
                        // There are is redundancy of information to identify the relevant calibration
                        // I currently prefer to find relevant calibration by searching K & filament_id & setting_id match in the calibrations of current nozzle diameter, ignoring name (which is easy to rename).
                        // But K may have changed on another spool for same filament, in such I select it based on the name.
                        // This means K is prioritized over name. Here is reasoning for either priorities
                        // I could also ignore K, or force only K and find something that match the K
                        // I can also check what to do exactly based on printer name - if its the original printer or not - see belo comment

                        if let Some(nozzle_calibrations) = bambu_printer.calibrations.get(&nozzle_diameter) {
                            if let Some(calibration) = nozzle_calibrations.values().find(|v| {
                                v.k_value.trim_end_matches('0') == k_value.trim_end_matches('0')
                                    && v.filament_id == filament_info_result.tray_info_idx
                                    && v.setting_id == setting_id
                            }) {
                                let calibration = Calibration::new_minimal(
                                    k_value,
                                    &calibration.filament_id,
                                    &calibration.setting_id,
                                    &calibration.name,
                                    calibration.cali_idx,
                                );
                                filament_info_result.calibrations.insert(nozzle_diameter, calibration);
                            } else if let Some(calibration) = nozzle_calibrations.values().find(|v| {
                                // TODO: Key note for multiprinter support
                                // if I'll remove the setting_id check it will allow tag from one printer to match another if PA profile named the same
                                // if I make similarity on name, it will be more flexible, maybe be flexible around color names
                                // I can also check what to do exactly based on printer name - if its the original printer or not
                                v.name.trim() == name.trim() && v.filament_id == filament_info_result.tray_info_idx && v.setting_id == setting_id
                            }) {
                                let calibration = Calibration::new_minimal(
                                    &calibration.k_value,
                                    &calibration.filament_id,
                                    &calibration.setting_id,
                                    &calibration.name,
                                    calibration.cali_idx,
                                );
                                filament_info_result.calibrations.insert(nozzle_diameter, calibration);
                            }
                        }
                    }
                    _ => (), // previous run already identified unrecognized parameters, here we skip also those that were ok so can't error
                }
            }
        }
        if v && id && m && fi && c && nn && nx {
            Ok(filament_info_result)
        } else {
            Err(Error::MissingFields)
        }
    }
}

const ENCODING_TABLE: [(char, &str); 8] = [
    ('%', "%25"),
    ('/', "%2F"),
    ('&', "%26"),
    ('?', "%3F"),
    (' ', "%20"),
    ('(', "%28"),
    (')', "%29"),
    ('~', "%7E"),
];

static ENCODING_MAP: Lazy<Mutex<CriticalSectionRawMutex, HashMap<char, &str>>> = Lazy::new(|| {
    let char_hashmap: HashMap<char, &str> = ENCODING_TABLE.into_iter().collect();
    Mutex::new(char_hashmap)
});

fn my_decode_from_url_part(text: &str) -> String {
    // % must be last (because some originated from encodings and will need to be replaced first)
    // let name = name.replace("%7E", "/").replace("%2F", "/").replace("%28", "(").replace("%29", ")").replace("%26", "&").replace("%3F", "?").replace("%20", " ").replace("%25", "%");
    efficient_decode(text, &ENCODING_TABLE)
}

fn my_encode_to_url_part(text: &str) -> String {
    // % must be first (because later added)
    // let name = name.replace("%", "%25").replace("/", "%2F").replace("&", "%26").replace("?", "%3F").replace(" ", "%20").replace("(", "%28").replace(")", "%29").replace( "~","%7E");
    ENCODING_MAP.lock(|encoding_map| efficient_encode(text, encoding_map))
}

/// Encodes specific characters in a string based on a provided mapping.
/// Minimizes allocations while still returning a String.
///
/// # Arguments
/// * `input` - The string to encode
/// * `char_map` - A mapping of characters to their encoded string representation
///
/// # Returns
/// The encoded string
pub fn efficient_encode(input: &str, char_map: &HashMap<char, &str>) -> String {
    // Pre-calculate output size to avoid reallocations
    let mut capacity = 0;
    for c in input.chars() {
        capacity += match char_map.get(&c) {
            Some(replacement) => replacement.len(),
            None => c.len_utf8(),
        };
    }

    // Pre-allocate output string with exact capacity needed
    let mut result = String::with_capacity(capacity);

    // Process each character
    for c in input.chars() {
        match char_map.get(&c) {
            Some(replacement) => result.push_str(replacement),
            None => result.push(c),
        }
    }

    result
}

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

impl From<bambu_api::PrintTray> for FilamentInfo {
    fn from(v: bambu_api::PrintTray) -> Self {
        Self {
            tray_info_idx: v.tray_info_idx.unwrap_or_default(),
            tray_type: v.tray_type.unwrap_or_default(),
            tray_color: v.tray_color.unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
            calibrations: HashMap::new(),
        }
    }
}
// impl From<bambu_api::PrintVtTray> for FilamentInfo {
//     fn from(v: bambu_api::PrintVtTray) -> Self {
//         Self {
//             tray_info_idx: v.tray_info_idx.unwrap_or_default(),
//             tray_type: v.tray_type.unwrap_or_default(),
//             tray_color: v.tray_color.unwrap_or_default(),
//             nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
//             nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
//             calibrations: HashMap::new(),
//         }
//     }
// }
impl From<&bambu_api::PrintTray> for FilamentInfo {
    fn from(v: &bambu_api::PrintTray) -> Self {
        Self {
            tray_info_idx: v.tray_info_idx.as_ref().cloned().unwrap_or_default(),
            tray_type: v.tray_type.as_ref().cloned().unwrap_or_default(),
            tray_color: v.tray_color.as_ref().cloned().unwrap_or_default(),
            nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
            nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
            calibrations: HashMap::new(),
        }
    }
}
// impl From<&bambu_api::PrintVtTray> for FilamentInfo {
//     fn from(v: &bambu_api::PrintVtTray) -> Self {
//         Self {
//             tray_info_idx: v.tray_info_idx.as_ref().cloned().unwrap_or_default(),
//             tray_type: v.tray_type.as_ref().cloned().unwrap_or_default(),
//             tray_color: v.tray_color.as_ref().cloned().unwrap_or_default(),
//             nozzle_temp_max: v.nozzle_temp_max.unwrap_or(250),
//             nozzle_temp_min: v.nozzle_temp_min.unwrap_or(190),
//             calibrations: HashMap::new(),
//         }
//     }
// }
/////////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Calibration {
    filament_id: String,
    k_value: String,
    n_coef: f32,
    setting_id: String,
    name: String,
    cali_idx: i32,
}

impl From<&bambu_api::Filament> for Calibration {
    fn from(v: &bambu_api::Filament) -> Self {
        Self {
            filament_id: v.filament_id.clone(),
            name: v.name.clone(),
            k_value: v.k_value.clone(),
            n_coef: f32::from_str(&v.n_coef).unwrap_or(-1.0),
            setting_id: v.setting_id.clone(),
            cali_idx: v.cali_idx,
        }
    }
}

impl Calibration {
    pub fn new_minimal(k_value: &str, filament_id: &str, setting_id: &str, name: &str, cali_idx: i32) -> Self {
        Self {
            k_value: String::from(k_value),
            filament_id: String::from(filament_id),
            setting_id: String::from(setting_id),
            name: String::from(name),
            cali_idx,
            ..Default::default()
        }
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////

// needs to be async to get a spawner even though shouldn't be async
pub async fn init(
    // Initializes stuff for Main Thread
    stack: Stack<'static>,
    app_config: Rc<RefCell<AppConfig>>,
    tls: TlsReference<'static>,
) -> Rc<RefCell<BambuPrinter>> {
    let spawner = embassy_executor::Spawner::for_current_executor().await;

    // == Setup MQTT ==================================================================
    let write_packets = mk_static!(
        embassy_sync::channel::Channel< embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3,>,
        embassy_sync::channel::Channel::< embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 3,>::new()
    );
    let read_packets = mk_static!(
        embassy_sync::pubsub::PubSubChannel<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 5, 2, 1,>,
        embassy_sync::pubsub::PubSubChannel::<embassy_sync::blocking_mutex::raw::NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 5, 2, 1,>::new()
    );

    spawner
        .spawn(bambu_mqtt_task(stack, read_packets, write_packets, app_config.clone(), tls))
        .ok();

    let bambu_printer_model = Rc::new(RefCell::new(BambuPrinter::new(write_packets, app_config)));

    let spawner = embassy_executor::Spawner::for_current_executor().await;
    spawner.spawn(incoming_messages_task(read_packets, bambu_printer_model.clone())).ok();

    spawner.spawn(fetch_initial_info(bambu_printer_model.clone())).ok();

    bambu_printer_model
}

// Important: This is the initial load task. Because it issues more commands than can fit the Channel, it can't await while borrowing bambu_printer
// in order to sendi messages over the channel. If it would, then it would await while bambu_printer is borrowed, and the response invokes the printer
// and will panic due to borrow_mut (response) while already borrowed here (RefCell will panic at runtine).
// This was tested to verify this indeed happens.
// Therefore, the code takes the data required from the bambu_printer and pass it to the functions that aren't methods because of that.
#[embassy_executor::task]
pub async fn fetch_initial_info(bambu_printer: Rc<RefCell<BambuPrinter>>) {
    let write_packets = bambu_printer.borrow().write_packets;
    let printer_serial = bambu_printer
        .borrow()
        .app_config
        .borrow()
        .printer_serial
        .as_ref()
        .unwrap_or(&"NO-SERIAL".to_string())
        .clone();

    // fetch first setting for all nozzles, need that in advance before getting filaments
    let nozzle_diameters = ["0.8", "0.6", "0.2", "0.4"];
    for nozzle_diameter in nozzle_diameters {
        BambuPrinter::fetch_filament_calibrations_async(&printer_serial, write_packets, nozzle_diameter).await;
    }

    // Now request full update, and wait until data is processed and have the nozzle diameter at hand for next request
    BambuPrinter::request_full_update(&printer_serial, write_packets).await;
    while bambu_printer.borrow().nozzle_diameter.is_none() {
        Timer::after_millis(100).await;
    }

    // Get again the filaments for current nozzle size,
    // that's because in slicer they don't check if data received from printer it's current nozzle or not
    // it's a bug there, can even be reproduced in the slicer by switching in the manage results to another nozzle diameter
    let curr_nozzle_diameter = bambu_printer.borrow().nozzle_diameter.as_ref().unwrap().clone();
    BambuPrinter::fetch_filament_calibrations_async(&printer_serial, write_packets, &curr_nozzle_diameter).await;
}

#[embassy_executor::task]
pub async fn incoming_messages_task(
    read_packets: &'static PubSubChannel<NoopRawMutex, BufferedMqttPacket, 5, 2, 1>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
) {
    let mut subscriber = read_packets.subscriber().unwrap();
    const KEEP_ALIVE_SEC: u32 = 20;

    let mut printer_known_to_be_up = false;
    loop {
        let wait_res = with_timeout(Duration::from_secs(KEEP_ALIVE_SEC as u64), subscriber.next_message_pure()).await;
        match wait_res {
            Ok(packet) => {
                printer_known_to_be_up = true;
                if let Ok(p) = mqttrust::Packet::try_from(&packet) {
                    #[allow(clippy::single_match)]
                    match p {
                        mqttrust::Packet::Publish(mqttrust::Publish {
                            dup: _,
                            qos: _,
                            pid: _,
                            retain: _,
                            topic_name: _,
                            payload,
                        }) => {
                            let parse_res = serde_json::from_slice::<bambu_api::Print>(payload);
                            if let Ok(print) = parse_res {
                                debug!("MQTT Receive: {:?}", print);
                                let previous_reading_bits = bambu_printer.borrow().tray_reading_bits;
                                let change_made = (*bambu_printer.borrow_mut()).process_print_message(&print.print);
                                let updated_reading_bits = bambu_printer.borrow().tray_reading_bits;
                                if change_made {
                                    (*bambu_printer.borrow()).update_ams_trays_done(previous_reading_bits, updated_reading_bits);
                                }
                            } else {
                                warn!("Unprocessed message {:?} : {:?}", parse_res, core::str::from_utf8(payload));
                            }
                        }

                        _ => (),
                    }
                } else {
                    error!("Unparsable MQTT message, this means an internal bug");
                }
            }
            Err(_) => {
                if printer_known_to_be_up {
                    warn!("Printer connectivity issues suspected (uncertain), checking");
                    let write_packets = bambu_printer.borrow().write_packets;
                    let printer_serial = bambu_printer
                        .borrow()
                        .app_config
                        .borrow()
                        .printer_serial
                        .as_ref()
                        .unwrap_or(&"NO-SERIAL".to_string())
                        .clone();
                    BambuPrinter::request_full_update(&printer_serial, write_packets).await;
                    printer_known_to_be_up = false;
                }
            }
        }
    }
}

// Usage example, this should be in the client code using the generic_mqtt_task, specific per scenario
// This indirection is because embassy can't have generic functions as tasks
// https://github.com/embassy-rs/embassy/issues/2454#issuecomment-2336644031
// This is specific to the hw and required detailes (buffer sizes, etc.)
#[embassy_executor::task]
pub async fn bambu_mqtt_task(
    stack: Stack<'static>,
    read_packets: &'static PubSubChannel<NoopRawMutex, BufferedMqttPacket, 5, 2, 1>,
    write_packets: &'static Channel<NoopRawMutex, BufferedMqttPacket, 3>,
    app_config: Rc<RefCell<AppConfig>>,
    tls: TlsReference<'static>,
) {
    let app_config_borrow = app_config.borrow();
    let printer_serial_config = &(app_config_borrow.printer_serial);
    let printer_access_code_config = &(app_config_borrow.printer_access_code);

    let mut printer_login_exist = false;
    if let (Some(printer_serial), Some(printer_access_code)) = (printer_serial_config, printer_access_code_config) {
        if !printer_serial.is_empty() && !printer_access_code.is_empty() {
            printer_login_exist = true;
        }
    }

    drop(app_config_borrow); // Important so it won't continue being borrowed forever and fail in other places

    if !printer_login_exist {
        term_info!("Missing Printer Serial and/or Access Code configurations");
        return;
    }

    let socket_rx_buffer = mk_static!([u8; 8192], [0; 8192]);
    let socket_tx_buffer = mk_static!([u8; 4096], [0; 4096]);

    let no_serial = "NO-SERIAL".to_string();
    let app_config_borrow = app_config.borrow();
    let printer_serial = app_config_borrow.printer_serial.as_ref().unwrap_or(&no_serial).clone();
    drop(app_config_borrow);

    let subscribe_topics = [mqttrust::SubscribeTopic {
        topic_path: &format!("device/{}/report", printer_serial),
        qos: mqttrust::QoS::AtLeastOnce,
    }];

    info!("Waiting for IP in Bambu Mqtt Task");
    // let mut wait_counter = 0;
    // const SKIP_CHECKS: i32 = 4;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after(Duration::from_millis(250)).await;
    }
    info!("From Bambu MQTT - got IP");
    Timer::after(Duration::from_millis(250)).await; // So log will come after wifi log

    let printer_ip: Ipv4Address;
    let printer_name: String;

    if app_config.borrow().printer_ip.is_none() {
        term_info!("No Printer IP configured, discovering Printer");
        let (mut rx_buffer1, mut rx_buffer2) = ([0; 512], [0; 512]);
        let (mut tx_buffer1, mut tx_buffer2) = ([0; 0], [0; 0]);
        let (mut rx_meta1, mut rx_meta2) = (
            [embassy_net::udp::PacketMetadata::EMPTY; 16],
            [embassy_net::udp::PacketMetadata::EMPTY; 16],
        );
        let (mut tx_meta1, mut tx_meta2) = (
            [embassy_net::udp::PacketMetadata::EMPTY; 16],
            [embassy_net::udp::PacketMetadata::EMPTY; 16],
        );
        let (mut buf1, mut buf2) = ([0; 512], [0; 512]);

        let _ = stack.join_multicast_group(embassy_net::Ipv4Address::new(239, 255, 255, 250)).unwrap();
        let recv_source_endpoint1 = embassy_net::IpEndpoint {
            addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
            port: 1990,
        };
        let mut recv_socket1 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta1, &mut rx_buffer1, &mut tx_meta1, &mut tx_buffer1);
        recv_socket1.bind(recv_source_endpoint1).unwrap();

        let recv_source_endpoint2 = embassy_net::IpEndpoint {
            addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
            port: 2021,
        };
        let mut recv_socket2 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta2, &mut rx_buffer2, &mut tx_meta2, &mut tx_buffer2);
        recv_socket2.bind(recv_source_endpoint2).unwrap();

        'outer_loop: loop {
            debug!("Waiting for SSDP UDP");

            let data = match select(recv_socket1.recv_from(&mut buf1), recv_socket2.recv_from(&mut buf2)).await {
                Either::First(Ok(inner_res)) => {
                    let data = &buf1[0..inner_res.0];
                    Ok(data)
                }
                Either::Second(Ok(inner_res)) => {
                    let data = &buf2[0..inner_res.0];
                    Ok(data)
                }
                _ => {
                    error!("There was some error");
                    Err("Error waiting for data")
                }
            };

            if let Ok(data) = data {
                if let Ok(s) = core::str::from_utf8(data) {
                    if s.contains("NT: urn:bambulab-com:device:3dprinter") && s.contains(&printer_serial) {
                        let mut found_printer_ip = None;
                        let mut found_printer_name = None;
                        for line in s.lines() {
                            if let Some((first, second)) = line.split_once(" ") {
                                match first {
                                    "Location:" => {
                                        if let Ok(ip) = embassy_net::Ipv4Address::from_str(second) {
                                            found_printer_ip = Some(ip);
                                        }
                                    }
                                    "DevName.bambu.com:" => {
                                        found_printer_name = Some(String::from(second));
                                    }
                                    _ => (),
                                }
                            }
                        }
                        if found_printer_ip.is_some() {
                            printer_ip = found_printer_ip.unwrap();
                            printer_name = found_printer_name.as_ref().unwrap_or(&String::from("Unknown")).to_string();
                            term_info!("Discovered Printer at {}", printer_ip);
                            term_info!("Printer named '{}'", &printer_name);
                            break 'outer_loop;
                        }
                    }
                }
            }
        }
    } else {
        printer_ip = app_config.borrow().printer_ip.unwrap();
        printer_name = app_config.borrow().printer_name.as_ref().unwrap_or(&String::from("Unknown")).to_string();
    }

    // Final name, theoretically if name explicitly supplied and IP not,  this could override the supplied name
    app_config.borrow_mut().printer_ip = Some(printer_ip);
    app_config.borrow_mut().printer_name = Some(printer_name);

    let remote_endpoint = (printer_ip, 8883);
    let password = {
        // this is in braces to remove warning
        let app_config_borrow = app_config.borrow();
        Some(
            app_config_borrow
                .printer_access_code
                .as_ref()
                .unwrap_or(&"NO-ACCESS-CODE".to_string())
                .clone()
                .into_bytes(),
        )
    };

    crate::my_mqtt::generic_mqtt_task(
        remote_endpoint,
        &printer_serial,
        Some("bblp"),
        password,
        0,
        &subscribe_topics,
        stack,
        write_packets,
        read_packets,
        socket_rx_buffer,
        socket_tx_buffer,
        Duration::from_secs(20),
        app_config,
        tls,
    )
    .await
}

pub async fn _wait_for_printer_ssdp(stack: Stack<'static>, duration: Duration) -> Option<String> {
    let (mut rx_buffer1, mut rx_buffer2) = ([0; 512], [0; 512]);
    let (mut tx_buffer1, mut tx_buffer2) = ([0; 0], [0; 0]);
    let (mut rx_meta1, mut rx_meta2) = (
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
    );
    let (mut tx_meta1, mut tx_meta2) = (
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
    );
    let (mut buf1, mut buf2) = ([0; 512], [0; 512]);

    let _ = stack.join_multicast_group(embassy_net::Ipv4Address::new(239, 255, 255, 250)).unwrap();
    let recv_source_endpoint1 = embassy_net::IpEndpoint {
        addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
        port: 1990,
    };
    let mut recv_socket1 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta1, &mut rx_buffer1, &mut tx_meta1, &mut tx_buffer1);
    recv_socket1.bind(recv_source_endpoint1).unwrap();

    let recv_source_endpoint2 = embassy_net::IpEndpoint {
        addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
        port: 2021,
    };
    let mut recv_socket2 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta2, &mut rx_buffer2, &mut tx_meta2, &mut tx_buffer2);
    recv_socket2.bind(recv_source_endpoint2).unwrap();

    let deadline = Instant::now().checked_add(duration).unwrap();

    '_outer_loop: loop {
        debug!("Waiting for SSDP UDP");

        let data = with_deadline(deadline, select(recv_socket1.recv_from(&mut buf1), recv_socket2.recv_from(&mut buf2))).await;

        if let Ok(data) = data {
            let data = match data {
                Either::First(Ok(inner_res)) => {
                    let data = &buf1[0..inner_res.0];
                    Ok(data)
                }
                Either::Second(Ok(inner_res)) => {
                    let data = &buf2[0..inner_res.0];
                    Ok(data)
                }
                _ => {
                    error!("There was some error");
                    Err("Error waiting for data")
                }
            };

            if let Ok(data) = data {
                if let Ok(s) = core::str::from_utf8(data) {
                    if s.contains("NT: urn:bambulab-com:device:3dprinter") {
                        let mut _printer_ip;
                        let printer_name;
                        for line in s.lines() {
                            if let Some((first, second)) = line.split_once(" ") {
                                match first {
                                    "Location:" => {
                                        if let Ok(ip) = embassy_net::Ipv4Address::from_str(second) {
                                            _printer_ip = Some(ip);
                                        }
                                    }
                                    "DevName.bambu.com:" => {
                                        printer_name = Some(String::from(second));
                                        return printer_name;
                                    }
                                    _ => (),
                                }
                            }
                        }
                    }
                }
            }
        } else {
            return None;
        }
    }
}
