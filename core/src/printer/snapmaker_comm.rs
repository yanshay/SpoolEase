use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    cell::RefCell,
    future::pending,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use edge_http::{
    io::client::Connection,
    ws::{MAX_BASE64_KEY_LEN, MAX_BASE64_KEY_RESPONSE_LEN, NONCE_LEN},
};
use edge_nal_embassy::{Tcp, TcpBuffers};
use edge_ws::{FrameHeader, FrameType};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use framework::{error, prelude::Framework, term_error, term_info, utils::random_u32, warn};
use serde_json::Value;

use super::{snapmaker_driver::U1Channel, snapmaker_state::U1Message};
use crate::app_config::SnapmakerU1PrinterConfig;

const SUBSCRIBE: &str = r#"{"jsonrpc":"2.0","id":2,"method":"printer.objects.subscribe","params":{"objects":{"filament_feed left":null,"filament_feed right":null,"filament_motion_sensor e0_filament":["filament_detected"],"filament_motion_sensor e1_filament":["filament_detected"],"filament_motion_sensor e2_filament":["filament_detected"],"filament_motion_sensor e3_filament":["filament_detected"],"print_task_config":["filament_type","filament_sub_type","filament_vendor","filament_color_rgba","filament_color_multi"],"extruder":["state"],"extruder1":["state"],"extruder2":["state"],"extruder3":["state"],"print_stats":["state"]}}}"#;
const IDENTITY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"machine.system_info"}"#;
const MAX_FRAME: usize = 32768;

async fn discover(framework: &Rc<RefCell<Framework>>, serial: &str, number: usize, name: &str) -> Option<Ipv4Addr> {
    if let Err(err) = framework.borrow_mut().start_mdns_service() {
        term_error!("[{}] Snapmaker U1 {:?} Error: could not start mDNS: {:?}", number, name, err);
        return None;
    }
    let discovery = framework.borrow().mdns_discovery.clone();
    let subscription = match discovery.subscribe(Some("_snapmaker._tcp.local")) {
        Ok(subscription) => subscription,
        Err(err) => {
            term_error!("[{}] Snapmaker U1 {:?} Error: discovery unavailable: {}", number, name, err);
            return None;
        }
    };
    term_info!("[{}] Snapmaker U1 {:?}: discovering printer", number, name);
    let mut reported: Vec<alloc::string::String> = Vec::new();
    loop {
        let service = subscription.next().await;
        if !service.txt_value("machine_type").is_some_and(|v| v.eq_ignore_ascii_case("Snapmaker U1")) {
            continue;
        }
        let Some(found) = service.txt_value("sn") else {
            continue;
        };
        if !found.eq_ignore_ascii_case(serial) {
            if !reported.iter().any(|v| v.eq_ignore_ascii_case(found)) {
                warn!(
                    "[{}] Snapmaker U1 {:?}: found printer with serial {}, but configured serial is {}",
                    number, name, found, serial
                );
                reported.push(found.to_string());
            }
            continue;
        }
        if let Some(ip) = service.addresses.first() {
            term_info!(
                "[{}] Snapmaker U1 {:?}: discovered printer {} at {}",
                number,
                name,
                service.txt_value("device_name").unwrap_or("Snapmaker U1"),
                ip
            );
            return Some(*ip);
        }
    }
}

async fn send_frame<W: Write>(socket: &mut W, kind: FrameType, data: &[u8]) -> Result<(), ()> {
    edge_ws::io::send(&mut *socket, kind, Some(random_u32()), data).await.map_err(|_| ())?;
    socket.flush().await.map_err(|_| ())
}

enum Outcome {
    Retry(bool),
    InvalidSerial,
}

fn upgrade_failure<E: embedded_io_async::Error>(stage: &str, err: edge_http::io::Error<E>) -> String {
    format!("{stage}: {err:?}")
}

#[derive(Default)]
struct RetryLog {
    last_log: Option<Instant>,
    last_terminal: Option<Instant>,
}

