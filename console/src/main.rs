#![no_std]
#![feature(asm_experimental_arch)]
#![feature(type_alias_impl_trait)]
#![feature(trait_alias)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![no_main]
#![feature(associated_type_defaults)]
#![recursion_limit = "256"] // due to picoserve complex types & embassy

mod app;
mod app_config;
mod bambu;
mod bambu_api;
mod color_utils;
mod filament_staging;
mod my_mqtt;
mod settings;
mod spool_scale;
mod ssdp;
mod view_model;
mod web_app;

use alloc::{format, rc::Rc, string::ToString};
use core::{cell::RefCell, net::Ipv4Addr};
use embassy_futures::yield_now;
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal_ota::Ota;
use esp_mbedtls::Tls;
use esp_storage::FlashStorage;
use esp_wifi::{init, EspWifiController};
use slint::ComponentHandle;

extern crate alloc;

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};

use embedded_hal_bus::spi::ExclusiveDevice;

use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Input, Level, Output, Pull},
    psram,
    rng::Rng,
    rtc_cntl::Rtc,
    spi::{self, master::Spi},
    time::RateExtU32,
    timer::timg::TimerGroup,
};

use framework::prelude::*;
use framework::{
    framework::FrameworkSettings,
    sdcard_store::SDCardStore,
    wt32_sc01_plus::{WT32SC01Plus, WT32SC01PlusDisplayPeripherals, WT32SC01PlusRunner, WT32SC01PlusSDCardPeripherals},
    RNG,
};

