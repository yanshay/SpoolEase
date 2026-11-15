use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::gpio::AnyPin;

use framework::prelude::*;

use crate::{app_config::AppConfig, settings::MAX_NUM_PRINTERS, view_model::ViewModel};

slint::include_modules!();

pub fn create_slint_app() -> AppWindow {
    AppWindow::new().expect("Failed to load UI")
}

pub type SharedI2cDevice =
    embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice<'static, NoopRawMutex, esp_hal::i2c::master::I2c<'static, esp_hal::Async>>;

#[allow(dead_code)]
pub enum NfcHardware {
    Spi {
        device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
        irq: esp_hal::gpio::Input<'static>,
    },
    I2c {
        device: SharedI2cDevice,
        reset: esp_hal::gpio::Output<'static>,
    },
}

#[allow(dead_code)]
pub enum LocalScaleHardware {
    Hx711 { sck: AnyPin<'static>, dt: AnyPin<'static> },
    Nau7802(SharedI2cDevice),
}

pub const MAX_NUM_SSDP_LISTENERS: usize = MAX_NUM_PRINTERS + 2; // 2 for spool_scale (monitor scales + connected scale)

pub fn init_app(
    stack: Stack<'static>,
    ui: slint::Weak<AppWindow>,
    framework: Rc<RefCell<Framework>>,
    // Application
    app_config: Rc<RefCell<AppConfig>>,
    nfc_hw: Option<NfcHardware>,
    local_scale_hw: Option<LocalScaleHardware>,
) -> Rc<RefCell<ViewModel>> {
    // == Setup ViewModel =============================================================
    crate::view_model::ViewModel::new(
        // Framework
        stack,
        ui,
        framework.clone(),
        // Application
        app_config.clone(),
        nfc_hw,
        local_scale_hw,
    )
}
