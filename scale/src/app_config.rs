use core::cell::RefCell;

use alloc::{rc::Rc, string::String};
use framework::prelude::Framework;

const SCALE_CALIBRATION_CONFIG_KEY: &str = "_scale_calibration_";
const NFC_MODULE_CONFIG_KEY: &str = "nfc_module_config";

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone)]
pub struct ScaleCalibrationConfig {
    pub zero_loadcell: i32,
    pub calib_weight: i32,
    pub calib_loadcell: i32,
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone)]
pub struct NfcModuleConfig {
    pub available: bool,
}

impl Default for NfcModuleConfig {
    fn default() -> Self {
        Self {
            available: true,
        }
    }
}

pub struct AppConfig {
    framework: Rc<RefCell<Framework>>,
    pub configured_calibration: Option<ScaleCalibrationConfig>,
    pub configured_nfc_module: Option<NfcModuleConfig>,
}

impl AppConfig {
    pub fn new(framework: Rc<RefCell<Framework>>) -> Self {
        Self {
            framework,
            configured_calibration: None,
            configured_nfc_module: None,
        }
    }

    pub fn load_config_flash_then_toml(&mut self, _toml_str: &str) -> Result<(), String> {
        let config = self
            .framework
            .borrow_mut()
            .fetch(String::from(SCALE_CALIBRATION_CONFIG_KEY));

        if let Ok(Some(calibration_store)) = config {
            if let Ok(calibration_config) =
                serde_json::from_str::<ScaleCalibrationConfig>(&calibration_store)
            {
                self.configured_calibration = Some(calibration_config);
            }
        }

        let config = self
            .framework
            .borrow_mut()
            .fetch(String::from(NFC_MODULE_CONFIG_KEY));
        if let Ok(Some(nfc_module_store)) = config {
            if let Ok(nfc_module_config) =
                serde_json::from_str::<NfcModuleConfig>(&nfc_module_store)
            {
                self.configured_nfc_module = Some(nfc_module_config);
            }
        }
        Ok(())
    }

    pub fn nfc_module_available(&self) -> bool {
        self.configured_nfc_module.as_ref().unwrap_or(&NfcModuleConfig::default()).available
    }

    pub fn set_scale_calibration_config(
        &mut self,
        zero_loadcell: i32,
        calib_weight: i32,
        calib_loadcell: i32,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        let calibration_config = ScaleCalibrationConfig {
            zero_loadcell,
            calib_weight,
            calib_loadcell,
        };
        let calibration_store = serde_json::to_string(&calibration_config).unwrap();
        self.framework.borrow().store(
            String::from(SCALE_CALIBRATION_CONFIG_KEY),
            calibration_store,
        )?;
        Ok(())
    }

    pub fn set_nfc_module_config(
        &mut self,
        nfc_module_config: NfcModuleConfig,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        let nfc_module_store = serde_json::to_string(&nfc_module_config).unwrap();
        self.framework
            .borrow()
            .store(String::from(NFC_MODULE_CONFIG_KEY), nfc_module_store)?;
        self.configured_nfc_module = Some(nfc_module_config);
        Ok(())
    }

    pub fn initialization_ok(&self) -> bool {
        self.framework.borrow().initialization_ok()
    }
}
