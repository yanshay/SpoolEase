use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::Duration;
use framework::{
    debug, error,
    framework::{Framework, WebServerCommands},
    mk_static,
    utils::SpawnerHeapExt,
    web_server::{GenericRunner, WebServerCommand, WebServerConfig},
};
use picoserve::{AppRouter, AppWithStateBuilder};

use crate::{
    api_app,
    app_config::AppConfig,
    settings::{
        API_REQUEST_BODY_MAX_BYTES, API_SERVER_HTTP_BUFFER_BYTES, API_SERVER_HTTP_RETAINED_BUFFERS, API_SERVER_HTTPS, API_SERVER_NUM_LISTENERS,
        API_SERVER_PORT, API_SERVER_TCP_RX_BUFFER_BYTES, API_SERVER_TCP_TX_BUFFER_BYTES,
    },
    slicer_ws::SlicerWsProxy,
    view_model::ViewModel,
};

pub type ApiServerCommands = WebServerCommands;
type ApiServerRunner = GenericRunner<ApiServerAppBuilder, ApiServerState>;

#[derive(Clone)]
pub struct ApiServerState {
    #[allow(dead_code)]
    pub framework: Rc<RefCell<Framework>>,
    pub app_config: Rc<RefCell<AppConfig>>,
    pub view_model: Rc<RefCell<ViewModel>>,
    pub slicer_ws_proxy: &'static SlicerWsProxy,
    pub request_body_max_bytes: usize,
}

pub struct ApiServerAppBuilder;

impl AppWithStateBuilder for ApiServerAppBuilder {
    type State = ApiServerState;
    type PathRouter = impl picoserve::routing::PathRouter<ApiServerState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        api_app::build_app()
    }
}

pub struct ApiServerHandle {
    commands: &'static ApiServerCommands,
}

impl ApiServerHandle {
    pub fn start(&self, stack: Stack<'static>) {
        self.commands.publisher().unwrap().publish_immediate(WebServerCommand::Start(stack));
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.commands.publisher().unwrap().publish_immediate(WebServerCommand::Stop);
    }
}

pub fn init_api_server(
    framework: Rc<RefCell<Framework>>,
    app_config: Rc<RefCell<AppConfig>>,
    view_model: Rc<RefCell<ViewModel>>,
    slicer_ws_proxy: &'static SlicerWsProxy,
    spawner: Spawner,
    tls_certificate: &'static str,
    tls_private_key: &'static str,
) -> &'static ApiServerHandle {
    let api_server_builder = ApiServerAppBuilder;

    let api_server_router = mk_static!(AppRouter<ApiServerAppBuilder>, AppWithStateBuilder::build_app(api_server_builder));

    let api_server_state = mk_static!(
        ApiServerState,
        ApiServerState {
            framework: framework.clone(),
            app_config: app_config.clone(),
            view_model: view_model.clone(),
            slicer_ws_proxy,
            request_body_max_bytes: API_REQUEST_BODY_MAX_BYTES,
        }
    );

    let api_server_commands = mk_static!(ApiServerCommands, ApiServerCommands::new());

    let api_server_config = WebServerConfig {
        web_app_name: "Api-Server",
        port: API_SERVER_PORT,
        tls: API_SERVER_HTTPS,
        tls_certificate,
        tls_private_key,
        buffer_sizes: framework::web_server::WebServerBufferSizes {
            tcp_rx: API_SERVER_TCP_RX_BUFFER_BYTES,
            tcp_tx: API_SERVER_TCP_TX_BUFFER_BYTES,
            http: API_SERVER_HTTP_BUFFER_BYTES,
            http_retained: API_SERVER_HTTP_RETAINED_BUFFERS,
        },
    };

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(3),
        persistent_start_read_request: Duration::from_secs(3),
        read_request: Duration::from_secs(3),
        write: Duration::from_secs(3),
    })
    .keep_connection_alive();

    let api_server_runner = mk_static!(
        ApiServerRunner,
        ApiServerRunner::new(
            framework,
            api_server_config,
            api_server_router,
            api_server_state,
            api_server_commands,
            config,
        )
    );

    for id in 0..API_SERVER_NUM_LISTENERS {
        debug!("Spawning ApiServer listener {id}");
        if let Err(err) = spawner.spawn_heap(api_server_task(api_server_runner, id)) {
            error!("Failed to spawn ApiServer listener {id}: {err:?}");
            panic!("Failed to spawn ApiServer listener task");
        }
    }

    mk_static!(
        ApiServerHandle,
        ApiServerHandle {
            commands: api_server_commands,
        }
    )
}

async fn api_server_task(runner: &'static ApiServerRunner, id: usize) {
    runner.run(id).await;
}
