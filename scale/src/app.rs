use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubBehavior};
use embassy_time::{Duration, Timer};
use esp_hal::{gpio::AnyPin, spi::AnySpi};
use framework::{
    error, framework::WebServerCommands, info, mk_static, prelude::Framework, web_server::WebServerCommand
};
use num_traits::abs;
use shared::scale::ScaleToConsole;

use crate::{app_config::AppConfig, console_proxy, load_cell::LoadCell};

#[derive(Clone, Copy)]
enum ScaleState {
    Unknown,
    Empty,
    Loaded(i32),
}

pub struct App {
    connected: bool,
    pub scale_to_console_channel: &'static ScaleToConsoleChannel,
    pub _load_cell: Rc<RefCell<LoadCell>>,
    scale_state: ScaleState,
}

impl App {
    pub fn new(
        scale_to_console_channel: &'static ScaleToConsoleChannel,
        load_cell: Rc<RefCell<LoadCell>>,
    ) -> Self {
       Self {
            connected: false,
            scale_to_console_channel,
            _load_cell: load_cell,
            scale_state: ScaleState::Unknown,
        } 
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
    pub fn try_send_to_console(&self, scale_to_console_msg: ScaleToConsole) {
        if self.connected {
            info!("Sending (on connect) {:?}", scale_to_console_msg);
            self.scale_to_console_channel
                .try_send(scale_to_console_msg).unwrap_or_else(|err| error!("Failed *trying* to send message to console {err:?}"));
        } else {
            info!("Not! sending (on connect) {:?}", scale_to_console_msg);
        }
    }
    pub fn notify_connected(&mut self) {
        self.connected = true;
        match self.scale_state {
            ScaleState::Unknown => (),
            ScaleState::Empty => (), // No need and better not send LoadRemoved here. So client will show connected. It tracks connection as well, so will switch on its own to Empty 
            ScaleState::Loaded(weight) => { self.try_send_to_console(ScaleToConsole::NewLoad(weight)); } 
        }
    }
    pub fn notify_disconnected(&mut self) {
        self.connected = false;
        self.scale_to_console_channel.clear();
    }
}

pub type ScaleToConsoleChannel = Channel<NoopRawMutex, ScaleToConsole, 5>;

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

    let load_cell = LoadCell::new(
        loadcell_spi,
        loadcell_dt,
        loadcell_sck,
        5,
        Duration::from_millis(100),
        spawner,
    );

    let app = App::new(scale_to_console_channel, load_cell.clone());

    let app = Rc::new(RefCell::new(app));

    console_proxy::init(
        framework.clone(),
        app_config,
        app.clone(),
        scale_to_console_channel,
        web_server_commands,
    )
    .await;

    LoadCell::tare(&load_cell).await;

    Framework::wait_for_wifi(&framework).await;

    web_server_commands.publish_immediate(WebServerCommand::Start(framework.borrow().stack));

    let load_cell_reader = load_cell.borrow_mut().reader();
    loop {
        let scale_state = app.borrow().scale_state;
        match scale_state {
            ScaleState::Empty | ScaleState::Unknown => {
                let read = load_cell_reader.read_changed().await;
                if read > 10 {
                    app.borrow().send_to_console(ScaleToConsole::NewLoad(read)).await;
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
                    app.borrow().send_to_console(ScaleToConsole::LoadRemoved).await;
                    app.borrow_mut().scale_state = ScaleState::Empty;
                } else if read != prev_read {
                    app.borrow().send_to_console(ScaleToConsole::LoadChanged(read)).await;
                    app.borrow_mut().scale_state = ScaleState::Loaded(read)
                }
                Timer::after_millis(500).await;
            }
        }
    }
}
