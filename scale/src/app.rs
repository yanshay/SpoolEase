use core::cell::RefCell;

use alloc::{
    rc::{Rc, Weak},
    string::ToString,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubBehavior};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{gpio::AnyPin, peripherals::RMT, spi::AnySpi};
use framework::{
    debug, error,
    framework::{FrameworkObserver, WebConfigMode, WebServerCommands},
    info, mk_static,
    prelude::Framework,
    terminal::{self, term_mut, TerminalObserver},
    web_server::WebServerCommand,
};
use num_traits::abs;
use shared::{scale::{ConsoleToScale, ScaleToConsole}, spool_tag::{self, SpoolTag, SpoolTagObserver}};

use crate::{app_config::AppConfig, console_proxy, load_cell::LoadCell, rgb_led::rgb_led_task};

const MIN_LOADED_WEIGHT: i32 = 5;

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn app_task(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    loadcell_dt: AnyPin,
    loadcell_sck: AnyPin,
    loadcell_spi: AnySpi,
    spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
    irq: esp_hal::gpio::Input<'static>,
    led_pin: AnyPin,
    rmt: RMT
) {
    let spawner = framework.borrow().spawner;

    let spool_tag_model = if app_config.borrow().nfc_module_available() {
        info!("NFC Module available");
        Some(spool_tag::init(spi_device, irq, spawner))
    } else { 
        info!("NFC Module not available");
        None
    };

    let scale_to_console_channel = mk_static!(ScaleToConsoleChannel, ScaleToConsoleChannel::new());
    let web_server_commands = crate::mk_static!(WebServerCommands, WebServerCommands::new());

    // it's not yet calibrated, calibration will be done in App::new
    // TODO: reorganize initialization, can probably most move into App::new, and it would spawn a task for the async code below
    let load_cell = LoadCell::new(
        loadcell_spi,
        loadcell_dt,
        loadcell_sck,
        10,
        Duration::from_millis(100),
        spawner,
    );

    let app = App::new(
        app_config.clone(),
        scale_to_console_channel,
        load_cell.clone(),
        spool_tag_model,
        framework.clone(),
    );

    spawner.spawn(rgb_led_task(app.clone(), framework.clone(), led_pin, rmt)).ok();

    console_proxy::init(
        framework.clone(),
        app_config,
        app.clone(),
        scale_to_console_channel,
        web_server_commands,
    )
    .await;

    Framework::wait_for_wifi(&framework).await;

    web_server_commands.publish_immediate(WebServerCommand::Start(framework.borrow().stack));

    // wait for cnosole to connect before initializing so we can send terminal messages
    loop {
        if app.borrow().connected {
            break;
        }
        Timer::after_millis(250).await;
    }

    load_cell.borrow_mut().start();

    let load_cell_reader = load_cell.borrow_mut().reader();
    loop {
        let scale_state = app.borrow().scale_state;
        match scale_state {
            ScaleState::Uncalibrated => {
                let uncalibrated = load_cell_reader.immediate_read_uncalibrated();
                app.borrow()
                    .send_to_console(ScaleToConsole::RawSamplesAvg(uncalibrated))
                    .await;
                Timer::after_millis(1000).await;
            }
            ScaleState::Unknown => {
                let unstable_read = load_cell_reader.immediate_read();
                if unstable_read > MIN_LOADED_WEIGHT {
                    app.borrow()
                        .send_to_console(ScaleToConsole::NewLoad(unstable_read))
                        .await;
                    // this is an unstable read, so updating only that
                    app.borrow_mut().scale_state = ScaleState::Loaded(0, unstable_read);
                }
                Timer::after_millis(250).await;
            }
            ScaleState::Empty => {
                let unstable_read = load_cell_reader.read_changed(0).await;
                if unstable_read > MIN_LOADED_WEIGHT {
                    app.borrow()
                        .send_to_console(ScaleToConsole::NewLoad(unstable_read))
                        .await;
                    // this is an unstable read, so updating only that
                    app.borrow_mut().scale_state = ScaleState::Loaded(0, unstable_read);
                }
                Timer::after_millis(250).await;
            }
            ScaleState::Loaded(last_stable_read, last_unstable_read) => {
                let read_res =
                    with_timeout(Duration::from_millis(500), load_cell_reader.read_stable()).await;
                match read_res {
                    Ok(new_stable_read) => {
                        if new_stable_read < 0 {
                            LoadCell::tare(&load_cell).await;
                        } else
                        if abs(new_stable_read) < MIN_LOADED_WEIGHT {
                            app.borrow()
                                .send_to_console(ScaleToConsole::LoadRemoved)
                                .await;
                            app.borrow_mut().scale_state = ScaleState::Empty;
                        } else if new_stable_read != last_stable_read || new_stable_read != last_unstable_read {
                            app.borrow()
                                .send_to_console(ScaleToConsole::LoadChangedStable(new_stable_read))
                                .await;
                            app.borrow_mut().scale_state =
                                ScaleState::Loaded(new_stable_read, new_stable_read)
                        }
                    }
                    Err(_timeout_err) => {
                        // debug!("Read stable Timeout");
                        if let Some(new_unstable_read) =
                            load_cell_reader.try_read_changed(last_unstable_read)
                        {
                            if abs(new_unstable_read) >= MIN_LOADED_WEIGHT {
                                app.borrow()
                                    .send_to_console(ScaleToConsole::LoadChangedUnstable(
                                        new_unstable_read,
                                    ))
                                    .await;
                                app.borrow_mut().scale_state =
                                    ScaleState::Loaded(last_stable_read, new_unstable_read);
                            }
                        }
                    }
                }
                Timer::after_millis(500).await;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ScaleState {
    Uncalibrated,
    Unknown,
    Empty,
    Loaded(i32, i32), // Stable-read, Unstable-read
}

pub struct App {
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    pub connected: bool,
    pub scale_to_console_channel: &'static ScaleToConsoleChannel,
    pub load_cell: Rc<RefCell<LoadCell>>,
    pub scale_state: ScaleState,
    tare_during_calibration: Option<i32>,
    _terminal_proxy: Option<Rc<RefCell<TerminalProxy>>>, // to hold it alive
    _spool_tag: Option<Rc<RefCell<SpoolTag>>>,
}

impl App {
    pub fn new(
        app_config: Rc<RefCell<AppConfig>>,
        scale_to_console_channel: &'static ScaleToConsoleChannel,
        load_cell: Rc<RefCell<LoadCell>>,
        spool_tag: Option<Rc<RefCell<SpoolTag>>>,
        framework: Rc<RefCell<Framework>>,
    ) -> Rc<RefCell<Self>> {
        let scale_state = if let Some(scale_config) = &app_config.borrow().configured_calibration {
            load_cell.borrow_mut().set_calibration(
                scale_config.zero_loadcell,
                scale_config.calib_weight,
                scale_config.calib_loadcell,
            );
            ScaleState::Unknown
        } else {
            ScaleState::Uncalibrated
        };

        let myself = Self {
            framework: framework.clone(),
            app_config,
            connected: false,
            scale_to_console_channel,
            load_cell,
            scale_state,
            tare_during_calibration: None,
            _terminal_proxy: None,
            _spool_tag: spool_tag.clone(),
        };
        let myself_rc = Rc::new(RefCell::new(myself));


        // Subscribe to rust spool_tag events
        let trait_for_spool_tag_rc: Rc<RefCell<dyn spool_tag::SpoolTagObserver>> = myself_rc.clone();
        let trait_for_spool_tag_weak: Weak<RefCell<dyn spool_tag::SpoolTagObserver>> = Rc::downgrade(&trait_for_spool_tag_rc);
        if let Some(spool_tag) = spool_tag {
            spool_tag.borrow_mut().subscribe(trait_for_spool_tag_weak);
        }

        let trait_for_framework_rc: Rc<RefCell<dyn FrameworkObserver>> = myself_rc.clone();
        let trait_for_framework_weak: Weak<RefCell<dyn FrameworkObserver>> = Rc::downgrade(&trait_for_framework_rc);
        framework.borrow_mut().subscribe(trait_for_framework_weak);

        let terminal_proxy = Rc::new(RefCell::new(TerminalProxy {
            app: myself_rc.clone(),
        }));
        let trait_for_terminal_rc: Rc<RefCell<dyn terminal::TerminalObserver>> =
            terminal_proxy.clone();
        let trait_for_terminal_weak: Weak<RefCell<dyn terminal::TerminalObserver>> =
            Rc::downgrade(&trait_for_terminal_rc);
        term_mut().subscribe(trait_for_terminal_weak);
        myself_rc.borrow_mut()._terminal_proxy = Some(terminal_proxy);

        myself_rc
    }

    pub async fn send_to_console(&self, scale_to_console_msg: ScaleToConsole) {
        if self.connected {
            if !matches!(scale_to_console_msg, ScaleToConsole::Term(_)) {
                info!("Sending {:?}", scale_to_console_msg);
            }
            self.scale_to_console_channel
                .send(scale_to_console_msg)
                .await;
        } else {
            if !matches!(scale_to_console_msg, ScaleToConsole::Term(_)) {
                info!("Not! sending {:?}", scale_to_console_msg);
            }
        }
    }
    pub fn try_send_to_console(&self, scale_to_console_msg: ScaleToConsole) {
        if self.connected {
            if !matches!(scale_to_console_msg, ScaleToConsole::Term(_)) {
                info!("Sending {:?}", scale_to_console_msg);
            }
            self.scale_to_console_channel
                .try_send(scale_to_console_msg)
                .unwrap_or_else(|err| error!("Failed *trying* to send message to console {err:?}"));
        } else {
            if !matches!(scale_to_console_msg, ScaleToConsole::Term(_)) {
                info!("Not! sending (on connect) {:?}", scale_to_console_msg);
            }
        }
    }
    pub fn handle_console_to_scale(&mut self, console_to_scale_msg: ConsoleToScale) {
        match console_to_scale_msg {
            ConsoleToScale::Calibrate(weight) => {
                debug!("Matched Calibrate");
                if weight == 0 {
                    let read_uncalibrated = self.load_cell.borrow().immediate_read_uncalibrated();
                    self.tare_during_calibration =
                        Some(self.load_cell.borrow().immediate_read_uncalibrated());
                    debug!("Set Tare, sample: {read_uncalibrated}");
                } else {
                    debug!("processing weight calibration");
                    if let Some(tare_during_calibration) = self.tare_during_calibration {
                        let weight_loadcell = self.load_cell.borrow().immediate_read_uncalibrated();
                        info!(
                            "Calibration info: zero_loadcell: {:?}, weight: {}, weight_loadcell:{}",
                            self.tare_during_calibration, weight, weight_loadcell
                        );
                        self.load_cell.borrow_mut().set_calibration(
                            tare_during_calibration,
                            weight,
                            weight_loadcell,
                        );
                        self.app_config
                            .borrow_mut()
                            .set_scale_calibration_config(
                                tare_during_calibration,
                                weight,
                                weight_loadcell,
                            )
                            .unwrap_or_else(|e| error!("Error storing calibration {e:?}"));
                        self.tare_during_calibration = None;
                        let read_weight = self.load_cell.borrow().immediate_read();
                        self.scale_state = ScaleState::Loaded(0, read_weight);
                        self.try_send_to_console(ScaleToConsole::NewLoad(read_weight));
                    } else {
                        error!("Calibration performed w/o first setting tare");
                    }
                }
            }
            _ => (),
        }
    }

    pub fn notify_connected(&mut self) {
        self.connected = true;
        match self.scale_state {
            ScaleState::Uncalibrated => {
                self.try_send_to_console(ScaleToConsole::Uncalibrated);
            }
            ScaleState::Unknown => (),
            ScaleState::Empty => (), // No need and better not send LoadRemoved here. So client will show connected. It tracks connection as well, so will switch on its own to Empty
            ScaleState::Loaded(stable_weight, unstable_weight) => {
                self.try_send_to_console(ScaleToConsole::NewLoad(unstable_weight));
                if stable_weight == unstable_weight {
                    self.try_send_to_console(ScaleToConsole::LoadChangedStable(stable_weight));
                }
            }
        }
    }
    pub fn notify_disconnected(&mut self) {
        self.connected = false;
        self.scale_to_console_channel.clear();
    }
}

pub type ScaleToConsoleChannel = Channel<NoopRawMutex, ScaleToConsole, 5>;

struct TerminalProxy {
    app: Rc<RefCell<App>>,
}

impl TerminalObserver for TerminalProxy {
    fn on_add_text(&self, text: &str) {
        // this is for optimizing comm and able to add "[S]" on console before message.
        // term sends first \n and then the text as two separate strings,
        // here this is undone and on the console the \n is added
        if text == "\n" {
            return;
        } else {
            self.app
                .borrow()
                .try_send_to_console(ScaleToConsole::Term(text.to_string()));
        }
    }
}

impl SpoolTagObserver for App {
    fn on_tag_status(&mut self, status: &spool_tag::Status) {
        match status { spool_tag::Status::FoundTagNowReading =>  {
                self.try_send_to_console(ScaleToConsole::TagStatus(status.clone()));
                info!("Tag found");
        }
            spool_tag::Status::FoundTagNowWriting => todo!(),
            spool_tag::Status::WriteSuccess(_, _) => todo!(),
            spool_tag::Status::ReadSuccess(tag) =>  {
                self.try_send_to_console(ScaleToConsole::TagStatus(status.clone()));
                info!("Tag Read: {tag}");
            }
            spool_tag::Status::Failure(failure) =>  {
                self.try_send_to_console(ScaleToConsole::TagStatus(status.clone()));
                info!("Tag failure: {failure:?}");
            }
        }
    }

    fn on_pn532_status(&mut self, status: bool) {
        self.try_send_to_console(ScaleToConsole::PN532Status(status));
        if status {
            info!("PN532 Initialized Successfuly");
        } else {
            error!("Failed to initialize PN532");
        }
    }
}

impl FrameworkObserver for App {
    fn on_webapp_url_update(&self, _ip_url: &str, _name_url: Option<&str>, _ssid: &str) {
    }

    fn on_initialization_completed(&self, _status: bool) {
    }

    fn on_ota_version_available(&self, _version: &str, _newer: bool) {
    }

    fn on_ota_start(&self) {
    }

    fn on_ota_status(&self, text: &str) {
        info!("OTA Status: {text}");
    }

    fn on_ota_failed(&self, text: &str) {
        info!("OTA Failed: {text}");
    }

    fn on_ota_completed(&self, text: &str) {
        info!("OTA completed: {text}");
    }

    fn on_web_config_started(&self, key: &str, mode: WebConfigMode) {
        info!("Web Config Started: key: {key}, mode: {mode:?}");
    }

    fn on_web_config_stopped(&self) {
        info!("Web Config Stopped");
    }

    fn on_wifi_sta_connected(&self) {
        info!("Connected to WiFi");
        self.framework.borrow().check_firmware_ota();
    }
}
