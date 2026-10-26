use alloc::{format, string::String, string::ToString};
use core::cell::RefCell;

use critical_section::Mutex;
use embassy_time::{Instant, Timer};
use esp_alloc::HeapStats;
use framework::{debug, ntp::InstantExt};

#[cfg(feature = "metrics")]
use core::fmt;
#[cfg(feature = "metrics")]
use framework::info;

static LAST_PERIODIC_SNAPSHOT: Mutex<RefCell<Option<String>>> = Mutex::new(RefCell::new(None));

#[cfg(feature = "metrics")]
static API_COLLECTION_PERIOD: Mutex<RefCell<Option<ApiCollectionPeriod>>> = Mutex::new(RefCell::new(None));

#[cfg(feature = "metrics")]
struct ApiCollectionPeriod {
    started_at: Instant,
    started_at_text: String,
}

fn capture_time() -> String {
    Instant::now()
        .to_date_time()
        .map(|date_time| format!("{date_time}"))
        .unwrap_or_else(|| "Time unavailable".to_string())
}

fn store_periodic_snapshot(snapshot: String) {
    critical_section::with(|cs| *LAST_PERIODIC_SNAPSHOT.borrow_ref_mut(cs) = Some(snapshot));
}

pub fn latest_periodic_snapshot() -> Option<String> {
    critical_section::with(|cs| LAST_PERIODIC_SNAPSHOT.borrow_ref(cs).clone())
}

#[cfg(feature = "metrics")]
fn initialize_api_collection_period() {
    let started_at = Instant::now();
    let started_at_text = capture_time();
    critical_section::with(|cs| {
        let mut period = API_COLLECTION_PERIOD.borrow_ref_mut(cs);
        if period.is_none() {
            *period = Some(ApiCollectionPeriod {
                started_at,
                started_at_text,
            });
        }
    });
}

#[cfg(feature = "metrics")]
fn take_api_collection_period() -> ApiCollectionPeriod {
    let started_at = Instant::now();
    let started_at_text = capture_time();
    critical_section::with(|cs| {
        let mut period = API_COLLECTION_PERIOD.borrow_ref_mut(cs);
        let previous_period = period.replace(ApiCollectionPeriod {
            started_at,
            started_at_text: started_at_text.clone(),
        });
        previous_period.unwrap_or(ApiCollectionPeriod {
            started_at,
            started_at_text,
        })
    })
}

#[cfg(feature = "metrics")]
fn format_elapsed(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;

    if days != 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours != 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes != 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Default)]
pub struct MetricStats {
    pub count: u64,
    pub sum: u64,
    pub min: u64,
    pub max: u64,
}

#[cfg(feature = "metrics")]
impl MetricStats {
    pub fn observe(&mut self, value: u64) {
        if self.count == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.count = self.count.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
    }

    pub fn average(&self) -> u64 {
        if self.count == 0 { 0 } else { self.sum / self.count }
    }
}

#[cfg(feature = "metrics")]
impl fmt::Display for MetricStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "n={} min={} avg={} max={}", self.count, self.min, self.average(), self.max)
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Default)]
pub struct MetricCounter(u64);

#[cfg(feature = "metrics")]
impl MetricCounter {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn increment(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

#[cfg(feature = "metrics")]
impl fmt::Display for MetricCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Default)]
pub struct ConsoleMetricsSnapshot {
    pub mqtt_queue_len: MetricStats,
    pub mqtt_payload_bytes: MetricStats,
    pub mqtt_json_parse_us: MetricStats,
    pub mqtt_state_apply_us: MetricStats,
    pub mqtt_changed_notify_us: MetricStats,
    pub mqtt_changed_count: MetricCounter,
    pub mqtt_change_fila_switch: MetricCounter,
    pub mqtt_change_command: MetricCounter,
    pub mqtt_change_extruder: MetricCounter,
    pub mqtt_change_tray_routing: MetricCounter,
    pub mqtt_change_ams_topology: MetricCounter,
    pub mqtt_change_ams_environment: MetricCounter,
    pub mqtt_change_ams_info: MetricCounter,
    pub mqtt_change_ams_tray: MetricCounter,
    pub mqtt_change_external_tray: MetricCounter,
    pub mqtt_change_print_project: MetricCounter,
    pub api_slots_build_us: MetricStats,
    pub api_slots_json_encode_us: MetricStats,
    pub api_slots_response_write_us: MetricStats,
    pub api_slots_response_bytes: MetricStats,
    pub api_slots_error_count: MetricCounter,
}

