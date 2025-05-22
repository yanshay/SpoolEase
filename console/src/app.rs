use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_net::Stack;
use embedded_hal_bus::spi::ExclusiveDevice;

use framework::prelude::*;

use crate::{app_config::AppConfig, settings::MAX_NUM_PRINTERS, view_model::ViewModel};

slint::include_modules!();

pub fn create_slint_app() -> AppWindow {
    AppWindow::new().expect("Failed to load UI")
}

pub const MAX_NUM_SSDP_LISTENERS: usize = MAX_NUM_PRINTERS + 2; // 2 for spool_scale (monitor scales + connected scale)

pub fn init_app(
    stack: Stack<'static>,
    ui: slint::Weak<AppWindow>,
    framework: Rc<RefCell<Framework>>,
    // Application
    app_config: Rc<RefCell<AppConfig>>,
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
) -> Rc<RefCell<ViewModel>> {
    // == Setup ViewModel =============================================================
    crate::view_model::ViewModel::new(
        // Framework
        stack,
        ui,
        framework.clone(),
        // Application
        app_config.clone(),
        spi_device,
        irq,
    )
}
