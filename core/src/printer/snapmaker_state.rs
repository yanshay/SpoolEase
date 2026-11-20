use super::snapmaker_driver::{U1Channel, U1CommandChannel};
use super::{
    MaterialSlotPresenceChange, MaterialSlotPresenceChangeKind, PrintState, PrinterChange, PrinterEvent, PrinterEventKind, PrinterFilament,
    PrinterFilamentInfo, PrinterId, PrinterObserver, PrinterSnapshotState, SlotState,
};
use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use framework::info;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub(super) enum U1Message {
    Reset,
    Initial(Value),
    Update(Value),
    Metadata(Value),
}

pub(super) struct U1Runtime {
    pub id: PrinterId,
    pub number: usize,
    pub state: PrinterSnapshotState,
    pub observers: RefCell<Vec<Weak<RefCell<dyn PrinterObserver>>>>,
    pub saved: RefCell<U1SavedState>,
    pub saved_dirty: Cell<bool>,
    pub pending_dirty: Cell<bool>,
    pub restore_finished: Signal<NoopRawMutex, ()>,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct U1SlotLoaded {
    feeder: bool,
    toolhead: bool,
    loaded: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct U1SavedState {
    slots: [U1SlotLoaded; 4],
}

impl U1Runtime {
    pub(super) fn notify(&self, kind: PrinterEventKind) {
        let event = PrinterEvent {
            printer_id: self.id.clone(),
            kind,
        };
        let observers = self.observers.borrow().clone();
        for observer in observers {
            if let Some(observer) = observer.upgrade() {
                observer.borrow_mut().on_printer_event(event.clone());
            }
        }
        self.observers.borrow_mut().retain(|observer| observer.strong_count() > 0);
    }

    fn set_connected(&self, connected: bool) {
        let changed = self.state.with_snapshot(|snapshot| snapshot.connected != connected);
        if changed {
            self.state.update(false, |snapshot| snapshot.connected = connected);
            self.notify(PrinterEventKind::ConnectivityChanged { connected });
        }
    }

    fn apply(
        &self,
        data: &Value,
        initial: bool,
        armed: &mut [bool; 4],
        previous_actions: &mut [Option<alloc::string::String>; 4],
        previous_operations: &mut [Option<alloc::string::String>; 4],
    ) {
        let mut snapshot = self.state.clone_snapshot();
        let mut changes = Vec::new();
        let mut saved = self.saved.borrow_mut();
        let mut loaded_changed = false;
        let mut spool_changed = false;
        if let Some(group) = snapshot.slot_groups.first_mut() {
            for (index, slot) in group.slots.iter_mut().enumerate() {
                let key = format!("extruder{index}");
                let feeder = data
                    .get("filament_feed left")
                    .and_then(|v| v.get(&key))
                    .or_else(|| data.get("filament_feed right").and_then(|v| v.get(&key)));
                let inlet = feeder.and_then(|v| v.get("filament_detected")).and_then(Value::as_bool);
                let action = feeder.and_then(|v| v.get("channel_state")).and_then(Value::as_str);
                let operation = feeder.and_then(|v| v.get("channel_action_state")).and_then(Value::as_str);
                let sensor = data
                    .get(format!("filament_motion_sensor e{index}_filament"))
                    .and_then(|v| v.get("filament_detected"))
                    .and_then(Value::as_bool);
                if let Some(inlet) = inlet {
                    let old = saved.slots[index];
                    let mut new = old;
                    let action_changed = action.is_some() && previous_actions[index].as_deref() != action;
                    let operation_changed = operation.is_some() && previous_operations[index].as_deref() != operation;
                    let completed = |value| matches!(value, Some("load_finish" | "manual_sta_finish" | "unload_finish"));
                    let completed_operation = if (initial || action_changed) && completed(action) {
                        action
                    } else if (initial || operation_changed) && completed(operation) {
                        operation
                    } else {
                        None
                    };
                    if !inlet {
                        new.feeder = false;
                    } else if action == Some("preload_finish") && (initial || action_changed) {
                        new.feeder = true;
                    }
                    let auto_feeder = inlet
                        && feeder.and_then(|v| v.get("module_exist")).and_then(Value::as_bool) == Some(true)
                        && feeder.and_then(|v| v.get("disable_auto")).and_then(Value::as_bool) == Some(false);
                    match completed_operation {
                        Some("load_finish" | "manual_sta_finish") => {
                            new.toolhead = true;
                            if auto_feeder {
                                new.feeder = true;
                            }
                        }
                        Some("unload_finish") => {
                            new.toolhead = false;
                            if auto_feeder && (initial || old.toolhead) {
                                new.feeder = true;
                            }
                        }
                        _ => {}
                    }
                    if initial && action == Some("preload_finish") && completed_operation.is_none() {
                        new.toolhead = false;
                    }
                    if !initial {
                        if let Some(completed_operation) = completed_operation {
                            info!("[{}] Snapmaker U1 slot {} operation {}", self.number, index + 1, completed_operation);
                        }
                        if action_changed && action == Some("preload_finish") && inlet {
                            info!("[{}] Snapmaker U1 slot {} pre-feeding completed", self.number, index + 1);
                        }
                    }
                    let was_loaded = old.loaded;
                    // On reconnect, current sensor readings can show a spool was removed while offline.
                    if initial && !inlet && sensor == Some(false) {
                        new.toolhead = false;
                    }
                    new.loaded = new.feeder || new.toolhead;
                    let is_loaded = new.loaded;
                    if initial {
                        armed[index] = !is_loaded && !loading_in_progress(action);
                        if !is_loaded && (!inlet && sensor == Some(false) || completed_operation == Some("unload_finish")) && slot.spool_id.is_some()
                        {
                            info!(
                                "[{}] Snapmaker U1 slot {} is empty after connection; clearing spool {}",
                                self.number,
                                index + 1,
                                slot.spool_id.as_deref().unwrap_or_default()
                            );
                            slot.clear_spool_id();
                            slot.consumed_since_load_g = 0.0;
                            slot.consumed_since_load_saved_g = 0.0;
                            slot.consumed_since_weight_g = 0.0;
                            spool_changed = true;
                        }
                    } else {
                        if was_loaded && !is_loaded {
                            let spool_id = slot.spool_id.clone();
                            info!("[{}] Snapmaker U1 slot {} filament removed, spool {:?}", self.number, index + 1, spool_id);
                            slot.clear_spool_id();
                            slot.consumed_since_load_g = 0.0;
                            slot.consumed_since_load_saved_g = 0.0;
                            slot.consumed_since_weight_g = 0.0;
                            spool_changed |= spool_id.is_some();
                            changes.push(MaterialSlotPresenceChange {
                                slot_id: slot.id.clone(),
                                change: MaterialSlotPresenceChangeKind::Removed,
                                spool_id,
                            });
                        } else if !was_loaded && is_loaded && armed[index] {
                            info!(
                                "[{}] Snapmaker U1 slot {} filament loaded ({})",
                                self.number,
                                index + 1,
                                if new.toolhead { "toolhead" } else { "feeder" }
                            );
                            changes.push(MaterialSlotPresenceChange {
                                slot_id: slot.id.clone(),
                                change: MaterialSlotPresenceChangeKind::Inserted,
                                spool_id: None,
                            });
                            armed[index] = false;
                        }
                        if !is_loaded && !loading_in_progress(action) {
                            armed[index] = true;
                        }
                    }
                    if old != new {
                        saved.slots[index] = new;
                        loaded_changed = true;
                    }
                    previous_actions[index] = action.map(ToString::to_string);
                    previous_operations[index] = operation.map(ToString::to_string);
                }
                let extruder = if index == 0 {
                    "extruder".to_string()
                } else {
                    format!("extruder{index}")
                };
                let tool = data.get(&extruder).and_then(|v| v.get("state")).and_then(Value::as_str);
                slot.state = match (sensor, tool, inlet, action) {
                    (Some(true), Some("ACTIVATE"), _, _) => SlotState::Loaded,
                    (Some(true), Some("PARKED"), _, _) => SlotState::InTool,
                    (Some(true), _, _, _) => SlotState::Unknown,
                    (Some(false), _, Some(false), _) => SlotState::Empty,
                    (Some(false), _, Some(true), Some("preload_finish")) => SlotState::Ready,
                    (Some(false), _, Some(true), Some("preload_prepare" | "preload_feeding")) => SlotState::Reading,
                    (Some(false), _, Some(true), _) => SlotState::Occupied,
                    _ => SlotState::Unknown,
                };
                let config = data.get("print_task_config");
                let field = |name: &str| {
                    config
                        .and_then(|v| v.get(name))
                        .and_then(|v| v.get(index))
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty() && *v != "NONE")
                };
                slot.filament = if let Some(material) = field("filament_type") {
                    let multi = config.and_then(|v| v.get("filament_color_multi")).and_then(|v| v.get(index));
                    let alpha = multi
                        .and_then(|v| v.get("alpha"))
                        .and_then(Value::as_u64)
                        .filter(|v| *v <= 255)
                        .unwrap_or(255);
                    let colors = multi
                        .and_then(|v| v.get("colors"))
                        .and_then(Value::as_array)
                        .map(|colors| {
                            colors
                                .iter()
                                .filter_map(Value::as_str)
                                .filter(|color| color.len() == 6 && color.bytes().all(|byte| byte.is_ascii_hexdigit()))
                                .map(|color| format!("{color}{alpha:02X}"))
                                .collect::<Vec<_>>()
                        })
                        .filter(|colors| !colors.is_empty())
                        .unwrap_or_else(|| {
                            field("filament_color_rgba")
                                .map(|color| alloc::vec![color.to_string()])
                                .unwrap_or_default()
                        });
                    PrinterFilament::Known(PrinterFilamentInfo {
                        material_type: material.into(),
                        material_subtype: field("filament_sub_type").unwrap_or_default().into(),
                        brand: field("filament_vendor").unwrap_or_default().into(),
                        color_codes: colors,
                        ..Default::default()
                    })
                } else {
                    PrinterFilament::Unknown
                };
            }
        }
        snapshot.print.state = match data.get("print_stats").and_then(|v| v.get("state")).and_then(Value::as_str) {
            Some("standby") => PrintState::Idle,
            Some("printing") => PrintState::Printing,
            Some("paused") => PrintState::Paused,
            Some("complete") => PrintState::Finished,
            Some("cancelled") => PrintState::Canceled,
            Some("error") => PrintState::Failed,
            _ => PrintState::Unknown,
        };
        let stats = data.get("print_stats");
        snapshot.print.job_name = stats
            .and_then(|v| v.get("filename"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string);
        let layer = |name: &str| {
            stats
                .and_then(|v| v.get("info"))
                .and_then(|v| v.get(name))
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
        };
        snapshot.print.current_layer = layer("current_layer");
        snapshot.print.total_layers = layer("total_layer");
        snapshot.print.progress_percent = None;
        snapshot.print.remaining_minutes = None;
        if matches!(snapshot.print.state, PrintState::Printing | PrintState::Paused) {
            let position = data.get("virtual_sdcard").and_then(|v| v.get("file_position")).and_then(Value::as_f64);
            let metadata = data.get("u1_print_metadata");
            let start = metadata.and_then(|v| v.get("gcode_start_byte")).and_then(Value::as_f64);
            let end = metadata.and_then(|v| v.get("gcode_end_byte")).and_then(Value::as_f64);
            if let (Some(position), Some(start), Some(end)) = (position, start, end) {
                if end > start {
                    let progress = ((position - start) / (end - start)).clamp(0.0, 1.0);
                    snapshot.print.progress_percent = Some((progress * 100.0 + 0.5) as u8);
                    if let Some(seconds) = metadata
                        .and_then(|v| v.get("estimated_time"))
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v > 0.0)
                    {
                        let minutes = seconds * (1.0 - progress) / 60.0;
                        snapshot.print.remaining_minutes = Some((minutes + 0.999_999) as u32);
                    }
                }
            }
        }
        if loaded_changed {
            self.saved_dirty.set(true);
        }
        drop(saved);
        if snapshot != self.state.clone_snapshot() {
            self.state.replace(snapshot.clone(), spool_changed);
            self.notify(PrinterEventKind::SnapshotChanged {
                change: PrinterChange::All,
                snapshot: Box::new(snapshot),
            });
        }
        if !changes.is_empty() {
            self.notify(PrinterEventKind::MaterialSlotPresenceChanged { changes });
        }
    }
}

fn loading_in_progress(action: Option<&str>) -> bool {
    matches!(
        action,
        Some(
            "preload_prepare"
                | "preload_feeding"
                | "load_prepare"
                | "load_homing"
                | "load_picking"
                | "load_heating"
                | "load_feeding"
                | "load_extruding"
                | "load_flushing"
                | "manual_sta_prepare"
                | "manual_sta_homing"
                | "manual_sta_picking"
                | "manual_sta_prepare_finish"
                | "manual_sta_heating"
                | "manual_sta_extruding"
                | "manual_sta_extrude_finish"
                | "manual_sta_flushing"
                | "manual_sta_flush_finish"
        )
    )
}

fn merge(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Object(dst), Value::Object(src)) => {
            for (key, value) in src {
                merge(dst.entry(key).or_insert(Value::Null), value);
            }
        }
        (dst, src) => *dst = src,
    }
}

