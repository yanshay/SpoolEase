use core::cell::RefCell;
use core::future::ready;
use core::net::Ipv4Addr;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use embedded_sdmmc::asynchronous::LfnBuffer;
use framework::framework_web_app::{FrameworkState, encrypt, encrypt_bytes};
use hashbrown::HashMap;
use picoserve::response::StatusCode;
use picoserve::response::chunked::{ChunkWriter, ChunkedResponse, ChunksWritten};
use picoserve::routing::{get, get_service, post_service};
use picoserve::{
    AppWithStateBuilder,
    extract::{FromRequest, State},
    io::Read,
    request::{RequestBody, RequestParts},
    routing::post,
};

use framework::{
    encrypted_input,
    framework_web_app::{
        CustomNotFound, Encryptable, EncryptedRejection, Encryption, NestedAppWithWebAppStateBuilder, SetConfigResponseDTO, WebAppState, decrypt,
    },
    prelude::*,
};
use framework_macros::include_bytes_gz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared::gcode_analysis_task::Fetch3mf;

use crate::app_config::{
    AiProviderAvailability, AiProviderId, AppConfig, DefaultPrinterConfig, FILAMENT_BRAND_NAMES, PrinterConfig, PrinterMode, PrintersConfig,
    SPOOLS_CATALOG, ScaleConfig,
};
use crate::bambu::{bambu_api::PrintCommand, BambuPrinter};
use crate::bambu::calibration::KInfo;
use crate::spool_record::{SpoolRecord, SpoolRecordExt};
use crate::spools_storage::StorageConfig;
use crate::store::{BackupMeta, FileMeta, Store};
use crate::view_model::{PrinterInfo, ViewModel};

#[derive(Clone)]
pub struct ConsoleAppState {
    pub app_config: Rc<RefCell<AppConfig>>,
    pub view_model: Rc<RefCell<ViewModel>>,
    pub store: Rc<Store>,
}

impl picoserve::extract::FromRef<WebAppState<ConsoleAppState>> for ConsoleAppState {
    fn from_ref(state: &WebAppState<ConsoleAppState>) -> Self {
        state.more_state.clone()
    }
}

pub struct NestedAppBuilder {
    pub framework: Rc<RefCell<Framework>>,
    pub app_config: Rc<RefCell<AppConfig>>,
}

impl NestedAppWithWebAppStateBuilder<ConsoleAppState> for NestedAppBuilder {
    fn path_description(&self) -> &'static str {
        "" // this nests it at the root.
    }
}

impl AppWithStateBuilder for NestedAppBuilder {
    type State = WebAppState<ConsoleAppState>;
    type PathRouter = impl picoserve::routing::PathRouter<WebAppState<ConsoleAppState>>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let _app_config = self.app_config.clone();
        let _framework = self.framework.clone();

        let router = picoserve::Router::from_service(CustomNotFound {
            web_server_captive: self.framework.borrow().settings.web_server_captive,
        }); // Handler in case page is not found for captive portal support
        // let router = router.route("/", get(|| Redirect::to("/config"))); // Redirect root for now

