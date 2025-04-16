use core::cell::RefCell;
use core::future::ready;

use alloc::{format, rc::Rc};
use picoserve::{
    extract::{FromRequest, State},
    io::Read,
    request::{RequestBody, RequestParts},
    response::Redirect,
    routing::{get, post},
    AppWithStateBuilder,
};

use framework::{
    encrypted_input,
    framework_web_app::{
        decrypt, CustomNotFound, Encryptable, EncryptedRejection, Encryption,
        NestedAppWithWebAppStateBuilder, SetConfigResponseDTO, WebAppState,
    },
    prelude::*,
};

use crate::app_config::{AppConfig, NfcModuleConfig};

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
            "/api/nfc-module-config",
            post(
                move |State(Encryption(key)): State<Encryption>,
                      nfc_module_config_dto: NfcModuleConfigDTO| {
                    ready(
                        match app_config_clone_post
                            .borrow_mut()
                            .set_nfc_module_config(nfc_module_config_dto.into())
                        {
                            Ok(_) => {
                                SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                            }
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready({
                    let borrowed_app_config = app_config_clone_get.borrow(); // notice the borrow, can't async here
                    let default_nfc_module_config = NfcModuleConfig::default();
                    let nfc_module = borrowed_app_config
                        .configured_nfc_module
                        .as_ref()
                        .unwrap_or(&default_nfc_module_config);
                    let nfc_module_config = NfcModuleConfigDTO::from(nfc_module);
                    nfc_module_config.encrypt(&key.borrow())
                })
            }),
        );

        router
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct NfcModuleConfigDTO {
    available: bool,
}
encrypted_input!(NfcModuleConfigDTO);

impl From<NfcModuleConfigDTO> for NfcModuleConfig {
    fn from(v: NfcModuleConfigDTO) -> Self {
        Self {
            available: v.available,
        }
    }
}
impl From<&NfcModuleConfig> for NfcModuleConfigDTO {
    fn from(v: &NfcModuleConfig) -> Self {
        Self {
            available: v.available,
        }
    }
}
