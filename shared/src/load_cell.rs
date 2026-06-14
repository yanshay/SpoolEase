use core::cell::RefCell;
use core::cmp::min;

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::semaphore::{GreedySemaphore, Semaphore};
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Timer};
use framework::error;

type ReadWaiter = GreedySemaphore<NoopRawMutex>;

type ChangeWaiter = Watch<NoopRawMutex, i32, 5>;

const MIN_SAMPLES_FOR_UNSTABLE: usize = 2;
pub const MIN_LOADED_WEIGHT: i32 = 5;

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug, Clone)]
pub struct LoadCellCalibrationConfig {
    pub zero_loadcell: i32,
    pub calib_weight: i32,
    pub calib_loadcell: i32,
}

#[derive(Clone, Copy, Debug)]
pub enum LoadCellState {
    Uncalibrated,
    Unknown,
    Empty,
    Loaded(i32, i32), // Stable-read, Unstable-read
}

pub struct LoadCell {
    samples_per_read: usize,
    duration_between_reads: Duration,
    change_threshold_g: i32,
    samples: Vec<i32>,
    next_sample_index: usize,
    calibration_weight: i64,        // from calibration, shouldn't be changed
    calibration_weight_sample: i64, // from calibration, shouldn't be changed
    calibration_tare_sample: i64,   // from calibration, shouldn't be changed
    current_tare: i64,              // could change during execution (like if negative number)

    stable_read_waiter: Rc<ReadWaiter>,
    change_waiter: Rc<ChangeWaiter>,
    load_cell_weak: Weak<RefCell<Self>>,
}

impl LoadCell {
    pub fn new(samples_per_read: usize, duration_between_reads: Duration) -> Rc<RefCell<Self>> {
        let myself = Self {
            samples_per_read,
            duration_between_reads,
            change_threshold_g: 3,
            samples: Vec::with_capacity(samples_per_read),
            next_sample_index: 0,

            // initial numbers on a 2kg load cell, need something to not have to deal with all kind of edge cases
            calibration_tare_sample: 63770,
            calibration_weight: 292,
            calibration_weight_sample: 383722,
            current_tare: 63770,

            stable_read_waiter: Rc::new(ReadWaiter::new(0)),
            change_waiter: Rc::new(ChangeWaiter::new()),
            load_cell_weak: Weak::new(),
        };
        let myself = Rc::new(RefCell::new(myself));
        myself.borrow_mut().load_cell_weak = Rc::downgrade(&myself);

        myself
    }

    pub fn set_calibration(
        &mut self,
        calibration_tare_sample: i32,
        calibration_weight: i32,
        calibration_weight_sample: i32,
    ) {
        if calibration_tare_sample == calibration_weight_sample {
            error!("Bad calibration - tare and weight load-cell values equal");
            return;
        }
        self.calibration_tare_sample = calibration_tare_sample as i64;
        self.calibration_weight = calibration_weight as i64;
        self.calibration_weight_sample = calibration_weight_sample as i64;

        self.current_tare = self.calibration_tare_sample;
    }

    pub fn set_calibration_config(&mut self, calibration: &LoadCellCalibrationConfig) {
        self.set_calibration(
            calibration.zero_loadcell,
            calibration.calib_weight,
            calibration.calib_loadcell,
        );
    }

    fn samples_sum(&self) -> i64 {
        self.samples.iter().map(|&x| x as i64).sum::<i64>()
    }

    fn stable_read_waiter(&self) -> Rc<ReadWaiter> {
        self.stable_read_waiter.clone()
    }

    pub fn reader(&self) -> LoadCellReader {
        let load_cell = self.load_cell_weak.upgrade().unwrap();

        LoadCellReader { load_cell }
    }

    pub fn immediate_read_uncalibrated(&self) -> i32 {
        if self.samples.is_empty() {
            // Should never happen except before initialization
            // Later there should always be at least one sample
            return -1;
        }
        let samples_sum = self.samples_sum();
        (samples_sum / self.samples.len() as i64) as i32
    }

