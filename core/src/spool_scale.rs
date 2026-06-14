use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    cell::RefCell,
    net::{Ipv4Addr, SocketAddr},
    str::FromStr,
};
use edge_http::{
    io::client::Connection,
    ws::{MAX_BASE64_KEY_LEN, MAX_BASE64_KEY_RESPONSE_LEN, NONCE_LEN},
};
use edge_nal_embassy::{Tcp, TcpBuffers};
use edge_ws::{FrameHeader, FrameType};
use embassy_executor::Spawner;
use embassy_futures::select::select3;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;
use esp_hal::gpio::AnyPin;
use framework::{debug, error, framework_web_app::encrypt, info, mk_static, prelude::*, term_error, term_info, utils::random_u32, warn};
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use shared::load_cell::{LoadCell as LoadCellCore, LoadCellState, MIN_LOADED_WEIGHT};
use shared::scale::{ConsoleToScale, OtaProgressUpdate, ScaleToConsole};
// Legacy scale-side G-code analysis path is obsolete in this console version.
// Reference only; the shared protocol remains for older consoles and scale firmware.
// use shared::gcode_analysis_task::{FilamentUsage, GcodeAnalysisNotification, GcodeAnalysisRequest};

use crate::{
    app_config::{AppConfig, ScaleSourceConfig},
    hx711_gpio::Hx711Gpio,
    settings::SPOOL_SCALE_WS_CLIENT_BUFFER_BYTES,
    ssdp::SSDPPubSubChannel,
};