#[cfg(feature = "metrics")]
impl fmt::Display for ConsoleMetricsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mqtt_queue_len=({}) mqtt_payload_bytes=({}) mqtt_json_parse_us=({}) mqtt_state_apply_us=({}) mqtt_changed_count={} mqtt_change_sources=(fila_switch={} command={} extruder={} tray_routing={} ams_topology={} ams_environment={} ams_info={} ams_tray={} external_tray={} print_project={}) mqtt_changed_notify_us=({}) api_slots_build_us=({}) api_slots_json_encode_us=({}) api_slots_response_write_us=({}) api_slots_response_bytes=({}) api_slots_error_count={}",
            self.mqtt_queue_len,
            self.mqtt_payload_bytes,
            self.mqtt_json_parse_us,
            self.mqtt_state_apply_us,
            self.mqtt_changed_count,
            self.mqtt_change_fila_switch,
            self.mqtt_change_command,
            self.mqtt_change_extruder,
            self.mqtt_change_tray_routing,
            self.mqtt_change_ams_topology,
            self.mqtt_change_ams_environment,
            self.mqtt_change_ams_info,
            self.mqtt_change_ams_tray,
            self.mqtt_change_external_tray,
            self.mqtt_change_print_project,
            self.mqtt_changed_notify_us,
            self.api_slots_build_us,
            self.api_slots_json_encode_us,
            self.api_slots_response_write_us,
            self.api_slots_response_bytes,
            self.api_slots_error_count,
        )
    }
}

#[cfg(feature = "metrics")]
mod enabled {
    use core::cell::RefCell;

    use critical_section::Mutex;
    use embassy_time::Instant;
    use framework::metrics::MetricWindows;

    use super::{ConsoleMetricsSnapshot, MetricCounter, MetricStats};

    #[derive(Clone, Copy)]
    pub struct MeasurementStart(Instant);

    static METRICS: Mutex<RefCell<Option<MetricWindows<ConsoleMetricsSnapshot>>>> = Mutex::new(RefCell::new(None));

    fn with_metrics<R>(operation: impl FnOnce(&mut MetricWindows<ConsoleMetricsSnapshot>) -> R) -> R {
        critical_section::with(|cs| {
            let mut metrics = METRICS.borrow_ref_mut(cs);
            operation(metrics.get_or_insert_with(MetricWindows::default))
        })
    }

    pub fn start_measurement() -> MeasurementStart {
        MeasurementStart(Instant::now())
    }

    fn observe(metric: fn(&mut ConsoleMetricsSnapshot) -> &mut MetricStats, value: u64) {
        with_metrics(|metrics| metrics.record(|snapshot| metric(snapshot).observe(value)));
    }

    fn record(metric: fn(&mut ConsoleMetricsSnapshot) -> &mut MetricStats, start: MeasurementStart) {
        let elapsed_us = start.0.elapsed().as_micros();
        observe(metric, elapsed_us);
    }

    fn increment(counter: fn(&mut ConsoleMetricsSnapshot) -> &mut MetricCounter) {
        with_metrics(|metrics| metrics.record(|snapshot| counter(snapshot).increment()));
    }

    pub fn record_mqtt_payload_bytes(bytes: u64) {
        observe(|metrics| &mut metrics.mqtt_payload_bytes, bytes);
    }

    pub fn record_mqtt_queue_len(len: u64) {
        observe(|metrics| &mut metrics.mqtt_queue_len, len);
    }

    pub fn record_mqtt_json_parse(start: MeasurementStart) {
        record(|metrics| &mut metrics.mqtt_json_parse_us, start);
    }

    pub fn record_mqtt_state_apply(start: MeasurementStart) {
        record(|metrics| &mut metrics.mqtt_state_apply_us, start);
    }

    pub fn record_mqtt_changed_notify(start: MeasurementStart) {
        record(|metrics| &mut metrics.mqtt_changed_notify_us, start);
    }

    pub fn increment_mqtt_changed() {
        increment(|metrics| &mut metrics.mqtt_changed_count);
    }

    pub fn increment_mqtt_change_fila_switch() {
        increment(|metrics| &mut metrics.mqtt_change_fila_switch);
    }

    pub fn increment_mqtt_change_command() {
        increment(|metrics| &mut metrics.mqtt_change_command);
    }

    pub fn increment_mqtt_change_extruder() {
        increment(|metrics| &mut metrics.mqtt_change_extruder);
    }

    pub fn increment_mqtt_change_tray_routing() {
        increment(|metrics| &mut metrics.mqtt_change_tray_routing);
    }

    pub fn increment_mqtt_change_ams_topology() {
        increment(|metrics| &mut metrics.mqtt_change_ams_topology);
    }

