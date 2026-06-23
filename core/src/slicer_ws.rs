use alloc::{collections::VecDeque, string::String, string::ToString};

use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use framework::{error, info, utils::random_u32, warn};
use once_cell::sync::OnceCell;
use picoserve::response::ws::{self, Message};
use serde::Serialize;
use static_cell::StaticCell;

use crate::settings::{
    SLICER_WS_DISCONNECT_QUEUE_GRACE_MS, SLICER_WS_INITIAL_QUEUE_GRACE_MS, SLICER_WS_MAX_QUEUED_MESSAGES,
    SLICER_WS_MAX_MISSED_HEARTBEATS, SLICER_WS_MESSAGE_BUFFER_BYTES, SLICER_WS_PING_INTERVAL_BASE_MS,
    SLICER_WS_PING_INTERVAL_JITTER_MS,
};

static SLICER_WS_PROXY_CELL: StaticCell<SlicerWsProxy> = StaticCell::new();
static SLICER_WS_PROXY: OnceCell<&'static SlicerWsProxy> = OnceCell::new();

pub struct SlicerWsProxy {
    state: Mutex<CriticalSectionRawMutex, SlicerWsState>,
    wake: Signal<CriticalSectionRawMutex, ()>,
}

struct SlicerWsState {
    ever_connected: bool,
    connected: bool,
    initial_queue_started_at: Instant,
    disconnected_at: Option<Instant>,
    active_connection_id: Option<u64>,
    next_connection_id: u64,
    queued_messages: VecDeque<String>,
}

#[derive(Serialize)]
struct PrinterSendJsonEnvelope {
    #[serde(rename = "type")]
    message_type: &'static str,
    printer_serial: String,
    payload: serde_json::Value,
}

fn json_field_string(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(value) if value.is_string() => value.as_str().unwrap_or("-").to_string(),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "-".to_string(),
    }
}

fn payload_summary(payload: &serde_json::Value) -> (String, String) {
    let print = payload.get("print");
    (
        json_field_string(print.and_then(|print| print.get("command"))),
        json_field_string(print.and_then(|print| print.get("sequence_id"))),
    )
}

fn heartbeat_interval_ms() -> u64 {
    if SLICER_WS_PING_INTERVAL_JITTER_MS == 0 {
        SLICER_WS_PING_INTERVAL_BASE_MS
    } else {
        SLICER_WS_PING_INTERVAL_BASE_MS + (random_u32() as u64 % SLICER_WS_PING_INTERVAL_JITTER_MS)
    }
}

pub fn init_slicer_ws_proxy() -> &'static SlicerWsProxy {
    let proxy = SLICER_WS_PROXY_CELL.init(SlicerWsProxy::new());
    let _ = SLICER_WS_PROXY.set(proxy);
    proxy
}

pub fn proxy_printer_json(printer_serial: &str, payload: &str) -> bool {
    let Some(proxy) = SLICER_WS_PROXY.get().copied() else {
        warn!("Slicer WebSocket proxy is not initialized; dropping printer message");
        return false;
    };
    proxy.enqueue_printer_json(printer_serial, payload)
}

