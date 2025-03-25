use core::cell::RefCell;

use alloc::rc::Rc;
use picoserve::response::Redirect;
use picoserve::routing::get;
use picoserve::AppWithStateBuilder;

use framework::{
    framework_web_app::{
        CustomNotFound, NestedAppWithWebAppStateBuilder, WebAppState,
    },
    prelude::*,
};

use crate::app_config::AppConfig;

pub struct NestedAppBuilder {
    pub framework: Rc<RefCell<Framework>>,
    pub app_config: Rc<RefCell<AppConfig>>,
}

impl NestedAppWithWebAppStateBuilder for NestedAppBuilder {
    fn path_description(&self) -> &'static str {
        "" // this nests it at the root.
    }
}

impl AppWithStateBuilder for NestedAppBuilder {
    type State = WebAppState;
    type PathRouter = impl picoserve::routing::PathRouter<WebAppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let _app_config = self.app_config.clone();
        let _framework = self.framework.clone();

        let router = picoserve::Router::from_service(CustomNotFound {
            web_server_captive: self.framework.borrow().settings.web_server_captive,
        }); // Handler in case page is not found for captive portal support
        let router = router.route("/", get(|| Redirect::to("/config"))); // Redirect root for now

        // let router = router.route(
        //     "/ws",
        //     get(move |upgrade: ws::WebSocketUpgrade| {
        //         debug!(">>>>>>>>>>> ws request arrived here");
        //         if let Some(protocols) = upgrade.protocols() {
        //             debug!("Protocols:");
        //             for protocol in protocols {
        //                 debug!("\t{protocol}");
        //             }
        //         }
        //         debug!(">>>> progressed to upgrade");
        //
        //         upgrade.on_upgrade(WebsocketHandler {
        //                 // tx: messages_tx.clone(),
        //                 // rx: messages_tx.subscribe(),
        //             })
        //         // .with_protocol("messages")
        //     }),
        // );
        router
    }
}

// struct WebsocketHandler {
//     // tx: std::rc::Rc<tokio::sync::broadcast::Sender<String>>,
//     // rx: tokio::sync::broadcast::Receiver<String>,
// }
//
// impl ws::WebSocketCallback for WebsocketHandler {
//     async fn run<R: picoserve::io::Read, W: picoserve::io::Write<Error = R::Error>>(
//         #[allow(unused_mut)]
//         mut self,
//         mut rx: ws::SocketRx<R>,
//         mut tx: ws::SocketTx<W>,
//     ) -> Result<(), W::Error> {
//         use picoserve::response::ws::Message;
//
//         let mut message_buffer = [0; 128];
//
//         let mut x = 0;
//         loop {
//             let wait_res =
//                 with_timeout(Duration::from_secs(2), rx.next_message(&mut message_buffer)).await;
//             match wait_res {
//                 Ok(v) => match v {
//                     Ok(m) => match m {
//                         Message::Text(_) => {
//                             debug!("message: {m:?}");
//                         }
//                         Message::Binary(_items) => todo!(),
//                         Message::Close(_c) => todo!(),
//                         Message::Ping(_items) => todo!(),
//                         Message::Pong(_items) => todo!(),
//                     },
//                     Err(e) => {
//                         error!("xxxx Error on websocket message {e:?}");
//                         match e {
//                             ws::ReadMessageError::Io(_) => todo!(),
//                             ws::ReadMessageError::ReadFrameError(read_frame_error) => {
//                                 match read_frame_error {
//                                     ReadFrameError::Io(_) => (),
//                                     ReadFrameError::UnexpectedEof => return Ok(()),
//                                     ReadFrameError::MessageIsTooLong(_) => (),
//                                     ReadFrameError::OutOfSpace => (),
//                                 }
//                             }
//                             ws::ReadMessageError::ReservedOpcode(_) => todo!(),
//                             ws::ReadMessageError::MessageStartsWithContinuation => todo!(),
//                             ws::ReadMessageError::UnexpectedMessageStart => todo!(),
//                             ws::ReadMessageError::TextIsNotUtf8 => todo!(),
//                         }
//                     }
//                 },
//                 Err(_) => {
//                     debug!("Sending");
//                     let res = tx.send_text(&format!("hello {x}")).await;
//                     debug!("{res:?}");
//                     x += 1;
//                 }
//             }
//         }
//     }
// }