pub type ConsoleToScaleChannel = Channel<NoopRawMutex, ConsoleToScale, 5>;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScaleWeight {
    Unknown,
    Stable(i32),
    Unstable(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleSource {
    Local,
    Remote,
}

impl From<ScaleSourceConfig> for ScaleSource {
    fn from(value: ScaleSourceConfig) -> Self {
        match value {
            ScaleSourceConfig::Local => Self::Local,
            ScaleSourceConfig::Remote => Self::Remote,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScaleUiState {
    NotAvailable,
    Unknown,
    Uncalibrated,
    Disconnected,
    Connected,
    Empty,
    Loaded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleStatus {
    NotAvailable,
    Unknown,
    Uncalibrated,
    Disconnected,
    Connected,
    Empty,
    Loaded,
}

impl From<ScaleUiState> for ScaleStatus {
    fn from(value: ScaleUiState) -> Self {
        match value {
            ScaleUiState::NotAvailable => Self::NotAvailable,
            ScaleUiState::Unknown => Self::Unknown,
            ScaleUiState::Uncalibrated => Self::Uncalibrated,
            ScaleUiState::Disconnected => Self::Disconnected,
            ScaleUiState::Connected => Self::Connected,
            ScaleUiState::Empty => Self::Empty,
            ScaleUiState::Loaded => Self::Loaded,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveScaleState {
    pub source: Option<ScaleSource>,
    pub state: ScaleStatus,
    pub weight: ScaleWeight,
    pub raw_data: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SourceSnapshot {
    state: ScaleUiState,
    weight: ScaleWeight,
    raw_data: i32,
}

impl SourceSnapshot {
    fn new(configured: bool) -> Self {
        Self {
            state: if configured { ScaleUiState::Unknown } else { ScaleUiState::NotAvailable },
            weight: ScaleWeight::Unknown,
            raw_data: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EffectiveSnapshot {
    source: Option<ScaleSource>,
    snapshot: SourceSnapshot,
}

pub struct SpoolScale {
    pub weight: ScaleWeight,
    effective: EffectiveSnapshot,
    remote: SourceSnapshot,
    local: SourceSnapshot,
    preferred_source: ScaleSource,
    app_config: Rc<RefCell<AppConfig>>,
    local_load_cell: Option<Rc<RefCell<LoadCellCore>>>,
    local_tare_during_calibration: Option<i32>,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>>,
    console_to_scale: &'static ConsoleToScaleChannel,
    pub connected_scale: Option<(Option<String>, Ipv4Addr)>,
    pub available_scales: HashSet<(Option<String>, Ipv4Addr)>,
}

pub trait SpoolScaleObserver {
    fn on_scale_loaded(&mut self, source: ScaleSource, weight: i32, stable: bool);
    fn on_scale_load_changed_stable(&mut self, source: ScaleSource, weight: i32);
    fn on_scale_load_changed_unstable(&mut self, source: ScaleSource, weight: i32);
    fn on_scale_load_removed(&mut self, source: ScaleSource);
    fn on_scale_raw_samples_avg(&mut self, source: ScaleSource, raw_data: i32);
    fn on_scale_connected(&mut self, source: ScaleSource);
    fn on_scale_disconnected(&mut self, source: ScaleSource);
    fn on_scale_uncalibrated(&mut self, source: ScaleSource);
    fn on_term_text(&mut self, text: &str);
    fn on_tag_status(&mut self, status: &shared::spool_tag::Status);
    fn on_pn532_status(&mut self, status: bool);
    fn on_button_pressed(&mut self, scale_weight: ScaleWeight) -> Option<bool>;
    // Legacy scale-side G-code analysis observer callbacks:
    // fn on_gcode_analysis(&mut self, job_number: i32, printer_index: usize, gcode_analysis: FilamentUsage);
    // fn on_gcode_analysis_failed(&mut self, job_number: i32, printer_index: usize);
    // fn on_gcode_analysis_canceled(&mut self, job_number: i32, printer_index: usize);
    // fn on_gcode_analysis_completed(&mut self, job_number: i32, printer_index: usize);
    fn on_scale_version(&mut self, scale_version: &str);
    fn on_ota_progress_update(&mut self, update: OtaProgressUpdate);
}

impl SpoolScale {
    // Notifications from Console to Scale  ////////////////////////

    pub fn calibrate(&mut self, source: ScaleSource, weight: i32) {
        match source {
            ScaleSource::Remote => {
                self.console_to_scale
                    .try_send(ConsoleToScale::Calibrate(weight))
                    .unwrap_or_else(|e| error!("Failed sending calibrate request to scale {e:?}"));
            }
            ScaleSource::Local => self.calibrate_local(weight),
        }
    }

    fn calibrate_local(&mut self, weight: i32) {
        let Some(load_cell) = self.local_load_cell.clone() else {
            error!("Local scale calibration requested but local load-cell is not initialized");
            return;
        };

        if weight == 0 {
            let read_uncalibrated = load_cell.borrow().immediate_read_uncalibrated();
            self.local_tare_during_calibration = Some(read_uncalibrated);
            debug!("Set local scale tare, sample: {read_uncalibrated}");
            return;
        }

        let Some(tare_during_calibration) = self.local_tare_during_calibration else {
            error!("Local scale calibration performed w/o first setting tare");
            return;
        };

        let weight_loadcell = load_cell.borrow().immediate_read_uncalibrated();
        info!(
            "Local calibration info: zero_loadcell: {:?}, weight: {}, weight_loadcell:{}",
            tare_during_calibration, weight, weight_loadcell
        );
        load_cell.borrow_mut().set_calibration(tare_during_calibration, weight, weight_loadcell);
        self.app_config
            .borrow_mut()
            .set_local_scale_calibration_config(tare_during_calibration, weight, weight_loadcell)
            .unwrap_or_else(|e| error!("Error storing local scale calibration {e:?}"));
        self.local_tare_during_calibration = None;
        let read_weight = load_cell.borrow().immediate_read();
        self.local.state = ScaleUiState::Loaded;
        self.local.weight = ScaleWeight::Unstable(read_weight);
        self.publish_effective_state();
    }

    pub fn button_response(&self, success: bool) {
        self.console_to_scale
            .try_send(ConsoleToScale::ButtonResponse(success))
            .unwrap_or_else(|e| error!("Failed sending button response request to scale {e:?}"));
    }

    // Legacy console-to-scale G-code analysis request:
    // #[allow(dead_code)]
    // pub fn request_gcode_analysis(&self, gcode_analysis_request: GcodeAnalysisRequest) -> Result<(), String> {
    //     if let Err(err) = self
    //         .console_to_scale
    //         .try_send(ConsoleToScale::RequestGcodeAnalysis { gcode_analysis_request })
    //     {
    //         Err(format!("Failed sending request_gcode_analysis to scale {err:?}"))
    //     } else {
    //         Ok(())
    //     }
    // }

    pub fn read_tag(&self) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::ReadTag) {
            Err(format!("Failed sending read_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn write_tag(&self, text: &str, check_uid: Option<Vec<Vec<u8>>>, cookie: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::WriteTag {
            text: text.to_string(),
            check_uid,
            cookie,
        }) {
            Err(format!("Failed sending write_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn erase_tag(&self, check_uid: Option<Vec<u8>>, cookie: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::EraseTag { check_uid, cookie }) {
            Err(format!("Failed sending erase_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }
    #[allow(dead_code)]
    pub fn emulate_tag(&self, url: &str) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::EmulateTag { url: url.to_string() }) {
            Err(format!("Failed sending emulate_tag to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    // Legacy console-to-scale G-code analysis notification:
    // #[allow(dead_code)]
    // pub fn gcode_analysis_notify(&self, gcode_analysis_notification: GcodeAnalysisNotification) -> Result<(), String> {
    //     if let Err(err) = self
    //         .console_to_scale
    //         .try_send(ConsoleToScale::GcodeAnalysisNotify { gcode_analysis_notification })
    //     {
    //         Err(format!("Failed sending gcode_analysis_notify to scale {err:?}"))
    //     } else {
    //         Ok(())
    //     }
    // }

    pub fn update_firmware(&self, ota_domain: &str, ota_path: &str, ota_toml_filename: &str, ota_cert: &str) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::UpdateFirmware {
            ota_domain: ota_domain.to_string(),
            ota_path: ota_path.to_string(),
            ota_toml_filename: ota_toml_filename.to_string(),
            ota_cert: ota_cert.to_string(),
        }) {
            Err(format!("Failed sending update_firmware to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    pub fn tags_in_store(&self, tags_in_store: String) -> Result<(), String> {
        if let Err(err) = self.console_to_scale.try_send(ConsoleToScale::TagsInStore { tags: tags_in_store }) {
            Err(format!("Failed sending tags_in_store to scale {err:?}"))
        } else {
            Ok(())
        }
    }

    // Technical Stuff  ////////////////////////

    fn snapshot_for(&self, source: ScaleSource) -> SourceSnapshot {
        match source {
            ScaleSource::Local => self.local,
            ScaleSource::Remote => self.remote,
        }
    }

    fn snapshot_for_mut(&mut self, source: ScaleSource) -> &mut SourceSnapshot {
        match source {
            ScaleSource::Local => &mut self.local,
            ScaleSource::Remote => &mut self.remote,
        }
    }

    fn other_source(source: ScaleSource) -> ScaleSource {
        match source {
            ScaleSource::Local => ScaleSource::Remote,
            ScaleSource::Remote => ScaleSource::Local,
        }
    }

    fn source_is_configured(&self, source: ScaleSource) -> bool {
        self.snapshot_for(source).state != ScaleUiState::NotAvailable
    }

    fn compute_effective(&self) -> EffectiveSnapshot {
        let local_loaded = self.local.state == ScaleUiState::Loaded;
        let remote_loaded = self.remote.state == ScaleUiState::Loaded;
        let source = if local_loaded && remote_loaded {
            self.preferred_source
        } else if local_loaded {
            ScaleSource::Local
        } else if remote_loaded {
            ScaleSource::Remote
        } else {
            let local_disconnected = self.source_is_configured(ScaleSource::Local) && self.local.state == ScaleUiState::Disconnected;
            let remote_disconnected = self.source_is_configured(ScaleSource::Remote) && self.remote.state == ScaleUiState::Disconnected;
            if local_disconnected && remote_disconnected && self.source_is_configured(self.preferred_source) {
                self.preferred_source
            } else if local_disconnected && remote_disconnected {
                Self::other_source(self.preferred_source)
            } else if local_disconnected {
                ScaleSource::Local
            } else if remote_disconnected {
                ScaleSource::Remote
            } else if self.source_is_configured(self.preferred_source) {
                self.preferred_source
            } else {
                Self::other_source(self.preferred_source)
            }
        };

        if self.source_is_configured(source) {
            EffectiveSnapshot {
                source: Some(source),
                snapshot: self.snapshot_for(source),
            }
        } else {
            EffectiveSnapshot {
                source: None,
                snapshot: SourceSnapshot::new(false),
            }
        }
    }

    fn publish_effective_state(&mut self) {
        let previous = self.effective;
        let next = self.compute_effective();
        self.effective = next;
        self.weight = next.snapshot.weight;

        if previous == next {
            return;
        }

        let Some(source) = next.source else {
            return;
        };

        match next.snapshot.state {
            ScaleUiState::NotAvailable | ScaleUiState::Unknown => (),
            ScaleUiState::Connected => self.notify_scale_connected(source),
            ScaleUiState::Disconnected => self.notify_scale_disconnected(source),
            ScaleUiState::Uncalibrated => self.notify_scale_uncalibrated(source),
            ScaleUiState::Empty => {
                if !matches!(previous.snapshot.state, ScaleUiState::Unknown | ScaleUiState::NotAvailable) {
                    self.notify_scale_load_removed(source);
                }
            }
            ScaleUiState::Loaded => {
                let (weight, stable) = match next.snapshot.weight {
                    ScaleWeight::Stable(weight) => (weight, true),
                    ScaleWeight::Unstable(weight) => (weight, false),
                    ScaleWeight::Unknown => (0, false),
                };
                if previous.snapshot.state != ScaleUiState::Loaded || previous.source != next.source {
                    self.notify_scale_loaded(source, weight, stable);
                } else {
                    match next.snapshot.weight {
                        ScaleWeight::Stable(weight) => self.notify_scale_load_changed_stable(source, weight),
                        ScaleWeight::Unstable(weight) => self.notify_scale_load_changed_unstable(source, weight),
                        ScaleWeight::Unknown => (),
                    }
                }
            }
        }
    }

    pub fn effective_state(&self) -> EffectiveScaleState {
        let effective = self.compute_effective();
        EffectiveScaleState {
            source: effective.source,
            state: effective.snapshot.state.into(),
            weight: effective.snapshot.weight,
            raw_data: effective.snapshot.raw_data,
        }
    }

    pub fn source_state(&self, source: ScaleSource) -> EffectiveScaleState {
        let snapshot = self.snapshot_for(source);
        EffectiveScaleState {
            source: if self.source_is_configured(source) { Some(source) } else { None },
            state: snapshot.state.into(),
            weight: snapshot.weight,
            raw_data: snapshot.raw_data,
        }
    }

    fn set_source_connected(&mut self, source: ScaleSource) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Connected;
        self.publish_effective_state();
    }

    fn set_source_disconnected(&mut self, source: ScaleSource) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Disconnected;
        snapshot.weight = ScaleWeight::Unknown;
        self.publish_effective_state();
    }

    fn set_source_uncalibrated(&mut self, source: ScaleSource) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Uncalibrated;
        snapshot.weight = ScaleWeight::Unknown;
        self.publish_effective_state();
    }

    fn set_source_raw_samples_avg(&mut self, source: ScaleSource, raw_data: i32) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.raw_data = raw_data;
        if self.compute_effective().source == Some(source) {
            self.notify_scale_raw_samples_avg(source, raw_data);
        }
    }

    fn set_source_loaded(&mut self, source: ScaleSource, weight: i32) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Loaded;
        snapshot.weight = ScaleWeight::Unstable(weight);
        self.publish_effective_state();
    }

    fn set_source_load_changed_unstable(&mut self, source: ScaleSource, weight: i32) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Loaded;
        snapshot.weight = ScaleWeight::Unstable(weight);
        self.publish_effective_state();
    }

    fn set_source_load_changed_stable(&mut self, source: ScaleSource, weight: i32) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Loaded;
        snapshot.weight = ScaleWeight::Stable(weight);
        self.publish_effective_state();
    }

    fn set_source_load_removed(&mut self, source: ScaleSource) {
        let snapshot = self.snapshot_for_mut(source);
        snapshot.state = ScaleUiState::Empty;
        snapshot.weight = ScaleWeight::Stable(0);
        self.publish_effective_state();
    }

    pub fn process_message(&mut self, _frame_header: &FrameHeader, payload: &[u8]) {
        let parse_res = serde_json::from_slice::<ScaleToConsole>(payload);
        if let Ok(scale_to_console) = parse_res {
            match scale_to_console {
                ScaleToConsole::NewLoad(weight) => {
                    self.set_source_loaded(ScaleSource::Remote, weight);
                }
                ScaleToConsole::LoadChangedUnstable(weight) => {
                    self.set_source_load_changed_unstable(ScaleSource::Remote, weight);
                }
                ScaleToConsole::LoadChangedStable(weight) => {
                    self.set_source_load_changed_stable(ScaleSource::Remote, weight);
                }
                ScaleToConsole::LoadRemoved => {
                    self.set_source_load_removed(ScaleSource::Remote);
                }
                ScaleToConsole::RawSamplesAvg(raw_data) => {
                    self.set_source_raw_samples_avg(ScaleSource::Remote, raw_data);
                }
                ScaleToConsole::Uncalibrated => {
                    self.set_source_uncalibrated(ScaleSource::Remote);
                }
                ScaleToConsole::Term(text) => {
                    self.notify_term_text(&text);
                }
                ScaleToConsole::TagStatus(status) => {
                    self.notify_tag_status(&status);
                }
                ScaleToConsole::PN532Status(status) => {
                    self.notify_pn532_status(status);
                }
                ScaleToConsole::ButtonPressed => {
                    self.notify_button_pressed();
                }
                // Legacy scale-to-console G-code analysis routing:
                // ScaleToConsole::GcodeAnalysis {
                //     job_number,
                //     printer_index,
                //     filament_usage_csv,
                // } => {
                //     self.notify_gcode_analysis(job_number, printer_index, filament_usage_csv);
                // }
                // ScaleToConsole::GcodeAnalysisFailed { job_number, printer_index } => {
                //     self.notify_gcode_analysis_failed(job_number, printer_index);
                // }
                // ScaleToConsole::GcodeAnalysisCanceled { job_number, printer_index } => {
                //     self.notify_gcode_analysis_canceled(job_number, printer_index);
                // }
                // ScaleToConsole::GcodeAnalysisCompleted { job_number, printer_index } => {
                //     self.notify_gcode_analysis_completed(job_number, printer_index);
                // }
                ScaleToConsole::ScaleVersion { version } => {
                    self.notify_scale_version(&version);
                }
                ScaleToConsole::OtaProgressUpdate(update) => self.notify_ota_progress_update(&update),
                _ => {}
            }
        } else {
            warn!(
                "Received an unsupported message from Scale, Console version update probably available : {}",
                String::from_utf8_lossy(payload)
            );
        }
    }
    pub fn connected(&self) {
        // don't change to &mut, if changed will panic on borrow since during connect notification sending data back to scale that needs borrow
        // one solution is to pass reference to self to the object being notified so it can use it instead of borrowing (maybe possible)
        if self.preferred_source == ScaleSource::Remote || !self.source_is_configured(ScaleSource::Local) {
            self.notify_scale_connected(ScaleSource::Remote);
        }
    }
    pub fn mark_remote_connected(&mut self) {
        self.remote.state = ScaleUiState::Connected;
        self.effective = self.compute_effective();
        self.weight = self.effective.snapshot.weight;
    }
    pub fn disconnected(&mut self) {
        self.set_source_disconnected(ScaleSource::Remote);
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>) {
        self.observers.push(observer);
    }

    // Notifications from Scale to Console  ////////////////////////

    pub fn notify_scale_loaded(&self, source: ScaleSource, weight: i32, stable: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_loaded(source, weight, stable);
        }
    }
    pub fn notify_scale_load_changed_stable(&self, source: ScaleSource, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_stable(source, weight);
        }
    }
    pub fn notify_scale_load_changed_unstable(&self, source: ScaleSource, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_unstable(source, weight);
        }
    }
    pub fn notify_scale_load_removed(&self, source: ScaleSource) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_removed(source);
        }
    }
    pub fn notify_scale_raw_samples_avg(&self, source: ScaleSource, raw_data: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_raw_samples_avg(source, raw_data);
        }
    }
    pub fn notify_scale_connected(&self, source: ScaleSource) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_connected(source);
        }
    }
    pub fn notify_scale_disconnected(&self, source: ScaleSource) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_disconnected(source);
        }
    }
    pub fn notify_scale_uncalibrated(&self, source: ScaleSource) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_uncalibrated(source);
        }
    }
    pub fn notify_term_text(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_term_text(text);
        }
    }
    pub fn notify_tag_status(&mut self, status: &shared::spool_tag::Status) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_status(status);
        }
    }
    pub fn notify_pn532_status(&mut self, status: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_pn532_status(status);
        }
    }
    pub fn notify_button_pressed(&mut self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            let observer_immediate_response = observer.borrow_mut().on_button_pressed(self.weight);
            if let Some(success) = observer_immediate_response {
                self.button_response(success);
            }
        }
    }

    // Legacy scale-to-console G-code analysis observer fan-out:
    // pub fn notify_gcode_analysis(&mut self, job_number: i32, printer_index: usize, filament_usage_csv: String) {
    //     // Optimized to create only as many clones as required (in case of several observers)
    //     if self.observers.is_empty() {
    //         return;
    //     }
    //     // let num_records = filament_usage_csv.lines().count();
    //     // let mut data = Vec::<FilamentUsageEntry>::with_capacity(num_records);
    //     // let mut csv_parser = serde_csv_core::Reader::<16>::new(); // 16 is max field size
    //     // for line in filament_usage_csv.lines() {
    //     //     match csv_parser.deserialize(line.as_bytes()) {
    //     //         Ok(v) => {
    //     //             data.push(v.0);
    //     //         }
    //     //         Err(err) => {
    //     //             error!("Internal error deserializing FilamentUsageEntry : {err}");
    //     //             return;
    //     //         }
    //     //     }
    //     // }
    //     let filament_usage = match FilamentUsage::from_csv(&filament_usage_csv) {
    //         Ok(v) => v,
    //         Err(err) => {
    //             error!("Internal error deserializing FilamentUsageEntry : {err}");
    //             return;
    //         }
    //     };
    //
    //     if let Some((last, rest)) = self.observers.split_last() {
    //         for weak_observer in rest.iter() {
    //             let observer = weak_observer.upgrade().unwrap();
    //             observer.borrow_mut().on_gcode_analysis(job_number, printer_index, filament_usage.clone());
    //         }
    //         let observer = last.upgrade().unwrap();
    //         observer.borrow_mut().on_gcode_analysis(job_number, printer_index, filament_usage);
    //     }
    // }
    // pub fn notify_gcode_analysis_failed(&self, job_number: i32, printer_index: usize) {
    //     for weak_observer in self.observers.iter() {
    //         let observer = weak_observer.upgrade().unwrap();
    //         observer.borrow_mut().on_gcode_analysis_failed(job_number, printer_index);
    //     }
    // }
    // pub fn notify_gcode_analysis_canceled(&self, job_number: i32, printer_index: usize) {
    //     for weak_observer in self.observers.iter() {
    //         let observer = weak_observer.upgrade().unwrap();
    //         observer.borrow_mut().on_gcode_analysis_canceled(job_number, printer_index);
    //     }
    // }
    // pub fn notify_gcode_analysis_completed(&self, job_number: i32, printer_index: usize) {
    //     for weak_observer in self.observers.iter() {
    //         let observer = weak_observer.upgrade().unwrap();
    //         observer.borrow_mut().on_gcode_analysis_completed(job_number, printer_index);
    //     }
    // }

    pub fn notify_scale_version(&self, scale_version: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_version(scale_version);
        }
    }
    pub fn notify_ota_progress_update(&self, update: &OtaProgressUpdate) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_progress_update(update.clone());
        }
    }
}