impl RetryLog {
    fn failure(&mut self, number: usize, name: &str, detail: &str) {
        let now = Instant::now();
        if self.last_terminal.is_none_or(|last| last.elapsed() >= Duration::from_secs(150)) {
            term_error!("[{}] Snapmaker U1 {:?} Errror: {} (will retry)", number, name, detail);
            self.last_terminal = Some(now);
            self.last_log = Some(now);
        } else if self.last_log.is_none_or(|last| last.elapsed() >= Duration::from_secs(30)) {
            error!("[{}] Snapmaker U1 {:?} Error: {} (will retry)", number, name, detail);
            self.last_log = Some(now);
        }
    }
}

pub(super) async fn snapmaker_u1_comm_task(
    framework: Rc<RefCell<Framework>>,
    config: SnapmakerU1PrinterConfig,
    number: usize,
    name: String,
    channel: Rc<U1Channel>,
) {
    Framework::wait_for_wifi(&framework).await;
    let Some(serial) = config.serial.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        term_error!("[{}] Snapmaker U1 {:?} Error: serial is missing", number, name);
        pending::<()>().await;
        return;
    };
    let stack = framework.borrow().stack;
    let buffers = Box::new(TcpBuffers::<1, 4096, 4096>::new());
    let tcp = Tcp::new(stack, &*buffers);
    let tcp = edge_nal::WithTimeout::new(10000, tcp);
    let mut http_buffer = vec![0u8; 4096];
    let mut ip = config.ip;
    let mut failing_since: Option<Instant> = None;
    let mut retry_log = RetryLog::default();
    loop {
        Framework::wait_for_wifi(&framework).await;
        if ip.is_none() {
            ip = discover(&framework, serial, number, &name).await;
            if ip.is_none() {
                Timer::after_secs(5).await;
                continue;
            }
            failing_since = None;
        }
        let address = ip.unwrap();
        if retry_log.last_log.is_none() {
            term_info!("[{}] Connecting to Snapmaker U1 {:?} at {}:7125", number, name, address);
        }
        let mut conn: Connection<_> = Connection::new(&mut http_buffer, &tcp, SocketAddr::new(IpAddr::V4(address), 7125));
        let mut nonce = [0u8; NONCE_LEN];
        if getrandom::getrandom(&mut nonce).is_err() {
            retry_log.failure(number, &name, "could not generate WebSocket nonce");
            Timer::after_secs(2).await;
            continue;
        }
        let mut key = [0u8; MAX_BASE64_KEY_LEN];
        let mut response = [0u8; MAX_BASE64_KEY_RESPONSE_LEN];
        let upgrade = async {
            conn.initiate_ws_upgrade_request(Some(&address.to_string()), None, "/websocket", None, &nonce, &mut key)
                .await
                .map_err(|err| upgrade_failure("opening WebSocket", err))?;
            conn.initiate_response()
                .await
                .map_err(|err| upgrade_failure("reading WebSocket upgrade response", err))?;
            match conn.is_ws_upgrade_accepted(&nonce, &mut response) {
                Ok(true) => {}
                Ok(false) => return Err("WebSocket upgrade rejected".to_string()),
                Err(err) => return Err(upgrade_failure("checking WebSocket upgrade", err)),
            }
            conn.complete()
                .await
                .map_err(|err| upgrade_failure("completing WebSocket upgrade", err))?;
            Ok::<(), String>(())
        }
        .await;
        let outcome = if let Err(err) = upgrade {
            retry_log.failure(number, &name, &err);
            Outcome::Retry(false)
        } else {
            term_info!("[{}] Snapmaker U1 {:?}: WebSocket connected, checking serial", number, name);
            let (mut socket, _) = conn.release();
            run_session(&mut socket, serial, number, &name, &mut retry_log, &channel).await
        };
        channel.send(U1Message::Reset).await;
        match outcome {
            Outcome::InvalidSerial => {
                pending::<()>().await;
            }
            Outcome::Retry(was_connected) => {
                if was_connected {
                    retry_log = RetryLog::default();
                }
                if was_connected || failing_since.is_none() {
                    failing_since = Some(Instant::now());
                }
                if config.ip.is_none() && failing_since.is_some_and(|start| start.elapsed() >= Duration::from_secs(300)) {
                    term_info!("[{}] Snapmaker U1 {:?}: connection failed for five minutes; rediscovering", number, name);
                    ip = None;
                }
                Timer::after_secs(2).await;
            }
        }
    }
}

