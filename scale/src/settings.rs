pub const AP_ADDR: (u8,u8,u8,u8) = (192, 168, 2, 1);

pub const OTA_DOMAIN: &str = "raw.githubusercontent.com";
pub const OTA_PATH: &str = "/yanshay/spoolease-bin/refs/heads/main/bins/ota/";
pub const OTA_TOML_FILENAME: &str = "ota.toml";

pub const WEB_SERVER_HTTPS: bool = false; // Don't forget to set also port below
pub const WEB_SERVER_PORT: u16 = 80; // For HTTPS use 443 normally, for HTTP 80, but either can be any other port number
pub const WEB_SERVER_CAPTIVE: bool = true;
pub const WEB_SERVER_NUM_LISTENERS: usize = 3;
pub const WEB_SERVER_TLS_CERTIFICATE: &str = concat!(include_str!("./certs/web-server-certificate.pem"), "\0");
pub const WEB_SERVER_TLS_PRIVATE_KEY: &str = concat!(include_str!("./certs/web-server-private-key.pem"), "\0");

pub const WEB_APP_DOMAIN: &str = "device.spoolease.io";
pub const WEB_APP_SECURITY_KEY_LENGTH: usize = 7; 
pub const WEB_APP_SALT: &str = "example_salt"; // to be aligned with WASM & Captive HTML
pub const WEB_APP_KEY_DERIVATION_ITERATIONS: u32 = 10_000; // to be aligned with WASM & Captive HTML
