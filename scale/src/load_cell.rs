use core::cell::RefCell;
use core::cmp::min;

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::semaphore::{GreedySemaphore, Semaphore};
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Timer};
use esp_hal::spi::{self, AnySpi};
use esp_hal::time::RateExtU32;
use esp_hal::{gpio::AnyPin, spi::master::Spi};
use framework::{error, term_info};
use hx711_spi::Hx711;
use num_traits::abs;

type ReadWaiter = GreedySemaphore<NoopRawMutex>;

type ChangeWaiter = Watch<NoopRawMutex, i32, 5>;

pub struct LoadCell {
    samples_per_read: usize,
    duration_between_reads: Duration,
    change_threshold_g: i32,
    samples: Vec<i32>,
    next_sample_index: usize,
    calibration_weight: i64, // from calibration, shouldn't be changed
    calibration_sample: i64, // from calibration, shouldn't be changed
    calibration_tare: i64,   // from calibration, shouldn't be changed
    current_tare: i64,       // could change during execution (like if negative number)

    pub stable_read_waiter: Rc<ReadWaiter>,
    pub change_waiter: Rc<ChangeWaiter>,
    pub load_cell_weak: Weak<RefCell<Self>>,

    spi: Option<AnySpi>,
    dt: Option<AnyPin>,
    sck: Option<AnyPin>,
    spawner: Spawner,
}

impl LoadCell {
    pub fn new(
        spi: AnySpi,
        dt: AnyPin,
        sck: AnyPin,
        samples_per_read: usize,
        duration_between_reads: Duration,
        spawner: Spawner,
    ) -> Rc<RefCell<Self>> {
        let myself = Self {
            samples_per_read,
            duration_between_reads,
            change_threshold_g: 5,
            samples: Vec::with_capacity(samples_per_read),
            next_sample_index: 0,

            // initial numbers on a 2kg load cell, need something to not have to deal with all kind of edge cases
            calibration_tare: 63770,
            calibration_weight: 292,
            calibration_sample: 383722,
            current_tare: 63770,

            stable_read_waiter: Rc::new(ReadWaiter::new(0)),
            change_waiter: Rc::new(ChangeWaiter::new()),
            load_cell_weak: Weak::new(),
            spi: Some(spi),
            dt: Some(dt),
            sck: Some(sck),
            spawner
        };
        let myself = Rc::new(RefCell::new(myself));
        myself.borrow_mut().load_cell_weak = Rc::downgrade(&myself);

        myself
    }

    pub fn start(&mut self) {
        let spi = self.spi.take().unwrap();
        let dt = self.dt.take().unwrap();
        let sck = self.sck.take().unwrap();
        let load_cell_rc = self.load_cell_weak.upgrade().unwrap();

        self.spawner
            .spawn(spi_async_load_cell_task(
                spi.into(),
                dt.into(),
                sck.into(),
                load_cell_rc.clone(),
                self.duration_between_reads,
            ))
            .ok();
    }

    pub fn set_calibration(
        &mut self,
        calibration_tare: i32,
        calibration_weight: i32,
        calibration_sample: i32,
    ) {
        self.calibration_tare = calibration_tare as i64;
        self.calibration_weight = calibration_weight as i64;
        self.calibration_sample = calibration_sample as i64;

        self.current_tare = self.calibration_tare;
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
        let samples_sum = self.samples_sum();
        let samples_sum_after_tare = samples_sum - self.samples.len() as i64 * self.current_tare; // here need to use the current_tare
        let calibrated_sum = samples_sum_after_tare * self.calibration_weight
            / (self.calibration_sample - self.calibration_tare); // here need to use the calibration_tare (because the calibration factor isn't supposed to change even if tare does)
        return (calibrated_sum / self.samples.len() as i64) as i32;
    }
    pub fn add_sample(&mut self, sample: i32) {
        if !self.samples.is_empty() && self.calibration_tare != 0 {
            let clear_samples_change_threshold =
                abs(self.change_threshold_g as i64 * (self.calibration_sample - self.calibration_tare) / self.calibration_weight); // 5g // here need to use calibration tare
            if abs(self.samples_sum() - sample as i64 * self.samples.len() as i64)
                / self.samples.len() as i64
                > clear_samples_change_threshold
            {
                self.samples.clear();
                self.next_sample_index = 0;
                self.change_waiter.sender().send(sample);
            }
        }

        if self.samples.len() < self.samples_per_read {
            self.samples.push(sample);
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

/////

pub struct LoadCellReader {
    load_cell: Rc<RefCell<LoadCell>>,
}

impl<'a> LoadCellReader {
    pub async fn read_stable(&self) -> i32 {
        let stable_read_waiter = self.load_cell.borrow().stable_read_waiter();
        stable_read_waiter.acquire_all(1).await.unwrap();
        stable_read_waiter.release(1);
        self.load_cell.borrow().immediate_read()
    }

    pub fn try_read_changed(&self, from: i32) -> Option<i32> {
        let immediate_read = self.load_cell.borrow().immediate_read();
        let change_threshold_g = self.load_cell.borrow().change_threshold_g;
        if abs(immediate_read - from) > change_threshold_g {
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
        change_receiver.changed().await;
        self.load_cell.borrow().immediate_read()
    }

    pub fn immediate_read(&self) -> i32 {
        self.load_cell.borrow().immediate_read()
    }

    pub fn immediate_read_uncalibrated(&self) -> i32 {
        self.load_cell.borrow().immediate_read_uncalibrated()
    }
}

#[embassy_executor::task]
async fn spi_async_load_cell_task(
    spi: AnySpi,
    dt: AnyPin,
    sck: AnyPin,
    load_cell: Rc<RefCell<LoadCell>>,
    duration_between_samples: Duration,
) {
    let lc_miso = dt;
    let lc_mosi = sck;

    let hx711_spi = Spi::new(
        spi,
        spi::master::Config::default()
            .with_frequency(1.MHz())
            .with_mode(spi::Mode::_1),
    )
    .unwrap()
    // .with_sck(sd_sclk)
    .with_miso(lc_miso)
    .with_mosi(lc_mosi)
    .into_async();

    let mut hx711_sensor = Hx711::new_async(hx711_spi);
    term_info!("Initializing Load-Cell reader");
    let err_count = 0;
    loop {
        if let Err(err) = hx711_sensor.reset_async().await {
            error!("Error initializing hx711 {err:?}");
            Timer::after_millis(500).await;
        } else {
            break;
        }
        if err_count == 10 {
            term_info!("Ending retries to initialize Load-Cell reader");
            return;
        }
    }
    term_info!("Load-Cell reader initialized successfully");

    // Skip first readings which are 0 / -1
    let initial_readings = true;
    let mut count_good_samples = 0;
    while initial_readings {
        let v = hx711_sensor.read_async().await;
        if let Ok(v) = v {
            if !([0, -1].contains(&v)) {
                count_good_samples += 1;
            } else {
            }
            if count_good_samples >= 5 {
                break;
            }
        }
        Timer::after(duration_between_samples).await;
    }

    Timer::after(duration_between_samples).await;

    loop {
        let v = hx711_sensor.read_async().await;
        if let Ok(v) = v {
            if v != -1 {
                load_cell.borrow_mut().add_sample(v);
            }
        }
        Timer::after(duration_between_samples).await;
    }
}
