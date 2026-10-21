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
pub struct ConsoleMetricsSnapshot {
    pub mqtt_queue_len: MetricStats,
    pub mqtt_payload_bytes: MetricStats,
    pub mqtt_json_parse_us: MetricStats,
    pub mqtt_state_apply_us: MetricStats,
    pub mqtt_changed_notify_us: MetricStats,
    pub mqtt_changed_count: u64,
    pub api_slots_build_us: MetricStats,
    pub api_slots_json_encode_us: MetricStats,
    pub api_slots_response_write_us: MetricStats,
    pub api_slots_response_bytes: MetricStats,
    pub api_slots_error_count: u64,
}

#[cfg(feature = "metrics")]
mod enabled {
    use core::cell::RefCell;

    use critical_section::Mutex;
    use embassy_time::Instant;

    use super::{ConsoleMetricsSnapshot, MetricStats};

    #[derive(Clone, Copy)]
    pub struct MeasurementStart(Instant);

    static METRICS: Mutex<RefCell<ConsoleMetricsSnapshot>> = Mutex::new(RefCell::new(ConsoleMetricsSnapshot {
        mqtt_queue_len: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        mqtt_payload_bytes: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        mqtt_json_parse_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        mqtt_state_apply_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        mqtt_changed_notify_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        mqtt_changed_count: 0,
        api_slots_build_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        api_slots_json_encode_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        api_slots_response_write_us: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        api_slots_response_bytes: MetricStats {
            count: 0,
            sum: 0,
            min: 0,
            max: 0,
        },
        api_slots_error_count: 0,
    }));

    pub fn start_measurement() -> MeasurementStart {
        MeasurementStart(Instant::now())
    }

    fn record(metric: fn(&mut ConsoleMetricsSnapshot) -> &mut MetricStats, start: MeasurementStart) {
        let elapsed_us = start.0.elapsed().as_micros();
        critical_section::with(|cs| metric(&mut METRICS.borrow_ref_mut(cs)).observe(elapsed_us));
    }

    pub fn record_mqtt_payload_bytes(bytes: u64) {
        critical_section::with(|cs| METRICS.borrow_ref_mut(cs).mqtt_payload_bytes.observe(bytes));
    }

    pub fn record_mqtt_queue_len(len: u64) {
        critical_section::with(|cs| METRICS.borrow_ref_mut(cs).mqtt_queue_len.observe(len));
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
        critical_section::with(|cs| {
            let mut metrics = METRICS.borrow_ref_mut(cs);
            metrics.mqtt_changed_count = metrics.mqtt_changed_count.saturating_add(1);
        });
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
        critical_section::with(|cs| METRICS.borrow_ref_mut(cs).api_slots_response_bytes.observe(bytes));
    }

    pub fn increment_api_slots_error() {
        critical_section::with(|cs| {
            let mut metrics = METRICS.borrow_ref_mut(cs);
            metrics.api_slots_error_count = metrics.api_slots_error_count.saturating_add(1);
        });
    }

    pub fn snapshot_and_reset() -> ConsoleMetricsSnapshot {
        critical_section::with(|cs| {
            let mut metrics = METRICS.borrow_ref_mut(cs);
            let snapshot = *metrics;
            *metrics = ConsoleMetricsSnapshot::default();
            snapshot
        })
    }
}

#[cfg(feature = "metrics")]
pub use enabled::*;

pub async fn periodic_stats_task() {
    #[cfg(feature = "metrics")]
    let mut sample = 0u64;
    let mut collect_metrics = false;

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
            let framework_metrics = framework::metrics::snapshot_and_reset();
            let console_metrics = snapshot_and_reset();

            let framework_log = format!(
                "METRICS[{sample}] framework: ui_draw_us=({}) ui_redraw_request_to_draw_us=({}) sd_write_us=({}) sd_write_bytes=({}) http_web_request_us=({}) tls_accept_to_first_read_us=({})",
                framework_metrics.ui_draw_us,
                framework_metrics.ui_redraw_request_to_draw_us,
                framework_metrics.sd_write_us,
                framework_metrics.sd_write_bytes,
                framework_metrics.http_web_request_us,
                framework_metrics.tls_accept_to_first_read_us,
            );
            let console_log = format!(
                "METRICS[{sample}] console: mqtt_queue_len=({}) mqtt_payload_bytes=({}) mqtt_json_parse_us=({}) mqtt_state_apply_us=({}) mqtt_changed_count={} mqtt_changed_notify_us=({}) api_slots_build_us=({}) api_slots_json_encode_us=({}) api_slots_response_write_us=({}) api_slots_response_bytes=({}) api_slots_error_count={}",
                console_metrics.mqtt_queue_len,
                console_metrics.mqtt_payload_bytes,
                console_metrics.mqtt_json_parse_us,
                console_metrics.mqtt_state_apply_us,
                console_metrics.mqtt_changed_count,
                console_metrics.mqtt_changed_notify_us,
                console_metrics.api_slots_build_us,
                console_metrics.api_slots_json_encode_us,
                console_metrics.api_slots_response_write_us,
                console_metrics.api_slots_response_bytes,
                console_metrics.api_slots_error_count,
            );

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
