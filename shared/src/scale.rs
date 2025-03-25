// might need to put this under feature flag to compile with std
use alloc::string::String;
use serde::{Deserialize, Serialize};

type Weight = i32;

#[derive(Serialize, Deserialize, Debug)]
pub struct WebConfigInfo {
    pub security_key: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ScaleToConsole {
    NewLoad(Weight),
    LoadChanged(Weight),
    LoadRemoved,
    WebConfigEnabled(WebConfigInfo),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ConsoleToScale {
    RequestMeasure,
    Tare,
    SetNotifyThreshold(Weight),
    Calibrate(Weight),
    ResetCalibrations,
    EnableWebConfig,
    DisableWebConfig,
}
