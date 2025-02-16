use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_mbedtls::TlsReference;

use framework::prelude::*;

use crate::{app_config::AppConfig, bambu, spool_tag};

slint::include_modules!();

pub fn create_slint_app() -> AppWindow {
    AppWindow::new().expect("Failed to load UI")
}

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
    // == Setup Bambu Printer Model ===================================================

    let bambu_printer_model = bambu::init(stack, app_config.clone(), tls).await;

    // == Setup spool_tag =============================================================

    let spool_tag_model = spool_tag::init(spi_device, irq, app_config.clone()).await;

    // == Setup ViewModel =============================================================
    let ui_strong = ui.upgrade().unwrap();
    let view_model = crate::view_model::ViewModel::new(
        // Framework
        stack,
        ui_strong.as_weak(),
        framework.clone(),
        // Application
        app_config.clone(),
        bambu_printer_model,
        spool_tag_model,
    );

    (*view_model).borrow_mut().init();

    loop {
        Timer::after(Duration::from_secs(2)).await;
    }
}
