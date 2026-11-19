use super::snapmaker_driver::U1Channel;
use super::{
    PrintState, PrinterChange, PrinterEvent, PrinterEventKind, PrinterFilament, PrinterFilamentInfo, PrinterId, PrinterObserver,
    PrinterSnapshotState, SlotState,
};
use alloc::{
    boxed::Box,
    format,
    rc::{Rc, Weak},
    string::ToString,
    vec::Vec,
};
use core::cell::RefCell;
use serde_json::{Map, Value};

pub(super) enum U1Message {
    Reset,
    Initial(Value),
    Update(Value),
}

pub(super) struct U1Runtime {
    pub id: PrinterId,
    pub state: PrinterSnapshotState,
    pub observers: RefCell<Vec<Weak<RefCell<dyn PrinterObserver>>>>,
}

impl U1Runtime {
    fn notify(&self, kind: PrinterEventKind) {
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

    fn apply(&self, data: &Value) {
        let mut snapshot = self.state.clone_snapshot();
        if let Some(group) = snapshot.slot_groups.first_mut() {
            for (index, slot) in group.slots.iter_mut().enumerate() {
                let key = format!("extruder{index}");
                let feeder = data
                    .get("filament_feed left")
                    .and_then(|v| v.get(&key))
                    .or_else(|| data.get("filament_feed right").and_then(|v| v.get(&key)));
                let inlet = feeder.and_then(|v| v.get("filament_detected")).and_then(Value::as_bool);
                let action = feeder.and_then(|v| v.get("channel_state")).and_then(Value::as_str);
                let sensor = data
                    .get(format!("filament_motion_sensor e{index}_filament"))
                    .and_then(|v| v.get("filament_detected"))
                    .and_then(Value::as_bool);
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
        if snapshot != self.state.clone_snapshot() {
            self.state.replace(snapshot.clone(), false);
            self.notify(PrinterEventKind::SnapshotChanged {
                change: PrinterChange::All,
                snapshot: Box::new(snapshot),
            });
        }
    }
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

pub(super) async fn snapmaker_u1_messages_task(runtime: Rc<U1Runtime>, channel: Rc<U1Channel>) {
    let mut current = Value::Object(Map::new());
    let mut pending = Value::Object(Map::new());
    let mut initialized = false;
    loop {
        match channel.receive().await {
            U1Message::Reset => {
                runtime.set_connected(false);
                current = Value::Object(Map::new());
                pending = Value::Object(Map::new());
                initialized = false;
            }
            U1Message::Initial(status) => {
                current = status;
                merge(&mut current, core::mem::replace(&mut pending, Value::Object(Map::new())));
                initialized = true;
                runtime.apply(&current);
                runtime.set_connected(true);
            }
            U1Message::Update(status) => {
                if initialized {
                    merge(&mut current, status);
                    runtime.apply(&current);
                } else {
                    merge(&mut pending, status);
                }
            }
        }
    }
}
