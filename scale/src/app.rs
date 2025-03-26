use core::cell::RefCell;

use alloc::{
    rc::{Rc, Weak}, string::ToString
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubBehavior};
use embassy_time::{Duration, Timer};
use esp_hal::{gpio::AnyPin, spi::AnySpi};
use framework::{
    debug, error,
    framework::WebServerCommands,
    info, mk_static,
    prelude::Framework,
    terminal::{self, term_mut, TerminalObserver},
    web_server::WebServerCommand,
};
use num_traits::abs;
use shared::scale::{ConsoleToScale, ScaleToConsole};

use crate::{app_config::AppConfig, console_proxy, load_cell::LoadCell};

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn app_task(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    loadcell_dt: AnyPin,
    loadcell_sck: AnyPin,
    loadcell_spi: AnySpi,
) {
    let spawner = framework.borrow().spawner;

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
    );

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
                // nothing to do ... wait for this to change
                Timer::after_millis(500).await;
            }
            ScaleState::Empty | ScaleState::Unknown => {
                let read = load_cell_reader.read_changed().await;
                if read > 10 {
                    app.borrow()
                        .send_to_console(ScaleToConsole::NewLoad(read))
                        .await;
                    app.borrow_mut().scale_state = ScaleState::Loaded(read);
                } else if read < 1 {
                    LoadCell::tare(&load_cell).await;
                }
                Timer::after_millis(250).await;
            }
            ScaleState::Loaded(prev_read) => {
                // debug!("In monitoring scale loop - loaded");
                let read = load_cell_reader.read().await;
                if abs(read) < 10 {
                    app.borrow()
                        .send_to_console(ScaleToConsole::LoadRemoved)
                        .await;
                    app.borrow_mut().scale_state = ScaleState::Empty;
                } else if read != prev_read {
                    app.borrow()
                        .send_to_console(ScaleToConsole::LoadChanged(read))
                        .await;
                    app.borrow_mut().scale_state = ScaleState::Loaded(read)
                }
                Timer::after_millis(500).await;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ScaleState {
    Uncalibrated,
    Unknown,
    Empty,
    Loaded(i32),
}

pub struct App {
    app_config: Rc<RefCell<AppConfig>>,
    connected: bool,
    pub scale_to_console_channel: &'static ScaleToConsoleChannel,
    pub load_cell: Rc<RefCell<LoadCell>>,
    scale_state: ScaleState,
    tare_during_calibration: Option<i32>,
    _terminal_proxy: Option<Rc<RefCell<TerminalProxy>>>, // to hold it alive
}

impl App {
    pub fn new(
        app_config: Rc<RefCell<AppConfig>>,
        scale_to_console_channel: &'static ScaleToConsoleChannel,
        load_cell: Rc<RefCell<LoadCell>>,
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
            app_config,
            connected: false,
            scale_to_console_channel,
            load_cell,
            scale_state,
            tare_during_calibration: None,
            _terminal_proxy: None,
        };
        let myself_rc = Rc::new(RefCell::new(myself));

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
            info!("Sending {:?}", scale_to_console_msg);
            self.scale_to_console_channel
                .send(scale_to_console_msg)
                .await;
        } else {
            info!("Not! sending {:?}", scale_to_console_msg);
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
                        self.scale_state = ScaleState::Loaded(read_weight);
                        self.try_send_to_console(ScaleToConsole::NewLoad(read_weight));
                    } else {
                        error!("Calibration performed w/o first setting tare");
                    }
                }
            }
            _ => (),
        }
    }
    pub fn try_send_to_console(&self, scale_to_console_msg: ScaleToConsole) {
        if self.connected {
            info!("Sending (on connect) {:?}", scale_to_console_msg);
            self.scale_to_console_channel
                .try_send(scale_to_console_msg)
                .unwrap_or_else(|err| error!("Failed *trying* to send message to console {err:?}"));
        } else {
            info!("Not! sending (on connect) {:?}", scale_to_console_msg);
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
            ScaleState::Loaded(weight) => {
                self.try_send_to_console(ScaleToConsole::NewLoad(weight));
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
            self.app.borrow().try_send_to_console(ScaleToConsole::Term(text.to_string()));
        }
    }
}
