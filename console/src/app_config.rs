use core::{cell::RefCell, str::FromStr};

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use derivative::Derivative;
use embassy_net::Ipv4Address;
use serde::{Deserialize, Deserializer, Serializer};

use framework::prelude::*;

pub const SPOOLS_CATALOG: &str = include_str!("../data/Spool-Core-Weights.csv");
pub const BASE_FILAMENTS: &str = include_str!("../data/base-filaments-index.csv");
pub const FILAMENT_BRAND_NAMES: &str = include_str!("../data/filament-brands.csv");
const PRINTER_CONFIG_KEY: &str = "_printer_"; // for backwards compatibility
const PRINTERS_CONFIG_KEY: &str = "_printers_";
const DEFAULT_PRINTER_CONFIG_KEY: &str = "_default_printer_";
const SCALE_CONFIG_KEY: &str = "_scale_"; // for backwards compatibility

const PREVIOUSLY_USED_CORES_CONFIG_KEY: &str = "prev_cores";
const USER_CORES_CONFIG_KEY: &str = "user_cores";
const CUSTOM_FILAMENTS_CONFIG_KEY: &str = "custom_filaments";

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

fn default_true() -> bool {
    true
}

// These struct is first and foremost for persistent configuration
// Changing it should be well dealt with including upgrade
#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone, Derivative)]
#[derivative(Default)]
pub struct PrinterConfig {
    #[serde(serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
    pub name: Option<String>,
    pub serial: Option<String>,
    pub access_code: Option<String>,
    pub log_filter: Option<log::LevelFilter>,
    #[derivative(Default(value = "true"))]
    #[serde(default = "default_true")]
    pub auto_restore_k: bool,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct PrintersConfig {
    pub printers: Vec<PrinterConfig>,
}
#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct DefaultPrinterConfig {
    pub serial: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Default, PartialEq, Debug, Clone)]
pub struct ScaleConfig {
    pub available: bool,
    pub name: Option<String>,
    #[serde(serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
}

pub struct AppConfig {
    pub framework: Rc<RefCell<Framework>>,
    // configured are what configured
    pub configured_printers: PrintersConfig,
    pub configured_default_printer: DefaultPrinterConfig,
    pub configured_scale: Option<ScaleConfig>,

    config_processed_ok: Option<bool>,
    pn532_ok: Option<bool>,
    pub user_cores: Option<String>,
    pub user_cores_changed_by_web_config: bool,
    pub previously_used_cores: Option<String>,
    pub custom_filaments: Option<String>,
    pub root_redirect: String,
}

impl AppConfig {
    #[allow(dead_code)]
    pub fn missing_configs(&self, log: bool) -> bool {
        let mut missing = true;
        let mut partial_missing = false;
        for printer in &self.configured_printers.printers {
            if printer.serial.is_some() && printer.access_code.is_some() {
                missing = false;
            }
            if printer.serial.is_none() || printer.access_code.is_none() {
                partial_missing = true;
            }
        }
        if log {
            if missing {
                term_error!("Missing printer(s) information");
            } else if partial_missing {
                term_error!("At least one printer is missing serial/access_code configuration");
            }
        }

        missing
    }

    pub fn new(framework: Rc<RefCell<Framework>>) -> Self {
        Self {
            framework,
            configured_printers: PrintersConfig { printers: Vec::new() },
            configured_default_printer: DefaultPrinterConfig { serial: None },
            configured_scale: None,

            config_processed_ok: None,
            pn532_ok: None,
            user_cores: None,
            user_cores_changed_by_web_config: false,
            previously_used_cores: None,
            custom_filaments: None,
            root_redirect: "/config".to_string(),
        }
    }

