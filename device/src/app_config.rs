use core::{cell::RefCell, str::FromStr};

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use embassy_net::Ipv4Address;
use serde::{Deserialize, Deserializer, Serializer};

use framework::prelude::*;

const PRINTER_CONFIG_KEY: &str = "_printer_"; // for backwards compatibility
const PRINTERS_CONFIG_KEY: &str = "_printers_";
const TAG_CONFIG_KEY: &str = "_tag_";
const DEFAULT_PRINTER_CONFIG_KEY: &str = "_default_printer_";

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

// These struct is first and foremost for persistent configuration
// Changing it should be well dealt with including upgrade
#[derive(serde::Deserialize, serde::Serialize, Default, PartialEq, Debug, Clone)]
pub struct PrinterConfig {
    #[serde(serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
    pub name: Option<String>,
    pub serial: Option<String>,
    pub access_code: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct PrintersConfig {
    pub printers: Vec<PrinterConfig>,
}
#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct DefaultPrinterConfig {
    pub serial: Option<String>,
}

// TODO: remove this one
#[derive(serde::Deserialize, serde::Serialize)]
struct TagConfig {
    pub scan_timeout: u64,
}
//////////////////////////////////////////////////////////////////

pub struct AppConfig {
    observers: Vec<alloc::rc::Weak<RefCell<dyn AppControlObserver>>>,
    framework: Rc<RefCell<Framework>>,
    // configured are what configured
    pub configured_printers: PrintersConfig,
    pub configured_default_printer: DefaultPrinterConfig,
    // w/o configured is also if learnt
    printer_ip: Option<Ipv4Address>,
    printer_name: Option<String>,
    printer_serial: Option<String>,
    printer_access_code: Option<String>,
    pub tag_scan_timeout: u64,

    config_processed_ok: Option<bool>,
    pn532_ok: Option<bool>,
    printer_connectivity_ok: Option<bool>,
}

impl AppConfig {
    pub fn get_printer_ip(&self) -> Option<Ipv4Address> {
        self.printer_ip
    }
    pub fn set_printer_ip(&mut self, printer_ip: Option<Ipv4Address>) {
        self.printer_ip = printer_ip;
    }
    pub fn get_printer_name(&self) -> &Option<String> {
        &self.printer_name
    }
    pub fn set_printer_name(&mut self, printer_name: Option<String>) {
        self.printer_name = printer_name;
    }
    pub fn get_printer_serial(&self) -> &Option<String> {
        &self.printer_serial
    }
    pub fn get_printer_access_code(&self) -> &Option<String> {
        &self.printer_access_code
    }

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

    pub fn new(framework: Rc<RefCell<Framework>>) -> Self {
        Self {
            observers: Vec::new(),
            framework,
            configured_printers: PrintersConfig { printers: Vec::new() },
            configured_default_printer: DefaultPrinterConfig { serial: None },
            printer_ip: None,
            printer_name: None,
            printer_serial: None,
            printer_access_code: None,
            tag_scan_timeout: 10,

            config_processed_ok: None,
            pn532_ok: None,
            printer_connectivity_ok: None,
        }
    }

    pub fn get_curr_printer_selector_text(&self) -> String {
        self.printer_name
            .as_ref()
            .unwrap_or(self.printer_serial.as_ref().unwrap_or(&"".to_string()))
            .clone()
    }

    pub fn get_default_printer_selector_text(&self) -> String {
        if let Some(default_printer_serial) = &self.configured_default_printer.serial {
            if let Some(printer) = self
                .configured_printers
                .printers
                .iter()
                .find(|printer| printer.serial.as_deref() == Some(&default_printer_serial))
            {
                printer
                    .name
                    .as_ref()
                    .unwrap_or(printer.access_code.as_ref().unwrap_or(&"".to_string()))
                    .clone()
            } else {
                String::default()
            }
        } else {
            String::default()
        }
    }

    pub fn get_printers_selector_texts(&self) -> Vec<String> {
        self.configured_printers
            .printers
            .iter()
            .map(|printer_config| {
                printer_config
                    .name
                    .as_ref()
                    .unwrap_or(printer_config.serial.as_ref().unwrap_or(&"".to_string()))
                    .clone()
            })
            .collect()
    }

    pub fn set_current_printer_by_name_then_serial(&mut self, name_or_serial: &str) -> Result<(), String> {
        if self.set_current_printer_by_name(name_or_serial).is_err() {
            self.set_current_printer_by_serial(name_or_serial)?
        }
        Ok(())
    }

    fn set_current_printer(&mut self, printer: PrinterConfig) {
        self.printer_ip = printer.ip;
        self.printer_name = printer.name;
        self.printer_serial = printer.serial;
        self.printer_access_code = printer.access_code;
    }

