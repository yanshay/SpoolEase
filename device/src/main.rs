#![no_std]
#![feature(asm_experimental_arch)]
#![feature(type_alias_impl_trait)]
#![feature(trait_alias)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![no_main]
#![feature(associated_type_defaults)]

slint::include_modules!();

mod app;
mod app_config;
mod bambu;
mod bambu_api;
mod filament_staging;
mod my_mqtt;
mod ndef;
mod nfc;
mod pn532_ext;
mod settings;
mod spool_tag;
mod view_model;
mod web_app;

use alloc::{boxed::Box, format, rc::Rc, string::ToString};
use framework::framework::FrameworkSettings;
use settings::OTA_DOMAIN;
use settings::OTA_PATH;
use settings::OTA_TOML_FILENAME;
use settings::WEB_APP_DOMAIN;
use settings::WEB_APP_KEY_DERIVATION_ITERATIONS;
use settings::WEB_APP_SALT;
use settings::WEB_APP_SECURITY_KEY_LENGTH;
use settings::WEB_SERVER_CAPTIVE;
use settings::WEB_SERVER_HTTPS;
use settings::WEB_SERVER_PORT;
use settings::WEB_SERVER_TLS_CERTIFICATE;
use settings::WEB_SERVER_TLS_PRIVATE_KEY;
use core::cell::RefCell;
use core::net::Ipv4Addr;
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal_ota::Ota;
use esp_mbedtls::Tls;
use esp_storage::FlashStorage;
use esp_wifi::EspWifiController;
use rand::RngCore;

extern crate alloc;

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_time::{Duration, Timer};

use embedded_hal_bus::spi::ExclusiveDevice;

use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Input, Level, Output, Pull},
    ledc::timer::TimerIFace,
    psram,
    rng::Rng,
    rtc_cntl::Rtc,
    spi::{self, master::Spi},
    time::RateExtU32,
    timer::timg::TimerGroup,
    Blocking,
};

use esp_wifi::init;

use mipidsi::models::ST7796;

use framework::prelude::*;
use framework::display::SC01DislpayOutputBus;
use framework::slint_ext::McuWindow;

use app_config::AppConfig;
use settings::AP_ADDR;
use settings::WEB_SERVER_NUM_LISTENERS;
use web_app::NestedAppBuilder;

const STA_STACK_RESOURCES: usize = WEB_SERVER_NUM_LISTENERS + 4; // web-config listeners + potentially https captive + mqtt + USDP(?) + ota + captive dns
const AP_STACK_RESOURCES: usize = WEB_SERVER_NUM_LISTENERS + 4;

#[macro_export]
macro_rules! heap_dram2_allocator {
    ($size:expr) => {{
        #[link_section = ".dram2_uninit"]
        static mut HEAP2: core::mem::MaybeUninit<[u8; $size]> = core::mem::MaybeUninit::uninit();

        unsafe {
            #[allow(static_mut_refs)]
            let region = HEAP2.as_mut_ptr() as *mut u8;
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(region, $size, esp_alloc::MemoryCapability::Internal.into()));
        }
    }};
}

static mut RNG: once_cell::sync::OnceCell<esp_hal::rng::Rng> = once_cell::sync::OnceCell::new();

#[no_mangle]
unsafe extern "Rust" fn __getrandom_custom(dest: *mut u8, len: usize) -> Result<(), getrandom::Error> {
    let mut buf = unsafe {
        // fill the buffer with zeros
        core::ptr::write_bytes(dest, 0, len);
        // create mutable byte slice
        core::slice::from_raw_parts_mut(dest, len)
    };
    #[allow(static_mut_refs)]
    RNG.get_mut().unwrap().fill_bytes(&mut buf);
    Ok(())
}

