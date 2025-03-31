use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_mbedtls::TlsReference;

use framework::prelude::*;

use crate::{app_config::AppConfig, settings::MAX_NUM_PRINTERS};

slint::include_modules!();

pub fn create_slint_app() -> AppWindow {
    AppWindow::new().expect("Failed to load UI")
}

pub const MAX_NUM_SSDP_LISTENERS: usize = MAX_NUM_PRINTERS + 1; // 1 for spool_scale

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn app_task(
    stack: Stack<'static>,
    ui: slint::Weak<AppWindow>,
    framework: Rc<RefCell<Framework>>,
    tls: TlsReference<'static>,
    // Application
    app_config: Rc<RefCell<AppConfig>>,
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
) {
    let spawner = embassy_executor::Spawner::for_current_executor().await;

    // == Setup spool_tag =============================================================


    // == Setup ViewModel =============================================================
    let ui_strong = ui.upgrade().unwrap();
    let _view_model = crate::view_model::ViewModel::new(
        // Framework
        stack,
        ui_strong.as_weak(),
        framework.clone(),
        // Application
        app_config.clone(),
        spawner, 
        tls,
        spi_device,
        irq,
    );

    loop {
        Timer::after(Duration::from_secs(2)).await;
    }
}
