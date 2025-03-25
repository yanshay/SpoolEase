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