pub fn init(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    stack: Stack<'static>,
    spawner: Spawner,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
    local_hx711_sck: Option<AnyPin<'static>>,
    local_hx711_dt: Option<AnyPin<'static>>,
) -> Rc<RefCell<SpoolScale>> {
    let console_to_scale = mk_static!(ConsoleToScaleChannel, ConsoleToScaleChannel::new());
    let remote_available = app_config.borrow().remote_scale_available();
    let local_available = app_config.borrow().local_scale_available();
    let preferred_source = ScaleSource::from(app_config.borrow().preferred_scale_source());

    let local_load_cell = if local_available {
        Some(LoadCellCore::new(10, Duration::from_millis(100)))
    } else {
        None
    };
    let local_calibration = app_config.borrow().configured_local_scale_calibration.clone();

    if let Some(load_cell) = &local_load_cell
        && let Some(calibration) = &local_calibration
    {
        load_cell.borrow_mut().set_calibration_config(calibration);
    }

    let mut local = SourceSnapshot::new(local_available);
    if local_available && local_calibration.is_none() {
        local.state = ScaleUiState::Uncalibrated;
    }
    if local_available && (local_hx711_sck.is_none() || local_hx711_dt.is_none()) {
        local.state = ScaleUiState::Disconnected;
    }

    let spool_scale_rc = Rc::new(RefCell::new(SpoolScale {
        weight: ScaleWeight::Unknown,
        effective: EffectiveSnapshot {
            source: None,
            snapshot: SourceSnapshot::new(false),
        },
        remote: SourceSnapshot::new(remote_available),
        local,
        preferred_source,
        app_config: app_config.clone(),
        local_load_cell: local_load_cell.clone(),
        local_tare_during_calibration: None,
        observers: Vec::new(),
        console_to_scale,
        connected_scale: None,
        available_scales: HashSet::new(),
    }));

    spawner.spawn_heap(monitor_scales_task(spool_scale_rc.clone(), ssdp_pub_sub)).ok();

    if remote_available {
        spawner
            .spawn_heap(spool_scale_task(framework, app_config, stack, spool_scale_rc.clone(), ssdp_pub_sub))
            .ok();
    }

    if let (Some(load_cell), Some(sck), Some(dt)) = (local_load_cell, local_hx711_sck, local_hx711_dt) {
        spawner
            .spawn_heap(local_hx711_sample_task(spool_scale_rc.clone(), load_cell.clone(), sck, dt))
            .ok();
        spawner.spawn_heap(local_scale_monitor_task(spool_scale_rc.clone(), load_cell)).ok();
    }

    spool_scale_rc
}

