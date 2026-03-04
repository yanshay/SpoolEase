// might need to put this under feature flag to compile with std
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

use crate::gcode_analysis_task::{GcodeAnalysisNotification, GcodeAnalysisRequest};

type Weight = i32;

#[derive(Serialize, Deserialize, Debug)]
pub struct WebConfigInfo {
    pub security_key: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum OtaProgressUpdate {
    Start,
    Status { text: String },
    Failed { text: String },
    Completed { text: String },
    VersionAvailable { version: String, newer: bool },
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
    ButtonPressed,
    TagStatus(crate::spool_tag::Status),
    PN532Status(bool),
    GcodeAnalysis {
        job_number: i32,
        printer_index: usize,
        filament_usage_csv: String,
    },
    GcodeAnalysisFailed {
        job_number: i32,
        printer_index: usize,
    },
    GcodeAnalysisCanceled {
        job_number: i32,
        printer_index: usize,
    },
    GcodeAnalysisCompleted {
        job_number: i32,
        printer_index: usize,
    },
    ScaleVersion {
        version: String,
    },
    OtaProgressUpdate(OtaProgressUpdate),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ConsoleToScale {
    Calibrate(Weight),
    ButtonResponse(bool),
    RequestGcodeAnalysis {
        gcode_analysis_request: GcodeAnalysisRequest,
    },
    GcodeAnalysisNotify {
        gcode_analysis_notification: GcodeAnalysisNotification,
    },
    ReadTag,
    WriteTag {
        text: String,
        check_uid: Option<Vec<Vec<u8>>>,
        cookie: String,
    },
    EraseTag {
        check_uid: Option<Vec<u8>>,
        cookie: String,
    },
    EmulateTag {
        url: String,
    },
    UpdateFirmware {
        ota_domain: String,
        ota_path: String,
        ota_toml_filename: String,
        ota_cert: String,
    },
    TagsInStore {
        tags: String,
    },
}
