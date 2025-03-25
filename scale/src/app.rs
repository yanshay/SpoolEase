use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use esp_hal::{gpio::AnyPin, spi::AnySpi};
use esp_mbedtls::TlsReference;
use framework::{debug, prelude::Framework};
use num_traits::abs;
use shared::scale::ScaleToConsole;

use crate::{app_config::AppConfig, console_proxy, load_cell::LoadCell};

enum ScaleState {
    Empty,
    Loaded(i32),
}

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn app_task(
    stack: Stack<'static>,
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    _tls: TlsReference<'static>,
    loadcell_dt: AnyPin,
    loadcell_sck: AnyPin,
    loadcell_spi: AnySpi,
) {
    let spawner = embassy_executor::Spawner::for_current_executor().await;
    let scale_to_console_channel = console_proxy::init(framework, app_config, stack, spawner).await;

    let load_cell = LoadCell::new(
        loadcell_spi,
        loadcell_dt,
        loadcell_sck,
        5,
        Duration::from_millis(100),
        spawner,
    );

    LoadCell::tare(&load_cell).await;
    let load_cell_reader = load_cell.borrow_mut().reader();
    let mut scale_state = ScaleState::Empty;
    loop {
        match scale_state {
            ScaleState::Empty => {
                // debug!("In monitoring scale loop - empty");
                let read = load_cell_reader.read_changed().await;
                // let read = load_cell_reader.read().await;
                if read > 10 {
                    scale_to_console_channel
                        .send(ScaleToConsole::NewLoad(read))
                        .await;
                    debug!("Sending Spool Loaded");
                    scale_state = ScaleState::Loaded(read);
                } else if read < 1 {
                    LoadCell::tare(&load_cell).await;
                }
                Timer::after_millis(250).await;
            }
            ScaleState::Loaded(prev_read) => {
                // debug!("In monitoring scale loop - loaded");
                let read = load_cell_reader.read().await;
                if abs(read) < 10 {
                    scale_to_console_channel
                        .send(ScaleToConsole::LoadRemoved)
                        .await;
                    debug!("Sending Spool Loaded Removed");
                    scale_state = ScaleState::Empty;
                } else if read != prev_read {
                    scale_to_console_channel
                        .send(ScaleToConsole::LoadChanged(read))
                        .await;
                    debug!("Sending Spool Loaded Changed");
                    scale_state = ScaleState::Loaded(read)
                }
                Timer::after_millis(500).await;
            }
        }
    }
}

// #[embassy_executor::task]
// async fn spi_async_load_cell_task(
//     spi: esp_hal::peripherals::SPI3,
//     dt: GpioPin<4>,
//     sck: GpioPin<5>,
// ) {
//     let sd_miso = dt;
//     let sd_mosi = sck;
//
//     let hx711_spi = Spi::new(
//         spi,
//         spi::master::Config::default()
//             .with_frequency(1.MHz())
//             .with_mode(spi::Mode::_1),
//     )
//     .unwrap()
//     // .with_sck(sd_sclk)
//     .with_miso(sd_miso)
//     .with_mosi(sd_mosi)
//     .into_async();
//
//     let mut hx711_sensor = Hx711::new_async(hx711_spi);
//     hx711_sensor.reset_async().await.unwrap();
//     // hx711_sensor.set_mode(hx711_spi::Mode::ChAGain128).unwrap(); // x128 works up to +-20mV
//
//     // let sdcard_spi_device = ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap();
//     let scale = 140.0 / 153100.0;
//     let mut tare = None;
//     loop {
//         let v = hx711_sensor.read_async().await;
//         if let Ok(v) = v {
//             if tare.is_none() {
//                 if v != -1 && v != 0 {
//                     tare = Some(v);
//                     info!("Setting tare to {tare:?}");
//                 }
//             } else {
//                 debug!(
//                     "{} : {}",
//                     (v - tare.unwrap()),
//                     ((v - tare.unwrap()) as f32) * scale
//                 );
//             }
//         }
//         Timer::after_millis(100).await;
//     }
// }
// #[embassy_executor::task]
// async fn spi_sync_load_cell_task(spi: esp_hal::peripherals::SPI3, dt: GpioPin<4>, sck: GpioPin<5>) {
//     let sd_miso = dt;
//     let sd_mosi = sck;
//
//     let hx711_spi = Spi::new(
//         spi,
//         spi::master::Config::default()
//             .with_frequency(1.MHz())
//             .with_mode(spi::Mode::_1),
//     )
//     .unwrap()
//     // .with_sck(sd_sclk)
//     .with_miso(sd_miso)
//     .with_mosi(sd_mosi);
//
//     let mut hx711_sensor = Hx711::new(hx711_spi);
//     hx711_sensor.reset().unwrap();
//     // hx711_sensor.set_mode(hx711_spi::Mode::ChAGain128).unwrap(); // x128 works up to +-20mV
//
//     // let sdcard_spi_device = ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap();
//     let scale = 140.0 / 153100.0;
//     let mut tare = None;
//     loop {
//         let v = hx711_sensor.read();
//         if let Ok(v) = v {
//             if tare.is_none() {
//                 tare = Some(v);
//             } else {
//                 info!(
//                     "{} : {}",
//                     (v - tare.unwrap()),
//                     ((v - tare.unwrap()) as f32) * scale
//                 );
//             }
//         }
//         Timer::after_millis(100).await;
//     }
// }
//
// #[embassy_executor::task]
// async fn load_cell_task(dt: GpioPin<4>, sck: GpioPin<5>) {
//     use loadcell::LoadCell;
//
//     let hx711_dt = Input::new(dt, Pull::None);
//     let hx711_sck = Output::new(sck, Level::Low);
//
//     let delay = Delay::new();
//
//     // create the load sensor
//     let mut load_sensor = loadcell::hx711::HX711::new(hx711_sck, hx711_dt, delay);
//     // zero the readings
//     load_sensor.tare(16);
//     info!("offset = {}", load_sensor.get_offset());
//
//     load_sensor.set_scale((474000.0) / 261100.0);
//     // (474000-32421)/261100;
//
//     loop {
//         if load_sensor.is_ready() {
//             let reading = load_sensor.read_scaled();
//             if let Ok(x) = reading {
//                 let x = (x / 1000.0).round();
//                 info!("Last Reading = {:?}", x)
//             }
//         } else {
//             debug!("no measure");
//         }
//         Timer::after(Duration::from_millis(100)).await;
//     }
// }