        // Redirect root to the current active application - either config, or encode or whatever
        // For that, in order to preserve the hash (for sk=...), using a html/js redirect technique
        let router = router.route(
            "/",
            get(move |state: State<ConsoleAppState>| {
                ready({
                    let redirect_url = &state.0.app_config.borrow().root_redirect;
                    let redirect_html =
                        format!(r#"<!doctype html><script>location.href=location.hash?"{redirect_url}"+location.hash:"{redirect_url}"</script>"#);
                    HtmlStringResponse::new(redirect_html)
                })
            }),
        );

        //        TODO: >>>>>> Move to framework with setting for the css
        let router = router.route(
            "/styles.css",
            get_service(picoserve::response::File::with_content_type_and_headers(
                "text/css",
                include_bytes_gz!("static/styles.css"),
                &[("Content-Encoding", "gzip")],
            )),
        );

        let router = router.route(
            "/favicon-48x48.png",
            get_service(picoserve::response::File::with_content_type(
                "image/png",
                include_bytes!("../static/favicon-48x48.png"),
            )),
        );

        let router = router.route(
            "/api/printer-config",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, printers_config_dto: PrintersConfigDTO| {
                    let default_printer_serial = printers_config_dto.default_printer_serial.clone();
                    ready(
                        match state.0.app_config.borrow_mut().set_printers_config(
                            printers_config_dto.into(),
                            DefaultPrinterConfig {
                                serial: default_printer_serial,
                            },
                        ) {
                            Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    let borrowed_app_config = state.0.app_config.borrow(); // notice the borrow, can't async here
                    let empty_printers_config = PrintersConfig {
                        printers: alloc::vec![PrinterConfig::default()],
                    };
                    let no_configured_printers = borrowed_app_config.configured_printers.printers.is_empty();
                    let printers = if no_configured_printers {
                        &empty_printers_config
                    } else {
                        &borrowed_app_config.configured_printers
                    };
                    let default_printer = &borrowed_app_config.configured_default_printer;
                    let mut printers_config = PrintersConfigDTO::from(printers);
                    printers_config.default_printer_serial = default_printer.serial.clone();
                    printers_config.encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/scale-config",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, scale_config_dto: ScaleConfigDTO| {
                    ready(match state.0.app_config.borrow_mut().set_scale_config(scale_config_dto.into()) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    let borrowed_app_config = state.0.app_config.borrow(); // notice the borrow, can't async here
                    let default_scale_config = ScaleConfig::default();
                    let scale = borrowed_app_config.configured_scale.as_ref().unwrap_or(&default_scale_config);
                    let scale_config = ScaleConfigDTO::from(scale);
                    scale_config.encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/console-info",
            get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    let borrowed_app_config = state.0.app_config.borrow();
                    ConsoleInfoResponse {
                        ai_providers: borrowed_app_config.ai_provider_key_availability(),
                    }
                    .encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/printers-status",
            get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    GetPrintersStatusResponse {
                        printers: state.0.view_model.borrow().get_printers_status(),
                    }
                    .encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/printer-command",
            post(async move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, printer_command: PrinterCommandDTO| {
                let PrinterCommandDTO { printer_serial, command } = printer_command;
                let command_name = command.get_command().to_string();

                let printer = {
                    let borrowed_view_model = state.0.view_model.borrow();
                    borrowed_view_model
                        .bambu_printer_model
                        .printers
                        .iter()
                        .find(|printer| printer.borrow().printer_serial == printer_serial.as_str())
                        .cloned()
                };

                match printer {
                    Some(printer) => {
                        BambuPrinter::request_printer_command_async(&printer, command).await;
                        GenericResponse {
                            text: format!("Sent {command_name} command to printer {printer_serial}"),
                            error: None,
                        }
                        .encrypt(&key.borrow())
                    }
                    None => GenericResponse {
                        text: "Printer not found".to_string(),
                        error: Some(format!("Printer not found: {printer_serial}")),
                    }
                    .encrypt(&key.borrow()),
                }
            }),
        );

        let router = router.route(
            "/api/ai-provider-config/get",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, ai_provider_ref: AiProviderRefDTO| {
                    ready({
                        let api_key = state.0.app_config.borrow().get_ai_provider_api_key(&ai_provider_ref.provider);
                        GetAiProviderApiKeyResponse {
                            provider: ai_provider_ref.provider,
                            api_key,
                        }
                        .encrypt(&key.borrow())
                    })
                },
            ),
        );

        let router = router.route(
            "/api/ai-provider-config/set",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, set_ai_provider_api_key: SetAiProviderApiKeyDTO| {
                    ready(
                        match state
                            .0
                            .app_config
                            .borrow_mut()
                            .set_ai_provider_api_key(set_ai_provider_api_key.provider, set_ai_provider_api_key.api_key)
                        {
                            Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            ),
        );

        let router = router.route(
            "/api/ai-provider-config/delete",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, ai_provider_ref: AiProviderRefDTO| {
                    ready(
                        match state.0.app_config.borrow_mut().delete_ai_provider_api_key(ai_provider_ref.provider) {
                            Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            ),
        );

        let router = router.route(
            "/spools-catalog",
            get_service(picoserve::response::File::with_content_type(
                "text/plain; charset=utf-8",
                SPOOLS_CATALOG.as_bytes(),
            )),
        );

        let router = router.route(
            "/filament-brands",
            get_service(picoserve::response::File::with_content_type(
                "text/plain; charset=utf-8",
                FILAMENT_BRAND_NAMES.as_bytes(),
            )),
        );

        let router = router.route(
            "/api/spools-config",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, SpoolsConfigDTO { spools }| {
                    let spools = if let Some(spools) = spools {
                        if !spools.trim().is_empty() {
                            Some(spools.trim().replace("\r\n", "\n").replace("\n", "\r\n"))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    ready(match state.0.app_config.borrow_mut().set_user_cores(spools) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    let borrowed_app_config = state.0.app_config.borrow(); // notice the borrow, can't async here
                    let spools = &borrowed_app_config.user_cores;
                    let spools_config = SpoolsConfigDTO { spools: spools.clone() };
                    spools_config.encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/filaments-config",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, FilamentsConfigDTO { custom_filaments }| {
                    let custom_filaments = if let Some(custom_filaments) = custom_filaments {
                        if !custom_filaments.trim().is_empty() {
                            Some(custom_filaments.trim().replace("\r\n", "\n").replace("\n", "\r\n"))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    ready(match state.0.app_config.borrow_mut().set_filaments(custom_filaments) {
                        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
                        Err(e) => SetConfigResponseDTO {
                            error_text: Some(format!("{e:?}")),
                        }
                        .encrypt(&key.borrow()),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                ready({
                    let borrowed_app_config = state.0.app_config.borrow(); // notice the borrow, can't async here
                    let custom_filaments = &borrowed_app_config.custom_filaments;
                    let filaments_config = FilamentsConfigDTO {
                        custom_filaments: custom_filaments.clone(),
                    };
                    filaments_config.encrypt(&key.borrow())
                })
            }),
        );

        let router = router.route(
            "/api/spools-in-printers",
            get(async move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| {
                GetSpoolsInPrintersResponse {
                    spools: state.0.view_model.borrow().get_spools_in_printers(),
                }
                .encrypt(&key.borrow())
            }),
        );

        let router = router.route(
            "/api/spools",
            get(
                async move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>| match state.0.store.query_spools() {
                    Some(csv) => encrypt(&key.borrow(), &csv),
                    None => {
                        error!("Failed to generate response to spoole query");
                        "".to_string()
                    }
                },
            ),
        );

        let router = router.route(
            "/api/spools/delete",
            post(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>, delete_spool: DeleteSpoolDTO| {
                    let store = state.store;
                    match store.delete_spool(&delete_spool.id).await {
                        Ok(_) => match store.query_spools() {
                            Some(csv) => encrypt(&key.borrow(), &csv),
                            None => {
                                error!("Failed to generate response to spoole query");
                                "".to_string()
                            }
                        },
                        Err(err) => {
                            error!("Failed to delete spool {} : {err}", delete_spool.id);
                            err.to_string()
                        }
                    }
                },
            ),
        );

        let router = router.route(
            "/api/spools/add-edit",
            post(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>, add_spool: AddSpoolDTO| {
                    let store = state.store;
                    let mut split_spool = None;

                    if let Some(split_id) = add_spool.split
                        && let Some(splitted_spool) = store.get_spool_by_id(&split_id)
                    {
                        if splitted_spool.spools_count - add_spool.spools_count < 1 {
                            return format!(
                                "Spool {split_id} had {} and can't split out {}",
                                splitted_spool.spools_count, add_spool.spools_count
                            );
                        } else {
                            split_spool = Some(splitted_spool);
                        }
                    }
                    let color_code = add_spool
                        .rgba
                        .split(';')
                        .map(str::trim)
                        .filter(|c| !c.is_empty())
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>();

                    // this happens on successful split or when a simple add/edit and not split
                    let new_spool = SpoolRecord {
                        id: add_spool.id,
                        tag_id: if add_spool.tag_id.is_empty() {
                            Vec::new()
                        } else {
                            vec![add_spool.tag_id]
                        },
                        material_type: add_spool.material,
                        material_subtype: add_spool.subtype,
                        color_name: add_spool.color_name,
                        color_code,
                        note: add_spool.note,
                        brand: add_spool.brand,
                        weight_advertised: if add_spool.label_weight == 0 {
                            None
                        } else {
                            Some(add_spool.label_weight)
                        },
                        weight_core: if add_spool.core_weight == 0 { None } else { Some(add_spool.core_weight) },
                        weight_new: None,
                        weight_current: None,
                        slicer_filament: add_spool.slicer_filament,
                        added_time: None,  // will be added by store if required
                        encode_time: None, // will be added by store if required
                        added_full: match add_spool.full_unused.to_lowercase().as_str() {
                            "y" => Some(true),
                            "n" => Some(false),
                            _ => None,
                        },
                        consumed_since_add: 0.0,
                        consumed_since_weight: 0.0,
                        ext_has_k: add_spool.k_info.is_some(),
                        data_origin: String::new(),
                        tag_type: String::new(),
                        assigned_location: add_spool.assigned_location,
                        actual_location: add_spool.actual_location,
                        spools_count: add_spool.spools_count,
                    };

                    let spool_id = if new_spool.id.is_empty() {
                        match store
                            .add_spool(
                                new_spool,
                                SpoolRecordExt {
                                    tag: None,
                                    k_info: add_spool.k_info,
                                    origin_data: None,
                                },
                            )
                            .await
                        {
                            Ok(new_id) => {
                                state.view_model.borrow_mut().recently_added_spool_id = Some(new_id.clone());
                                new_id
                            }
                            Err(err) => {
                                error!("Failed to add spool : {err}");
                                return err.to_string();
                            }
                        }
                    } else {
                        let id = new_spool.id.clone();
                        match store.edit_spool_from_web(new_spool, add_spool.k_info).await {
                            Ok(_) => id,
                            Err(err) => {
                                error!("Failed to edit spool : {err}");
                                return err.to_string();
                            }
                        }
                    };

                    if let Some(mut split_spool) = split_spool {
                        split_spool.spools_count -= add_spool.spools_count;
                        if let Err(err) = store.update_spool(split_spool, None).await {
                            error!("Critical: Added new splitted spool/stock but failed to update splitted stock : {err}");
                            return err.to_string();
                        }
                    }

                    match store.query_spools() {
                        Some(csv) => AddSpoolDTOResponse { id: spool_id, csv }.encrypt(&key.borrow()),
                        None => {
                            error!("Failed to generate response to spoole query");
                            "Failed to generate response to spoole query".to_string()
                        }
                    }
                },
            ),
        );

        let router = router.route(
            "/api/printers-filament-pa",
            post(
                move |State(Encryption(key)): State<Encryption>,
                      state: State<ConsoleAppState>,
                      get_printers_filament_pa: GetPrintersFilamentPaDTO| {
                    ready({
                        let view_model_borrow = state.0.view_model.borrow_mut();
                        let printers = &view_model_borrow.bambu_printer_model.printers;
                        let printers_filament_pa = printers
                            .iter()
                            .map(|printer| {
                                (
                                    printer.borrow().printer_serial.clone(),
                                    PrinterEntry {
                                        name: printer.borrow().printer_name().clone(),
                                        extruders: printer.borrow().num_extruders(),
                                        pressure_advance: printer
                                            .borrow()
                                            .calibrations
                                            .iter()
                                            .filter(|cal| cal.filament_id == get_printers_filament_pa.slicer_filament_code)
                                            .map(|pa| PressureAdvanceEntry {
                                                extruder: pa.extruder,
                                                diameter: pa.diameter.clone(),
                                                nozzle_id: pa.nozzle_id.clone(),
                                                name: pa.name.clone(),
                                                k_value: pa.k_value.clone(),
                                                cali_idx: pa.cali_idx,
                                                setting_id: pa.setting_id.clone(),
                                            })
                                            .collect::<Vec<_>>(),
                                    },
                                )
                            })
                            .collect::<HashMap<_, _>>();
                        GetPrintersFilamentPaDTOResponse {
                            printers: printers_filament_pa,
                        }
                        .encrypt(&key.borrow())
                    })
                },
            ),
        );

        let router = router.route(
            "/api/add-printer-pa",
            post(
                move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, add_pa: AddPressureAdvanceDTO| {
                    ready({
                        match state.0.view_model.borrow_mut().add_calibration_to_printer(
                            &add_pa.printer_serial,
                            add_pa.pressure_advance_entry.extruder,
                            &add_pa.pressure_advance_entry.diameter,
                            &add_pa.pressure_advance_entry.nozzle_id,
                            &add_pa.filament_id,
                            &add_pa.pressure_advance_entry.setting_id.unwrap_or_default(),
                            &add_pa.pressure_advance_entry.k_value,
                            &add_pa.pressure_advance_entry.name,
                        ) {
                            Ok(_) => GenericResponse {
                                error: None,
                                text: "Sent Pressure Advance Add Request to Printer".to_string(),
                            }
                            .encrypt(&key.borrow()),
                            Err(err) => GenericResponse { error: None, text: err }.encrypt(&key.borrow()),
                        }
                    })
                },
            ),
        );

        let router = router.route(
            "/api/spool-kinfo",
            post(
                async move |State(Encryption(key)): State<Encryption>, state: State<ConsoleAppState>, get_spool_kinfo: GetSpoolKInfoDTO| {
                    let store = state.0.view_model.borrow_mut().store.clone();
                    match store.get_spool_ext_by_id(&get_spool_kinfo.id).await {
                        Ok(spool_rec_ext) => Ok::<String, StatusCode>(
                            GetSpoolKInfoDTOResponse {
                                k_info: spool_rec_ext.k_info,
                            }
                            .encrypt(&key.borrow()),
                        ),
                        Err(_) => Err::<String, StatusCode>(StatusCode::new(404)),
                    }
                },
            ),
        );

        // Web App //

        let router = router.route(
            "/inventory",
            get_service(picoserve::response::File::with_content_type_and_headers(
                "text/html",
                include_bytes_gz!("static/inventory/index.html"),
                &[("Content-Encoding", "gzip")],
            )),
        );

        let router = router.route(
            "/L1/",
            get_service(picoserve::response::File::with_content_type_and_headers(
                "text/html",
                include_bytes_gz!("static/consoletag/index.html"),
                &[("Content-Encoding", "gzip")],
            )),
        );

        // let router = router.route(
        //     "/inventory.js",
        //     get_service(picoserve::response::File::with_content_type_and_headers(
        //         "application/javascript; charset=utf-8",
        //         include_bytes!("../static/inventory/inventory.js.gz"),
        //         &[("Content-Encoding", "gzip")],
        //     )),
        // );

        let router = router.route(
            "/api/store-backup",
            get(move |State(Encryption(key)), State(FrameworkState(framework))| async move {
                ChunkedResponse::new(StoreBackupChunks {
                    framework: framework.clone(),
                    key,
                })
                .into_response()
            }),
        );

        let router = router.route("/api/store-restore-upload", post_service(StoreRestoreUploadService));

        let router = router.route(
            "/api/store-restore-delete",
            post(
                async move |State(Encryption(key)): State<Encryption>,
                            State(FrameworkState(framework)): State<FrameworkState>,
                            RestoreDeleteDTO {}| {
                    cleanup_restore_upload_temp(&framework).await;
                    GenericResponse {
                        text: "Deleted uploaded backup files".to_string(),
                        error: None,
                    }
                    .encrypt(&key.borrow())
                },
            ),
        );

        #[derive(serde::Deserialize)]
        struct ScreenshotQueryParams {
            key: String,
            file: String,
        }
        let router = router.route(
            "/insecure/screenshot",
            get(
                move |picoserve::extract::Query(ScreenshotQueryParams { file, key }),
                      State(Encryption(_key)),
                      State(state): State<ConsoleAppState>,
                      State(FrameworkState(framework))| async move {
                    if key == framework.borrow().web_config_key {
                        let screenshot = state.view_model.borrow().taks_screenshot();
                        let resp = ChunkedResponse::new(ScreenshotChunks { screenshot }).into_response();
                        resp.with_header("Content-Disposition", format!("attachment; filename=\"{file}\""))
                            .with_status_code(StatusCode::OK)
                    } else {
                        let screenshot = Err(slint::PlatformError::Other("Security Key Error".to_string()));
                        let resp = ChunkedResponse::new(ScreenshotChunks { screenshot }).into_response();
                        resp.with_header("", String::new()).with_status_code(StatusCode::UNAUTHORIZED)
                    }
                },
            ),
        );

        let router = router.route(
            "/api/storage-config",
            post(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>, storage_config: StorageConfig| {
                    let store = state.store;
                    match store.set_storage_config(storage_config).await {
                        Ok(storage_config_str) => encrypt(&key.borrow(), &storage_config_str),
                        Err(err) => {
                            error!("Failed to store Storage Configuration : {err}");
                            err.to_string()
                        }
                    }
                },
            )
            .get(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>| {
                    let store = state.store;
                    let storage_config_str = serde_json::to_string(&*store.storage_config.borrow()).unwrap();
                    encrypt(&key.borrow(), &storage_config_str)
                },
            ),
        );

        let router = router.route(
            "/api/tag-scanned",
            post(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>, tag_scanned: TagScannedDTO| {
                    let store = state.store;
                    if let Some(location_rec) = store.get_location_by_hex_tag(&tag_scanned.tag_id_hex) {
                        TagScannedResponse {
                            tag_info: TagInfo::Location {
                                location: location_rec.location,
                            },
                        }
                        .encrypt(&key.borrow())
                    } else {
                        TagScannedResponse { tag_info: TagInfo::Unknown }.encrypt(&key.borrow())
                    }
                },
            ),
        );

        let router = router.route(
            "/api/set-tag-location",
            post(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>, set_tag_location: SetTagLocationDTO| {
                    let store = state.store;
                    if set_tag_location.location.is_empty() {
                        match store.delete_location(&set_tag_location.tag_id_hex).await {
                            Ok(_) => GenericResponse {
                                error: None,
                                text: "Tag location cleared".to_string(),
                            }
                            .encrypt(&key.borrow()),
                            Err(err) => GenericResponse {
                                error: Some(format!("Failed to clear tag location: {err:?}")),
                                text: String::new(),
                            }
                            .encrypt(&key.borrow()),
                        }
                    } else {
                        match store.insert_tag_location(&set_tag_location.tag_id_hex, &set_tag_location.location).await {
                            Ok(true) => GenericResponse {
                                error: None,
                                text: "Location assigned to tag".to_string(),
                            }
                            .encrypt(&key.borrow()),
                            Ok(false) => GenericResponse {
                                error: None,
                                text: "Tag's location updated".to_string(),
                            }
                            .encrypt(&key.borrow()),
                            Err(err) => GenericResponse {
                                error: Some(format!("Error setting tag location: {err:?}")),
                                text: String::new(),
                            }
                            .encrypt(&key.borrow()),
                        }
                    }
                },
            ),
        );

        let router = router.route(
            "/api/spool-staging",
            get(
                async move |State(Encryption(key)): State<Encryption>, State(state): State<ConsoleAppState>| {
                    let view_model = state.view_model.borrow();
                    let filament_staging = view_model.filament_staging.borrow();
                    if let Some(spool_rec) = filament_staging.spool_rec() {
                        let store = state.store;
                        if let Some(record_csv) = store.get_spool_csv_by_id(&spool_rec.id) {
                            return SpoolStagingResponse { csv_record: record_csv }.encrypt(&key.borrow());
                        }
                    }
                    return SpoolStagingResponse { csv_record: String::new() }.encrypt(&key.borrow());
                },
            ),
        );

        #[allow(clippy::let_and_return)]
        let router = router.route(
            "/api/spool-location",
            post(
                async move |State(Encryption(key)): State<Encryption>,
                            State(state): State<ConsoleAppState>,
                            set_spool_location: SetSpoolLocationDTO| {
                    let store = state.store;
                    if let Some(mut spool_rec) = store.get_spool_by_id(&set_spool_location.spool_id) {
                        match set_spool_location.location_type {
                            LocationType::Assigned => spool_rec.assigned_location = set_spool_location.location,
                            LocationType::Actual => spool_rec.actual_location = set_spool_location.location,
                        }
                        match store.update_spool(spool_rec, None).await {
                            Ok(_) => {
                                return GenericResponse {
                                    text: format!("Spool {} updated", set_spool_location.spool_id),
                                    error: None,
                                }
                                .encrypt(&key.borrow());
                            }
                            Err(err) => {
                                return GenericResponse {
                                    text: format!("Tried to update Spool {}", set_spool_location.spool_id),
                                    error: Some(format!("Failed to update Spool {} : {err}", set_spool_location.spool_id)),
                                }
                                .encrypt(&key.borrow());
                            }
                        }
                    } else {
                        return GenericResponse {
                            text: format!("Tried to update Spool {}", set_spool_location.spool_id),
                            error: Some(format!("No Spool {} in store", set_spool_location.spool_id)),
                        }
                        .encrypt(&key.borrow());
                    }
                },
            ),
        );

        router
    }
}

struct StoreRestoreUploadService;

async fn cleanup_restore_upload_temp(framework: &Rc<RefCell<Framework>>) {
    let file_store = framework.borrow().file_store();
    let mut file_store = file_store.lock().await;
    let _ = file_store.delete_file("/store.bak").await;
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();

    let mut out = String::with_capacity(64);
    for byte in digest {
        let hi = (byte >> 4) & 0x0f;
        let lo = byte & 0x0f;
        out.push((if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) }) as char);
        out.push((if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) }) as char);
    }
    out
}

fn parse_sha256_header(mut raw_sha: String) -> Result<String, String> {
    raw_sha = raw_sha.trim().to_ascii_lowercase();
    if raw_sha.len() != 64 {
        return Err("Invalid x-file-sha256 header length".to_string());
    }
    if !raw_sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Invalid x-file-sha256 header characters".to_string());
    }
    Ok(raw_sha)
}

fn append_restore_frame(data: &mut Vec<u8>, key: &'static RefCell<Vec<u8>>, encrypted_frame: &[u8], expected_size: usize) -> Result<(), String> {
    if encrypted_frame.is_empty() {
        return Ok(());
    }

    let decrypted = decrypt(&key.borrow(), encrypted_frame).map_err(|e| format!("Failed decrypting frame: {e}"))?;
    let frame_bytes = BASE64_STANDARD
        .decode(decrypted.as_bytes())
        .map_err(|e| format!("Failed base64 decoding frame: {e}"))?;

    if data.len() + frame_bytes.len() > expected_size {
        return Err("Uploaded data exceeds x-file-size".to_string());
    }
    data.extend_from_slice(&frame_bytes);

    Ok(())
}

impl picoserve::routing::RequestHandlerService<WebAppState<ConsoleAppState>> for StoreRestoreUploadService {
    async fn call_request_handler_service<R: Read, W: picoserve::response::ResponseWriter<Error = R::Error>>(
        &self,
        state: &WebAppState<ConsoleAppState>,
        (): (),
        mut request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let key = state.encryption.0;
        let framework = state.framework.0.clone();

        let headers = request.parts.headers();
        let expected_size = headers
            .get("x-file-size")
            .and_then(|value| value.as_str().ok().map(|text| text.to_string()))
            .and_then(|value| value.parse::<usize>().ok());
        let expected_size = match expected_size {
            Some(size) => size,
            None => {
                let connection = request.body_connection.finalize().await?;
                let resp = GenericResponse {
                    text: "Backup upload failed".to_string(),
                    error: Some("Missing or invalid x-file-size header".to_string()),
                };
                let payload = resp.encrypt(&key.borrow());
                return response_writer
                    .write_response(connection, picoserve::response::Response::new(StatusCode::BAD_REQUEST, payload))
                    .await;
            }
        };

        let expected_sha = headers
            .get("x-file-sha256")
            .and_then(|value| value.as_str().ok().map(|text| text.to_string()))
            .map(parse_sha256_header);
        let expected_sha = match expected_sha {
            Some(Ok(sha)) => sha,
            Some(Err(err)) => {
                let connection = request.body_connection.finalize().await?;
                let resp = GenericResponse {
                    text: "Backup upload failed".to_string(),
                    error: Some(err),
                };
                let payload = resp.encrypt(&key.borrow());
                return response_writer
                    .write_response(connection, picoserve::response::Response::new(StatusCode::BAD_REQUEST, payload))
                    .await;
            }
            None => {
                let connection = request.body_connection.finalize().await?;
                let resp = GenericResponse {
                    text: "Backup upload failed".to_string(),
                    error: Some("Missing x-file-sha256 header".to_string()),
                };
                let payload = resp.encrypt(&key.borrow());
                return response_writer
                    .write_response(connection, picoserve::response::Response::new(StatusCode::BAD_REQUEST, payload))
                    .await;
            }
        };

        cleanup_restore_upload_temp(&framework).await;

        let mut response = GenericResponse {
            text: "Backup upload completed to /store.bak".to_string(),
            error: None,
        };
        let mut status_code = StatusCode::OK;

        let upload_result: Result<(), String> = async {
            let mut reader = request.body_connection.body().reader();
            let mut read_buffer = vec![0u8; 4096];
            let mut frame_buffer = Vec::<u8>::with_capacity(8192);
            let mut file_data = Vec::<u8>::new();
            file_data
                .try_reserve_exact(expected_size)
                .map_err(|_| "Not enough memory for upload".to_string())?;
            let mut processed_frames = 0usize;

            loop {
                let read_size = reader
                    .read(&mut read_buffer[..])
                    .await
                    .map_err(|e| format!("Failed reading request body: {e:?}"))?;
                if read_size == 0 {
                    break;
                }

                let chunk = &read_buffer[..read_size];
                let mut chunk_start = 0usize;
                while let Some(rel_delimiter_index) = chunk[chunk_start..].iter().position(|&byte| byte == b'|') {
                    let delimiter_index = chunk_start + rel_delimiter_index;
                    frame_buffer.extend_from_slice(&chunk[chunk_start..delimiter_index]);

                    if !frame_buffer.is_empty() {
                        append_restore_frame(&mut file_data, key, &frame_buffer, expected_size)?;
                        processed_frames += 1;
                    }
                    frame_buffer.clear();
                    chunk_start = delimiter_index + 1;
                }

                if chunk_start < chunk.len() {
                    frame_buffer.extend_from_slice(&chunk[chunk_start..]);
                }
            }

            if !frame_buffer.is_empty() {
                append_restore_frame(&mut file_data, key, &frame_buffer, expected_size)?;
                processed_frames += 1;
            }

            if processed_frames == 0 {
                return Err("Upload body is empty".to_string());
            }

            if file_data.len() != expected_size {
                return Err(format!(
                    "Uploaded data size mismatch. expected={} actual={}",
                    expected_size,
                    file_data.len()
                ));
            }

            let file_store = framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            file_store
                .create_write_file_bytes("/store.bak", &file_data)
                .await
                .map_err(|err| format!("Failed writing /store.bak: {err:?}"))?;

            let verify_data = file_store
                .read_file_bytes("/store.bak")
                .await
                .map_err(|err| format!("Failed reading /store.bak for checksum verification: {err:?}"))?;
            let actual_sha = sha256_hex(&verify_data);
            if actual_sha != expected_sha {
                let _ = file_store.delete_file("/store.bak").await;
                return Err(format!("Checksum mismatch after save. expected={expected_sha} actual={actual_sha}"));
            }

            Ok(())
        }
        .await;

        if let Err(err) = upload_result {
            cleanup_restore_upload_temp(&framework).await;
            response = GenericResponse {
                text: "Backup upload failed".to_string(),
                error: Some(err),
            };
            status_code = StatusCode::BAD_REQUEST;
        }

        let connection = request.body_connection.finalize().await?;
        let payload = response.encrypt(&key.borrow());
        response_writer
            .write_response(connection, picoserve::response::Response::new(status_code, payload))
            .await
    }
}

struct ScreenshotChunks {
    screenshot: Result<slint::SharedPixelBuffer<slint::Rgba8Pixel>, slint::PlatformError>,
}

impl picoserve::response::chunked::Chunks for ScreenshotChunks {
    fn content_type(&self) -> &'static str {
        "application/octet-stream"
    }
    async fn write_chunks<W: picoserve::io::Write>(self, mut chunk_writer: ChunkWriter<W>) -> Result<ChunksWritten, W::Error> {
        if let Ok(screenshot) = self.screenshot {
            chunk_writer.write_chunk(screenshot.as_bytes()).await?;
        }
        chunk_writer.finalize().await
    }
}

struct StoreBackupChunks {
    framework: Rc<RefCell<Framework>>,
    key: &'static RefCell<Vec<u8>>,
}

impl picoserve::response::chunked::Chunks for StoreBackupChunks {
    fn content_type(&self) -> &'static str {
        "text/plain"
    }

    async fn write_chunks<W: picoserve::io::Write>(self, mut chunk_writer: ChunkWriter<W>) -> Result<ChunksWritten, W::Error> {
        info!("Backup Store Started");
        let file_store = self.framework.borrow().file_store();
        let mut files: Vec<String> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        dirs.push("/store".to_string());
        let mut lfn_buffer_storage = alloc::vec![0u8;32];
        let mut lfn_buffer = LfnBuffer::new(lfn_buffer_storage.as_mut_slice());
        let backup_meta = BackupMeta {
            spoolease_console_ver: self.framework.borrow().settings.app_cargo_pkg_version.to_string(),
        };
        let mut backup_meta_str = serde_json::to_string(&backup_meta).unwrap();
        backup_meta_str += "\n";
        let encrypted = encrypt(&self.key.borrow(), &backup_meta_str);
        chunk_writer.write_chunk(encrypted.as_bytes()).await?;
        chunk_writer.write_chunk("|".as_bytes()).await?;
        while !dirs.is_empty() {
            let curr_dir_path = dirs.remove(0);
            {
                info!("Traversing directory: {curr_dir_path}");
                {
                    let mut file_store = file_store.lock().await;
                    match file_store.open_dir(&curr_dir_path, framework::sdcard_store::Mode::ReadOnly).await {
                        Ok(rawdir) => {
                            let dir = rawdir.to_directory(file_store.volume_mgr());
                            if let Err(e) = dir
                                .iterate_dir_lfn(&mut lfn_buffer, |dir_entry, long_name| {
                                    let dir_entry_name = if let Some(long_name) = long_name {
                                        long_name.to_string()
                                    } else {
                                        dir_entry.name.to_string()
                                    };
                                    if !dir_entry_name.starts_with(".") {
                                        let full_path = format!("{}/{}", curr_dir_path, dir_entry.name);
                                        if dir_entry.attributes.is_directory() {
                                            dirs.push(full_path);
                                        } else {
                                            files.push(full_path);
                                        }
                                    }
                                })
                                .await
                            {
                                error!("Error iterating directory {curr_dir_path} : {e:?}");
                            }
                            let rawdir = dir.to_raw_directory();
                            if let Err(e) = file_store.close_dir(rawdir).await {
                                error!("Error closing sdcard directory : {e:?}");
                            }
                        }
                        Err(_) => todo!(),
                    }
                }
                let mut buffer = Vec::<u8>::with_capacity(1024);

                for file_path in files.drain(..) {
                    info!("Backing up file {file_path}");
                    buffer.clear();
                    let file_content = {
                        let mut file_store = file_store.lock().await;
                        if let Ok(file_content) = file_store.read_file_str(&file_path).await {
                            file_content
                        } else {
                            error!("Error reading file {file_path}");
                            format!("Error reading file {file_path}")
                        }
                    };

                    let file_meta = FileMeta {
                        path: file_path,
                        length: file_content.len(),
                    };
                    let file_meta_str = serde_json::to_string(&file_meta).unwrap();
                    buffer.extend_from_slice(file_meta_str.as_bytes());
                    buffer.extend_from_slice("\n".as_bytes());
                    buffer.extend_from_slice(file_content.as_bytes());
                    buffer.extend_from_slice("\n".as_bytes());
                    let encrypted = encrypt_bytes(&self.key.borrow(), &buffer);
                    chunk_writer.write_chunk(encrypted.as_bytes()).await?;
                    chunk_writer.write_chunk("|".as_bytes()).await?;
                }
            }
        }
        let res = chunk_writer.finalize().await;
        info!("Backup Store Completed");
        res
    }
}
#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfigDTO {
    ip: Option<String>,
    name: Option<String>,
    serial: Option<String>,
    access_code: Option<String>,
    log_filter: Option<log::LevelFilter>,
    auto_restore_k: bool,
    track_print_consume: bool,
    fetch_3mf: Option<String>,
    #[serde(default)]
    ignore_certificates: bool,
    #[serde(default)]
    printer_mode: PrinterMode,
}
encrypted_input!(PrinterConfigDTO);
impl From<PrinterConfigDTO> for PrinterConfig {
    fn from(v: PrinterConfigDTO) -> Self {
        Self {
            ip: v.ip.and_then(|s| s.parse::<Ipv4Addr>().ok()),
            name: v.name,
            serial: v.serial,
            access_code: v.access_code,
            log_filter: v.log_filter,
            auto_restore_k: v.auto_restore_k,
            track_print_consume: v.track_print_consume,
            fetch_3mf: if v.fetch_3mf.as_deref().unwrap_or("") == "printer-ftp" {
                Fetch3mf::PrinterFtp
            } else {
                Fetch3mf::CloudHttp
            },
            ignore_certificates: v.ignore_certificates,
            printer_mode: v.printer_mode,
        }
    }
}
impl From<&PrinterConfig> for PrinterConfigDTO {
    fn from(v: &PrinterConfig) -> Self {
        Self {
            ip: v.ip.map(|ip| ip.to_string()),
            name: v.name.clone(),
            serial: v.serial.clone(),
            access_code: v.access_code.clone(),
            log_filter: v.log_filter,
            auto_restore_k: v.auto_restore_k,
            track_print_consume: v.track_print_consume,
            fetch_3mf: match v.fetch_3mf {
                Fetch3mf::PrinterFtp => Some("printer-ftp".to_string()),
                Fetch3mf::CloudHttp => Some("cloud-http".to_string()),
            },
            ignore_certificates: v.ignore_certificates,
            printer_mode: v.printer_mode,
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
    fn from(v: PrintersConfigDTO) -> Self {
        Self {
            printers: v
                .printers
                .into_iter()
                .map(PrinterConfig::from) // Convert each Printer to PrinterDTO
                .collect(),
        }
    }
}
impl From<&PrintersConfig> for PrintersConfigDTO {
    fn from(v: &PrintersConfig) -> Self {
        Self {
            printers: v
                .printers
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
struct FilamentsConfigDTO {
    custom_filaments: Option<String>,
}
encrypted_input!(FilamentsConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct ScaleConfigDTO {
    available: bool,
    name: Option<String>,
    ip: Option<String>,
    key: Option<String>,
}
encrypted_input!(ScaleConfigDTO);

impl From<ScaleConfigDTO> for ScaleConfig {
    fn from(v: ScaleConfigDTO) -> Self {
        Self {
            available: v.available,
            ip: v.ip.and_then(|s| s.parse::<Ipv4Addr>().ok()),
            name: v.name.filter(|s| !s.is_empty()),
            key: v.key.filter(|s| !s.is_empty()),
        }
    }
}
impl From<&ScaleConfig> for ScaleConfigDTO {
    fn from(v: &ScaleConfig) -> Self {
        Self {
            available: v.available,
            ip: v.ip.map(|ip| ip.to_string()),
            name: v.name.clone(),
            key: v.key.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AiProviderRefDTO {
    provider: AiProviderId,
}
encrypted_input!(AiProviderRefDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct SetAiProviderApiKeyDTO {
    provider: AiProviderId,
    api_key: String,
}
encrypted_input!(SetAiProviderApiKeyDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct GetAiProviderApiKeyResponse {
    provider: AiProviderId,
    api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConsoleInfoResponse {
    ai_providers: Vec<AiProviderAvailability>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetPrintersStatusResponse {
    printers: Vec<PrinterInfo>,
}

// #[derive(serde::Deserialize, serde::Serialize, Default, Debug)]
// pub struct EncodeInfoDTO {
//     pub tray_id: i32,
//     pub id: String,
//     pub tag_id: String,
//     pub color_code: String,
//     pub color_name: String,
//     pub material: String,
//     pub filament_subtype: String,
//     pub slicer_filament: String,
//     pub brand: String,
//     pub weight_advertised: i32,
//     pub weight_core: i32,
//     pub note: String,
// }
// encrypted_input!(EncodeInfoDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub struct DeleteSpoolDTO {
    pub id: String,
}
encrypted_input!(DeleteSpoolDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub struct AddSpoolDTO {
    pub tag_id: String,
    pub id: String,
    pub rgba: String,
    pub color_name: String,
    pub material: String,
    pub subtype: String,
    pub brand: String,
    pub core_weight: i32,
    pub label_weight: i32,
    pub note: String,
    pub slicer_filament: String,
    pub full_unused: String,
    pub k_info: Option<KInfo>,
    pub assigned_location: String,
    pub actual_location: String,
    pub spools_count: i32,
    pub split: Option<String>,
}
encrypted_input!(AddSpoolDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub struct AddSpoolDTOResponse {
    pub id: String,
    pub csv: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct GetPrintersFilamentPaDTO {
    slicer_filament_code: String,
}
encrypted_input!(GetPrintersFilamentPaDTO);

#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct GetPrintersFilamentPaDTOResponse {
    pub printers: HashMap<String, PrinterEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrinterEntry {
    pub name: String,
    pub extruders: u32,
    pub pressure_advance: Vec<PressureAdvanceEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PressureAdvanceEntry {
    pub extruder: i32,
    pub diameter: String,
    pub nozzle_id: String,
    pub name: String,
    pub k_value: String,
    pub cali_idx: i32,
    pub setting_id: Option<String>,
}

//

#[derive(serde::Deserialize, serde::Serialize)]
pub struct GetSpoolKInfoDTO {
    id: String,
}
encrypted_input!(GetSpoolKInfoDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub struct GetSpoolKInfoDTOResponse {
    pub k_info: Option<KInfo>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct GetSpoolsInPrintersResponse {
    pub spools: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddPressureAdvanceDTO {
    printer_serial: String,
    filament_id: String,
    pressure_advance_entry: PressureAdvanceEntry,
}
encrypted_input!(AddPressureAdvanceDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct PrinterCommandDTO {
    printer_serial: String,
    command: PrintCommand,
}
encrypted_input!(PrinterCommandDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericResponse {
    text: String,
    error: Option<String>,
}
encrypted_input!(GenericResponse);

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreDeleteDTO {}
encrypted_input!(RestoreDeleteDTO);

encrypted_input!(StorageConfig);

#[derive(Debug, Serialize, Deserialize)]
pub struct TagScannedDTO {
    tag_id_hex: String,
}

encrypted_input!(TagScannedDTO);

#[derive(Debug, Serialize, Deserialize)]
pub enum TagInfo {
    Unknown,
    // Spool { spool_rec: SpoolRecord },
    Location { location: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TagScannedResponse {
    tag_info: TagInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetTagLocationDTO {
    tag_id_hex: String,
    location: String,
}
encrypted_input!(SetTagLocationDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct SpoolStagingResponse {
    pub csv_record: String,
}

#[derive(Debug, Serialize, Deserialize)]
enum LocationType {
    Assigned,
    Actual,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SetSpoolLocationDTO {
    location: String,
    location_type: LocationType,
    spool_id: String,
}
encrypted_input!(SetSpoolLocationDTO);

/////////////////////////////////////////////

struct HtmlStringResponse {
    html: String,
}

impl HtmlStringResponse {
    pub fn new(html: String) -> Self {
        Self { html }
    }
}

impl picoserve::response::Content for HtmlStringResponse {
    fn content_type(&self) -> &'static str {
        "text/html; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        self.html.len()
    }

    async fn write_content<W: embedded_io_async::Write>(self, writer: W) -> Result<(), W::Error> {
        self.html.as_bytes().write_content(writer).await
    }
}