impl SlicerWsProxy {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlicerWsState {
                ever_connected: false,
                connected: false,
                initial_queue_started_at: Instant::now(),
                disconnected_at: None,
                active_connection_id: None,
                next_connection_id: 1,
                queued_messages: VecDeque::new(),
            }),
            wake: Signal::new(),
        }
    }

    pub fn handler(&'static self) -> SlicerWsHandler {
        SlicerWsHandler { proxy: self }
    }

    fn enqueue_printer_json(&self, printer_serial: &str, payload: &str) -> bool {
        let payload_json = match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => value,
            Err(err) => {
                error!("Failed to parse printer JSON for slicer proxy: {err:?}");
                return false;
            }
        };

        let (command, sequence_id) = payload_summary(&payload_json);
        let envelope = PrinterSendJsonEnvelope {
            message_type: "printer_send_json",
            printer_serial: printer_serial.to_string(),
            payload: payload_json,
        };

        let message = match serde_json::to_string(&envelope) {
            Ok(value) => value,
            Err(err) => {
                error!("Failed to serialize slicer proxy message: {err:?}");
                return false;
            }
        };

        self.enqueue_message(message, printer_serial, &command, &sequence_id)
    }

    fn enqueue_message(&self, message: String, printer_serial: &str, command: &str, sequence_id: &str) -> bool {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(_) => {
                warn!("Slicer WebSocket queue is busy; dropping printer message: printer={printer_serial} command={command} sequence_id={sequence_id}");
                return false;
            }
        };

        if !state.connected {
            if !state.ever_connected {
                let within_initial_grace = state.initial_queue_started_at.elapsed() <= Duration::from_millis(SLICER_WS_INITIAL_QUEUE_GRACE_MS);
                if !within_initial_grace {
                    if !state.queued_messages.is_empty() {
                        warn!("Slicer WebSocket initial connection grace expired; clearing queued printer messages");
                        state.queued_messages.clear();
                    }
                    warn!("Slicer WebSocket has not connected within initial grace period; dropping printer message: printer={printer_serial} command={command} sequence_id={sequence_id}");
                    return false;
                }
            } else {
                let within_grace = state
                    .disconnected_at
                    .map(|disconnected_at| disconnected_at.elapsed() <= Duration::from_millis(SLICER_WS_DISCONNECT_QUEUE_GRACE_MS))
                    .unwrap_or(false);

                if !within_grace {
                    if !state.queued_messages.is_empty() {
                        warn!("Slicer WebSocket reconnect grace expired; clearing queued printer messages");
                        state.queued_messages.clear();
                    }
                    warn!("Slicer WebSocket is disconnected beyond grace period; dropping printer message: printer={printer_serial} command={command} sequence_id={sequence_id}");
                    return false;
                }
            }
        }

        if state.queued_messages.len() >= SLICER_WS_MAX_QUEUED_MESSAGES {
            state.queued_messages.pop_front();
            warn!("Slicer WebSocket queue full; dropped oldest printer message");
        }

        state.queued_messages.push_back(message);
        self.wake.signal(());
        true
    }

    async fn mark_connected(&self) -> u64 {
        let mut state = self.state.lock().await;
        if !state.ever_connected
            && state.initial_queue_started_at.elapsed() > Duration::from_millis(SLICER_WS_INITIAL_QUEUE_GRACE_MS)
            && !state.queued_messages.is_empty()
        {
            warn!("Slicer WebSocket initial connection grace expired before first connect; clearing queued printer messages");
            state.queued_messages.clear();
        }
        let connection_id = state.next_connection_id;
        state.next_connection_id = state.next_connection_id.wrapping_add(1).max(1);
        state.ever_connected = true;
        state.connected = true;
        state.disconnected_at = None;
        state.active_connection_id = Some(connection_id);
        info!("Slicer WebSocket connected; queued_messages={}", state.queued_messages.len());
        self.wake.signal(());
        connection_id
    }

    async fn mark_disconnected(&self, connection_id: u64) {
        let mut state = self.state.lock().await;
        if state.active_connection_id != Some(connection_id) {
            return;
        }
        state.connected = false;
        state.disconnected_at = Some(Instant::now());
        state.active_connection_id = None;
        info!("Slicer WebSocket disconnected; queued messages will be retained for {SLICER_WS_DISCONNECT_QUEUE_GRACE_MS}ms");
    }

    async fn is_active_connection(&self, connection_id: u64) -> bool {
        let state = self.state.lock().await;
        state.active_connection_id == Some(connection_id)
    }

    async fn pop_next_message(&self, connection_id: u64) -> Option<String> {
        let mut state = self.state.lock().await;
        if state.active_connection_id != Some(connection_id) {
            return None;
        }
        state.queued_messages.pop_front()
    }

    async fn wait_for_message(&self) {
        self.wake.wait().await;
    }
}

pub struct SlicerWsHandler {
    proxy: &'static SlicerWsProxy,
}