use app_config::AppConfig;
use settings::WEB_SERVER_NUM_LISTENERS;
use settings::{AP_ADDR, MAX_NUM_PRINTERS, OTA_TLS_CERTIFICATE};
use settings::{
    OTA_DOMAIN, OTA_PATH, OTA_TOML_FILENAME, WEB_APP_DOMAIN, WEB_APP_KEY_DERIVATION_ITERATIONS, WEB_APP_SALT, WEB_APP_SECURITY_KEY_LENGTH,
    WEB_SERVER_CAPTIVE, WEB_SERVER_HTTPS, WEB_SERVER_PORT, WEB_SERVER_TLS_CERTIFICATE, WEB_SERVER_TLS_PRIVATE_KEY,
};
use web_app::NestedAppBuilder;
const STA_STACK_RESOURCES: usize = WEB_SERVER_NUM_LISTENERS + 1 + MAX_NUM_PRINTERS + FRAMEWORK_STA_STACK_RESOURCES; // web-config listeners + USDP + mqtt*num-of-printers + from framework: potentially https captive +  ota + captive dns + ? initial firmware check if doen't complete
const AP_STACK_RESOURCES: usize = WEB_SERVER_NUM_LISTENERS + FRAMEWORK_AP_STACK_RESOURCES;

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
    esp_alloc::heap_allocator!(100 * 1024);

    // == Setup timers & delay ========================================================

    let _rtc: Rtc<'static> = Rtc::new(peripherals.LPWR); // don't move from here, will cause all kinds of timer/embassy
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    // == Create Tls ==================================================================

    let tls = mk_static!(Tls<'static>, Tls::new(peripherals.SHA).unwrap().with_hardware_rsa(peripherals.RSA));
    tls.set_debug(0);

    // == Initialize Embassy ==========================================================

    esp_hal_embassy::init(timg0.timer1);

    // == Setup Flash Storage =========================================================

    debug!("Setting up flash storage");

    let storage = FlashStorage::new();

    // == Setup Flash Map =============================================================

    debug!("Setting up Flash Map");

    let blocking_async_storage = BlockingAsync::new(storage);
    let flash_map = FlashMap::new_in_region(blocking_async_storage, "map", 2560, env!("CARGO_PKG_NAME")).await;
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

    let (wifi_ap_interface, wifi_sta_interface, controller) = esp_wifi::wifi::new_ap_sta(init, wifi).unwrap();

    let sta_config = Config::dhcpv4(Default::default());

    let mut seed_bytes = [0u8; 8];
    getrandom::getrandom(&mut seed_bytes).unwrap();
    let seed = u64::from_le_bytes(seed_bytes);

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
        ota_certs: OTA_TLS_CERTIFICATE,

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
        default_fixed_security_key: None,

        mdns: true,
    };

    let framework = Framework::new(framework_settings, flash_map.clone(), spawner, sta_stack, tls.reference(), None);

    // == Setup Display Interface =====================================================

    let display_peripherals = WT32SC01PlusDisplayPeripherals {
        GPIO47: peripherals.GPIO47,
        GPIO0: peripherals.GPIO0,
        GPIO45: peripherals.GPIO45,
        GPIO4: peripherals.GPIO4,
        LCD_CAM: peripherals.LCD_CAM,
        GPIO9: peripherals.GPIO9,
        GPIO46: peripherals.GPIO46,
        GPIO3: peripherals.GPIO3,
        GPIO8: peripherals.GPIO8,
        GPIO18: peripherals.GPIO18,
        GPIO17: peripherals.GPIO17,
        GPIO16: peripherals.GPIO16,
        GPIO15: peripherals.GPIO15,
        LEDC: peripherals.LEDC,
        GPIO5: peripherals.GPIO5,
        GPIO6: peripherals.GPIO6,
        GPIO7: peripherals.GPIO7,
        DMA_CHx: peripherals.DMA_CH0,
        I2Cx: peripherals.I2C0,
    };

    let sdcard_peripherals = WT32SC01PlusSDCardPeripherals {
        // SDCard
        GPIO38: peripherals.GPIO38,
        GPIO39: peripherals.GPIO39,
        GPIO40: peripherals.GPIO40,
        GPIO41: peripherals.GPIO41,
        SPIx: peripherals.SPI3,
    };

    let display_orientation = mipidsi::options::Orientation::new()
        .rotate(mipidsi::options::Rotation::Deg270)
        .flip_horizontal();
    let (display, runner, sdcard_device) = WT32SC01Plus::new(display_peripherals, sdcard_peripherals, display_orientation, framework.clone());

    spawner.spawn(display_runner(runner)).ok();
    let _ = display.wait_init_done().await; // important to wait for init stage to complete before moving on

    // == Configure the App UI ========================================================
    // (need to be done after the call to slint::platform::set_platform)

    debug!("Configuring App UI");

    let ui: &mut crate::app::AppWindow = mk_static!(crate::app::AppWindow, crate::app::create_slint_app());

    let app_config = Rc::new(RefCell::new(AppConfig::new(framework.clone())));

    // == Mark current app ota is working =============================================
    let boot_partition;
    {
        // where should this be located?  as early as possible or only after initialization worked?
        let mut ota = Ota::new(FlashStorage::new()).expect("Cannot create ota");
        ota.ota_mark_app_valid().unwrap();
        if let Some(partition) = ota.get_currently_booted_partition() {
            boot_partition = format!("{partition}");
        } else {
            boot_partition = "default".to_string();
        }
    }

    // ==================================================================================================================================================
    // == Optional Infrastructure =======================================================================================================================
    // ==================================================================================================================================================

    // == Setup the sdcard ============================================================

    debug!("Setting up SDCard");

    Framework::set_sdcard_device(framework.clone(), sdcard_device).await;
    // framework.borrow_mut().set_sdcard_device(sdcard_device).await;
    // framework.bo
    // let file_store = framework.borrow().file_store.clone();

    // let file_store = SDCardStore::<_, 20, 5>::new(sdcard_device).await;
    // let file_store = Rc::new(Mutex::<CriticalSectionRawMutex, SDCardStore<_, 20, 5>>::new(file_store));

    // == Load Configuration from SDCard, required here for WiFi ssid & password ======

    let file_store = framework.borrow().file_store();
    let config_toml = {
        let mut file_store = file_store.lock().await;
        let config_filename = "/config/console.cfg";
        term_info!("Loading optional config file '{}' from SDCard", config_filename);

        let read_file_str = file_store.read_file_str(config_filename).await;
        match read_file_str {
            Ok(config_toml) => {
                term_info!("Read config file '{}' from SDCard", config_filename);
                config_toml
            }
            Err(e) => {
                term_info!("Failed to load optional config file '{}' : {}", config_filename, e);
                "".to_string()
                // SDCard is not mandatory, so can continue
            }
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
        .spawn(framework::wifi::connection(controller, sta_stack, ap_stack, rx, tx, framework.clone()))
        .ok();
    spawner.spawn(framework::wifi::sta_net_task(sta_runner)).ok();
    spawner.spawn(framework::wifi::ap_net_task(ap_runner)).ok(); // TODO: Maybe move this to run only when needed (in wifi.rs)

    // ===============================================================================================================================================
    // == Applicative Initialization =================================================================================================================
    // ===============================================================================================================================================

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
            .with_frequency(2000.kHz())
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

    let view_model = crate::app::init_app(
        sta_stack,
        ui.as_weak(),
        framework.clone(),
        app_config.clone(),
        pn532_spi_device,
        pn532_irq,
    );

    // == Setup Web Application and Run Web Server ====================================

    let web_app_builder = framework::framework_web_app::WebAppBuilder::<NestedAppBuilder> {
        framework: framework.clone(),
        captive_html: include_str!("../static/captive.html"),
        web_app_html: include_str!("../static/config.html"),
        app_builder: NestedAppBuilder {
            framework: framework.clone(),
            app_config: app_config.clone(),
            view_model: view_model.clone(),
        },
    };

    let web_app_router = mk_static!(
        picoserve::AppRouter<framework::framework_web_app::WebAppBuilder<NestedAppBuilder>>,
        picoserve::AppWithStateBuilder::build_app(web_app_builder)
    );

    let web_app_state = mk_static!(
        framework::framework_web_app::WebAppState,
        framework::framework_web_app::WebAppState::new(framework.borrow().encryption_key)
    );

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Some(Duration::from_secs(5)),
        read_request: Some(Duration::from_millis(5000)),
        write: Some(Duration::from_millis(5000)),
    })
    .keep_connection_alive();

    let web_server_runner = mk_static!(
        framework::web_server::WebAppRunner<NestedAppBuilder>,
        framework::web_server::WebAppRunner::new(framework.clone(), web_app_router, web_app_state, config)
    );

    for id in 0..WEB_SERVER_NUM_LISTENERS {
        spawner.spawn(web_server_task(web_server_runner, id)).unwrap();
    }

    // yields for term initialization to complete until term is fixed to not require this
    yield_now().await;
    yield_now().await;
    yield_now().await;
    yield_now().await;
    term_info!("Booting from partition {}", boot_partition);

    loop {
        if app_config.borrow().initialization_ok(false).is_some() {
            break;
        }
        Timer::after(Duration::from_millis(250)).await;
    }

    framework
        .borrow()
        .notify_initialization_completed(app_config.borrow().initialization_ok(true).unwrap());

    Framework::wait_for_wifi(&framework).await; // this is mostly to start the web app after all tasks initialized and won't miss this start message
    framework.borrow_mut().start_web_app(sta_stack, framework::framework::WebConfigMode::STA);

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[embassy_executor::task(pool_size = WEB_SERVER_NUM_LISTENERS)]
async fn web_server_task(runner: &'static framework::web_server::WebAppRunner<NestedAppBuilder>, id: usize) {
    runner.run(id).await;
}

#[embassy_executor::task]
pub async fn display_runner(mut runner: WT32SC01PlusRunner<esp_hal::dma::DmaChannel0, esp_hal::peripherals::I2C0>) {
    runner.run().await;
}