pub async fn local_hx711_sample_task(
    spool_scale_rc: Rc<RefCell<SpoolScale>>,
    load_cell: Rc<RefCell<LoadCellCore>>,
    sck: AnyPin<'static>,
    dt: AnyPin<'static>,
) {
    info!("Task local_hx711_sample_task started");
    let mut hx711 = Hx711Gpio::new(sck, dt);
    hx711.reset();
    Timer::after_millis(500).await;

    let duration_between_samples = Duration::from_millis(100);

    // Skip first readings which are commonly 0 / -1 after reset or power-up.
    let mut count_good_samples = 0;
    loop {
        if let Some(v) = hx711.read_raw() {
            if !([0, -1].contains(&v)) {
                count_good_samples += 1;
            }
            if count_good_samples >= 5 {
                break;
            }
            Timer::after(duration_between_samples).await;
        } else {
            Timer::after_millis(5).await;
        }
    }

    if spool_scale_rc.borrow().local.state == ScaleUiState::Uncalibrated {
        spool_scale_rc.borrow_mut().set_source_uncalibrated(ScaleSource::Local);
    } else {
        spool_scale_rc.borrow_mut().set_source_connected(ScaleSource::Local);
    }

    Timer::after(duration_between_samples).await;

    loop {
        if let Some(v) = hx711.read_raw() {
            if v != -1 {
                load_cell.borrow_mut().add_sample(v);
            }
            Timer::after(duration_between_samples).await;
        } else {
            Timer::after_millis(5).await;
        }
    }
}

