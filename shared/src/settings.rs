pub const SCALE_STABLE_OTA_PATH: &str = "/bins/0.8/scale/ota/";
pub const SCALE_UNSTABLE_OTA_PATH: &str = "/bins/0.8/scale/ota-unstable/";
pub const SCALE_DEBUG_OTA_PATH: &str = "/bins/0.8/scale/debug/";
pub const SCALE_ONEOFF_OTA_PATH: &str = "/bins/0.8/scale/oneoff/";
pub const OTA_DOMAIN_STABLE: &str = "bin.spoolease.io";
pub const OTA_DOMAIN_UNSTABLE: &str = "bin.spoolease.io";
pub const OTA_DOMAIN_DEBUG: &str = "bin.spoolease.io";
pub const OTA_TLS_CERTIFICATE: &str = concat!(include_str!("./certs/bin.spoolease.io.pem"), "\0");

pub const GCODE_ANALYSIS_PRINTER_FTP_RETRY_INTERVAL_SECS: u64 = 30;
pub const GCODE_ANALYSIS_PRINTER_FTP_RETRY_TIMEOUT_SECS: u64 = 10 * 60;

// MIFARE identification timeouts, shared by the console and scale readers.
// The payload limit covers all authentication and block reads for one format.
pub const NFC_MIFARE_PAYLOAD_TIMEOUT_MS: u64 = 3000; // Existing overall backstop, not based on the latest measurements.
pub const NFC_MIFARE_REFIND_TIMEOUT_MS: u64 = 2000; // Max measured 69 ms, excluding the 1930 ms outlier.
pub const NFC_MIFARE_AUTH_TIMEOUT_MS: u64 = 500; // Max measured 238 ms, including rejected authentication.
pub const NFC_MIFARE_BLOCK_READ_TIMEOUT_MS: u64 = 250; // Max measured 49 ms successful, 95 ms failed.