fn request_print_metadata(current: &mut Value, requested: &mut Option<String>, commands: &U1CommandChannel) {
    let stats = current.get("print_stats");
    let active = matches!(stats.and_then(|v| v.get("state")).and_then(Value::as_str), Some("printing" | "paused"));
    let filename = stats
        .and_then(|v| v.get("filename"))
        .and_then(Value::as_str)
        .filter(|name| active && !name.is_empty())
        .map(ToString::to_string);
    if *requested != filename {
        if let Some(object) = current.as_object_mut() {
            object.remove("u1_print_metadata");
        }
        *requested = None;
        if let Some(filename) = filename {
            let request = json!({"jsonrpc":"2.0", "id":4, "method":"server.files.metadata", "params":{"filename":filename}}).to_string();
            if commands.try_send(Box::new(request)).is_ok() {
                *requested = Some(filename);
            }
        }
    }
}

pub(super) async fn snapmaker_u1_messages_task(runtime: Rc<U1Runtime>, channel: Rc<U1Channel>, commands: Rc<U1CommandChannel>) {
    runtime.restore_finished.wait().await;
    let mut current = Value::Object(Map::new());
    let mut pending = Value::Object(Map::new());
    let mut initialized = false;
    let mut armed = [false; 4];
    let mut previous_actions = [None, None, None, None];
    let mut previous_operations = [None, None, None, None];
    let mut requested_metadata = None;
    loop {
        match channel.receive().await {
            U1Message::Reset => {
                runtime.set_connected(false);
                current = Value::Object(Map::new());
                pending = Value::Object(Map::new());
                initialized = false;
                requested_metadata = None;
            }
            U1Message::Initial(status) => {
                current = status;
                merge(&mut current, core::mem::replace(&mut pending, Value::Object(Map::new())));
                initialized = true;
                request_print_metadata(&mut current, &mut requested_metadata, &commands);
                runtime.apply(&current, true, &mut armed, &mut previous_actions, &mut previous_operations);
                runtime.set_connected(true);
            }
            U1Message::Update(status) => {
                if initialized {
                    merge(&mut current, status);
                    request_print_metadata(&mut current, &mut requested_metadata, &commands);
                    runtime.apply(&current, false, &mut armed, &mut previous_actions, &mut previous_operations);
                } else {
                    merge(&mut pending, status);
                }
            }
            U1Message::Metadata(metadata) => {
                if initialized && requested_metadata.is_some() && metadata.get("filename").and_then(Value::as_str) == requested_metadata.as_deref() {
                    if let Some(object) = current.as_object_mut() {
                        object.insert("u1_print_metadata".into(), metadata);
                    }
                    runtime.apply(&current, false, &mut armed, &mut previous_actions, &mut previous_operations);
                }
            }
        }
    }
}