pub async fn local_scale_monitor_task(spool_scale_rc: Rc<RefCell<SpoolScale>>, load_cell: Rc<RefCell<LoadCellCore>>) {
    info!("Task local_scale_monitor_task started");
    let load_cell_reader = load_cell.borrow().reader();
    let mut load_cell_state = if spool_scale_rc.borrow().local.state == ScaleUiState::Uncalibrated {
        LoadCellState::Uncalibrated
    } else {
        LoadCellState::Unknown
    };

    loop {
        if matches!(load_cell_state, LoadCellState::Uncalibrated) && spool_scale_rc.borrow().local.state != ScaleUiState::Uncalibrated {
            load_cell_state = LoadCellState::Unknown;
        }

        match load_cell_state {
            LoadCellState::Uncalibrated => {
                let uncalibrated = load_cell_reader.immediate_read_uncalibrated();
                spool_scale_rc.borrow_mut().set_source_raw_samples_avg(ScaleSource::Local, uncalibrated);
                Timer::after_millis(1000).await;
            }
            LoadCellState::Unknown => {
                let unstable_read = load_cell_reader.read_stable().await;
                if unstable_read > MIN_LOADED_WEIGHT {
                    spool_scale_rc.borrow_mut().set_source_loaded(ScaleSource::Local, unstable_read);
                    load_cell_state = LoadCellState::Loaded(0, unstable_read);
                } else {
                    load_cell_state = LoadCellState::Empty;
                    spool_scale_rc.borrow_mut().set_source_load_removed(ScaleSource::Local);
                }
            }
            LoadCellState::Empty => {
                let unstable_read = load_cell_reader.read_changed(0).await;
                if unstable_read > MIN_LOADED_WEIGHT {
                    spool_scale_rc.borrow_mut().set_source_loaded(ScaleSource::Local, unstable_read);
                    // this is an unstable read, so updating only that
                    load_cell_state = LoadCellState::Loaded(0, unstable_read);
                }
                Timer::after_millis(250).await;
            }
            LoadCellState::Loaded(last_stable_read, last_unstable_read) => {
                let read_res = with_timeout(Duration::from_millis(500), load_cell_reader.read_stable()).await;
                match read_res {
                    Ok(new_stable_read) => {
                        if new_stable_read < 0 {
                            LoadCellCore::tare(&load_cell).await;
                        } else if new_stable_read.abs() < MIN_LOADED_WEIGHT {
                            spool_scale_rc.borrow_mut().set_source_load_removed(ScaleSource::Local);
                            load_cell_state = LoadCellState::Empty;
                        } else if new_stable_read != last_stable_read || new_stable_read != last_unstable_read {
                            spool_scale_rc
                                .borrow_mut()
                                .set_source_load_changed_stable(ScaleSource::Local, new_stable_read);
                            load_cell_state = LoadCellState::Loaded(new_stable_read, new_stable_read)
                        }
                    }
                    Err(_timeout_err) => {
                        if let Some(new_unstable_read) = load_cell_reader.try_read_changed(last_unstable_read)
                            && new_unstable_read.abs() >= MIN_LOADED_WEIGHT
                        {
                            spool_scale_rc
                                .borrow_mut()
                                .set_source_load_changed_unstable(ScaleSource::Local, new_unstable_read);
                            load_cell_state = LoadCellState::Loaded(last_stable_read, new_unstable_read);
                        }
                    }
                }
                Timer::after_millis(500).await;
            }
        }
    }
}

