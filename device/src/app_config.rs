use core::{cell::RefCell, str::FromStr};

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString}, vec::Vec,
};
use embassy_net::Ipv4Address;
use serde::{Deserialize, Deserializer, Serializer};

use framework::prelude::*;

const PRINTER_CONFIG_KEY: &str = "_printer_";
const TAG_CONFIG_KEY: &str = "_tag_";

fn serialize_option_ipv4<S>(ip: &Option<Ipv4Address>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match ip {
        Some(ip_addr) => {
            let ip_str = ip_addr.to_string(); // Convert Ipv4Addr to a string (e.g., "192.168.0.1")
            serializer.serialize_some(&ip_str)
        }
        None => serializer.serialize_none(),
    }
}

fn deserialize_option_ipv4<'de, D>(deserializer: D) -> Result<Option<Ipv4Address>, D::Error>
where
    D: Deserializer<'de>,
{
    // Deserialize as Option<&str> to avoid needing String::deserialize
    let ip_str: Option<&str> = Deserialize::deserialize(deserializer)?;

    match ip_str {
        Some(ip) => ip
            .parse::<Ipv4Address>()
            .map(Some)
            .map_err(|_| serde::de::Error::invalid_value(serde::de::Unexpected::Str(ip), &"a valid IPv4 address string")),
        None => Ok(None),
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfig {
    #[serde(serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
    // pub name: Option<String>,
    pub serial: Option<String>,
    pub access_code: Option<String>,
}
#[derive(serde::Deserialize, serde::Serialize)]
struct TagConfig {
    pub scan_timeout: u64,
}

pub struct AppConfig {
    observers: Vec<alloc::rc::Weak<RefCell<dyn AppControlObserver>>>,
    framework: Rc<RefCell<Framework>>,
    pub printer_ip: Option<Ipv4Address>,
    // pub printer_name: Option<String>,
    pub printer_serial: Option<String>,
    pub printer_access_code: Option<String>,
    pub tag_scan_timeout: u64,

    config_processed_ok: Option<bool>,
    pn532_ok: Option<bool>,
    printer_connectivity_ok: Option<bool>,
}

impl AppConfig {
    #[allow(dead_code)]
    pub fn missing_configs(&self) -> bool {
        let mut missing = false;
        if self.printer_serial.is_none() {
            term_error!("Missing configuration for Printer Serial");
            missing = true;
        }
        if self.printer_access_code.is_none() {
            term_error!("Missing configuration for Printer Access Code");
            missing = true;
        }
        if missing {
            term_error!("Use Web Config to set missing configuration(s)");
        }

        missing
    }

    pub fn new(
        framework: Rc<RefCell<Framework>>,
    ) -> Self {
        Self {
            observers: Vec::new(),
            framework,
            printer_ip: None,
            // printer_name: None,
            printer_serial: None,
            printer_access_code: None,
            tag_scan_timeout: 10,

            config_processed_ok: None,
            pn532_ok: None,
            printer_connectivity_ok: None,
        }
    }
    // A function to parse the TOML-like string and populate the structure
    pub fn load_config_flash_then_toml(&mut self, toml_str: &str) -> Result<(), String> {
        if let Ok(Some(printer_store)) = self.framework.borrow_mut().fetch(String::from(PRINTER_CONFIG_KEY)) {
            if let Ok(printer_config) = serde_json::from_str::<PrinterConfig>(&printer_store) {
                self.printer_ip = printer_config.ip;
                // self.printer_name = printer_config.name;
                self.printer_serial = printer_config.serial;
                self.printer_access_code = printer_config.access_code;
            }
        }

        if let Ok(Some(tag_store)) = self.framework.borrow_mut().fetch(String::from(TAG_CONFIG_KEY)) {
            if let Ok(tag_config) = serde_json::from_str::<TagConfig>(&tag_store) {
                self.tag_scan_timeout = tag_config.scan_timeout;
            }
        }

        let mut section = String::from("");

        let mut parse_errors = false;

        for (line_num, line) in toml_str.lines().enumerate() {
            // Trim whitespace and ignore empty lines or comments
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("[") && line.ends_with("]") {
                section = String::from(&line[1..line.len() - 1]);
                continue;
            }

            // Check if the line contains a key-value pair
            if let Some((key, value)) = line.split_once('=') {
                // Trim key and value to remove any surrounding whitespace
                let key = key.trim();
                let value = value.trim().trim_matches('"'); // Remove surrounding quotes if present

                // Match the key and assign the value to the corresponding field
                let expanded_key = format!("{}_{}", &section, &key);
                match expanded_key.as_str() {
                    "printer_ip" => {
                        if let Ok(addr) = Ipv4Address::from_str(value) {
                            self.printer_ip = Some(addr);
                        } else {
                            parse_errors = true;
                            term_error!("config file format error at printer ip");
                        }
                    }
                    "printer_serial" => self.printer_serial = Some(String::from(value)),
                    "printer_access_code" => self.printer_access_code = Some(String::from(value)),
                    "tag_timeout" => {
                        if let Ok(tag_timeout) = value.parse::<u64>() {
                            self.tag_scan_timeout = tag_timeout;
                        } else {
                            parse_errors = true;
                            term_error!("config file format error at tag timeout");
                        }
                    }
                    _ => {
                        // allow unknown configs, ignore them
                    }
                }
            } else {
                term_error!("Warning: configuration line {} syntax error: {} in section {}", line_num, line, section);
                // treat as warning, don't fail load because of that
            }

            // TODO: add error handling with notification on missing mandatory selfs
            if parse_errors {
                self.config_processed_ok = Some(false);
                return Err(String::from("Parse Error"));
            }
        }
        self.config_processed_ok = Some(true);
        Ok(())
    }

    pub fn report_pn532(&mut self, status: bool) {
        self.pn532_ok = Some(status);
    }
    pub fn report_printer_connectivity(&mut self, status: bool) {
        self.printer_connectivity_ok = Some(status);
        self.notify_printer_connect_status(status);
    }

    pub fn initialization_ok(&self) -> bool {
        self.framework.borrow().initialization_ok() && 
        matches!(self.config_processed_ok, Some(true)) && matches!(self.pn532_ok, Some(true)) && self.printer_serial != None && self.printer_access_code != None
    }

    #[allow(dead_code)]
    pub fn boot_completed(&self) -> bool {
        self.framework.borrow().boot_completed() &&
        self.initialization_ok() && matches!(self.printer_connectivity_ok, Some(true))
    }

    pub fn set_printer_config(
        &mut self,
        printer_ip: String,
        printer_serial: String,
        printer_access_code: String,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        self.printer_ip = Ipv4Address::from_str(&printer_ip).ok();
        // self.printer_name = if printer_name.is_empty() { None } else { Some(printer_name) };
        self.printer_serial = if printer_serial.is_empty() { None } else { Some(printer_serial) };
        self.printer_access_code = if printer_access_code.is_empty() {
            None
        } else {
            Some(printer_access_code)
        };
        let printer_config = PrinterConfig {
            ip: self.printer_ip,
            // name: self.printer_name.clone(),
            serial: self.printer_serial.clone(),
            access_code: self.printer_access_code.clone(),
        };
        let printer_store = serde_json::to_string(&printer_config).unwrap();
        self.framework.borrow().store(String::from(PRINTER_CONFIG_KEY), printer_store)
    }

    pub fn set_tag_config(&mut self, tag_scan_timeout: u64) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        self.tag_scan_timeout = tag_scan_timeout;
        let tag_config = TagConfig {
            scan_timeout: self.tag_scan_timeout,
        };
        let tag_store = serde_json::to_string(&tag_config).unwrap();
        self.framework.borrow().store(String::from(TAG_CONFIG_KEY), tag_store)
    }

    // Events 

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn AppControlObserver>>) {
        self.observers.push(observer);
    }

    pub fn notify_printer_connect_status(&self, status: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_printer_connect_status(status);
        }
    }
}


pub trait AppControlObserver {
    fn on_printer_connect_status(&self, status: bool); 
}
