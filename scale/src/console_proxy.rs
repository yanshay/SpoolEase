use core::cell::RefCell;

use crate::{
    app::{App, ScaleToConsoleChannel},
    app_config::AppConfig,
    ssdp,
};
use alloc::rc::Rc;
use embassy_futures::select::select3;
use embassy_time::{Duration, Instant, Timer};
use framework::{
    debug, error, framework::WebServerCommands, info, mk_static, prelude::Framework,
    utils::random_u32, warn, web_server::WebServerConfig,
};
use picoserve::{
    io::Error,
    response::ws::{self},
    routing::get,
    AppRouter, AppWithStateBuilder,
};
use shared::scale::ConsoleToScale;

pub struct ConsoleProxyWebAppState {}

pub struct ConsoleProxyAppBuilder {
    #[allow(dead_code)]
    pub framework: Rc<RefCell<Framework>>,
    #[allow(dead_code)]
    pub app_config: Rc<RefCell<AppConfig>>,
    scale_to_console_channel: &'static ScaleToConsoleChannel,
    pub app: Rc<RefCell<App>>,
}

const NUM_LISTENERS: usize = 1; // increasing this to suport more than one connection simultaniously requires allowing a PubSubChannel, and probably other applicative issues.

pub async fn init(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    app: Rc<RefCell<App>>,
    scale_to_console_channel: &'static ScaleToConsoleChannel,
    web_server_commands: &'static WebServerCommands,
) -> &'static ScaleToConsoleChannel {
    let console_proxy_web_app_builder = ConsoleProxyAppBuilder {
        framework: framework.clone(),
        app_config: app_config.clone(),
        app,
        scale_to_console_channel,
    };

    let web_app_router = mk_static!(
        AppRouter<ConsoleProxyAppBuilder>,
        AppWithStateBuilder::build_app(console_proxy_web_app_builder)
    );

    let console_proxy_app_state = mk_static!(ConsoleProxyWebAppState, ConsoleProxyWebAppState {});

    let web_server_config = WebServerConfig {
        web_app_name: "Console-Proxy",
        port: 81,
        tls: false,
        tls_certificate: "",
        tls_private_key: "",
    };

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Some(Duration::from_secs(5)),
        read_request: Some(Duration::from_millis(5000)),
        write: Some(Duration::from_millis(5000)),
    })
    .keep_connection_alive();

    let console_proxy_web_app_runner = mk_static!(
        framework::web_server::GenericRunner<ConsoleProxyAppBuilder, ConsoleProxyWebAppState>,
        framework::web_server::GenericRunner::<ConsoleProxyAppBuilder, ConsoleProxyWebAppState>::new(
            framework.clone(),
            web_server_config,
            web_app_router,
            console_proxy_app_state,
            web_server_commands,
            config,
        )
    );

    for id in 0..NUM_LISTENERS {
        debug!("Spawning console proxy web-task {id}");
        framework
            .borrow()
            .spawner
            .spawn(console_proxy_web_server_task(
                console_proxy_web_app_runner,
                id,
            ))
            .unwrap();
    }

    framework
        .borrow()
        .spawner
        .spawn(ssdp::ssdp_broadcast(framework.clone()))
        .ok();

    scale_to_console_channel
}

#[embassy_executor::task(pool_size = NUM_LISTENERS)]
async fn console_proxy_web_server_task(
    runner: &'static framework::web_server::GenericRunner<
        ConsoleProxyAppBuilder,
        ConsoleProxyWebAppState,
    >,
    id: usize,
) {
    runner.run(id).await;
}

impl AppWithStateBuilder for ConsoleProxyAppBuilder {
    type State = ConsoleProxyWebAppState;
    type PathRouter = impl picoserve::routing::PathRouter<ConsoleProxyWebAppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let router = picoserve::Router::new();

        #[allow(clippy::let_and_return)]
        let router = router.route(
            "/ws",
            get(move |upgrade: ws::WebSocketUpgrade| {
                upgrade.on_upgrade(ConsoleCommHandler::new(
                    self.scale_to_console_channel,
                    self.app.clone(),
                ))
            }),
        );