    pub fn immediate_read(&self) -> i32 {
        if self.samples.is_empty() {
            // Should never happen except before initialization
            // Later there should always be at least one sample
            return -1;
        }
        if self.calibration_weight_sample == self.calibration_tare_sample {
            return 9999;
        }
        let samples_sum = self.samples_sum();
        let samples_sum_after_tare = samples_sum - self.samples.len() as i64 * self.current_tare; // here need to use the current_tare
        let calibrated_sum = samples_sum_after_tare * self.calibration_weight
            / (self.calibration_weight_sample - self.calibration_tare_sample); // here need to use the calibration_tare (because the calibration factor isn't supposed to change even if tare does)
        (calibrated_sum / self.samples.len() as i64) as i32
    }

    pub fn add_sample(&mut self, sample: i32) {
        let bad_calibration = self.calibration_weight_sample == self.calibration_tare_sample;

        // bad_calibration is to prevent panic on div by zero
        if !bad_calibration && !self.samples.is_empty() && self.calibration_tare_sample != 0 {
            let clear_samples_change_threshold = (self.change_threshold_g as i64
                * (self.calibration_weight_sample - self.calibration_tare_sample)
                / self.calibration_weight)
                .abs(); // 5g // here need to use calibration tare
            if (self.samples_sum() - sample as i64 * self.samples.len() as i64).abs()
                / self.samples.len() as i64
                > clear_samples_change_threshold
            {
                self.samples.clear();
                self.next_sample_index = 0;
            }
        }

        if self.samples.len() < self.samples_per_read {
            self.samples.push(sample);
            if self.samples.len() == MIN_SAMPLES_FOR_UNSTABLE {
                self.change_waiter.sender().send(self.immediate_read());
            }
        } else {
            self.samples[self.next_sample_index] = sample;
            self.next_sample_index += 1;
            if self.next_sample_index == self.samples.len() {
                self.next_sample_index = 0;
            }
        }
        let stable_read_waiter = self.stable_read_waiter();
        if self.samples.len() == self.samples_per_read {
            stable_read_waiter.set(1);
        } else {
            stable_read_waiter.set(0);
        }
    }

    pub async fn tare(myself: &Rc<RefCell<Self>>) {
        let required_samples = myself.borrow().samples_per_read;
        let duration_between_reads = myself.borrow().duration_between_reads;
        let mut available_samples = myself.borrow().samples.len();

        while available_samples < required_samples {
            let wait_duration =
                duration_between_reads * min(1, (required_samples - available_samples) as u32);
            Timer::after(wait_duration).await;
            available_samples = myself.borrow().samples.len();
        }
        let samples_sum = myself
            .borrow()
            .samples
            .iter()
            .map(|&x| x as i64)
            .sum::<i64>();
        let tare_value = samples_sum / required_samples as i64;
        myself.borrow_mut().current_tare = tare_value;
    }
}

pub struct LoadCellReader {
    load_cell: Rc<RefCell<LoadCell>>,
}

impl LoadCellReader {
    pub async fn read_stable(&self) -> i32 {
        let stable_read_waiter = self.load_cell.borrow().stable_read_waiter();
        stable_read_waiter.acquire_all(1).await.unwrap();
        stable_read_waiter.release(1);
        self.load_cell.borrow().immediate_read()
    }

    pub fn try_read_changed(&self, from: i32) -> Option<i32> {
        // in the case of 0 (so distance from empty) to ignore glitches every few minutes, require two samples,
        // otherwise, continuous update and 1 sample is enough for continuous reading
        let min_samples = if from == 0 {
            MIN_SAMPLES_FOR_UNSTABLE
        } else {
            1
        };
        if self.load_cell.borrow().samples.len() < min_samples {
            return None;
        }
        let immediate_read = self.load_cell.borrow().immediate_read();
        let change_threshold_g = self.load_cell.borrow().change_threshold_g;
        if (immediate_read - from).abs() > change_threshold_g {
            Some(immediate_read)
        } else {
            None
        }
    }

    pub async fn read_changed(&self, from: i32) -> i32 {
        if let Some(change_read) = self.try_read_changed(from) {
            return change_read;
        }
        let change_waiter = self.load_cell.borrow().change_waiter.clone();
        let mut change_receiver = change_waiter.receiver().unwrap();
        change_receiver.changed().await
    }

    #[allow(dead_code)]
    pub fn immediate_read(&self) -> i32 {
        self.load_cell.borrow().immediate_read()
    }

    pub fn immediate_read_uncalibrated(&self) -> i32 {
        self.load_cell.borrow().immediate_read_uncalibrated()
    }
}
