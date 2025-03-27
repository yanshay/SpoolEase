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
mod settings;
mod web_app;
mod console_proxy;
mod load_cell;
mod ssdp;

use alloc::{format, rc::Rc, string::ToString};
use core::{cell::RefCell, net::Ipv4Addr};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal_ota::Ota;
use esp_mbedtls::Tls;
use esp_storage::FlashStorage;
use esp_wifi::{init, EspWifiController};
use framework::framework::FrameworkSettings;
use framework::RNG;

extern crate alloc;

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_time::{Duration, Timer};

use esp_backtrace as _;
use esp_hal::{clock::CpuClock, rng::Rng, rtc_cntl::Rtc, timer::timg::TimerGroup};

use framework::prelude::*;

use app_config::AppConfig;
use settings::AP_ADDR;
use settings::WEB_SERVER_NUM_LISTENERS;
use settings::{
    OTA_DOMAIN, OTA_PATH, OTA_TOML_FILENAME, WEB_APP_DOMAIN, WEB_APP_KEY_DERIVATION_ITERATIONS,
    WEB_APP_SALT, WEB_APP_SECURITY_KEY_LENGTH, WEB_SERVER_CAPTIVE, WEB_SERVER_HTTPS,
    WEB_SERVER_PORT, WEB_SERVER_TLS_CERTIFICATE, WEB_SERVER_TLS_PRIVATE_KEY,
};
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
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                region,
                $size,
                esp_alloc::MemoryCapability::Internal.into(),
            ));
        }
    }};
}

