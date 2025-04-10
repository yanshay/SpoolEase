use core::cell::RefCell;
use core::future::ready;
use core::net::Ipv4Addr;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use picoserve::response::Redirect;
use picoserve::routing::{get, get_service};
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

use crate::app_config::{AppConfig, DefaultPrinterConfig, PrinterConfig, PrintersConfig, ScaleConfig, SPOOLS_CATALOG};

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
                move |State(Encryption(key)): State<Encryption>, printers_config_dto: PrintersConfigDTO| {
                    let default_printer_serial = printers_config_dto.default_printer_serial.clone();
                    ready(match app_config_clone_post.borrow_mut().set_printers_config(printers_config_dto.into(), DefaultPrinterConfig {serial: default_printer_serial} ) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready( {
                        let borrowed_app_config = app_config_clone_get.borrow(); // notice the borrow, can't async here
                        let printers = &borrowed_app_config.configured_printers;
                        let default_printer = &borrowed_app_config.configured_default_printer;
                        let mut printers_config = PrintersConfigDTO::from(printers);
                        printers_config.default_printer_serial = default_printer.serial.clone();
                        printers_config.encrypt(&key.borrow())
                    }
                )
            }),
        );

        let app_config_clone_post = app_config.clone();
        let app_config_clone_get = app_config.clone();
        let router = router.route(
            "/api/scale-config",
            post(
                move |State(Encryption(key)): State<Encryption>, scale_config_dto: ScaleConfigDTO| {
                    ready(match app_config_clone_post.borrow_mut().set_scale_config(scale_config_dto.into()) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready( {
                        let borrowed_app_config = app_config_clone_get.borrow(); // notice the borrow, can't async here
                        let default_scale_config = ScaleConfig::default();
                        let scale = borrowed_app_config.configured_scale.as_ref().unwrap_or(&default_scale_config);
                        let scale_config = ScaleConfigDTO::from(scale);
                        scale_config.encrypt(&key.borrow())
                    }
                )
            }),
        );

        let router = router.route(
            "/spools-catalog",
            get_service(picoserve::response::File::with_content_type("text/plain; charset=utf-8", SPOOLS_CATALOG.as_bytes())));

        let app_config_clone_post = app_config.clone();
        let app_config_clone_get = app_config.clone();
        let router = router.route(
            "/api/spools-config",
            post(
                move |State(Encryption(key)): State<Encryption>, SpoolsConfigDTO { spools }| {
                    let spools = if let Some(spools) = spools {
                        if !spools.trim().is_empty() {
                            Some(spools.trim().replace("\r\n", "\n").replace("\n", "\r\n"))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    ready(match app_config_clone_post.borrow_mut().set_user_cores(spools) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready( {
                        let borrowed_app_config = app_config_clone_get.borrow(); // notice the borrow, can't async here
                        let spools = &borrowed_app_config.user_cores;
                        let spools_config = SpoolsConfigDTO { spools: spools.clone() };
                        spools_config.encrypt(&key.borrow())
                    }
                )
            }),
        );

        router
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfigDTO {
    ip: Option<String>,
    name: Option<String>,
    serial: Option<String>,
    access_code: Option<String>,
    log_filter: Option<log::LevelFilter>
}
encrypted_input!(PrinterConfigDTO);
impl From<PrinterConfigDTO> for PrinterConfig {
    fn from (v: PrinterConfigDTO) -> Self {
        Self {
            ip: v.ip.and_then(|s| s.parse::<Ipv4Addr>().ok()),
            name: v.name, 
            serial: v.serial,
            access_code: v.access_code,
            log_filter: v.log_filter,
        }
    }
}
impl From<&PrinterConfig> for PrinterConfigDTO {
    fn from (v: &PrinterConfig) -> Self {
        Self {
            ip: v.ip.and_then(|ip| Some(ip.to_string())),
            name: v.name.clone(), 
            serial: v.serial.clone(),
            access_code: v.access_code.clone(),
            log_filter: v.log_filter,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrintersConfigDTO {
    printers: Vec<PrinterConfigDTO>,
    default_printer_serial: Option<String>,
}
encrypted_input!(PrintersConfigDTO);
impl From<PrintersConfigDTO> for PrintersConfig {
    fn from (v: PrintersConfigDTO) -> Self {
        Self {
            printers: v.printers
                .into_iter()
                .map(PrinterConfig::from) // Convert each Printer to PrinterDTO
                .collect(),
        }
    }
}
impl From<&PrintersConfig> for PrintersConfigDTO {
    fn from (v: &PrintersConfig) -> Self {
        Self {
            printers: v.printers
                .iter()
                .map(PrinterConfigDTO::from) // Convert each Printer to PrinterDTO
                .collect(),
            default_printer_serial: None,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SpoolsConfigDTO {
    spools: Option<String>,
}
encrypted_input!(SpoolsConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct ScaleConfigDTO {
    available: bool,
    name: Option<String>,
    ip: Option<String>,
}
encrypted_input!(ScaleConfigDTO);

impl From<ScaleConfigDTO> for ScaleConfig {
    fn from (v: ScaleConfigDTO) -> Self {
        Self {
            available: v.available,
            ip: v.ip.and_then(|s| s.parse::<Ipv4Addr>().ok()),
            name: v.name.filter(|s| !s.is_empty()), 
        }
    }
}
impl From<&ScaleConfig> for ScaleConfigDTO {
    fn from (v: &ScaleConfig) -> Self {
        Self {
            available: v.available,
            ip: v.ip.and_then(|ip| Some(ip.to_string())),
            name: v.name.clone(), 
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct TagConfigDTO {
    tag_scan_timeout: u64,
}
encrypted_input!(TagConfigDTO);
