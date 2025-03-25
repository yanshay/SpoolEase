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

        router
    }
}