    // A function to parse the TOML-like string and populate the structure
    pub fn load_config_flash_then_toml(&mut self, toml_str: &str) -> Result<(), String> {
        // Load printers configurtion
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
            }
        } else {
            // backwards compatibility with a single printer
            let config = self.framework.borrow_mut().fetch(String::from(PRINTER_CONFIG_KEY));
            if let Ok(Some(printer_store)) = config {
                if let Ok(printer_config) = serde_json::from_str::<PrinterConfig>(&printer_store) {
                    self.configured_default_printer.serial = printer_config.serial.clone();
                    self.configured_printers.printers.push(printer_config);
                }
            }
        }
        let config = self.framework.borrow_mut().fetch(String::from(DEFAULT_PRINTER_CONFIG_KEY));
        if let Ok(Some(default_printer_store)) = config {
            if let Ok(printers_config) = serde_json::from_str::<DefaultPrinterConfig>(&default_printer_store) {
                self.configured_default_printer = printers_config;
            }
        }
        // Load core weights configuration

        let config = self.framework.borrow_mut().fetch(String::from(PREVIOUSLY_USED_CORES_CONFIG_KEY));
        if let Ok(previously_used_cores) = config {
            self.previously_used_cores = previously_used_cores;
        }

        let config = self.framework.borrow_mut().fetch(String::from(USER_CORES_CONFIG_KEY));
        if let Ok(user_cores) = config {
            self.user_cores = user_cores;
        }

        let config = self.framework.borrow_mut().fetch(String::from(CUSTOM_FILAMENTS_CONFIG_KEY));
        if let Ok(custom_filaments) = config {
            self.custom_filaments = custom_filaments;
        }

        let config = self.framework.borrow_mut().fetch(String::from(SCALE_CONFIG_KEY));
        if let Ok(Some(scale_store)) = config {
            if let Ok(scale_config) = serde_json::from_str::<ScaleConfig>(&scale_store) {
                self.configured_scale = Some(scale_config);
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
                        toml_priner_config.serial = Some(String::from(value));
                    }
                    "printer_access_code" => toml_priner_config.access_code = Some(String::from(value)),
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
        }

        // If after all, no printer configured, fill in an empty printer config
        if self.configured_printers.printers.is_empty() {
            self.configured_printers.printers.push(PrinterConfig::default());
        }

        self.config_processed_ok = Some(true);
        Ok(())
    }

    pub fn report_pn532(&mut self, status: bool) {
        self.pn532_ok = Some(status);
    }

    pub fn initialization_ok(&self, log: bool) -> Option<bool> {
        self.pn532_ok?;
        Some(
            self.framework.borrow().initialization_ok()
                && matches!(self.config_processed_ok, Some(true))
                && matches!(self.pn532_ok, Some(true))
                && !self.missing_configs(log),
        )
    }

    #[allow(dead_code)]
    pub fn boot_completed(&self) -> bool {
        self.framework.borrow().boot_completed() && matches!(self.initialization_ok(false), Some(true))
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

    pub fn set_scale_config(&mut self, scale_config: ScaleConfig) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if !scale_config.available && scale_config.name.is_none() && scale_config.ip.is_none() {
            self.framework.borrow().remove(SCALE_CONFIG_KEY.to_string())?;
            self.configured_scale = None;
        } else {
            let scale_store = serde_json::to_string(&scale_config).unwrap();
            self.framework.borrow().store(String::from(SCALE_CONFIG_KEY), scale_store)?;
            self.configured_scale = Some(scale_config);
        }
        Ok(())
    }

    pub fn set_previously_used_cores(
        &mut self,
        previously_used_cores: Option<String>,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if previously_used_cores.is_some() {
            self.framework.borrow().store(
                PREVIOUSLY_USED_CORES_CONFIG_KEY.to_string(),
                previously_used_cores.as_ref().unwrap().clone(),
            )?;
        } else {
            self.framework.borrow().remove(PREVIOUSLY_USED_CORES_CONFIG_KEY.to_string())?;
        }
        self.previously_used_cores = previously_used_cores;
        Ok(())
    }

    pub fn set_user_cores(&mut self, user_cores: Option<String>) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if user_cores.is_some() {
            self.framework
                .borrow()
                .store(USER_CORES_CONFIG_KEY.to_string(), user_cores.as_ref().unwrap().clone())?;
        } else {
            self.framework.borrow().remove(USER_CORES_CONFIG_KEY.to_string())?;
        }
        self.user_cores = user_cores;
        self.user_cores_changed_by_web_config = true;
        Ok(())
    }

    pub fn set_filaments(&mut self, custom_filaments: Option<String>) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if let Some(custom_filaments) = &custom_filaments {
            let mut skip_store = false;
            if let Some(curr_custom_filaments) = &self.custom_filaments {
                if curr_custom_filaments == custom_filaments {
                    skip_store = true; // no change, better skip writing to flash
                }
            }
            if !skip_store {
                self.framework
                    .borrow()
                    .store(CUSTOM_FILAMENTS_CONFIG_KEY.to_string(), custom_filaments.clone())?;
            }
        } else {
            self.framework.borrow().remove(CUSTOM_FILAMENTS_CONFIG_KEY.to_string())?;
        }
        self.custom_filaments = custom_filaments;
        Ok(())
    }

    pub fn set_redirect_web_to_config(&mut self) {
        self.root_redirect = "/config".to_string();
    }

    pub fn set_redirect_to_encode(&mut self) {
        self.root_redirect = "/encode".to_string();
    }
}