fn init_psram_heap(start: *mut u8, size: usize) {
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(start, size, esp_alloc::MemoryCapability::External.into()));
    }
}

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // ==================================================================================================================================================
    // == Mandatory Infrastructure ======================================================================================================================
    // ==================================================================================================================================================

    esp_println::logger::init_logger_from_env();
    info!("Application Start");

    let mut peripherals = esp_hal::init(
        esp_hal::Config::default()
            .with_cpu_clock(CpuClock::max())
            .with_psram(psram::PsramConfig::default()),
    );

    #[allow(static_mut_refs)]
    unsafe {
        RNG.set(Rng::new(&mut peripherals.RNG)).ok();
    }

    let (start, size) = esp_hal::psram::psram_raw_parts(&peripherals.PSRAM);
    // IMPORTANT: PSRAM need to be initialized first, so 'Normal' allocations will use the region
    init_psram_heap(start, size);

    info!("Using PSRAM start: {start:x?} size: {size}");

    // Second, reserve DRAM2 area (area used by bootloader during boot)
    heap_dram2_allocator!(64 * 1024);

    // Last, reserve from 'standard' area, if need additional memory for esp-wifi/esp-mbedtls, need to increase this
    esp_alloc::heap_allocator!(132 * 1024);

    // == Setup timers & delay ========================================================

    let mut delay = esp_hal::delay::Delay::new();
    let rtc = Rtc::new(peripherals.LPWR);
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    // == Create Tls ==================================================================
    let tls = mk_static!(Tls<'static>, Tls::new(peripherals.SHA).unwrap().with_hardware_rsa(peripherals.RSA));
    tls.set_debug(0);

    // == Initialize Embassy ==========================================================

    esp_hal_embassy::init(timg0.timer1);

    // == Setup Display Interface (di) ================================================

    debug!("Setting up display interface");

    let di_wr = Output::new(&mut peripherals.GPIO47, Level::High);
    let di_dc = Output::new(&mut peripherals.GPIO0, Level::High);
    let di_bl = peripherals.GPIO45;
    let di_rst = Output::new(peripherals.GPIO4, Level::High);

    let fastbus = SC01DislpayOutputBus::new();
    let di = display_interface_parallel_gpio::PGPIO8BitInterface::new(fastbus, di_dc, di_wr);

    // Initialize display using standard mipidsi dislay driver, then switch to faster display method for screen data
    let display = mipidsi::Builder::new(ST7796, di)
        .display_size(320, 480)
        .invert_colors(mipidsi::options::ColorInversion::Inverted)
        .color_order(mipidsi::options::ColorOrder::Bgr)
        .orientation(
            mipidsi::options::Orientation::new()
                .rotate(mipidsi::options::Rotation::Deg270)
                .flip_horizontal(),
        )
        .reset_pin(di_rst)
        .init(&mut delay)
        .unwrap();

    let (di, _model, _rst) = display.release();
    let (_bus, _di_dc, _di_wr) = di.release();

    // Display initialization is done, now switch to LCD_CAM/DMA for driving data fast to the display

    let lcd_cam = esp_hal::lcd_cam::LcdCam::new(peripherals.LCD_CAM);

    let tx_pins = esp_hal::lcd_cam::lcd::i8080::TxEightBits::new(
        peripherals.GPIO9,
        peripherals.GPIO46,
        peripherals.GPIO3,
        peripherals.GPIO8,
        peripherals.GPIO18,
        peripherals.GPIO17,
        peripherals.GPIO16,
        peripherals.GPIO15,
    );

    let di_wr = peripherals.GPIO47;
    let di_dc = peripherals.GPIO0;

    let mut i8080_config = esp_hal::lcd_cam::lcd::i8080::Config::default();
    i8080_config.frequency = 40.MHz();

    let mut i8080 = esp_hal::lcd_cam::lcd::i8080::I8080::new(lcd_cam.lcd, peripherals.DMA_CH0, tx_pins, i8080_config)
        .unwrap()
        .with_ctrl_pins(di_dc, di_wr);
    i8080.set_8bits_order(esp_hal::lcd_cam::ByteOrder::Inverted);

    let (_, _, tx_buffer0, tx_descriptors0) = dma_buffers!(0, 480 * core::mem::size_of::<slint::platform::software_renderer::Rgb565Pixel>());
    let (_, _, tx_buffer1, tx_descriptors1) = dma_buffers!(0, 480 * core::mem::size_of::<slint::platform::software_renderer::Rgb565Pixel>());
    let dma_buf0 = DmaTxBuf::new(tx_descriptors0, tx_buffer0).unwrap();
    let dma_buf1 = DmaTxBuf::new(tx_descriptors1, tx_buffer1).unwrap();

    let (_, _, tx_buffer_cmd, tx_descriptors_cmd) = dma_buffers!(0, 4);
    let dma_buf_cmd = DmaTxBuf::new(tx_descriptors_cmd, tx_buffer_cmd).unwrap();

    let buffer_provider = framework::display::DrawBuffer {
        i8080: Some(i8080),
        dma_buf0: Some(dma_buf0),
        dma_buf1: Some(dma_buf1),
        dma_buf_cmd: Some(dma_buf_cmd),
        transfer: None,
        curr_buffer: 0,
        prev_range: core::ops::Range::<usize> { start: 10000, end: 10000 },
        prev_line: 0,
    };

    // Initialize backlight pwm control
    let mut ledc = esp_hal::ledc::Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);
    let lstimer0: &mut esp_hal::ledc::timer::Timer<esp_hal::ledc::LowSpeed> = mk_static!(
        esp_hal::ledc::timer::Timer<esp_hal::ledc::LowSpeed>,
        ledc.timer::<esp_hal::ledc::LowSpeed>(esp_hal::ledc::timer::Number::Timer0)
    );
    lstimer0
        .configure(esp_hal::ledc::timer::config::Config {
            duty: esp_hal::ledc::timer::config::Duty::Duty5Bit,
            clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
            frequency: 24u32.kHz(),
        })
        .unwrap();
    let channel0 = ledc.channel(esp_hal::ledc::channel::Number::Channel0, di_bl);

    // == Setup the Slint Bacdkend ====================================================

    let size = slint::PhysicalSize::new(480, 320);
    let window = McuWindow::new(slint::platform::software_renderer::RepaintBufferType::ReusedBuffer);
    window.set_size(size);
    let rtc = Rc::new(rtc); // using Rc so we'll have access to the rtc later if needed
    slint::platform::set_platform(Box::new(framework::display::EspBackend {
        window: window.clone(),
        rtc: rtc.clone(),
    }))
    .expect("backend already initialized");

    // == Setup Touch Interface =======================================================

    debug!("Setting up touch interface");

    let ti_sda = peripherals.GPIO6; //.into_push_pull_output();
    let ti_scl = peripherals.GPIO5; //.into_push_pull_output();
    let ti_irq = Input::new(peripherals.GPIO7, Pull::Down); //.into_push_pull_output();

    // TODO: Check the option of switching to async I2C instead of my own interrupt approach
    // let _ti_i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, {
    //     let mut config = esp_hal::i2c::master::Config::default();
    //     config.frequency = 400u32.kHz();
    //     config
    // });

    let ti_i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, esp_hal::i2c::master::Config::default().with_frequency(400.kHz()))
        .unwrap()
        .with_sda(ti_sda)
        .with_scl(ti_scl);

    esp_hal::interrupt::enable(esp_hal::peripherals::Interrupt::GPIO, esp_hal::interrupt::Priority::Priority3).unwrap();

    // == Setup Flash Storage =========================================================

    debug!("Setting up flash storage");

    let storage = FlashStorage::new();

    // == Setup Flash Map =============================================================

    debug!("Setting up Flash Map");

    let blocking_async_storage = BlockingAsync::new(storage);
    let flash_map = FlashMap::new_in_region(blocking_async_storage, "map", 1024, env!("CARGO_PKG_NAME")).await;
    let flash_map = match flash_map {
        Ok(v) => v,
        Err(err) => {
            error!("Error setting up flash map: {err:?}");
            // TODO: reorder/reorganize config /app/ui so can display errors if there are such during flash initialization
            // boot.borrow().add_text_new_line("Can't initialize flash, boot halted!");
            return;
        }
    };
    let flash_map = Rc::new(RefCell::new(flash_map));

    // == Prepare Wifi Structs ========================================================

    debug!("Setting up Wifi Structs");

    let init = &*mk_static!(
        EspWifiController<'static>,
        init(timg0.timer0, Rng::new(&mut peripherals.RNG), peripherals.RADIO_CLK,).unwrap()
    );
    let wifi = peripherals.WIFI;

    let (wifi_ap_interface, wifi_sta_interface, controller) = esp_wifi::wifi::new_ap_sta(&init, wifi).unwrap();

    let sta_config = Config::dhcpv4(Default::default());

    let seed: u64 = 0;
    let mut seed_bytes = seed.to_ne_bytes();
    getrandom::getrandom(&mut seed_bytes).unwrap();

    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_sta_interface,
        sta_config,
        mk_static!(StackResources<STA_STACK_RESOURCES>, StackResources::<STA_STACK_RESOURCES>::new()),
        seed,
    );
    let ap_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Addr::new(AP_ADDR.0, AP_ADDR.1, AP_ADDR.2, AP_ADDR.3), 24),
        gateway: Some(Ipv4Addr::new(AP_ADDR.0, AP_ADDR.1, AP_ADDR.2, AP_ADDR.3)),
        dns_servers: Default::default(),
    });
    let (ap_stack, ap_runner) = embassy_net::new(
        wifi_ap_interface,
        ap_config,
        mk_static!(StackResources<AP_STACK_RESOURCES>, StackResources::<AP_STACK_RESOURCES>::new()),
        seed,
    );

    // == Prepare Framework ===========================================================

    debug!("Setting up Framework Config");

    let framework_settings = FrameworkSettings {
        ota_domain: OTA_DOMAIN,
        ota_path: OTA_PATH,
        ota_toml_filename: OTA_TOML_FILENAME,
        ota_certs: concat!(include_str!("./certs/ota-certs.pem"), "\0"),

        ap_addr: AP_ADDR,

        web_server_https: WEB_SERVER_HTTPS,
        web_server_port: WEB_SERVER_PORT,
        web_server_captive: WEB_SERVER_CAPTIVE,
        web_server_num_listeners: WEB_SERVER_NUM_LISTENERS,
        web_server_tls_certificate: WEB_SERVER_TLS_CERTIFICATE,
        web_server_tls_private_key: WEB_SERVER_TLS_PRIVATE_KEY,

        web_app_domain: WEB_APP_DOMAIN,
        web_app_security_key_length: WEB_APP_SECURITY_KEY_LENGTH,
        web_app_salt: WEB_APP_SALT,
        web_app_key_derivation_iterations: WEB_APP_KEY_DERIVATION_ITERATIONS,

        app_cargo_pkg_name: env!("CARGO_PKG_NAME"),
        app_cargo_pkg_version: env!("CARGO_PKG_VERSION"),
    };

    let framework = Framework::new(
        framework_settings,
        flash_map.clone(),
        spawner,
        sta_stack,
        tls.reference(),
    );

    let app_config = Rc::new(RefCell::new(AppConfig::new(framework.clone())));

    // == Configure the App UI ========================================================
    // (need to be done after the call to slint::platform::set_platform)

    debug!("Configuring App UI");

    let ui: &mut crate::app::AppWindow = mk_static!(crate::app::AppWindow, crate::app::create_slint_app());
    spawner
        .spawn(framework::display::event_loop(
            ti_i2c,
            ti_irq,
            window,
            buffer_provider,
            channel0,
            lstimer0,
            size,
            framework.clone(),
        ))
        .ok();

    // == Setup Web Application and Run Web Server ====================================

    let web_app_builder = framework::framework_web_app::WebAppProps::<NestedAppBuilder> {
        framework: framework.clone(),
        captive_html: include_str!("../static/captive.html"),
        web_app_html: include_str!("../static/config.html"),
        app_builder: NestedAppBuilder {
            framework: framework.clone(),
            app_config: app_config.clone(),
        },
    };

    let web_app_router = mk_static!(
        picoserve::AppRouter<framework::framework_web_app::WebAppProps<NestedAppBuilder>>,
        picoserve::AppWithStateBuilder::build_app(web_app_builder)
    );

    let web_app_state = mk_static!(
        framework::framework_web_app::WebAppState,
        framework::framework_web_app::WebAppState::new(framework.borrow().encryption_key)
    );

    let web_server_runner = mk_static!(
        framework::web_server::Runner<NestedAppBuilder>,
        framework::web_server::Runner::new(framework.clone(), web_app_router, web_app_state, spawner, framework.borrow().web_server_commands, tls.reference())
    );

    for id in 0..WEB_SERVER_NUM_LISTENERS {
        debug!("// spawning web-task {id}");
        spawner.spawn(web_server_task(web_server_runner, id)).unwrap();
    }

    // == Mark current app ota is working =============================================
    {
        // where should this be located?  as early as possible or only after initialization worked?
        let mut ota = Ota::new(FlashStorage::new()).expect("Cannot create ota");
        ota.ota_mark_app_valid().unwrap();
        term_info!("Booted from partition : {:?}", ota.get_currently_booted_partition());
    }

    // ==================================================================================================================================================
    // == Optional Infrastructure =======================================================================================================================
    // ==================================================================================================================================================

    // == Setup the sdcard ============================================================

    debug!("Setting up SDCard");

    let sd_cs = Output::new(peripherals.GPIO41, Level::High);
    let sd_sclk = peripherals.GPIO39;
    let sd_miso = peripherals.GPIO38;
    let sd_mosi = peripherals.GPIO40;

    let spi_bus = Spi::new(
        peripherals.SPI3,
        spi::master::Config::default().with_frequency(2.MHz()).with_mode(spi::Mode::_0),
    )
    .unwrap()
    .with_sck(sd_sclk)
    .with_miso(sd_miso)
    .with_mosi(sd_mosi);

    let sdcard_spi_device = ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap();

    let sdcard = mk_static!(
        framework::sdcard::SDCard<
            embedded_hal_bus::spi::ExclusiveDevice<
                esp_hal::spi::master::Spi<'_, Blocking>,
                esp_hal::gpio::Output<'_>,
                embedded_hal_bus::spi::NoDelay,
            >,
            esp_hal::delay::Delay,
        >,
        framework::sdcard::SDCard::new(sdcard_spi_device, delay)
    );

    // == Load Configuration from SDCard, required here for WiFi ssid & password ======

    let config_filename = format!("/{}.cfg", env!("CARGO_PKG_NAME").to_lowercase());
    term_info!("Loading config file '{}' from SDCard", config_filename);

    let read_file_str = sdcard.read_file_str(&config_filename);
    let config_toml = match read_file_str {
        Ok(config_toml) => {
            term_info!("Read config file '{}' from SDCard", config_filename);
            config_toml
        }
        Err(e) => {
            term_error!("Failed to load config file '{}' : {}", config_filename, e);
            "".to_string()
            // SDCard is not mandatory, so can continue
        }
    };

    // == Load configuration ==========================================================

    let _ = framework.borrow_mut().load_config_flash_then_toml(&config_toml);
    let _ = app_config.borrow_mut().load_config_flash_then_toml(&config_toml);

    // == Setup Serial for Improv Wifi ================================================

    let (rx, tx) = esp_hal::usb_serial_jtag::UsbSerialJtag::new(peripherals.USB_DEVICE).into_async().split();

    // == Setup Wifi ==================================================================

    debug!("Setting up Wifi");

    spawner
        .spawn(framework::wifi::connection(
            controller,
            sta_stack,
            ap_stack,
            rx,
            tx,
            framework.clone(),
        ))
        .ok();
    spawner.spawn(framework::wifi::sta_net_task(sta_runner)).ok();
    spawner.spawn(framework::wifi::ap_net_task(ap_runner)).ok(); // TODO: Maybe move this to run only when needed (in wifi.rs)

    // ==================================================================================================================================================
    // == Applicative Initialization ====================================================================================================================
    // ==================================================================================================================================================

    // == Setup PN532 =================================================================

    // PN532

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(64);
    let spi_dma_rx_buf = esp_hal::dma::DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
    let spi_dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();
    let pn532_irq = Input::new(peripherals.GPIO14, Pull::None);

    let sck = peripherals.GPIO13;
    let mosi = Output::new(peripherals.GPIO11, Level::High);
    let miso = peripherals.GPIO12;
    let cs = Output::new(peripherals.GPIO10, Level::High);

    let spi = Spi::new(
        peripherals.SPI2,
        esp_hal::spi::master::Config::default()
            .with_frequency(100.kHz())
            .with_mode(spi::Mode::_0)
            .with_read_bit_order(spi::BitOrder::LsbFirst)
            .with_write_bit_order(spi::BitOrder::LsbFirst),
    )
    .unwrap()
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso)
    // .with_cs(cs) // cs is handled by the ExclusiveDevice
    // .with_dma(spi_dma_channel.configure(false, esp_hal::dma::DmaPriority::Priority0))
    .with_dma(peripherals.DMA_CH1)
    .with_buffers(spi_dma_rx_buf, spi_dma_tx_buf)
    .into_async();

    let pn532_spi_device = embedded_hal_bus::spi::ExclusiveDevice::new(spi, cs, embassy_time::Delay).unwrap();

    // == Configure App ===============================================================
    // This initializes all the applicative stuff, and is provided with all the required hw access

    spawner
        .spawn(crate::app::app_task(
            sta_stack,
            ui.as_weak(),
            framework.clone(),
            tls.reference(),
            app_config.clone(),
            pn532_spi_device,
            pn532_irq,
        ))
        .ok();

    Timer::after(Duration::from_millis(200)).await;
    framework.borrow().notify_initialization_completed(app_config.borrow().initialization_ok());

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[embassy_executor::task(pool_size = WEB_SERVER_NUM_LISTENERS)]
async fn web_server_task(runner: &'static framework::web_server::Runner<NestedAppBuilder>, id: usize) {
    runner.run(id).await;
}