// #[embassy_executor::task]
pub async fn monitor_scales_task(spool_scale_rc: Rc<RefCell<SpoolScale>>, ssdp_pub_sub: &'static SSDPPubSubChannel) {
    let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
    loop {
        let ssdp_info = ssdp_subscribe.next_message().await;
        match ssdp_info {
            embassy_sync::pubsub::WaitResult::Lagged(_) => (),
            embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale")
                    && let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location)
                {
                    let spoolscale_ip = found_ip;
                    let spoolscale_name = Some(ssdp_info.usn);
                    spool_scale_rc.borrow_mut().available_scales.insert((spoolscale_name, spoolscale_ip));
                }
            }
        }
    }
}

// #[embassy_executor::task]
pub async fn spool_scale_task(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    stack: Stack<'static>,
    spool_scale_rc: Rc<RefCell<SpoolScale>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    info!("Task spool_scale_task started");
    let console_to_scale = spool_scale_rc.borrow().console_to_scale;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after_millis(250).await;
    }

    let mut configured_ip = None;
    let mut configured_name = None;
    let spoolscale_ip;
    let mut spoolscale_name = None;

    if let Some(configured_scale) = &app_config.borrow().configured_scale {
        configured_ip = configured_scale.ip;
        configured_name = configured_scale.name.clone();
        spoolscale_name = configured_scale.name.clone();
    }

    if let Some(configured_ip) = configured_ip {
        spoolscale_ip = configured_ip
    } else {
        term_info!(
            "No SpoolScale IP configured, discovering {}",
            configured_name.as_ref().unwrap_or(&"".to_string())
        );
        let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
        loop {
            let ssdp_info = ssdp_subscribe.next_message().await;
            match ssdp_info {
                embassy_sync::pubsub::WaitResult::Lagged(_) => (),
                embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                    if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale") {
                        if let Some(spoolscale_name) = &configured_name
                            && ssdp_info.usn != *spoolscale_name
                        {
                            debug!("Found a SpoolScale, but with name {} and not {spoolscale_name}", ssdp_info.usn);
                            continue;
                        }
                        if let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location) {
                            spoolscale_ip = found_ip;
                            spoolscale_name = Some(ssdp_info.usn);
                            term_info!("Discovered SpoolScale at {}", spoolscale_ip);
                            break;
                        }
                    }
                }
            }
        }
    }

    spool_scale_rc.borrow_mut().connected_scale = Some((spoolscale_name.clone(), spoolscale_ip));

    let tcp_buffers = Box::new(TcpBuffers::<1, 1024, 1024>::new());
    let tcp = Tcp::new(stack, &*tcp_buffers);
    let tcp = edge_nal::WithTimeout::new(15000, tcp);

    let mut first_connect = true;
    let mut connect_error_counter = 0;
    let mut conn_buf = alloc::vec![0_u8; SPOOL_SCALE_WS_CLIENT_BUFFER_BYTES]; // TODO: large size for gcode_analysis (not required that long any longer since g-code analysis not using scale, don't remove comment)
    'connect_loop: loop {
        Framework::wait_for_wifi(&framework).await;
        if first_connect {
            first_connect = false;
        } else {
            Timer::after_secs(2).await;
        }
        let mut conn: Connection<_> = Connection::new(&mut conn_buf, &tcp, SocketAddr::new(core::net::IpAddr::V4(spoolscale_ip), 81));

        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).unwrap();
        let mut nonce_base64_buf = [0_u8; MAX_BASE64_KEY_LEN];
        if connect_error_counter % 10 == 0 {
            term_info!("Connecting to SpoolScale at {}", spoolscale_ip);
        }
        if let Err(err) = conn
            .initiate_ws_upgrade_request(Some(&spoolscale_ip.to_string()), None, "/ws", None, &nonce, &mut nonce_base64_buf)
            .await
        {
            if connect_error_counter % 10 == 0 && connect_error_counter != 0 {
                term_error!("SpoolScale: Error connecting to Scale ({:?})", err);
            }
            connect_error_counter += 1;
            continue 'connect_loop;
        }
        if let Err(err) = conn.initiate_response().await {
            term_error!("SpoolScale: Error initiating web socket response ({:?})", err);
            continue 'connect_loop;
        }

        let mut buf = [0_u8; MAX_BASE64_KEY_RESPONSE_LEN];
        let upgrade_accepted_res = conn.is_ws_upgrade_accepted(&nonce, &mut buf);
        match upgrade_accepted_res {
            Ok(true) => (),
            Ok(false) => {
                term_error!("SpoolScale: Upgrading to websocket rejected");
                continue 'connect_loop;
            }
            Err(err) => {
                term_error!("SpoolScale: Error during websocket upgrade {:?}", err);
                continue 'connect_loop;
            }
        }

        if let Err(err) = conn.complete().await {
            error!("SpoolScale: Error completing the connection {:?}", err);
            return;
        }

        // Now we have the TCP socket in a state where it can be operated as a WS connection
        // Send some traffic to a WS echo server and read it back

        let (mut socket, buf) = conn.release();

        connect_error_counter = 0;

        term_info!("Connection with SpoolScale established");

        spool_scale_rc.borrow_mut().mark_remote_connected();
        spool_scale_rc.borrow().connected();

        'send_recv_loop: loop {
            // max timeout_for_ping need to be less than above WithTimeout wrapper
            let timeout_for_ping = 12000 + (random_u32() % 2000);
            let with_timeout_res = select3(
                Timer::after_millis(timeout_for_ping as u64),
                FrameHeader::recv(&mut socket),
                console_to_scale.receive(),
            )
            .await;
            match with_timeout_res {
                embassy_futures::select::Either3::First(_timeout_res) => {
                    // Sending Ping on timeout
                    let now = Instant::now().as_ticks();
                    let ping_header = FrameHeader {
                        frame_type: FrameType::Ping,
                        payload_len: 8,
                        mask_key: None,
                    };
                    let send_ping_header_res = ping_header.send(&mut socket).await;
                    match send_ping_header_res {
                        Ok(_) => {
                            let send_ping_payload_res = ping_header.send_payload(&mut socket, &now.to_le_bytes()).await;
                            match send_ping_payload_res {
                                Ok(_) => {
                                    let res = socket.flush().await;
                                    match res {
                                        Ok(_) => {
                                            debug!("SpoolScale: Sent Ping");
                                        }
                                        Err(send_ping_flush_err) => {
                                            error!("SpoolScale: Error sending Ping payload (1) {send_ping_flush_err:?}, disconnecting");
                                            break 'send_recv_loop;
                                        }
                                    }
                                }
                                Err(send_ping_payload_err) => {
                                    error!("SpoolScale: Error sending Ping payload (2) {send_ping_payload_err:?}");
                                }
                            }
                        }
                        Err(send_ping_header_err) => {
                            error!("SpoolScale: Error sending Ping header {send_ping_header_err:?}");
                        }
                    }
                    // in case of timeut (which is the Err(_timeout_err) case we want to continue send_recv_loop
                    continue 'send_recv_loop;
                }
                embassy_futures::select::Either3::Second(from_scale_res) => {
                    match from_scale_res {
                        Ok(header) => {
                            let recv_payload_res = header.recv_payload(&mut socket, buf).await;
                            if let Ok(payload) = recv_payload_res {
                                match header.frame_type {
                                    FrameType::Text(_fragmented) => {
                                        spool_scale_rc.borrow_mut().process_message(&header, payload);
                                    }
                                    FrameType::Binary(_) => {
                                        error!("Got binary message, header: {header}, payload: {payload:?}");
                                    }
                                    FrameType::Ping => {
                                        let pong_header = FrameHeader {
                                            frame_type: FrameType::Pong,
                                            payload_len: header.payload_len,
                                            mask_key: header.mask_key,
                                        };
                                        let send_pong_header_res = pong_header.send(&mut socket).await;
                                        match send_pong_header_res {
                                            Ok(_) => {
                                                let res = pong_header.send_payload(&mut socket, payload).await;
                                                match res {
                                                    Ok(_) => {
                                                        let flush_res = socket.flush().await;
                                                        match flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Received Ping, replied with Pong");
                                                            }
                                                            Err(err) => {
                                                                error!("SpoolScale: Error sending Pong reply {err:?}, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Pong payload (3) {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                error!("SpoolScale: Error sending Pong header {err:?}");
                                                break 'send_recv_loop;
                                            }
                                        }
                                    }
                                    FrameType::Pong => {
                                        let tick_res: Result<&[u8; 8], _> = payload.try_into();
                                        if let Ok(ticks) = tick_res {
                                            let ping_ticks = u64::from_le_bytes(*ticks);
                                            let ping_instant = Instant::from_ticks(ping_ticks);
                                            let elapsed_duration = ping_instant.elapsed();
                                            debug!("SpoolScale: Ping-Pong duration was {} millis", elapsed_duration.as_millis());
                                        } else {
                                            warn!("SpoolScale: Received pong wrongly formatted, header: {header:?}, payload: {payload:?}");
                                        }
                                    }
                                    FrameType::Close => {
                                        let close_resp_header = FrameHeader {
                                            frame_type: FrameType::Close,
                                            payload_len: header.payload_len,
                                            mask_key: header.mask_key,
                                        };
                                        let close_resp_header_res = close_resp_header.send(&mut socket).await;
                                        match close_resp_header_res {
                                            Ok(_) => {
                                                let close_resp_payload_res = close_resp_header.send_payload(&mut socket, payload).await;
                                                match close_resp_payload_res {
                                                    Ok(_) => {
                                                        let close_resp_flush_res = socket.flush().await;
                                                        match close_resp_flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Replied to Close, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                            Err(close_resp_flush_err) => {
                                                                error!(
                                                                    "SpoolScale: Error sending Close reply {close_resp_flush_err:?}, disconnecting"
                                                                );
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Close Response payload {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(close_resp_header_err) => {
                                                error!("SpoolScale: Error sending Close Response header {close_resp_header_err:?}");
                                                break 'send_recv_loop;
                                            }
                                        }
                                    }
                                    FrameType::Continue(_fragmented) => {
                                        warn!(
                                            "SpoolScale Recv(continue): header: {header}, payload: {}",
                                            core::str::from_utf8(payload).unwrap()
                                        );
                                    }
                                }

                                if !header.frame_type.is_final() {
                                    warn!("SpoolScale: Unexpected fragmented frame header: {header:?}, payload: {payload:?}");
                                }
                            } else {
                                error!("SpoolScale: Error while reading payload {:?}", recv_payload_res.err().unwrap());
                                // can continue, will try to read next header and if will fail, will fail on the header and disconnect
                            }
                        }
                        Err(recv_header_err) => {
                            match recv_header_err {
                                edge_ws::Error::Io(io_err) => {
                                    error!("SpoolScale: IO error while reading header, disconnecting {io_err:?}");
                                    // breaking out of the loop, because when an IO error happens here, it happens continuously and turns to a busy loop
                                    break 'send_recv_loop;
                                }
                                // edge_ws::Error::Incomplete(_) => todo!(),
                                // edge_ws::Error::Invalid => todo!(),
                                // edge_ws::Error::BufferOverflow => todo!(),
                                // edge_ws::Error::InvalidLen => todo!(),
                                _ => {
                                    error!("SpoolScale: Error receiving web-socket header {recv_header_err:?}");
                                    break 'send_recv_loop;
                                }
                            }
                        }
                    }
                }
                embassy_futures::select::Either3::Third(console_to_scale) => {
                    let json_res = serde_json::to_string(&console_to_scale);
                    match json_res {
                        Ok(mut json) => {
                            // Legacy G-code analysis requests were encrypted like firmware updates:
                            // if matches!(console_to_scale, ConsoleToScale::RequestGcodeAnalysis { .. })
                            //     || matches!(console_to_scale, ConsoleToScale::UpdateFirmware { .. })
                            // {
                            if matches!(console_to_scale, ConsoleToScale::UpdateFirmware { .. }) {
                                let key = &app_config.borrow().scale_encryption_key.borrow();
                                json = if key.is_empty() {
                                    term_error!("Empty SpoolScale Security Key configured in Console , can't send message to scale");
                                    if matches!(console_to_scale, ConsoleToScale::UpdateFirmware { .. }) {
                                        spool_scale_rc.borrow().notify_ota_progress_update(&OtaProgressUpdate::Failed {
                                            text: "Scale Security Key not set in Console\nCan't Update Scale Firmware".to_string(),
                                        });
                                    }
                                    continue;
                                } else {
                                    encrypt(key, &json)
                                };
                            }
                            let send_to_scale_header = FrameHeader {
                                frame_type: FrameType::Text(false),
                                payload_len: json.len() as u64,
                                mask_key: None,
                            };

                            let send_to_scale_header_res = send_to_scale_header.send(&mut socket).await;
                            match send_to_scale_header_res {
                                Ok(_) => {
                                    let send_to_scale_payload_res = send_to_scale_header.send_payload(&mut socket, json.as_bytes()).await;
                                    match send_to_scale_payload_res {
                                        Ok(_) => {
                                            let res = socket.flush().await;
                                            match res {
                                                Ok(_) => {
                                                    // log at most 200 characters
                                                    let idx = json.char_indices().nth(200).map(|(i, _)| i).unwrap_or(json.len());
                                                    let str_to_print: &str = &json[..idx];
                                                    debug!(
                                                        "SpoolScale: Sent message to scale: {str_to_print}{}",
                                                        if str_to_print.len() < json.len() { "  ..." } else { "" }
                                                    );
                                                }
                                                Err(send_to_scale_flush_err) => {
                                                    error!("SpoolScale: Error sending message payload {send_to_scale_flush_err:?}, disconnecting");
                                                    break 'send_recv_loop;
                                                }
                                            }
                                        }
                                        Err(send_to_scale_payload_err) => {
                                            error!("SpoolScale: Error sending Ping payload {send_to_scale_payload_err:?}");
                                        }
                                    }
                                }
                                Err(send_to_scale_header_err) => {
                                    error!("SpoolScale: Error sending Ping header {send_to_scale_header_err:?}");
                                }
                            }
                        }
                        Err(err) => {
                            error!("SpoolScale: Error serializing data {:?}, {:?}", console_to_scale, err)
                        }
                    }
                }
            }
        }
        spool_scale_rc.borrow_mut().disconnected();
    }
}
