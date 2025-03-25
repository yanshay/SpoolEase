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
use hx711_spi::Hx711;
use num_traits::abs;

type ReadWaiter = GreedySemaphore<NoopRawMutex>;

type ChangeWaiter = Watch<NoopRawMutex, i32, 5>;

pub struct LoadCell {
    samples_per_read: usize,
    duration_between_reads: Duration,
    samples: Vec<i32>,
    next_sample_index: usize,
    calibration_weight: i64,
    calibration_sample: i64,
    calibration_tare: i64,

    clear_samples_change_threshold: i64,
    pub full_read_waiter: Rc<ReadWaiter>,
    pub change_waiter: Rc<ChangeWaiter>,
    pub load_cell_weak: Weak<RefCell<Self>>,
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
        let calibration_weight = 140;
        let calibration_sample = 153100;
        let clear_samples_change_threshold = 10 * calibration_sample / calibration_weight; // 10g
        let myself = Self {
            samples_per_read,
            duration_between_reads,
            samples: Vec::with_capacity(samples_per_read),
            next_sample_index: 0,
            calibration_weight: 140,
            calibration_sample,
            calibration_tare: 0,
            clear_samples_change_threshold,
            full_read_waiter: Rc::new(ReadWaiter::new(0)),
            change_waiter: Rc::new(ChangeWaiter::new()),
            load_cell_weak: Weak::new(),
        };
        let myself = Rc::new(RefCell::new(myself));
        myself.borrow_mut().load_cell_weak = Rc::downgrade(&myself);
        spawner
            .spawn(spi_async_load_cell_task(
                spi.into(),
                dt.into(),
                sck.into(),
                myself.clone(),
                duration_between_reads,
            ))
            .ok();

        myself
    }

    fn samples_sum(&self) -> i64 {
        self.samples.iter().map(|&x| x as i64).sum::<i64>()
    }

    fn full_read_waiter(&self) -> Rc<ReadWaiter> {
        self.full_read_waiter.clone()
    }

    pub fn reader(&self) -> LoadCellReader {
        let load_cell = self.load_cell_weak.upgrade().unwrap();

        LoadCellReader { load_cell }
    }

    pub fn immediate_read(&self) -> i32 {
        if self.samples.is_empty() {
            // Should never happen except before initialization
            // Later there should always be at least one sample
            return -1;
        }
        let samples_sum = self.samples_sum();
        let samples_sum_after_tare =
            samples_sum - self.samples.len() as i64 * self.calibration_tare;
        let calibrated_sum =
            samples_sum_after_tare * self.calibration_weight / self.calibration_sample;
        (calibrated_sum / self.samples.len() as i64) as i32
    }
    pub fn add_sample(&mut self, sample: i32) {
        // debug!("New sample {sample}");
        if !self.samples.is_empty() {
            if abs(self.samples_sum() - sample as i64 * self.samples.len() as i64)
                / self.samples.len() as i64
                > self.clear_samples_change_threshold
            {
                // debug!("Clearing samples");
                self.samples.clear();
                self.next_sample_index = 0;
                self.change_waiter.sender().send(sample);
                // trace!("Sending change waiter {sample}");
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
        // debug!("dealing with full_read_waiter");
        let full_read_waiter = self.full_read_waiter();
        if self.samples.len() == self.samples_per_read {
            full_read_waiter.set(1);
        } else {
            full_read_waiter.set(0);
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
        myself.borrow_mut().calibration_tare = tare_value;
    }
}

/////

pub struct LoadCellReader {
    load_cell: Rc<RefCell<LoadCell>>,
}

impl<'a> LoadCellReader {
    pub async fn read(&self) -> i32 {
        let read_waiter = self.load_cell.borrow().full_read_waiter();
        read_waiter.acquire_all(1).await.unwrap();
        read_waiter.release(1);
        self.load_cell.borrow().immediate_read()
    }
    pub async fn read_changed(&self) -> i32 {
        let change_waiter = self.load_cell.borrow().change_waiter.clone();
        let mut change_receiver = change_waiter.receiver().unwrap();
        change_receiver.changed().await;
        self.load_cell.borrow().immediate_read()
    }

    #[allow(dead_code)]
    pub fn immediate_read(&self) -> i32 {
        self.load_cell.borrow().immediate_read()
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
    hx711_sensor.reset_async().await.unwrap();
    // hx711_sensor.set_mode(hx711_spi::Mode::ChAGain128).unwrap(); // x128 works up to +-20mV

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
