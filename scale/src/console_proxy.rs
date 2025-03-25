use core::cell::RefCell;

use crate::{app_config::AppConfig, ssdp};
use alloc::rc::Rc;
use embassy_executor::Spawner;
use embassy_futures::select::select3;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubBehavior};
use embassy_time::{Duration, Instant, Timer};
use framework::{
    debug, error,
    framework::WebServerCommands,
    info, mk_static,
    prelude::Framework,
    utils::random_u32,
    warn,
    web_server::{WebServerCommand, WebServerConfig},
};
use picoserve::{
    io::Error,
    response::ws::{self},
    routing::get,
    AppRouter, AppWithStateBuilder,
};
use shared::scale::ScaleToConsole;

pub struct ConsoleProxyWebAppState {}

pub struct ConsoleProxyAppBuilder {
    #[allow(dead_code)]
    pub framework: Rc<RefCell<Framework>>,
    #[allow(dead_code)]
    pub app_config: Rc<RefCell<AppConfig>>,
    scale_to_console_channel: &'static ScaleToConsoleChannel,
}

const NUM_LISTENERS: usize = 1; // increasing this to suport more than one connection simultaniously requires allowing a PubSubChannel, and probably other applicative issues.

type ScaleToConsoleChannel = Channel<NoopRawMutex, ScaleToConsole, 5>;

pub async fn init(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    stack: Stack<'static>,
    spawner: Spawner,
) -> &'static ScaleToConsoleChannel {
    let scale_to_console_channel = mk_static!(ScaleToConsoleChannel, ScaleToConsoleChannel::new());

    let web_app_builder = ConsoleProxyAppBuilder {
        framework: framework.clone(),
        app_config: app_config.clone(),
        scale_to_console_channel,
    };

    let web_app_router = mk_static!(
        AppRouter<ConsoleProxyAppBuilder>,
        AppWithStateBuilder::build_app(web_app_builder)
    );

    let app_state = mk_static!(ConsoleProxyWebAppState, ConsoleProxyWebAppState {});

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

    let web_server_commands = crate::mk_static!(WebServerCommands, WebServerCommands::new());

    let console_proxy_web_app_runner = mk_static!(
        framework::web_server::GenericRunner<ConsoleProxyAppBuilder, ConsoleProxyWebAppState>,
        framework::web_server::GenericRunner::<ConsoleProxyAppBuilder, ConsoleProxyWebAppState>::new(
            framework.clone(),
            web_server_config,
            web_app_router,
            app_state,
            web_server_commands,
            config,
        )
    );

    for id in 0..NUM_LISTENERS {
        debug!("Spawning console proxy web-task {id}");
        spawner
            .spawn(console_proxy_web_server_task(
                console_proxy_web_app_runner,
                id,
            ))
            .unwrap();
    }

    Timer::after_secs(2).await;
    web_server_commands.publish_immediate(WebServerCommand::Start(stack));

    spawner.spawn(ssdp::ssdp_broadcast(stack)).ok();

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

        let router = router.route(
            "/ws",
            get(move |upgrade: ws::WebSocketUpgrade| {
                if let Some(protocols) = upgrade.protocols() {
                    debug!("Protocols:");
                    for protocol in protocols {
                        debug!("\t{protocol}");
                    }
                }

                upgrade.on_upgrade(ConsoleCommHandler {
                    scale_to_console_channel: self.scale_to_console_channel, // tx: messages_tx.clone(),
                                                                             // rx: messages_tx.subscribe(),
                })
                // .with_protocol("messages")
            }),
        );

        router
    }
}

struct ConsoleCommHandler {
    scale_to_console_channel: &'static ScaleToConsoleChannel,
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
                                debug!("Received msg {txt}");
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
                                ws_tx.send_pong(items).await.unwrap();
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