fn init_psram_heap(start: *mut u8, size: usize) {
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            start,
            size,
            esp_alloc::MemoryCapability::External.into(),
        ));
    }
}

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // ==================================================================================================================================================
    // == Mandatory Infrastructure ======================================================================================================================
    // ==================================================================================================================================================

    esp_println::logger::init_logger_from_env();
    info!("Application Start");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);

    // == Setup Standard Random Generator =============================================

    #[allow(static_mut_refs)]
    unsafe {
        RNG.set(Rng::new(&mut peripherals.RNG)).ok();
    }

    // == Setup Heap Memory ===========================================================

    // IMPORTANT: PSRAM need to be initialized first, so 'Normal' allocations will use the region
    let (start, size) = esp_hal::psram::psram_raw_parts(&peripherals.PSRAM);
    init_psram_heap(start, size);
    info!("Using PSRAM start: {start:x?} size: {size}");

    // Second, reserve DRAM2 area (area used by bootloader during boot)
    heap_dram2_allocator!(64 * 1024);

    // Last, reserve from 'standard' area, if need additional memory for esp-wifi/esp-mbedtls, need to increase this
    esp_alloc::heap_allocator!(32 * 1024);

    // == Setup timers & delay ========================================================

    let _delay = esp_hal::delay::Delay::new();
    let _rtc: Rtc<'static> = Rtc::new(peripherals.LPWR); // don't move from here, will cause all kinds of timer/embassy
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    // == Create Tls ==================================================================

    let tls = mk_static!(
        Tls<'static>,
        Tls::new(peripherals.SHA)
            .unwrap()
            .with_hardware_rsa(peripherals.RSA)
    );
    tls.set_debug(0);

    // == Initialize Embassy ==========================================================

    esp_hal_embassy::init(timg0.timer1);

    // == Setup Flash Storage =========================================================

    let storage = FlashStorage::new();

    // == Setup Flash Map =============================================================

    debug!("Setting up Flash Map");

    let blocking_async_storage = BlockingAsync::new(storage);
    let flash_map =
        FlashMap::new_in_region(blocking_async_storage, "map", 1024, env!("CARGO_PKG_NAME")).await;
    let flash_map = match flash_map {
        Ok(v) => v,
        Err(err) => {
            error!("Fatal: Error setting up flash map: {err:?}");
            return;
        }
    };
    let flash_map = Rc::new(RefCell::new(flash_map));

    // == Prepare Wifi Structs ========================================================

    debug!("Setting up Wifi Structs");

    let init = &*mk_static!(
        EspWifiController<'static>,
        init(
            timg0.timer0,
            Rng::new(&mut peripherals.RNG),
            peripherals.RADIO_CLK,
        )
        .unwrap()
    );
    let wifi = peripherals.WIFI;

    let (wifi_ap_interface, wifi_sta_interface, controller) =
        esp_wifi::wifi::new_ap_sta(&init, wifi).unwrap();

    let sta_config = Config::dhcpv4(Default::default());

    let seed: u64 = 0;
    let mut seed_bytes = seed.to_ne_bytes();
    getrandom::getrandom(&mut seed_bytes).unwrap();

    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_sta_interface,
        sta_config,
        mk_static!(
            StackResources<STA_STACK_RESOURCES>,
            StackResources::<STA_STACK_RESOURCES>::new()
        ),
        seed,
    );
    let ap_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(
            Ipv4Addr::new(AP_ADDR.0, AP_ADDR.1, AP_ADDR.2, AP_ADDR.3),
            24,
        ),
        gateway: Some(Ipv4Addr::new(AP_ADDR.0, AP_ADDR.1, AP_ADDR.2, AP_ADDR.3)),
        dns_servers: Default::default(),
    });
    let (ap_stack, ap_runner) = embassy_net::new(
        wifi_ap_interface,
        ap_config,
        mk_static!(
            StackResources<AP_STACK_RESOURCES>,
            StackResources::<AP_STACK_RESOURCES>::new()
        ),
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
        default_fixed_security_key: Some("Replace-Me!".to_string()),
    };

    let framework = Framework::new(
        framework_settings,
        flash_map.clone(),
        spawner,
        sta_stack,
        tls.reference(),
        Some(peripherals.GPIO0.into())
    );

    // == Configure the App UI ========================================================
    // (need to be done after the call to slint::platform::set_platform)

    let app_config = Rc::new(RefCell::new(AppConfig::new(framework.clone())));

    // == Setup Web Application and Run Web Server ====================================

    let web_app_builder = framework::framework_web_app::WebAppBuilder::<NestedAppBuilder> {
        framework: framework.clone(),
        captive_html: include_str!("../static/captive.html"),
        web_app_html: include_str!("../static/config.html"),
        app_builder: NestedAppBuilder {
            framework: framework.clone(),
            app_config: app_config.clone(),
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

    let web_app_runner = mk_static!(
        framework::web_server::WebAppRunner<NestedAppBuilder>,
        framework::web_server::WebAppRunner::new(
            framework.clone(),
            web_app_router,
            web_app_state,
            config,
        )
    );

    for id in 0..WEB_SERVER_NUM_LISTENERS {
        debug!("Spawning web-task {id}");
        spawner
            .spawn(web_server_task(web_app_runner, id))
            .unwrap();
    }

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

    info!("Booting from partition {}", boot_partition);

    // ==================================================================================================================================================
    // == Optional Infrastructure =======================================================================================================================
    // ==================================================================================================================================================

    // == Load configuration ==========================================================

    let config_toml = "";
    let _ = framework
        .borrow_mut()
        .load_config_flash_then_toml(&config_toml);
    let _ = app_config
        .borrow_mut()
        .load_config_flash_then_toml(&config_toml);

    // == Setup Serial for Improv Wifi ================================================

    // let mut uart1 = Uart::new(
    //     peripherals.UART1,
    //     Config::default())
    //     .unwrap()
    //     .with_rx(peripherals.GPIO1)
    //     .with_tx(peripherals.GPIO2);

    // let mut uart = esp_hal::uart::Uart::new(peripherals.UART1, esp_hal::uart::Config::default()).unwrap();
    // let (rx, tx) = uart.into_async().split();

    let (rx, tx) = esp_hal::usb_serial_jtag::UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

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
    spawner
        .spawn(framework::wifi::sta_net_task(sta_runner))
        .ok();
    spawner.spawn(framework::wifi::ap_net_task(ap_runner)).ok(); // TODO: Maybe move this to run only when needed (in wifi.rs)

    // ==================================================================================================================================================
    // == Applicative Initialization ====================================================================================================================
    // ==================================================================================================================================================

    // == Configure App ===============================================================
    // This initializes all the applicative stuff, and is provided with all the required hw access

    spawner
        .spawn(crate::app::app_task(
            framework.clone(),
            app_config.clone(),
            peripherals.GPIO4.into(),
            peripherals.GPIO5.into(),
            peripherals.SPI3.into(),
        ))
        .ok();   // // yields for term initialization to complete until term is fixed to not require this

    framework
        .borrow()
        .notify_initialization_completed(app_config.borrow().initialization_ok());

    info!("--------------------------------------------");
    info!(" Current security key is {}", framework.borrow().fixed_key.clone().unwrap_or("Ooops - no security key".to_string()));
    info!("--------------------------------------------");

    Framework::wait_for_wifi(&framework).await;// this is mostly to start the web app after all tasks initialized and won't miss this start message
    framework
        .borrow()
        .start_web_app(sta_stack, framework::framework::WebConfigMode::STA);

    loop {
        Timer::after_secs(60).await;
    }
}

#[embassy_executor::task(pool_size = WEB_SERVER_NUM_LISTENERS)]
async fn web_server_task(
    runner: &'static framework::web_server::WebAppRunner<NestedAppBuilder>,
    id: usize,
) {
    runner.run(id).await;
}
