use core::cell::RefCell;

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
};
use framework::{error, info};
use mqttrust::QoS;

use crate::{
    bambu::{
        BambuPrinter, FilamentInfo,
        bambu_api::{
            AmsFilamentSettingCommand, ExtrusionCaliGetCommand, ExtrusionCaliSelCommand, ExtrusionCaliSetCommand, GetVersionCommand, PrintCommand,
            PrinterCommand, PushAllCommand,
        },
    },
    my_mqtt::BufferedMqttPacket,
    spool_record::FullSpoolRecord,
};

impl BambuPrinter {
    // TODO: Unify sending messages, no need for two functions

    pub fn publish_payload(&self, mut payload: String) {
        self.pre_message_send(&mut payload);
        let topic_name = format!("device/{}/request", &self.printer_serial);
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

    pub async fn publish_payload_async(bambu_printer: &Rc<RefCell<BambuPrinter>>, mut payload: String) {
        let printer_serial = bambu_printer.borrow().printer_serial.clone();
        let write_packets = bambu_printer.borrow().write_packets.clone();

        bambu_printer.borrow().pre_message_send(&mut payload);

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

    pub async fn request_version_info_async(bambu_printer: &Rc<RefCell<BambuPrinter>>) {
        let mut cmd = GetVersionCommand::new();
        let payload = bambu_printer.borrow_mut().printer_message(&mut cmd);
        BambuPrinter::publish_payload_async(bambu_printer, payload).await;
    }

    pub fn request_full_update_sync(&mut self) {
        let mut cmd = PushAllCommand::new();
        let payload = self.printer_message(&mut cmd);
        self.publish_payload(payload);
    }

    pub async fn request_full_update_async(bambu_printer: &Rc<RefCell<BambuPrinter>>) {
        let mut cmd = PushAllCommand::new();
        let payload = bambu_printer.borrow_mut().printer_message(&mut cmd);
        BambuPrinter::publish_payload_async(bambu_printer, payload).await;
    }

    #[allow(dead_code)]
    pub fn request_printer_command_sync(&mut self, command: PrintCommand) {
        let mut cmd = PrinterCommand::new(command);
        let payload = self.printer_message(&mut cmd);
        self.publish_payload(payload);
    }

    #[allow(dead_code)]
    pub async fn request_printer_command_async(bambu_printer: &Rc<RefCell<BambuPrinter>>, command: PrintCommand) {
        let mut cmd = PrinterCommand::new(command);
        let payload = bambu_printer.borrow_mut().printer_message(&mut cmd);
        BambuPrinter::publish_payload_async(bambu_printer, payload).await;
    }

    pub fn fetch_filament_calibrations(&mut self, nozzle_diameter: &str) {
        // Is this command also causing errors when printer is locked?

        let mut cmd = ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = self.printer_message(&mut cmd);
        if !self.is_locked() {
            self.publish_payload(payload);
        }
    }

    pub async fn fetch_filament_calibrations_async(bambu_printer: &Rc<RefCell<BambuPrinter>>, nozzle_diameter: &str) {
        let mut cmd = ExtrusionCaliGetCommand::new(nozzle_diameter);
        let payload = bambu_printer.borrow_mut().printer_message(&mut cmd);
        BambuPrinter::publish_payload_async(bambu_printer, payload).await;
    }

    pub fn reset_tray(&mut self, tray_id: i32) {
        let (ams_id, ams_tray_id, slot_id, original_tray_id) = self.get_quad_for_set_filament_from_tray_id(tray_id);
        let mut cmd = AmsFilamentSettingCommand::new(
            ams_id,
            ams_tray_id, // here we need the tray_id within the specific ams (newer versions)
            slot_id,     // slot number within ams
            "",
            Some(""),
            "",
            "",
            0,
            0,
        );
        if !self.is_locked() {
            let payload = self.printer_message(&mut cmd);
            self.publish_payload(payload);
        }

        if let Some(extruder_id) = self.get_unique_extruder_id_for_tray(tray_id) {
            let mut cmd = ExtrusionCaliSelCommand::new(
                &self.nozzle_diameter(extruder_id).clone().unwrap_or_default(),
                ams_id,
                original_tray_id, // here we need the original tray_id
                slot_id,
                "", // tray_info_idx is filament_id in this command
                Some(-1),
            );
            if !self.is_locked() {
                let payload = self.printer_message(&mut cmd);
                self.publish_payload(payload);
            }
        } else {
            // Defensive support only: real FTS internal AMS groups are ambiguous, while non-FTS groups should be uniquely bound.
            info!("[{}] Skipping pressure advance reset for tray {tray_id}: no unique extruder", self.printer_number);
        }
    }

    pub fn set_tray_filament(&mut self, tray_id: i32, full_spool_rec: &FullSpoolRecord, temp_min: u32, temp_max: u32) -> Result<(), String> {
        let (ams_id_for_set_filament, ams_tray_id, slot_id, original_tray_id) = self.get_quad_for_set_filament_from_tray_id(tray_id);

        // setting_id can't be extracted from just tray information, it's available only if there is a cali_idx on the tray.
        // on the other hand it is required to set tray information.
        // So if we have calibration information, we send the setting_id from there. If we don't we send None and it seems to work
        // The slicer have the setting-if from the data it has when it selects everything together

        let unique_extruder_id = self.get_unique_extruder_id_for_tray(tray_id);
        let matching_calibration = unique_extruder_id
            .and_then(|extruder_id| self.get_matching_printer_calibration_for_extruder(full_spool_rec, extruder_id));

        let setting_id: Option<&str> = matching_calibration.as_ref().and_then(|c| c.setting_id.as_deref());

        let mut filament = FilamentInfo {
            tray_info_idx: full_spool_rec.spool_rec.slicer_filament.clone(),
            tray_type: full_spool_rec.spool_rec.material_type.clone(),
            tray_color: full_spool_rec.spool_rec.color_code.clone(),
            nozzle_temp_min: temp_min,
            nozzle_temp_max: temp_max,
        };
        let filament_ok_to_send = self.fill_filament_defaults_if_needed(&mut filament);

        // Send printer material & color

        if filament_ok_to_send {
            let mut cmd = AmsFilamentSettingCommand::new(
                ams_id_for_set_filament,
                ams_tray_id, // here we need the tray_id within the specific ams (newer versions)
                slot_id,     // slot number within ams
                &filament.tray_info_idx,
                setting_id,
                &filament.tray_type,
                &filament.primary_color(),
                filament.nozzle_temp_min,
                filament.nozzle_temp_max,
            );
            if !self.is_locked() {
                let payload = self.printer_message(&mut cmd);
                self.publish_payload(payload);
            }

            // Send printer pressure advance

            if let Some(extruder_id) = unique_extruder_id {
                let mut cmd = ExtrusionCaliSelCommand::new(
                    &self.nozzle_diameter(extruder_id).clone().unwrap_or_default(),
                    ams_id_for_set_filament,
                    original_tray_id, // here we need the original tray_id
                    slot_id,
                    &filament.tray_info_idx, // tray_info_idx is filament_id in this command
                    if let Some(calibration) = &matching_calibration {
                        Some(calibration.cali_idx)
                    } else {
                        Some(-1)
                    },
                );
                if !self.is_locked() {
                    let payload = self.printer_message(&mut cmd);
                    self.publish_payload(payload);
                }
            } else {
                // Defensive support only: real FTS internal AMS groups are ambiguous, while non-FTS groups should be uniquely bound.
                info!("[{}] Skipping automatic pressure advance selection for tray {tray_id}: no unique extruder", self.printer_number);
            }

            // Record the app-level slot assignment in the generic snapshot state.

            self.set_tray_spool_rec(tray_id as usize, &full_spool_rec.spool_rec);

            Ok(())
        } else {
            error!("Error trying to set slot information due to missing information (material type at least is required)");
            Err("Missing information".to_string())
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_calibration_to_printer(
        &mut self,
        extruder_id: i32,
        nozzle_diameter: &str,
        nozzle_id: &str,
        filament_id: &str,
        setting_id: &str,
        k_value: &str,
        name: &str,
    ) {
        let mut cmd = ExtrusionCaliSetCommand::new(extruder_id, nozzle_diameter, nozzle_id, filament_id, setting_id, k_value, name);
        let payload = self.printer_message(&mut cmd);
        self.publish_payload(payload);
    }

    pub fn get_quad_for_set_filament_from_tray_id(&self, tray_id: i32) -> (i32, i32, i32, i32) {
        // (ams_id, ams_tray_id, slot_id, original_tray_id)

        let ams_id;
        let mut ams_tray_id_for_set_filament;
        let slot_id;
        let original_tray_id;

        (ams_id, ams_tray_id_for_set_filament) = Self::get_ams_and_slot_id(tray_id as usize);

        if tray_id < 16 {
            // AMS
            slot_id = ams_tray_id_for_set_filament as i32;
            original_tray_id = tray_id;
        } else if tray_id < 16 + 8 {
            // AMS-HT
            slot_id = 0;
            original_tray_id = (ams_id * 4 + ams_tray_id_for_set_filament) as i32; // seems like this is what Bambustudio is placing there (so 512 for first HT, others are assumed)
        } else {
            // external
            ams_tray_id_for_set_filament = 254;
            slot_id = 0;
            if self.num_extruders() == 1 {
                original_tray_id = 254;
            } else {
                original_tray_id = tray_id;
            }
        }
        (ams_id as i32, ams_tray_id_for_set_filament as i32, slot_id, original_tray_id)
    }
}