    pub fn set_current_printer_by_name(&mut self, name: &str) -> Result<(), String> {
        let printer = self
            .configured_printers
            .printers
            .iter()
            .find(|printer| printer.name.as_deref() == Some(name))
            .cloned();
        if let Some(printer) = printer {
            self.set_current_printer(printer);
            return Ok(());
        } else {
            debug!(">>>> Not Found the default printer {name}");
            Err("Name not found".into())
        }
    }
    pub fn set_current_printer_by_serial(&mut self, serial: &str) -> Result<(), String> {
        let printer = self
            .configured_printers
            .printers
            .iter()
            .find(|printer| printer.serial.as_deref() == Some(serial))
            .cloned();
        if let Some(printer) = printer {
            self.set_current_printer(printer);
            return Ok(());
        } else {
            debug!(">>>> Not Found the default printer {serial}");
            Err("Serial not found".into())
        }
    }
    pub fn set_current_printer_to_default(&mut self) -> Result<(), String> {
        let mut serial = self.configured_default_printer.serial.clone();

        if serial.is_none() {
            if self.configured_printers.printers.len() > 0 {
                serial = self.configured_printers.printers[0].serial.clone()
            }
        }
        if let Some(serial) = serial {
            return self.set_current_printer_by_serial(&serial);
        } else {
            return Err("No printers to select".into());
        }
    }

    // A function to parse the TOML-like string and populate the structure
    pub fn load_config_flash_then_toml(&mut self, toml_str: &str) -> Result<(), String> {
        let config = self.framework.borrow_mut().fetch(String::from(PRINTERS_CONFIG_KEY));
        if let Ok(Some(printers_store)) = config {
            if let Ok(printers_config) = serde_json::from_str::<PrintersConfig>(&printers_store) {
                self.configured_printers = printers_config;
                let config = self.framework.borrow_mut().fetch(String::from(DEFAULT_PRINTER_CONFIG_KEY));
                if let Ok(Some(default_printer_store)) = config {
                    if let Ok(default_printer_config) = serde_json::from_str::<DefaultPrinterConfig>(&default_printer_store) {
                        self.configured_default_printer = default_printer_config;
                    }
                }
                if let Err(err) = self.set_current_printer_to_default() {
                    term_info!("Bad printers configuration, can't select printer: {}", err);
                }
            }
        } else {
            // backwards compatibility with a single printer
            let config = self.framework.borrow_mut().fetch(String::from(PRINTER_CONFIG_KEY));
            if let Ok(Some(printer_store)) = config {
                if let Ok(printer_config) = serde_json::from_str::<PrinterConfig>(&printer_store) {
                    self.configured_default_printer.serial = printer_config.serial.clone();
                    self.configured_printers.printers.push(printer_config);
                    if let Err(err) = self.set_current_printer_to_default() {
                        term_info!("Bad printers configuration, can't select printer: {}", err);
                    }
                }
            }
        } 
        let config = self.framework.borrow_mut().fetch(String::from(DEFAULT_PRINTER_CONFIG_KEY));
        if let Ok(Some(default_printer_store)) = config {
            if let Ok(printers_config) = serde_json::from_str::<DefaultPrinterConfig>(&default_printer_store) {
                self.configured_default_printer = printers_config;
            }
        }

        if let Ok(Some(tag_store)) = self.framework.borrow_mut().fetch(String::from(TAG_CONFIG_KEY)) {
            if let Ok(tag_config) = serde_json::from_str::<TagConfig>(&tag_store) {
                self.tag_scan_timeout = tag_config.scan_timeout;
            }
        }

        let mut section = String::from("");

        let mut parse_errors = false;
        let mut toml_priner_config = PrinterConfig::default();

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
                            toml_priner_config.ip = Some(addr);
                        } else {
                            parse_errors = true;
                            term_error!("config file format error at printer ip");
                        }
                    }
                    "printer_name" => {
                        toml_priner_config.name = Some(String::from(value));
                    }
                    "printer_serial" => {
                        self.printer_serial = Some(String::from(value));
                    }
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
        if toml_priner_config != PrinterConfig::default() {
            self.configured_printers.printers.push(toml_priner_config);
            if let Err(err) = self.set_current_printer_to_default() {
                term_info!("Bad printer configuration {}", err);
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
        self.framework.borrow().initialization_ok()
            && matches!(self.config_processed_ok, Some(true))
            && matches!(self.pn532_ok, Some(true))
            && self.printer_serial != None
            && self.printer_access_code != None
    }

    #[allow(dead_code)]
    pub fn boot_completed(&self) -> bool {
        self.framework.borrow().boot_completed() && self.initialization_ok() && matches!(self.printer_connectivity_ok, Some(true))
    }

    pub fn set_printers_config(
        &mut self,
        printers_config: PrintersConfig,
        default_printer_config: DefaultPrinterConfig,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        let printers_store = serde_json::to_string(&printers_config).unwrap();
        self.framework.borrow().store(String::from(PRINTERS_CONFIG_KEY), printers_store)?;
        let default_printer_store = serde_json::to_string(&default_printer_config).unwrap();
        self.framework
            .borrow()
            .store(String::from(DEFAULT_PRINTER_CONFIG_KEY), default_printer_store)?;
        self.configured_printers = printers_config;
        self.configured_default_printer = default_printer_config;
        Ok(())
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
