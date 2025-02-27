use core::cell::RefCell;
use core::future::ready;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use picoserve::response::Redirect;
use picoserve::routing::get;
use picoserve::{
    extract::{FromRequest, State},
    io::Read,
    request::{RequestBody, RequestParts},
    routing::post,
    AppWithStateBuilder,
};

use framework::{
    encrypted_input,
    framework_web_app::{
        decrypt, CustomNotFound, Encryptable, EncryptedRejection, Encryption, NestedAppWithWebAppStateBuilder, SetConfigResponseDTO, WebAppState,
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
        let app_config = self.app_config.clone();
        let _framework = self.framework.clone();

        let router = picoserve::Router::from_service(CustomNotFound {
            web_server_captive: self.framework.borrow().settings.web_server_captive,
        }); // Handler in case page is not found for captive portal support
        let router = router.route("/", get(|| Redirect::to("/config"))); // Redirect root for now

        let app_config_clone_post = app_config.clone();
        let app_config_clone_get = app_config.clone();
        let router = router.route(
            "/api/printer-config",
            post(
                move |State(Encryption(key)): State<Encryption>, PrinterConfigDTO { ip, serial, name, access_code }| {
                    ready(match app_config_clone_post.borrow_mut().set_printer_config(ip, name, serial, access_code) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    PrinterConfigDTO {
                        ip: app_config_clone_get.borrow().configured_printer_ip.map(|v| v.to_string()) .unwrap_or(String::from("")),
                        name: app_config_clone_get.borrow().configured_printer_name.clone().unwrap_or(String::from("")),
                        serial: app_config_clone_get.borrow().printer_serial.clone().unwrap_or(String::from("")),
                        access_code: app_config_clone_get.borrow().printer_access_code.clone().unwrap_or(String::from("")),
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        let app_config_clone_post = app_config.clone();
        let app_config_clone_get = app_config.clone();
        let router = router.route(
            "/api/tag-config",
            post(move |State(Encryption(key)): State<Encryption>, TagConfigDTO { tag_scan_timeout }| {
                ready(match app_config_clone_post.borrow_mut().set_tag_config(tag_scan_timeout) {
                    Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                    Err(e) => SetConfigResponseDTO {
                        error_text: Some(format!("{e:?}")),
                    }
                    .encrypt(&key.borrow()),
                })
            })
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    TagConfigDTO {
                        tag_scan_timeout: app_config_clone_get.borrow().tag_scan_timeout,
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        router
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfigDTO {
    ip: String,
    name: String,
    serial: String,
    access_code: String,
}
encrypted_input!(PrinterConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct TagConfigDTO {
    tag_scan_timeout: u64,
}
encrypted_input!(TagConfigDTO);