async fn run_session<S: Read + Write>(
    socket: &mut S,
    serial: &str,
    number: usize,
    name: &str,
    retry_log: &mut RetryLog,
    channel: &U1Channel,
) -> Outcome {
    if send_frame(socket, FrameType::Text(false), IDENTITY.as_bytes()).await.is_err() {
        retry_log.failure(number, name, "failed to send identity request");
        return Outcome::Retry(false);
    }
    let mut input = vec![0u8; MAX_FRAME];
    let mut len = 0usize;
    let mut fragments = Vec::new();
    let mut verified = false;
    let mut initialized = false;
    let mut request_deadline = Some(Instant::now() + Duration::from_secs(15));
    let mut next_ping = Instant::now() + Duration::from_secs(10);
    let mut ping: Option<([u8; 4], Instant)> = None;
    macro_rules! retry {
        ($($arg:tt)*) => {{
            if initialized {
                term_error!("[{}] Snapmaker U1 {:?} Error: disconnected: {} (will retry)", number, name, format!($($arg)*));
            } else {
                retry_log.failure(number, name, &format!($($arg)*));
            }
            return Outcome::Retry(initialized);
        }};
    }
    loop {
        let now = Instant::now();
        if request_deadline.is_some_and(|deadline| now >= deadline) {
            retry!("identity or subscription request timed out");
        }
        if ping.as_ref().is_some_and(|(_, deadline)| now >= *deadline) {
            retry!("WebSocket pong timed out");
        }
        if now >= next_ping && ping.is_none() {
            let payload = random_u32().to_le_bytes();
            if send_frame(socket, FrameType::Ping, &payload).await.is_err() {
                retry!("failed to send WebSocket ping");
            }
            ping = Some((payload, Instant::now() + Duration::from_secs(10)));
            next_ping = Instant::now() + Duration::from_secs(10);
        }
        if len == input.len() {
            retry!("WebSocket frame too large");
        }
        match select(socket.read(&mut input[len..]), Timer::after_millis(200)).await {
            Either::First(Ok(0)) => retry!("WebSocket closed by printer"),
            Either::First(Err(err)) => retry!("WebSocket read failed: {:?}", err),
            Either::First(Ok(n)) => len += n,
            Either::Second(_) => continue,
        }
        loop {
            let (header, header_len) = match FrameHeader::deserialize(&input[..len]) {
                Ok(value) => value,
                Err(edge_ws::Error::Incomplete(_)) => break,
                Err(err) => retry!("invalid WebSocket frame: {:?}", err),
            };
            let Ok(payload_len) = usize::try_from(header.payload_len) else {
                retry!("invalid WebSocket frame length");
            };
            let Some(end) = header_len.checked_add(payload_len) else {
                retry!("invalid WebSocket frame length");
            };
            if end > MAX_FRAME {
                retry!("WebSocket frame exceeds {} bytes", MAX_FRAME);
            }
            if len < end {
                break;
            }
            header.mask(&mut input[header_len..end], 0);
            let payload = &input[header_len..end];
            match header.frame_type {
                FrameType::Ping => {
                    if send_frame(socket, FrameType::Pong, payload).await.is_err() {
                        retry!("failed to send WebSocket pong");
                    }
                }
                FrameType::Pong => {
                    if ping.as_ref().is_some_and(|(expected, _)| payload == expected) {
                        ping = None;
                    }
                }
                FrameType::Close => retry!("printer closed WebSocket"),
                FrameType::Text(fragmented) => {
                    if !fragments.is_empty() {
                        retry!("unexpected fragmented WebSocket message");
                    }
                    fragments.extend_from_slice(payload);
                    if !fragmented {
                        if let Some(outcome) = process_message(
                            socket,
                            &fragments,
                            serial,
                            number,
                            name,
                            retry_log,
                            channel,
                            &mut verified,
                            &mut initialized,
                            &mut request_deadline,
                        )
                        .await
                        {
                            return outcome;
                        }
                        fragments.clear();
                    }
                }
                FrameType::Continue(last) => {
                    if fragments.is_empty() || fragments.len() + payload.len() > MAX_FRAME {
                        retry!("invalid or oversized fragmented WebSocket message");
                    }
                    fragments.extend_from_slice(payload);
                    if last {
                        if let Some(outcome) = process_message(
                            socket,
                            &fragments,
                            serial,
                            number,
                            name,
                            retry_log,
                            channel,
                            &mut verified,
                            &mut initialized,
                            &mut request_deadline,
                        )
                        .await
                        {
                            return outcome;
                        }
                        fragments.clear();
                    }
                }
                _ => retry!("unexpected WebSocket frame type {:?}", header.frame_type),
            }
            input.copy_within(end..len, 0);
            len -= end;
        }
    }
}