impl ws::WebSocketCallback for SlicerWsHandler {
    async fn run<R: picoserve::io::Read, W: picoserve::io::Write<Error = R::Error>>(
        self,
        mut ws_rx: ws::SocketRx<R>,
        mut ws_tx: ws::SocketTx<W>,
    ) -> Result<(), W::Error> {
        let connection_id = self.proxy.mark_connected().await;
        let mut message_buffer = alloc::vec![0; SLICER_WS_MESSAGE_BUFFER_BYTES];
        let mut waiting_for_pong = false;
        let mut missed_heartbeats = 0usize;

        loop {
            if !self.proxy.is_active_connection(connection_id).await {
                info!("Slicer WebSocket connection superseded; closing old connection");
                ws_tx.close(Some((1000, "Superseded by another slicer WebSocket connection"))).await.ok();
                return Ok(());
            }

            if let Some(message) = self.proxy.pop_next_message(connection_id).await {
                if let Err(io_err) = ws_tx.send_text(&message).await {
                    error!("Error sending slicer WebSocket message: {io_err:?}");
                    self.proxy.mark_disconnected(connection_id).await;
                    return Err(io_err);
                }
                continue;
            }

            let wait_res = ws_rx
                .next_message(
                    &mut message_buffer,
                    select(self.proxy.wait_for_message(), Timer::after_millis(heartbeat_interval_ms())),
                )
                .await;

            match wait_res {
                Ok(picoserve::futures::Either::Second(embassy_futures::select::Either::First(_))) => {}
                Ok(picoserve::futures::Either::Second(embassy_futures::select::Either::Second(_))) => {
                    if waiting_for_pong {
                        missed_heartbeats += 1;
                        warn!(
                            "Slicer WebSocket missed heartbeat: missed={} max={SLICER_WS_MAX_MISSED_HEARTBEATS}",
                            missed_heartbeats
                        );
                        if missed_heartbeats >= SLICER_WS_MAX_MISSED_HEARTBEATS {
                            warn!("Slicer WebSocket heartbeat timed out; disconnecting");
                            ws_tx.close(Some((1001, "Slicer WebSocket heartbeat timeout"))).await.ok();
                            self.proxy.mark_disconnected(connection_id).await;
                            return Ok(());
                        }
                    }

                    if let Err(io_err) = ws_tx.send_ping(&Instant::now().as_ticks().to_le_bytes()).await {
                        error!("Error sending slicer WebSocket ping: {io_err:?}");
                        self.proxy.mark_disconnected(connection_id).await;
                        return Err(io_err);
                    }
                    waiting_for_pong = true;
                }
                Ok(picoserve::futures::Either::First(read_res)) => match read_res {
                    Ok(Message::Text(_)) => {}
                    Ok(Message::Binary(items)) => {
                        warn!("Received unsupported slicer WebSocket binary message: {items:?}");
                    }
                    Ok(Message::Close(reason)) => {
                        ws_tx.close(reason).await.ok();
                        self.proxy.mark_disconnected(connection_id).await;
                        return Ok(());
                    }
                    Ok(Message::Ping(items)) => {
                        if let Err(io_err) = ws_tx.send_pong(items).await {
                            error!("Error sending slicer WebSocket pong: {io_err:?}");
                            self.proxy.mark_disconnected(connection_id).await;
                            return Err(io_err);
                        }
                    }
                    Ok(Message::Pong(items)) => {
                        waiting_for_pong = false;
                        missed_heartbeats = 0;
                        let tick_res: Result<&[u8; 8], _> = items.try_into();
                        if tick_res.is_err() {
                            warn!("Slicer WebSocket received bad pong response: {items:?}");
                        }
                    }
                    Err(err) => {
                        warn!("Error reading slicer WebSocket message: {err:?}");
                        ws_tx.close(Some((err.code(), "WebSocket read error"))).await.ok();
                        self.proxy.mark_disconnected(connection_id).await;
                        return Ok(());
                    }
                },
                Err(io_err) => {
                    error!("IO error on slicer WebSocket: {io_err:?}");
                    self.proxy.mark_disconnected(connection_id).await;
                    return Err(io_err);
                }
            }
        }
    }
}
