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
use shared::gcode_analysis_task::Fetch3mf;

use crate::printer::{PrinterDriverKind, PrinterId};

pub const SPOOLS_CATALOG: &str = include_str!("../data/Spool-Core-Weights.csv");
pub const BASE_FILAMENTS: &str = include_str!("../data/base-filaments-index.csv");
pub const BAMBU_COLOR_NAMES: &str = include_str!("../data/bambu-color-names.csv");
pub const FILAMENT_BRAND_NAMES: &str = include_str!("../data/filament-brands.csv");
pub const MATERIALS: &str = include_str!("../data/materials.csv");
const PRINTER_CONFIG_KEY: &str = "_printer_"; // for backwards compatibility
const PRINTERS_CONFIG_KEY: &str = "_printers_";
const DEFAULT_PRINTER_CONFIG_KEY: &str = "_default_printer_";
const SCALE_CONFIG_KEY: &str = "_scale_"; // for backwards compatibility

// const PREVIOUSLY_USED_CORES_CONFIG_KEY: &str = "prev_cores";
const USER_CORES_CONFIG_KEY: &str = "user_cores";
const CUSTOM_FILAMENTS_CONFIG_KEY: &str = "custom_filaments";
const AI_PROVIDERS_CONFIG_KEY: &str = "ai_providers";

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub enum AiProviderId {
    OpenAi,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AiProviderCredential {
    pub provider: AiProviderId,
    pub api_key: String,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AiProvidersConfig {
    #[serde(default)]
    pub providers: Vec<AiProviderCredential>,
}

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

fn default_false() -> bool {
    false
}

pub fn default_printer_driver_kind() -> PrinterDriverKind {
    PrinterDriverKind::Bambu
}

use serde::Serialize;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum PrinterMode {
    #[default]
    Auto,
    DevOrOldFirmware,
    Cloud,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum UseAmsScan {
    SpoolIdAndConfigure,
    SpoolId,
    #[default]
    Disabled,
}

// These struct is first and foremost for persistent configuration
// Changing it should be well dealt with including upgrade
#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone, Default)]
pub struct PrinterConfig {
    pub name: Option<String>,
    #[serde(flatten)]
    pub driver: PrinterDriverConfig,
}

impl PrinterConfig {
    pub fn bambu(name: Option<String>, config: BambuPrinterConfig) -> Self {
        Self {
            name,
            driver: PrinterDriverConfig::Bambu(config),
        }
    }

    pub fn fake(name: Option<String>, config: FakePrinterConfig) -> Self {
        Self {
            name,
            driver: PrinterDriverConfig::Fake(config),
        }
    }

    #[allow(dead_code)]
    pub fn driver_kind(&self) -> PrinterDriverKind {
        match &self.driver {
            PrinterDriverConfig::Bambu(_) => PrinterDriverKind::Bambu,
            PrinterDriverConfig::Fake(_) => PrinterDriverKind::Fake,
        }
    }

    pub fn printer_id(&self) -> Result<PrinterId, String> {
        match &self.driver {
            PrinterDriverConfig::Bambu(config) => config.printer_id(),
            PrinterDriverConfig::Fake(config) => config.printer_id(),
        }
    }

    pub fn bambu_config(&self) -> Option<&BambuPrinterConfig> {
        match &self.driver {
            PrinterDriverConfig::Bambu(config) => Some(config),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn fake_config(&self) -> Option<&FakePrinterConfig> {
        match &self.driver {
            PrinterDriverConfig::Fake(config) => Some(config),
            _ => None,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone)]
#[serde(tag = "driver_kind", content = "driver_config")]
pub enum PrinterDriverConfig {
    Bambu(BambuPrinterConfig),
    Fake(FakePrinterConfig),
}

impl Default for PrinterDriverConfig {
    fn default() -> Self {
        Self::Bambu(BambuPrinterConfig::default())
    }
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone, Derivative)]
#[derivative(Default)]
pub struct BambuPrinterConfig {
    #[serde(default, serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
    pub serial: Option<String>,
    pub access_code: Option<String>,
    pub log_filter: Option<log::LevelFilter>,
    #[derivative(Default(value = "false"))]
    #[serde(default = "default_false")]
    pub auto_restore_k: bool,
    #[derivative(Default(value = "true"))]
    #[serde(default = "default_true")]
    pub track_print_consume: bool,
    #[serde(default)]
    pub fetch_3mf: Fetch3mf,
    #[derivative(Default(value = "false"))]
    #[serde(default = "default_false")]
    pub ignore_certificates: bool,
    #[derivative(Default(value = "PrinterMode::Auto"))]
    #[serde(default)]
    pub printer_mode: PrinterMode,
    #[derivative(Default(value = "UseAmsScan::Disabled"))]
    #[serde(default)]
    pub use_ams_scan: UseAmsScan,
}

impl BambuPrinterConfig {
    pub fn printer_id_for_serial(serial: &str) -> PrinterId {
        PrinterId::new(format!("bambu_printer_{serial}"))
    }

    pub fn printer_id(&self) -> Result<PrinterId, String> {
        self.serial
            .as_deref()
            .map(Self::printer_id_for_serial)
            .ok_or_else(|| "Missing Bambu printer serial".to_string())
    }
}

fn default_fake_slot_count() -> u8 {
    4
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone, Derivative)]
#[derivative(Default)]
pub struct FakePrinterConfig {
    pub unique_id: String,
    #[derivative(Default(value = "4"))]
    #[serde(default = "default_fake_slot_count")]
    pub slot_count: u8,
}

impl FakePrinterConfig {
    pub fn printer_id_for_unique_id(unique_id: &str) -> PrinterId {
        PrinterId::new(format!("fake_printer_{unique_id}"))
    }

    pub fn printer_id(&self) -> Result<PrinterId, String> {
        if self.unique_id.trim().is_empty() {
            Err("Missing fake printer unique ID".to_string())
        } else {
            Ok(Self::printer_id_for_unique_id(self.unique_id.trim()))
        }
    }
}

#[derive(serde::Deserialize)]
struct LegacyPrinterConfig {
    #[serde(default = "default_printer_driver_kind")]
    driver_kind: PrinterDriverKind,
    #[serde(default, deserialize_with = "deserialize_option_ipv4")]
    ip: Option<Ipv4Address>,
    name: Option<String>,
    serial: Option<String>,
    access_code: Option<String>,
    log_filter: Option<log::LevelFilter>,
    #[serde(default = "default_false")]
    auto_restore_k: bool,
    #[serde(default = "default_true")]
    track_print_consume: bool,
    #[serde(default)]
    fetch_3mf: Fetch3mf,
    #[serde(default = "default_false")]
    ignore_certificates: bool,
    #[serde(default)]
    printer_mode: PrinterMode,
    #[serde(default)]
    use_ams_scan: UseAmsScan,
}

impl From<LegacyPrinterConfig> for PrinterConfig {
    fn from(v: LegacyPrinterConfig) -> Self {
        match v.driver_kind {
            PrinterDriverKind::Bambu | PrinterDriverKind::Unknown => Self::bambu(
                v.name,
                BambuPrinterConfig {
                    ip: v.ip,
                    serial: v.serial,
                    access_code: v.access_code,
                    log_filter: v.log_filter,
                    auto_restore_k: v.auto_restore_k,
                    track_print_consume: v.track_print_consume,
                    fetch_3mf: v.fetch_3mf,
                    ignore_certificates: v.ignore_certificates,
                    printer_mode: v.printer_mode,
                    use_ams_scan: v.use_ams_scan,
                },
            ),
            PrinterDriverKind::Fake => Self::fake(v.name, FakePrinterConfig::default()),
            _ => Self::default(),
        }
    }
}

#[derive(serde::Deserialize)]
struct LegacyPrintersConfig {
    printers: Vec<LegacyPrinterConfig>,
}

impl From<LegacyPrintersConfig> for PrintersConfig {
    fn from(v: LegacyPrintersConfig) -> Self {
        Self {
            printers: v.printers.into_iter().map(PrinterConfig::from).collect(),
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct PrintersConfig {
    pub printers: Vec<PrinterConfig>,
}
#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct DefaultPrinterConfig {
    pub printer_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct StoredDefaultPrinterConfig {
    printer_id: Option<String>,
    serial: Option<String>,
}

impl From<StoredDefaultPrinterConfig> for DefaultPrinterConfig {
    fn from(v: StoredDefaultPrinterConfig) -> Self {
        Self {
            printer_id: v
                .printer_id
                .or_else(|| v.serial.map(|serial| BambuPrinterConfig::printer_id_for_serial(&serial).0)),
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Default, PartialEq, Debug, Clone)]
pub struct ScaleConfig {
    pub available: bool,
    pub name: Option<String>,
    #[serde(serialize_with = "serialize_option_ipv4", deserialize_with = "deserialize_option_ipv4")]
    pub ip: Option<Ipv4Address>,
    pub key: Option<String>,
}

pub struct AppConfig {
    pub framework: Rc<RefCell<Framework>>,
    // configured are what configured
    pub configured_printers: PrintersConfig,
    pub configured_default_printer: DefaultPrinterConfig,
    pub configured_scale: Option<ScaleConfig>,
    pub scale_encryption_key: &'static RefCell<Vec<u8>>,

    config_processed_ok: Option<bool>,
    pn532_ok: Option<bool>,
    pub user_cores: Option<String>,
    pub user_cores_changed_by_web_config: bool,
    // pub previously_used_cores: Option<String>,
    pub custom_filaments: Option<String>,
    pub ai_providers_config: AiProvidersConfig,
    pub root_redirect: String,
}

impl AppConfig {
    #[allow(dead_code)]
    pub fn missing_configs(&self, log: bool) -> bool {
        if self.configured_printers.printers.is_empty() {
            term_info!("No printers configured");
            return false;
        }

        let mut has_printers = false;
        let mut missing = true;
        let mut partial_missing = false;
        for printer in &self.configured_printers.printers {
            has_printers = true;
            match &printer.driver {
                PrinterDriverConfig::Bambu(bambu_config) => {
                    if bambu_config.serial.is_some() && bambu_config.access_code.is_some() {
                        missing = false;
                    }
                    if bambu_config.serial.is_none() || bambu_config.access_code.is_none() {
                        partial_missing = true;
                    }
                }
                PrinterDriverConfig::Fake(fake_config) => {
                    if fake_config.printer_id().is_ok() {
                        missing = false;
                    } else {
                        partial_missing = true;
                    }
                }
            }
        }
        if !has_printers {
            return false;
        }
        if log {
            if missing {
                term_error!("Missing printer(s) information");
            } else if partial_missing {
                term_error!("At least one printer has incomplete configuration");
            }
        }

        missing
    }

    pub fn new(framework: Rc<RefCell<Framework>>) -> Self {
        Self {
            framework,
            configured_printers: PrintersConfig { printers: Vec::new() },
            configured_default_printer: DefaultPrinterConfig { printer_id: None },
            configured_scale: None,
            scale_encryption_key: crate::mk_static!(RefCell<Vec<u8>>, RefCell::new(alloc::vec![])),

            config_processed_ok: None,
            pn532_ok: None,
            user_cores: None,
            user_cores_changed_by_web_config: false,
            // previously_used_cores: None,
            custom_filaments: None,
            ai_providers_config: AiProvidersConfig::default(),
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
                if let Ok(Some(default_printer_store)) = config
                    && let Ok(default_printer_config) = serde_json::from_str::<StoredDefaultPrinterConfig>(&default_printer_store)
                {
                    self.configured_default_printer = default_printer_config.into();
                }
            } else if let Ok(legacy_printers_config) = serde_json::from_str::<LegacyPrintersConfig>(&printers_store) {
                self.configured_printers = legacy_printers_config.into();
            }
        } else {
            // backwards compatibility with a single printer
            let config = self.framework.borrow_mut().fetch(String::from(PRINTER_CONFIG_KEY));
            if let Ok(Some(printer_store)) = config
                && let Ok(printer_config) = serde_json::from_str::<LegacyPrinterConfig>(&printer_store)
            {
                let printer_config = PrinterConfig::from(printer_config);
                self.configured_default_printer.printer_id = printer_config.printer_id().ok().map(|printer_id| printer_id.0);
                self.configured_printers.printers.push(printer_config);
            }
        }
        let config = self.framework.borrow_mut().fetch(String::from(DEFAULT_PRINTER_CONFIG_KEY));
        if let Ok(Some(default_printer_store)) = config
            && let Ok(printers_config) = serde_json::from_str::<StoredDefaultPrinterConfig>(&default_printer_store)
        {
            self.configured_default_printer = printers_config.into();
        }
        // Load core weights configuration

        // let config = self.framework.borrow_mut().fetch(String::from(PREVIOUSLY_USED_CORES_CONFIG_KEY));
        // if let Ok(previously_used_cores) = config {
        //     self.previously_used_cores = previously_used_cores;
        // }

        let config = self.framework.borrow_mut().fetch(String::from(USER_CORES_CONFIG_KEY));
        if let Ok(user_cores) = config {
            self.user_cores = user_cores;
        }

        let config = self.framework.borrow_mut().fetch(String::from(CUSTOM_FILAMENTS_CONFIG_KEY));
        if let Ok(custom_filaments) = config {
            self.custom_filaments = custom_filaments;
        }

        let config = self.framework.borrow_mut().fetch(String::from(AI_PROVIDERS_CONFIG_KEY));
        if let Ok(Some(ai_providers_store)) = config
            && let Ok(ai_providers_config) = serde_json::from_str::<AiProvidersConfig>(&ai_providers_store)
        {
            self.ai_providers_config = ai_providers_config;
        }

        let config = self.framework.borrow_mut().fetch(String::from(SCALE_CONFIG_KEY));
        if let Ok(Some(scale_store)) = config
            && let Ok(scale_config) = serde_json::from_str::<ScaleConfig>(&scale_store)
        {
            self.configured_scale = Some(scale_config);
            self.update_scale_encryption_key();
        }

        let mut section = String::from("");

        let mut parse_errors = false;
        let mut toml_printer_name = None;
        let mut toml_bambu_config = BambuPrinterConfig::default();
        let mut toml_has_printer_config = false;

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
                            toml_bambu_config.ip = Some(addr);
                            toml_has_printer_config = true;
                        } else {
                            parse_errors = true;
                            term_error!("config file format error at printer ip");
                        }
                    }
                    "printer_name" => {
                        toml_printer_name = Some(String::from(value));
                        toml_has_printer_config = true;
                    }
                    "printer_serial" => {
                        toml_bambu_config.serial = Some(String::from(value));
                        toml_has_printer_config = true;
                    }
                    "printer_access_code" => {
                        toml_bambu_config.access_code = Some(String::from(value));
                        toml_has_printer_config = true;
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
        if toml_has_printer_config {
            self.configured_printers
                .printers
                .push(PrinterConfig::bambu(toml_printer_name, toml_bambu_config));
        }

        // If after all, no printer configured, fill in an empty printer config
        // if self.configured_printers.printers.is_empty() {
        //     self.configured_printers.printers.push(PrinterConfig::default());
        // }

        self.config_processed_ok = Some(true);
        Ok(())
    }

    pub fn report_pn532(&mut self, status: bool) {
        self.pn532_ok = Some(status);
    }

    pub fn initialization_ok(&self, log: bool) -> Option<bool> {
        #[cfg(feature = "wt32-sc01-plus")]
        {
            // separate treatment for pn532 for easier board dependency
            if !self.pn532_ok? {
                return None;
            }
        }

        Some(self.framework.borrow().initialization_ok() && matches!(self.config_processed_ok, Some(true)) && !self.missing_configs(log))
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
        self.update_scale_encryption_key();
        Ok(())
    }

    pub fn update_scale_encryption_key(&mut self) {
        if let Some(configured_scale) = &self.configured_scale
            && let Some(scale_security_key) = &configured_scale.key
        {
            let encryption_key = self.framework.borrow().derive_encryption_key(scale_security_key);
            self.scale_encryption_key.replace(encryption_key);
            return;
        }
        self.scale_encryption_key.replace(alloc::vec![]);
    }

    // pub fn set_previously_used_cores(
    //     &mut self,
    //     previously_used_cores: Option<String>,
    // ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
    //     if previously_used_cores.is_some() {
    //         self.framework.borrow().store(
    //             PREVIOUSLY_USED_CORES_CONFIG_KEY.to_string(),
    //             previously_used_cores.as_ref().unwrap().clone(),
    //         )?;
    //     } else {
    //         self.framework.borrow().remove(PREVIOUSLY_USED_CORES_CONFIG_KEY.to_string())?;
    //     }
    //     self.previously_used_cores = previously_used_cores;
    //     Ok(())
    // }

    pub fn set_user_cores(&mut self, user_cores: Option<String>) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if let Some(user_cores) = &user_cores {
            self.framework.borrow().store(USER_CORES_CONFIG_KEY.to_string(), user_cores.clone())?;
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
            if let Some(curr_custom_filaments) = &self.custom_filaments
                && curr_custom_filaments == custom_filaments
            {
                skip_store = true; // no change, better skip writing to flash
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

    fn persist_ai_providers_config(
        &self,
        ai_providers_config: &AiProvidersConfig,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if ai_providers_config.providers.is_empty() {
            self.framework.borrow().remove(AI_PROVIDERS_CONFIG_KEY.to_string())?;
        } else {
            let ai_providers_store = serde_json::to_string(ai_providers_config).unwrap();
            self.framework.borrow().store(AI_PROVIDERS_CONFIG_KEY.to_string(), ai_providers_store)?;
        }
        Ok(())
    }

    pub fn set_ai_provider_api_key(
        &mut self,
        provider: AiProviderId,
        api_key: String,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        let normalized_api_key = api_key.trim().to_string();
        if normalized_api_key.is_empty() {
            return self.delete_ai_provider_api_key(provider);
        }

        let mut next_ai_providers_config = self.ai_providers_config.clone();
        if let Some(existing_provider) = self
            .ai_providers_config
            .providers
            .iter()
            .find(|existing_provider| existing_provider.provider == provider)
        {
            if existing_provider.api_key == normalized_api_key {
                return Ok(());
            }
            next_ai_providers_config
                .providers
                .iter_mut()
                .find(|next_provider| next_provider.provider == provider)
                .unwrap()
                .api_key = normalized_api_key;
        } else {
            next_ai_providers_config.providers.push(AiProviderCredential {
                provider,
                api_key: normalized_api_key,
            });
        }

        self.persist_ai_providers_config(&next_ai_providers_config)?;
        self.ai_providers_config = next_ai_providers_config;
        Ok(())
    }

    pub fn get_ai_provider_api_key(&self, provider: &AiProviderId) -> Option<String> {
        self.ai_providers_config
            .providers
            .iter()
            .find(|existing_provider| &existing_provider.provider == provider)
            .map(|existing_provider| existing_provider.api_key.clone())
    }

    pub fn delete_ai_provider_api_key(&mut self, provider: AiProviderId) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        let mut next_ai_providers_config = self.ai_providers_config.clone();
        let orig_num_providers = next_ai_providers_config.providers.len();
        next_ai_providers_config
            .providers
            .retain(|existing_provider| existing_provider.provider != provider);
        if next_ai_providers_config.providers.len() == orig_num_providers {
            return Ok(());
        }

        self.persist_ai_providers_config(&next_ai_providers_config)?;
        self.ai_providers_config = next_ai_providers_config;
        Ok(())
    }

    pub fn ai_provider_key_availability(&self) -> Vec<AiProviderAvailability> {
        [AiProviderId::OpenAi]
            .into_iter()
            .map(|provider| AiProviderAvailability {
                key_available: self.get_ai_provider_api_key(&provider).is_some(),
                provider,
            })
            .collect()
    }

    pub fn _set_redirect_web_to_config(&mut self) {
        self.root_redirect = "/config".to_string();
    }

    pub fn set_redirect_web_to_inventory(&mut self) {
        self.root_redirect = "/inventory".to_string();
    }

    #[allow(dead_code)]
    pub fn is_scale_available(&self) -> bool {
        let mut available = false;
        if let Some(scale_config) = &self.configured_scale {
            available = scale_config.available;
        }
        available
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AiProviderAvailability {
    pub provider: AiProviderId,
    pub key_available: bool,
}