        router
    }
}

struct ConsoleCommHandler {
    scale_to_console_channel: &'static ScaleToConsoleChannel,
    app: Rc<RefCell<App>>,
}

impl ConsoleCommHandler {
    pub fn new(
        scale_to_console_channel: &'static ScaleToConsoleChannel,
        app: Rc<RefCell<App>>,
    ) -> Self {
        let myself = Self {
            scale_to_console_channel,
            app,
        };
        myself.app.borrow_mut().notify_connected();
        myself
    }
}
impl Drop for ConsoleCommHandler {
    fn drop(&mut self) {
        self.app.borrow_mut().notify_disconnected();
    }
}

impl ws::WebSocketCallback for ConsoleCommHandler {
    async fn run<R: picoserve::io::Read, W: picoserve::io::Write<Error = R::Error>>(
        #[allow(unused_mut)] mut self,
        mut ws_rx: ws::SocketRx<R>,
        mut ws_tx: ws::SocketTx<W>,
    ) -> Result<(), W::Error> {
        info!("SpoolEase Console connected");
        use picoserve::response::ws::Message;

        let mut message_buffer = [0; 128];

        loop {
            let timeout_for_ping = (random_u32() % 5000) + 5000;
            let wait_res =
                // with_timeout(Duration::from_secs(2), rx.next_message(&mut message_buffer)).await;
                select3(self.scale_to_console_channel.receive(), ws_rx.next_message(&mut message_buffer), Timer::after_millis(timeout_for_ping as u64)).await;
            match wait_res {
                // Receive channel-message from load_cell to send out to SpoolEase
                embassy_futures::select::Either3::First(scale_to_console) => {
                    let json_res = serde_json::to_string(&scale_to_console);
                    match json_res {
                        Ok(json) => {
                            if let Err(io_err) = ws_tx.send_text(&json).await {
                                error!(
                                    "Error sending message to console {io_err:?}, disconnecting"
                                );
                                return Err(io_err);
                            }
                        }
                        Err(err) => {
                            error!("Error serializing data {:?}, {:?}", scale_to_console, err)
                        }
                    }
                }
                // Receive websocket-message from SpoolEase Console to handle
                embassy_futures::select::Either3::Second(read_res) => {
                    match read_res {
                        Ok(msg) => match msg {
                            Message::Text(txt) => {
                                let console_to_scale_res =
                                    serde_json::from_str::<ConsoleToScale>(txt);
                                match console_to_scale_res {
                                    Ok(console_to_scale) => {
                                        self.app
                                            .borrow_mut()
                                            .handle_console_to_scale(console_to_scale);
                                    }
                                    Err(err) => {
                                        error!("Error deserializing message from SpoolEase Console {err:?}");
                                    }
                                }
                            }
                            Message::Binary(items) => {
                                error!("Received unsupported binary data {items:?}");
                            }
                            Message::Close(reason) => {
                                ws_tx.close(reason).await.ok();
                                return Ok(());
                            }
                            Message::Ping(items) => {
                                info!("Received Ping, replying Pong");
                                ws_tx.send_pong(items).await.ok();
                            }
                            Message::Pong(items) => {
                                let tick_res: Result<&[u8; 8], _> = items.try_into();
                                if let Ok(ticks) = tick_res {
                                    let ping_ticks = u64::from_le_bytes(*ticks);
                                    let ping_instant = Instant::from_ticks(ping_ticks);
                                    let elapsed_duration = ping_instant.elapsed();
                                    info!(
                                        "Ping-Pong duration was {} millis",
                                        elapsed_duration.as_millis()
                                    );
                                } else {
                                    warn!("SpoolScale: Received bad Pong response {items:?}");
                                }
                            }
                        },
                        Err(err) => {
                            match err {
                                ws::ReadMessageError::Io(io_err) => {
                                    #[allow(clippy::match_single_binding)]
                                    match io_err.kind() {
                                        // picoserve::io::ErrorKind::Other => todo!(),
                                        // picoserve::io::ErrorKind::NotFound => todo!(),
                                        // picoserve::io::ErrorKind::PermissionDenied => todo!(),
                                        // picoserve::io::ErrorKind::ConnectionRefused => todo!(),
                                        // picoserve::io::ErrorKind::ConnectionReset => todo!(),
                                        // picoserve::io::ErrorKind::ConnectionAborted => todo!(),
                                        // picoserve::io::ErrorKind::NotConnected => todo!(),
                                        // picoserve::io::ErrorKind::AddrInUse => todo!(),
                                        // picoserve::io::ErrorKind::AddrNotAvailable => todo!(),
                                        // picoserve::io::ErrorKind::BrokenPipe => todo!(),
                                        // picoserve::io::ErrorKind::AlreadyExists => todo!(),
                                        // picoserve::io::ErrorKind::InvalidInput => todo!(),
                                        // picoserve::io::ErrorKind::InvalidData => todo!(),
                                        // picoserve::io::ErrorKind::TimedOut => todo!(),
                                        // picoserve::io::ErrorKind::Interrupted => todo!(),
                                        // picoserve::io::ErrorKind::Unsupported => todo!(),
                                        // picoserve::io::ErrorKind::OutOfMemory => todo!(),
                                        // picoserve::io::ErrorKind::WriteZero => todo!(),
                                        _ => {
                                            error!("IO Error reading message from SpoolEase {io_err:?}");
                                            return Err(io_err);
                                        }
                                    }
                                }
                                ws::ReadMessageError::ReadFrameError(_read_frame_error) => todo!(),
                                ws::ReadMessageError::ReservedOpcode(_) => todo!(),
                                ws::ReadMessageError::MessageStartsWithContinuation => todo!(),
                                ws::ReadMessageError::UnexpectedMessageStart => todo!(),
                                ws::ReadMessageError::TextIsNotUtf8 => todo!(),
                            }
                        }
                    }
                }
                embassy_futures::select::Either3::Third(_) => {
                    // Timeout
                    let now = Instant::now().as_ticks();
                    let data = now.to_le_bytes();
                    let send_res = ws_tx.send_ping(&data).await;
                    match send_res {
                        Ok(_) => info!("Sent Ping to SpoolEase"),
                        Err(io_err) => {
                            #[allow(clippy::match_single_binding)]
                            match io_err.kind() {
                                // picoserve::io::ErrorKind::Other => todo!(),
                                // picoserve::io::ErrorKind::NotFound => todo!(),
                                // picoserve::io::ErrorKind::PermissionDenied => todo!(),
                                // picoserve::io::ErrorKind::ConnectionRefused => todo!(),
                                // picoserve::io::ErrorKind::ConnectionReset => todo!(),
                                // picoserve::io::ErrorKind::ConnectionAborted => todo!(),
                                // picoserve::io::ErrorKind::NotConnected => todo!(),
                                // picoserve::io::ErrorKind::AddrInUse => todo!(),
                                // picoserve::io::ErrorKind::AddrNotAvailable => todo!(),
                                // picoserve::io::ErrorKind::BrokenPipe => todo!(),
                                // picoserve::io::ErrorKind::AlreadyExists => todo!(),
                                // picoserve::io::ErrorKind::InvalidInput => todo!(),
                                // picoserve::io::ErrorKind::InvalidData => todo!(),
                                // picoserve::io::ErrorKind::TimedOut => todo!(),
                                // picoserve::io::ErrorKind::Interrupted => todo!(),
                                // picoserve::io::ErrorKind::Unsupported => todo!(),
                                // picoserve::io::ErrorKind::OutOfMemory => todo!(),
                                // picoserve::io::ErrorKind::WriteZero => todo!(),
                                _ => {
                                    error!("IO Error sending ping message to SpoolEase {io_err:?}");
                                    return Err(io_err);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
