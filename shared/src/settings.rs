pub const SCALE_STABLE_OTA_PATH: &str = "/bins/0.6/scale/ota/";
pub const SCALE_UNSTABLE_OTA_PATH: &str = "/bins/0.6/scale/ota-unstable/";
pub const SCALE_DEBUG_OTA_PATH: &str = "/bins/0.6/scale/debug/";
pub const OTA_DOMAIN_STABLE: &str = "bin.spoolease.io";
pub const OTA_DOMAIN_UNSTABLE: &str = "bin.spoolease.io";
pub const OTA_DOMAIN_DEBUG: &str = "bin.spoolease.io";
pub const OTA_TLS_CERTIFICATE: &str = concat!(include_str!("./certs/bin.spoolease.io.pem"), "\0");

pub const GCODE_ANALYSIS_PRINTER_FTP_RETRY_INTERVAL_SECS: u64 = 30;
pub const GCODE_ANALYSIS_PRINTER_FTP_RETRY_TIMEOUT_SECS: u64 = 10 * 60;