async fn process_message<S: Write>(
    socket: &mut S,
    data: &[u8],
    serial: &str,
    number: usize,
    name: &str,
    retry_log: &mut RetryLog,
    channel: &U1Channel,
    verified: &mut bool,
    initialized: &mut bool,
    deadline: &mut Option<Instant>,
) -> Option<Outcome> {
    macro_rules! retry {
        ($($arg:tt)*) => {{
            if *initialized {
                term_error!("[{}] Snapmaker U1 {:?} Error: disconnected: {} (will retry)", number, name, format!($($arg)*));
            } else {
                retry_log.failure(number, name, &format!($($arg)*));
            }
            return Some(Outcome::Retry(*initialized));
        }};
    }
    let Ok(message) = serde_json::from_slice::<Value>(data) else {
        retry!("invalid Moonraker message");
    };
    if message.get("id").and_then(Value::as_u64) == Some(1) {
        if message.get("error").is_some() || message.get("result").is_none() {
            retry!("Moonraker identity request failed: {:?}", message.get("error"));
        }
        let found = message
            .pointer("/result/system_info/product_info/serial_number")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if !found.is_some_and(|v| v.eq_ignore_ascii_case(serial)) {
            term_error!(
                "[{}] Snapmaker U1 {:?} Error: serial mismatch: configured {}, reported {}",
                number,
                name,
                serial,
                found.unwrap_or("<missing>")
            );
            return Some(Outcome::InvalidSerial);
        }
        *verified = true;
        term_info!("[{}] Snapmaker U1 {:?}: serial verified; subscribing to printer status", number, name);
        if send_frame(socket, FrameType::Text(false), SUBSCRIBE.as_bytes()).await.is_err() {
            retry!("failed to send status subscription");
        }
        *deadline = Some(Instant::now() + Duration::from_secs(15));
    } else if message.get("id").and_then(Value::as_u64) == Some(2) && *verified {
        let Some(status) = message.pointer("/result/status").filter(|v| v.is_object()) else {
            retry!("status subscription failed: {:?}", message.get("error"));
        };
        if channel.try_send(U1Message::Initial(status.clone())).is_err() {
            retry!("status processing queue full");
        }
        term_info!("[{}] Snapmaker U1 {:?}: status subscription confirmed", number, name);
        *retry_log = RetryLog::default();
        *initialized = true;
        *deadline = None;
    } else if *verified {
        match message.get("method").and_then(Value::as_str) {
            Some("notify_status_update") => {
                if let Some(status) = message.pointer("/params/0").filter(|v| v.is_object()) {
                    if channel.try_send(U1Message::Update(status.clone())).is_err() {
                        retry!("status processing queue full");
                    }
                }
            }
            Some("notify_klippy_disconnected" | "notify_klippy_shutdown") => {
                term_error!(
                    "[{}] Snapmaker U1 {:?} Error: Moonraker reports {}",
                    number,
                    name,
                    message.get("method").and_then(Value::as_str).unwrap_or("Klipper unavailable")
                );
                if channel.try_send(U1Message::Reset).is_err() {
                    retry!("status processing queue full");
                }
                *initialized = false;
                *deadline = None;
            }
            Some("notify_klippy_ready") => {
                term_info!("[{}] Snapmaker U1 {:?}: Klipper ready; resubscribing to printer status", number, name);
                if channel.try_send(U1Message::Reset).is_err() {
                    retry!("status processing queue full");
                }
                *initialized = false;
                if send_frame(socket, FrameType::Text(false), SUBSCRIBE.as_bytes()).await.is_err() {
                    retry!("failed to resend status subscription");
                }
                *deadline = Some(Instant::now() + Duration::from_secs(15));
            }
            _ => {}
        }
    }
    None
}
