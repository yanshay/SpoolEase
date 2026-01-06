use core::cell::RefCell;
use core::future::ready;
use core::net::Ipv4Addr;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use embedded_sdmmc::asynchronous::LfnBuffer;
use framework::framework_web_app::{encrypt, encrypt_bytes, FrameworkState};
use hashbrown::HashMap;
use picoserve::response::chunked::{ChunkWriter, ChunkedResponse, ChunksWritten};
use picoserve::response::StatusCode;
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
    not_encrypted_input,
    prelude::*,
};
use framework_macros::include_bytes_gz;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::Fetch3mf;

use crate::app_config::{AppConfig, DefaultPrinterConfig, PrinterConfig, PrintersConfig, ScaleConfig, FILAMENT_BRAND_NAMES, SPOOLS_CATALOG};
use crate::bambu::KInfo;
use crate::spool_record::{SpoolRecord, SpoolRecordExt};
use crate::spools_storage::{StorageConfig, TagLocationRecord};
use crate::store::{BackupMeta, FileMeta, Store};
use crate::view_model::ViewModel;

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
                    let printers = &borrowed_app_config.configured_printers;
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
                    let new_spool = SpoolRecord {
                        id: add_spool.id,
                        tag_id: add_spool.tag_id,
                        material_type: add_spool.material,
                        material_subtype: add_spool.subtype,
                        color_name: add_spool.color_name,
                        color_code: add_spool.rgba,
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
                    };
                    if new_spool.id.is_empty() {
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
                            Ok(new_id) => match store.query_spools() {
                                Some(csv) => {
                                    state.view_model.borrow_mut().recently_added_spool_id = Some(new_id.clone());
                                    AddSpoolDTOResponse { id: new_id, csv }.encrypt(&key.borrow())
                                }
                                None => {
                                    error!("Failed to generate response to spoole query");
                                    "".to_string()
                                }
                            },
                            Err(err) => {
                                error!("Failed to add spool : {err}");
                                err.to_string()
                            }
                        }
                    } else {
                        let id = new_spool.id.clone();
                        match store.edit_spool_from_web(new_spool, add_spool.k_info).await {
                            Ok(_) => match store.query_spools() {
                                Some(csv) => AddSpoolDTOResponse { id, csv }.encrypt(&key.borrow()),
                                None => {
                                    error!("Failed to generate response to spoole query");
                                    "".to_string()
                                }
                            },
                            Err(err) => {
                                error!("Failed to edit spool : {err}");
                                err.to_string()
                            }
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
                async move |State(Encryption(_key)): State<Encryption>, State(state): State<ConsoleAppState>, tag_scanned: TagScannedDTO| {
                    let store = state.store;
                    if let Some(location_rec) = store.get_location_by_hex_tag(&tag_scanned.tag_id_hex) {
                        serde_json::to_string(&TagScannedResponse {
                            tag_info: TagInfo::Location { location_rec },
                        })
                        .unwrap()
                    } else {
                        serde_json::to_string(&TagScannedResponse { tag_info: TagInfo::Unknown }).unwrap()
                    }
                },
            ),
        );

        #[allow(clippy::let_and_return)]
        let router = router.route(
            "/api/set-tag-location",
            post(
                async move |State(Encryption(_key)): State<Encryption>, State(state): State<ConsoleAppState>, set_tag_location: SetTagLocationDTO| {
                    let store = state.store;
                    match store.insert_tag_location(&set_tag_location.tag_id_hex, &set_tag_location.location).await {
                        Ok(true) => serde_json::to_string(&GenericResponse { error: None, text: "Location assigned to tag".to_string()}).unwrap(),
                        Ok(false) => serde_json::to_string(&GenericResponse { error: None, text: "Tag's location updated".to_string()}).unwrap(),
                        Err(err) => serde_json::to_string(&GenericResponse{ error: Some(format!("Error setting tag location: {err:?}")), text: String::new()}).unwrap(),
                    }
                },
            ),
        );

        // #[allow(clippy::let_and_return)]
        // let router = router.route(
        //     "/style.css",
        //     get_service(picoserve::response::File::with_content_type_and_headers(
        //         "text/css",
        //         include_bytes!("../static/inventory/style.css.gz"),
        //         &[("Content-Encoding", "gzip")],
        //     )),
        // );

        router
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
pub struct GenericResponse {
    text: String,
    error: Option<String>,
}
encrypted_input!(GenericResponse);

encrypted_input!(StorageConfig);

#[derive(Debug, Serialize, Deserialize)]
pub struct TagScannedDTO {
    tag_id_hex: String,
}

not_encrypted_input!(TagScannedDTO);

#[derive(Debug, Serialize, Deserialize)]
pub enum TagInfo {
    Unknown,
    Spool { spool_rec: SpoolRecord },
    Location { location_rec: TagLocationRecord },
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
not_encrypted_input!(SetTagLocationDTO);

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
