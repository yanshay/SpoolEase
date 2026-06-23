use shared::settings::OTA_DOMAIN_STABLE;

pub const AP_ADDR: (u8, u8, u8, u8) = (192, 168, 2, 1);

pub const WEB_SERVER_HTTPS: bool = false; // Don't forget to set also port below
pub const WEB_SERVER_PORT: u16 = 80; // For HTTPS use 443 normally, for HTTP 80, but either can be any other port number
pub const WEB_SERVER_CAPTIVE: bool = true;
pub const WEB_SERVER_NUM_LISTENERS: usize = 12; // 6 per browser simultaniously
pub const WEB_SERVER_TLS_CERTIFICATE: &str = concat!(include_str!("./certs/web-server-certificate.pem"), "\0");
pub const WEB_SERVER_TLS_PRIVATE_KEY: &str = concat!(include_str!("./certs/web-server-private-key.pem"), "\0");

pub const API_SERVER_HTTPS: bool = true;
pub const API_SERVER_PORT: u16 = 443;
pub const API_SERVER_NUM_LISTENERS: usize = 2;
pub const API_SERVER_TLS_CERTIFICATE: &str = WEB_SERVER_TLS_CERTIFICATE;
pub const API_SERVER_TLS_PRIVATE_KEY: &str = WEB_SERVER_TLS_PRIVATE_KEY;

pub const WEB_APP_DOMAIN: &str = "device.spoolease.io";
pub const WEB_APP_SECURITY_KEY_LENGTH: usize = 7;
pub const WEB_APP_SALT: &str = "example_salt"; // to be aligned with WASM & Captive HTML
pub const WEB_APP_KEY_DERIVATION_ITERATIONS: u32 = 10_000; // to be aligned with WASM & Captive HTML

pub const MAX_NUM_PRINTERS: usize = 5;

// Framework basic OTA (from web-config)
pub const OTA_DOMAIN: &str = OTA_DOMAIN_STABLE;
pub const OTA_PATH: &str = CONSOLE_STABLE_OTA_PATH;

pub const OTA_TOML_FILENAME: &str = "ota.toml";
// pub const OTA_TLS_CERTIFICATE: &str = concat!(include_str!("./certs/raw.githubusercontent.com.pem"), "\0");
pub const CONSOLE_STABLE_OTA_PATH: &str = "/bins/0.6/console/ota/";
pub const CONSOLE_UNSTABLE_OTA_PATH: &str = "/bins/0.6/console/ota-unstable/";
pub const CONSOLE_DEBUG_OTA_PATH: &str = "/bins/0.6/console/debug/";

#[cfg(feature = "jc8048w550c")]
pub const DISPLAY_WIDTH_PX: u32 = 800;
#[cfg(feature = "jc8048w550c")]
pub const DISPLAY_HEIGHT_PX: u32 = 480;

#[cfg(feature = "wt32-sc01-plus")]
pub const DISPLAY_WIDTH_PX: u32 = 480;
#[cfg(feature = "wt32-sc01-plus")]
pub const DISPLAY_HEIGHT_PX: u32 = 320;

// Memory Tuning
pub const WEB_SERVER_TCP_RX_BUFFER_BYTES: usize = 2 * 1024; // Main web listener TCP receive buffer per listener.
pub const WEB_SERVER_TCP_TX_BUFFER_BYTES: usize = 2 * 1024; // Main web listener TCP transmit buffer per listener.
pub const WEB_SERVER_HTTP_BUFFER_BYTES: usize = 4 * 1024; // Main web listener picoserve request/header parse buffer per listener.
pub const WEB_SERVER_HTTP_RETAINED_BUFFERS: usize = 6; // Retained shared HTTP buffers for typical browser concurrency.
pub const WEB_REQUEST_BODY_MAX_BYTES: usize = 32 * 1024; // Main web maximum accepted heap-read request body size.
pub const API_SERVER_TCP_RX_BUFFER_BYTES: usize = 2 * 1024; // API listener TCP receive buffer per listener.
pub const API_SERVER_TCP_TX_BUFFER_BYTES: usize = 2 * 1024; // API listener TCP transmit buffer per listener.
pub const API_SERVER_HTTP_BUFFER_BYTES: usize = 4 * 1024; // API listener picoserve request/header parse buffer per listener.
pub const API_SERVER_HTTP_RETAINED_BUFFERS: usize = 1; // Retained shared HTTP buffers for typical API concurrency.
pub const API_REQUEST_BODY_MAX_BYTES: usize = 32 * 1024; // API maximum accepted heap-read request body size.
pub const SLICER_WS_INITIAL_QUEUE_GRACE_MS: u64 = 60 * 1000; // Queue slicer-bound messages this long after boot before the first slicer connection.
pub const SLICER_WS_DISCONNECT_QUEUE_GRACE_MS: u64 = 60 * 1000; // Queue slicer-bound messages for this long after a slicer disconnects.
pub const SLICER_WS_MAX_QUEUED_MESSAGES: usize = 32; // Maximum queued slicer-bound messages; oldest messages are dropped first.
pub const SLICER_WS_MESSAGE_BUFFER_BYTES: usize = 64 * 1024; // Full incoming WebSocket message buffer for slicer integration.
pub const SLICER_WS_PING_INTERVAL_BASE_MS: u64 = 12 * 1000; // Keepalive ping interval base for the slicer WebSocket.
pub const SLICER_WS_PING_INTERVAL_JITTER_MS: u64 = 2 * 1000; // Random keepalive jitter, matching SpoolScale-console communication.
pub const SLICER_WS_MAX_MISSED_HEARTBEATS: usize = 1; // Missed ping responses before disconnecting the slicer WebSocket.
pub const SPOOL_SCALE_WS_CLIENT_BUFFER_BYTES: usize = 4 * 1024; // Console-side SpoolScale WebSocket upgrade/payload buffer.
pub const MQTT_TCP_RX_BUFFER_BYTES: usize = 4 * 1024; // Per-printer MQTT TCP receive buffer below TLS/MQTT.
pub const MQTT_TCP_TX_BUFFER_BYTES: usize = 4 * 1024; // Per-printer MQTT TCP transmit buffer below TLS/MQTT.
pub const MQTT_INITIAL_PACKET_BUFFER_BYTES: usize = 16 * 1024; // Initial per-printer MQTT inbound packet buffer.
pub const MQTT_MAX_PACKET_BUFFER_BYTES: usize = 48 * 1024; // Maximum per-printer MQTT inbound packet buffer after growth.
pub const MQTT_PACKET_BUFFER_GROW_STEP_BYTES: usize = 8 * 1024; // Amount to grow MQTT inbound packet buffer when full.
pub const MQTT_INITIAL_OUT_BUFFER_BYTES: usize = 512; // Initial per-printer MQTT outbound packet encoding buffer.