    pub fn increment_mqtt_change_ams_environment() {
        increment(|metrics| &mut metrics.mqtt_change_ams_environment);
    }

    pub fn increment_mqtt_change_ams_info() {
        increment(|metrics| &mut metrics.mqtt_change_ams_info);
    }

    pub fn increment_mqtt_change_ams_tray() {
        increment(|metrics| &mut metrics.mqtt_change_ams_tray);
    }

    pub fn increment_mqtt_change_external_tray() {
        increment(|metrics| &mut metrics.mqtt_change_external_tray);
    }

    pub fn increment_mqtt_change_print_project() {
        increment(|metrics| &mut metrics.mqtt_change_print_project);
    }

    pub fn record_api_slots_build(start: MeasurementStart) {
        record(|metrics| &mut metrics.api_slots_build_us, start);
    }

    pub fn record_api_slots_json_encode(start: MeasurementStart) {
        record(|metrics| &mut metrics.api_slots_json_encode_us, start);
    }

    pub fn record_api_slots_response_write(start: MeasurementStart) {
        record(|metrics| &mut metrics.api_slots_response_write_us, start);
    }

    pub fn record_api_slots_response_bytes(bytes: u64) {
        observe(|metrics| &mut metrics.api_slots_response_bytes, bytes);
    }

    pub fn increment_api_slots_error() {
        increment(|metrics| &mut metrics.api_slots_error_count);
    }

    pub fn snapshot_period_and_reset() -> ConsoleMetricsSnapshot {
        with_metrics(|metrics| metrics.take_period())
    }

    pub fn snapshot_since_api_collection_and_reset() -> ConsoleMetricsSnapshot {
        with_metrics(|metrics| metrics.take_since_api_collection())
    }
}

#[cfg(feature = "metrics")]
pub use enabled::*;

pub fn collect_debug_snapshot() -> Option<String> {
    let last_period_snapshot = latest_periodic_snapshot().unwrap_or_else(|| "No periodic snapshot available yet.".to_string());

    #[cfg(feature = "metrics")]
    {
        let api_collection_period = take_api_collection_period();
        let framework_metrics = framework::metrics::snapshot_since_api_collection_and_reset();
        let console_metrics = snapshot_since_api_collection_and_reset();
        let collected_over = format_elapsed(api_collection_period.started_at.elapsed().as_secs());

        return Some(format!(
            "Last period\n{last_period_snapshot}\n\n----------------------------------------\n\nMetrics since {} collected over {collected_over}\nframework: {framework_metrics}\nconsole: {console_metrics}",
            api_collection_period.started_at_text,
        ));
    }

    #[cfg(not(feature = "metrics"))]
    Some(last_period_snapshot)
}

pub async fn periodic_stats_task() {
    #[cfg(feature = "metrics")]
    let mut sample = 0u64;
    let mut collect_metrics = false;

    #[cfg(feature = "metrics")]
    initialize_api_collection_period();

    loop {
        if collect_metrics {
            Timer::after_secs(30).await;
        }

        let captured_at = capture_time();
        let heap_stats: HeapStats = esp_alloc::HEAP.stats();
        let heap_log = format!("{heap_stats}");

        #[cfg(feature = "metrics")]
        if collect_metrics {
            sample = sample.saturating_add(1);
            let framework_metrics = framework::metrics::snapshot_period_and_reset();
            let console_metrics = snapshot_period_and_reset();

            let framework_log = format!("METRICS[{sample}] framework: {framework_metrics}");
            let console_log = format!("METRICS[{sample}] console: {console_metrics}");

            store_periodic_snapshot(format!("{captured_at}\n\n{framework_log}\n{console_log}\n{heap_log}"));
            info!("{framework_log}");
            info!("{console_log}");
        } else {
            store_periodic_snapshot(format!("{captured_at}\n\n{heap_log}"));
        }

        #[cfg(not(feature = "metrics"))]
        store_periodic_snapshot(format!("{captured_at}\n\n{heap_log}"));

        debug!("{heap_log}");
        collect_metrics = true;
    }
}

#[cfg(all(test, feature = "metrics"))]
mod tests {
    use super::MetricStats;

    #[test]
    fn metric_stats_tracks_bounds_and_average() {
        let mut stats = MetricStats::default();
        stats.observe(20);
        stats.observe(10);
        stats.observe(30);

        assert_eq!(stats.count, 3);
        assert_eq!(stats.min, 10);
        assert_eq!(stats.average(), 20);
        assert_eq!(stats.max, 30);
    }
}
