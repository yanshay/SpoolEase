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
    Term(String),
    Uncalibrated,
    NewLoad(Weight),
    LoadChangedStable(Weight),
    LoadChangedUnstable(Weight),
    LoadRemoved,
    RawSamplesAvg(i32),
    TagStatus(crate::spool_tag::Status),
    PN532Status(bool),
    WebConfigEnabled(WebConfigInfo),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ConsoleToScale {
    Calibrate(Weight),
    ResetCalibration,
    SetNotifyThreshold(Weight),
    EnableWebConfig,
    DisableWebConfig,
}
