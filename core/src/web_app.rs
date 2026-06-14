use core::cell::RefCell;
use core::future::Future;
use core::net::Ipv4Addr;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use framework::framework_web_app::{encrypt, encrypt_bytes, encrypt_bytes_compact};
use hashbrown::HashMap;
use picoserve::response::chunked::{ChunkWriter, ChunkedResponse, ChunksWritten};
use picoserve::response::{Body, HeadersIter, Response, ResponseWriter, StatusCode};
use picoserve::routing::RequestHandlerService;
use picoserve::{
    ResponseSent,
    extract::{FromRequest, FromRequestParts, Query},
    io::Read,
    request::{Path, Request, RequestBody, RequestBodyConnection, RequestParts},
};

use framework::handled_route_future as box_route_future;
use framework::{
    display_snapshot::{DisplaySnapshotBmp, DisplaySnapshotError},
    encrypted_input,
    framework_web_app::{
        ApiMethod, Encryptable, EncryptedRejection, NestedAppWithWebAppState, NestedAppWithWebAppStateBuilder, RouteAttempt, SetConfigResponseDTO,
        WebAppState, decrypt, extract_web_app_request, write_rejection,
    },
    prelude::*,
};
use framework_macros::include_bytes_gz;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::Fetch3mf;

use crate::app_config::{
    AiProviderAvailability, AiProviderId, ApiTokenMetadata, AppConfig, BackupConfig, BackupStatus, BambuPrinterConfig, DefaultPrinterConfig,
    DeviceCertificateGenerationRequest, DeviceCertificateLeafRequest, DeviceCertificateStatus, FILAMENT_BRAND_NAMES, FakePrinterConfig,
    PrinterConfig, PrinterDriverConfig, PrinterMode, PrintersConfig, SPOOLS_CATALOG, ScaleConfig, ScaleSourceConfig, UseAmsScan,
};
use crate::bambu::bambu_api::PrintCommand;
use crate::bambu::calibration::KInfo;
use crate::settings;
use crate::spool_record::{SpoolRecord, SpoolRecordExt};
use crate::spools_storage::StorageConfig;
use crate::store::{Store, StoreError};
use crate::utils::sha256_hex;
use crate::view_model::{PrinterInfo, StoreBackupGenerator, StoreBackupPlaintextSink, ViewModel};

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

fn encrypt_spools_csv_response(key: &[u8], store: &Store) -> String {
    let csv_len = match store.spools_csv_len() {
        Ok(csv_len) => csv_len,
        Err(err) => {
            error!("Failed to generate response to spoole query: {err}");
            return "".to_string();
        }
    };

    match encrypt_bytes_compact(key, csv_len, |plaintext| {
        let written = store.write_spools_csv(plaintext)?;
        if written != plaintext.len() {
            error!(
                "Spools CSV length changed from {} to {} while generating response",
                plaintext.len(),
                written
            );
            return Err(StoreError::InternalError);
        }
        Ok(())
    }) {
        Ok(encrypted) => encrypted,
        Err(err) => {
            error!("Failed to generate response to spoole query: {err}");
            "".to_string()
        }
    }
}

pub struct NestedAppBuilder {
    pub framework: Rc<RefCell<Framework>>,
    pub app_config: Rc<RefCell<AppConfig>>,
}

impl NestedAppWithWebAppStateBuilder<ConsoleAppState> for NestedAppBuilder {
    type WebApp = ConsoleWebService;

    fn build_web_app(self) -> Self::WebApp {
        let _framework = self.framework;
        let _app_config = self.app_config;
        ConsoleWebService
    }
}

pub struct ConsoleWebService;

async fn write_ok<R: Read, W: ResponseWriter<Error = R::Error>>(
    body_connection: RequestBodyConnection<'_, R>,
    response_writer: W,
    payload: String,
) -> Result<ResponseSent, W::Error> {
    response_writer
        .write_response(body_connection.finalize().await?, Response::new(StatusCode::OK, payload))
        .await
}

async fn write_empty_response<R: Read, W: ResponseWriter<Error = R::Error>>(
    body_connection: RequestBodyConnection<'_, R>,
    response_writer: W,
    status_code: StatusCode,
) -> Result<ResponseSent, W::Error> {
    response_writer
        .write_response(body_connection.finalize().await?, Response::new(status_code, ""))
        .await
}

async fn handle_web_app_post<T, R, W, Handler, HandlerFuture>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    app_state: ConsoleAppState,
    handler: Handler,
) -> Result<ResponseSent, W::Error>
where
    T: for<'r> FromRequest<'r, WebAppState<ConsoleAppState>, Rejection = EncryptedRejection>,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Handler: FnOnce(&'static RefCell<Vec<u8>>, ConsoleAppState, T) -> HandlerFuture,
    HandlerFuture: Future<Output = String>,
{
    let input = match extract_web_app_request(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let payload = handler(key, app_state, input).await;
    write_ok(request.body_connection, response_writer, payload).await
}

impl NestedAppWithWebAppState<ConsoleAppState> for ConsoleWebService {
    async fn try_handle_route<'p, 'r, R, W>(
        &self,
        state: &WebAppState<ConsoleAppState>,
        path: Path<'p>,
        request: Request<'r, R>,
        response_writer: W,
    ) -> RouteAttempt<'p, 'r, R, W>
    where
        R: Read,
        W: ResponseWriter<Error = R::Error>,
    {
        let method = ApiMethod::from(request.parts.method());
        let key = state.encryption.0;
        let app_state = state.more_state.clone();

        match (method, path.encoded()) {
            (ApiMethod::Get, "/") => box_route_future!(handle_root_redirect(request, response_writer, app_state).await),
            (ApiMethod::Get, "/styles.css") => {
                box_route_future!(STYLES_CSS_FILE.call_request_handler_service(state, (), request, response_writer).await)
            }
            (ApiMethod::Get, "/favicon-48x48.png") => {
                box_route_future!(FAVICON_FILE.call_request_handler_service(state, (), request, response_writer).await)
            }
            (ApiMethod::Get, "/spools-catalog") => box_route_future!({
                picoserve::response::File::with_content_type("text/plain; charset=utf-8", SPOOLS_CATALOG.as_bytes())
                    .call_request_handler_service(state, (), request, response_writer)
                    .await
            }),
            (ApiMethod::Get, "/filament-brands") => box_route_future!({
                picoserve::response::File::with_content_type("text/plain; charset=utf-8", FILAMENT_BRAND_NAMES.as_bytes())
                    .call_request_handler_service(state, (), request, response_writer)
                    .await
            }),
            (ApiMethod::Get, "/inventory") => box_route_future!(handle_inventory_redirect(request, response_writer).await),
            (ApiMethod::Get, app_path) if app_path == "/app" || app_path.starts_with("/app/") => box_route_future!({
                APP_INDEX_HTML_FILE
                    .call_request_handler_service(state, (), request, response_writer)
                    .await
            }),
            (ApiMethod::Get, "/L1/") => {
                box_route_future!(L1_INDEX_HTML_FILE.call_request_handler_service(state, (), request, response_writer).await)
            }
            (ApiMethod::Get, "/insecure/screenshot") => box_route_future!(handle_screenshot(state, request, response_writer).await),
            (ApiMethod::Post, "/api/printer-config") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_printer_config_post).await)
            }
            (ApiMethod::Get, "/api/printer-config") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_printer_config_get).await)
            }
            (ApiMethod::Post, "/api/scale-config") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_scale_config_post).await)
            }
            (ApiMethod::Get, "/api/scale-config") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_scale_config_get).await)
            }
            (ApiMethod::Get, "/api/console-info") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_console_info_get).await)
            }
            (ApiMethod::Get, "/api/printers-status") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_printers_status_get).await)
            }
            (ApiMethod::Post, "/api/printer-command") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_printer_command_post).await)
            }

            (ApiMethod::Post, "/api/ai-provider-config/get") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_ai_provider_config_get).await)
            }
            (ApiMethod::Post, "/api/ai-provider-config/set") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_ai_provider_config_set).await)
            }
            (ApiMethod::Post, "/api/ai-provider-config/delete") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_ai_provider_config_delete).await)
            }

            (ApiMethod::Get, "/api/api-tokens") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_api_tokens_get).await)
            }
            (ApiMethod::Post, "/api/api-tokens/create") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_api_tokens_create).await)
            }
            (ApiMethod::Post, "/api/api-tokens/delete") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_api_tokens_delete).await)
            }

            (ApiMethod::Post, "/api/device-certificate/create") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_device_certificate_create).await)
            }
            (ApiMethod::Post, "/api/device-certificate/update-leaf") => {
                box_route_future!(
                    handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_device_certificate_update_leaf).await
                )
            }
            (ApiMethod::Post, "/api/device-certificate/set-enabled") => {
                box_route_future!(
                    handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_device_certificate_set_enabled).await
                )
            }
            (ApiMethod::Post, "/api/device-certificate/delete") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_device_certificate_delete).await)
            }
            (ApiMethod::Get, "/api/device-certificate/ca-cert") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_device_certificate_ca_cert).await)
            }

            (ApiMethod::Post, "/api/spools-config") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_spools_config_post).await)
            }
            (ApiMethod::Get, "/api/spools-config") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_spools_config_get).await)
            }
            (ApiMethod::Post, "/api/filaments-config") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_filaments_config_post).await)
            }
            (ApiMethod::Get, "/api/filaments-config") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_filaments_config_get).await)
            }
            (ApiMethod::Get, "/api/spools-in-printers") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_spools_in_printers_get).await)
            }
            (ApiMethod::Get, "/api/spools") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_spools_get).await)
            }
            (ApiMethod::Post, "/api/spools/delete") => {
                box_route_future!(handle_web_app_post(state, request, response_writer, key, app_state, handle_spools_delete).await)
            }
            (ApiMethod::Post, "/api/spools/add-edit") => {
                box_route_future!(handle_web_app_post(state, request, response_writer, key, app_state, handle_spools_add_edit).await)
            }
            (ApiMethod::Post, "/api/printers-filament-pa") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_printers_filament_pa).await)
            }
            (ApiMethod::Post, "/api/add-printer-pa") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_add_printer_pa).await)
            }
            (ApiMethod::Post, "/api/spool-kinfo") => {
                box_route_future!(handle_spool_kinfo(state, request, response_writer, key, app_state).await)
            }

            (ApiMethod::Get, "/api/store-backup") => {
                box_route_future!({
                    let backup = app_state.view_model.borrow().store_backup_generator();
                    handle_store_backup_get(request, response_writer, key, backup).await
                })
            }
            (ApiMethod::Post, "/api/store-backup/config") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_store_backup_config).await)
            }
            (ApiMethod::Post, "/api/store-backup/mark-completed") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_store_backup_mark_completed).await)
            }
            (ApiMethod::Post, "/api/store-backup/reset-status") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_store_backup_reset_status).await)
            }
            (ApiMethod::Post, "/api/store-restore-upload") => box_route_future!({
                StoreRestoreUploadService
                    .call_request_handler_service(state, (), request, response_writer)
                    .await
            }),
            (ApiMethod::Post, "/api/store-restore-delete") => {
                box_route_future!(handle_store_restore_delete(state, request, response_writer, key).await)
            }
            (ApiMethod::Post, "/api/storage-config") => {
                box_route_future!(handle_storage_config_post(state, request, response_writer, key).await)
            }
            (ApiMethod::Get, "/api/storage-config") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_storage_config_get).await)
            }
            (ApiMethod::Post, "/api/dashboard-config") => {
                box_route_future!(handle_dashboard_config_post(state, request, response_writer, key).await)
            }
            (ApiMethod::Get, "/api/dashboard-config") => {
                box_route_future!(handle_dashboard_config_get(request, response_writer, key, app_state).await)
            }
            (ApiMethod::Post, "/api/tag-scanned") => {
                box_route_future!(handle_web_app_post_sync(state, request, response_writer, key, app_state, handle_tag_scanned).await)
            }
            (ApiMethod::Post, "/api/set-tag-location") => {
                box_route_future!(handle_set_tag_location(state, request, response_writer, key).await)
            }
            (ApiMethod::Get, "/api/spool-staging") => {
                box_route_future!(handle_web_app_get_sync(request, response_writer, key, app_state, handle_spool_staging_get).await)
            }
            (ApiMethod::Post, "/api/spool-location") => {
                box_route_future!(handle_spool_location(state, request, response_writer, key).await)
            }

            _ => RouteAttempt::NotMatched {
                path,
                request,
                response_writer,
            },
        }
    }
}

async fn write_response_value<R, W, H, B>(
    body_connection: RequestBodyConnection<'_, R>,
    response_writer: W,
    response: Response<H, B>,
) -> Result<ResponseSent, W::Error>
where
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    H: HeadersIter,
    B: Body,
{
    response_writer.write_response(body_connection.finalize().await?, response).await
}

async fn write_html_ok<R: Read, W: ResponseWriter<Error = R::Error>>(
    body_connection: RequestBodyConnection<'_, R>,
    response_writer: W,
    html: String,
) -> Result<ResponseSent, W::Error> {
    write_response_value(
        body_connection,
        response_writer,
        Response::new(StatusCode::OK, HtmlStringResponse::new(html)),
    )
    .await
}

async fn handle_web_app_get_sync<R, W, Handler>(
    request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    app_state: ConsoleAppState,
    handler: Handler,
) -> Result<ResponseSent, W::Error>
where
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Handler: FnOnce(&'static RefCell<Vec<u8>>, ConsoleAppState) -> String,
{
    let payload = handler(key, app_state);
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_web_app_post_sync<T, R, W, Handler>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    app_state: ConsoleAppState,
    handler: Handler,
) -> Result<ResponseSent, W::Error>
where
    T: for<'r> FromRequest<'r, WebAppState<ConsoleAppState>, Rejection = EncryptedRejection>,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Handler: FnOnce(&'static RefCell<Vec<u8>>, ConsoleAppState, T) -> String,
{
    let input = match extract_web_app_request(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let payload = handler(key, app_state, input);
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_root_redirect<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
    app_state: ConsoleAppState,
) -> Result<ResponseSent, W::Error> {
    let redirect_html = {
        let borrowed_app_config = app_state.app_config.borrow();
        let redirect_url = &borrowed_app_config.root_redirect;
        format!(r#"<!doctype html><script>location.href=location.hash?"{redirect_url}"+location.hash:"{redirect_url}"</script>"#)
    };
    write_html_ok(request.body_connection, response_writer, redirect_html).await
}

async fn handle_inventory_redirect<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    write_html_ok(
        request.body_connection,
        response_writer,
        r#"<!doctype html><script>location.replace("/app/inventory"+location.hash)</script>"#.to_string(),
    )
    .await
}

#[derive(serde::Deserialize)]
struct ScreenshotQueryParams {
    key: String,
    file: String,
}

async fn handle_screenshot<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let query = match Query::<ScreenshotQueryParams>::from_request_parts(state, &request.parts).await {
        Ok(Query(value)) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let response = if query.key == state.framework.0.borrow().web_config_key {
        let screenshot = state.framework.0.borrow().take_display_snapshot_bmp().map_err(ScreenshotError::from);
        let status_code = if screenshot.is_ok() { StatusCode::OK } else { StatusCode::BAD_REQUEST };
        ChunkedResponse::new(ScreenshotChunks { screenshot })
            .into_response()
            .with_header("Content-Disposition", format!("attachment; filename=\"{}\"", query.file))
            .with_status_code(status_code)
    } else {
        let screenshot = Err(ScreenshotError::SecurityKey);
        ChunkedResponse::new(ScreenshotChunks { screenshot })
            .into_response()
            .with_header("", String::new())
            .with_status_code(StatusCode::UNAUTHORIZED)
    };

    write_response_value(request.body_connection, response_writer, response).await
}

async fn handle_store_backup_get<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    backup: StoreBackupGenerator,
) -> Result<ResponseSent, W::Error> {
    write_response_value(
        request.body_connection,
        response_writer,
        ChunkedResponse::new(StoreBackupChunks { backup, key }).into_response(),
    )
    .await
}

fn handle_printer_config_post(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, printers_config_dto: PrintersConfigDTO) -> String {
    let default_printer_id = printers_config_dto.default_printer_id.clone().or_else(|| {
        printers_config_dto
            .default_printer_serial
            .as_deref()
            .map(|serial| BambuPrinterConfig::printer_id_for_serial(serial).0)
    });

    match state.app_config.borrow_mut().set_printers_config(
        printers_config_dto.into(),
        DefaultPrinterConfig {
            printer_id: default_printer_id,
        },
    ) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_printer_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let borrowed_app_config = state.app_config.borrow();
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
    printers_config.default_printer_id = default_printer.printer_id.clone();
    printers_config.encrypt(&key.borrow())
}

fn handle_scale_config_post(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, scale_config_dto: ScaleConfigDTO) -> String {
    match state.app_config.borrow_mut().set_scale_config(scale_config_dto.into()) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_scale_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let borrowed_app_config = state.app_config.borrow();
    let default_scale_config = ScaleConfig::default();
    let scale = borrowed_app_config.configured_scale.as_ref().unwrap_or(&default_scale_config);
    let scale_config = ScaleConfigDTO::from(scale);
    scale_config.encrypt(&key.borrow())
}

fn handle_console_info_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let borrowed_app_config = state.app_config.borrow();
    ConsoleInfoResponse {
        ai_providers: borrowed_app_config.ai_provider_key_availability(),
        device_name: borrowed_app_config.device_name(),
        device_ip: borrowed_app_config.device_ip(),
        device_certificate: borrowed_app_config.device_certificate_status(),
        backup_config: borrowed_app_config.backup_config.clone(),
        backup_status: borrowed_app_config.backup_status.clone(),
        store_initialized: state.store.is_initialized(),
        console_errors: state.store.console_errors(),
    }
    .encrypt(&key.borrow())
}

fn handle_printers_status_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    GetPrintersStatusResponse {
        printers: state.view_model.borrow().get_printers_status(),
    }
    .encrypt(&key.borrow())
}

fn handle_printer_command_post(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, printer_command: PrinterCommandDTO) -> String {
    let PrinterCommandDTO { printer_serial, command } = printer_command;
    let command_name = command.get_command().to_string();

    match state.view_model.borrow().request_printer_command(&printer_serial, command) {
        Ok(()) => GenericResponse {
            text: format!("Sent {command_name} command to printer {printer_serial}"),
            error: None,
        }
        .encrypt(&key.borrow()),
        Err(err) => GenericResponse {
            text: "Printer not found".to_string(),
            error: Some(err),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_ai_provider_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, ai_provider_ref: AiProviderRefDTO) -> String {
    let api_key = state.app_config.borrow().get_ai_provider_api_key(&ai_provider_ref.provider);
    GetAiProviderApiKeyResponse {
        provider: ai_provider_ref.provider,
        api_key,
    }
    .encrypt(&key.borrow())
}

fn handle_ai_provider_config_set(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, set_ai_provider_api_key: SetAiProviderApiKeyDTO) -> String {
    match state
        .app_config
        .borrow_mut()
        .set_ai_provider_api_key(set_ai_provider_api_key.provider, set_ai_provider_api_key.api_key)
    {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_ai_provider_config_delete(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, ai_provider_ref: AiProviderRefDTO) -> String {
    match state.app_config.borrow_mut().delete_ai_provider_api_key(ai_provider_ref.provider) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_api_tokens_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    ApiTokensResponse {
        tokens: state.app_config.borrow().list_api_tokens(),
    }
    .encrypt(&key.borrow())
}

fn handle_api_tokens_create(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, create_api_token: CreateApiTokenDTO) -> String {
    match state
        .app_config
        .borrow_mut()
        .create_api_token(create_api_token.name, create_api_token.created_at)
    {
        Ok(generated_token) => CreateApiTokenResponse {
            token: Some(generated_token.token),
            token_metadata: Some(generated_token.metadata),
            error_text: None,
        },
        Err(err) => CreateApiTokenResponse {
            token: None,
            token_metadata: None,
            error_text: Some(err),
        },
    }
    .encrypt(&key.borrow())
}

fn handle_api_tokens_delete(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, delete_api_token: DeleteApiTokenDTO) -> String {
    match state.app_config.borrow_mut().delete_api_token(delete_api_token.id) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_device_certificate_create(
    key: &'static RefCell<Vec<u8>>,
    state: ConsoleAppState,
    create_certificate: CreateDeviceCertificateDTO,
) -> String {
    match state.app_config.borrow_mut().create_device_certificate(create_certificate.into()) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_device_certificate_update_leaf(
    key: &'static RefCell<Vec<u8>>,
    state: ConsoleAppState,
    update_certificate: UpdateDeviceCertificateLeafDTO,
) -> String {
    match state.app_config.borrow_mut().update_device_certificate_leaf(update_certificate.into()) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_device_certificate_set_enabled(
    key: &'static RefCell<Vec<u8>>,
    state: ConsoleAppState,
    set_enabled: SetDeviceCertificateEnabledDTO,
) -> String {
    match state.app_config.borrow_mut().set_device_certificate_enabled(set_enabled.enabled) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_device_certificate_delete(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    match state.app_config.borrow_mut().delete_device_certificate() {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_device_certificate_ca_cert(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    DeviceCertificateCaCertResponse {
        ca_cert_pem: state.app_config.borrow().device_ca_cert_pem(settings::API_SERVER_TLS_CERTIFICATE),
    }
    .encrypt(&key.borrow())
}

fn handle_spools_config_post(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, SpoolsConfigDTO { spools }: SpoolsConfigDTO) -> String {
    let spools = if let Some(spools) = spools {
        if !spools.trim().is_empty() {
            Some(spools.trim().replace("\r\n", "\n").replace("\n", "\r\n"))
        } else {
            None
        }
    } else {
        None
    };

    match state.app_config.borrow_mut().set_user_cores(spools) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_spools_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let borrowed_app_config = state.app_config.borrow();
    let spools_config = SpoolsConfigDTO {
        spools: borrowed_app_config.user_cores.clone(),
    };
    spools_config.encrypt(&key.borrow())
}

fn handle_filaments_config_post(
    key: &'static RefCell<Vec<u8>>,
    state: ConsoleAppState,
    FilamentsConfigDTO { custom_filaments }: FilamentsConfigDTO,
) -> String {
    match state.app_config.borrow_mut().set_filaments(custom_filaments) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

fn handle_filaments_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let borrowed_app_config = state.app_config.borrow();
    let filaments_config = FilamentsConfigDTO {
        custom_filaments: borrowed_app_config.custom_filaments.clone(),
    };
    filaments_config.encrypt(&key.borrow())
}

fn handle_spools_in_printers_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    GetSpoolsInPrintersResponse {
        spools: state.view_model.borrow().get_spools_in_printers(),
    }
    .encrypt(&key.borrow())
}

fn handle_spools_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    encrypt_spools_csv_response(&key.borrow(), state.store.as_ref())
}

fn handle_printers_filament_pa(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, get_printers_filament_pa: GetPrintersFilamentPaDTO) -> String {
    let printers_filament_pa = state
        .view_model
        .borrow()
        .get_printers_filament_pa(&get_printers_filament_pa.slicer_filament_code)
        .into_iter()
        .map(|(identifier, name, extruders, pressure_advance)| {
            (
                identifier,
                PrinterEntry {
                    name,
                    extruders,
                    pressure_advance: pressure_advance
                        .into_iter()
                        .map(|pa| PressureAdvanceEntry {
                            extruder: pa.extruder,
                            diameter: pa.diameter,
                            nozzle_id: pa.nozzle_id,
                            name: pa.name,
                            k_value: pa.k_value,
                            cali_idx: pa.cali_idx,
                            setting_id: pa.setting_id,
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
}

fn handle_add_printer_pa(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, add_pa: AddPressureAdvanceDTO) -> String {
    match state.view_model.borrow_mut().add_calibration_to_printer(
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
}

fn handle_store_backup_config(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, backup_config: BackupConfig) -> String {
    match state.app_config.borrow_mut().set_backup_config(backup_config) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_store_backup_mark_completed(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, mark_backup: MarkBackupCompletedDTO) -> String {
    match state.app_config.borrow_mut().mark_backup_completed(mark_backup.date_time) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_store_backup_reset_status(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    match state.app_config.borrow_mut().reset_backup_status() {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO { error_text: Some(e) }.encrypt(&key.borrow()),
    }
}

fn handle_storage_config_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let storage_config_str = serde_json::to_string(&*state.store.storage_config.borrow()).unwrap();
    encrypt(&key.borrow(), &storage_config_str)
}

fn handle_tag_scanned(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, tag_scanned: TagScannedDTO) -> String {
    if let Some(location_rec) = state.store.get_location_by_hex_tag(&tag_scanned.tag_id_hex) {
        TagScannedResponse {
            tag_info: TagInfo::Location {
                location: location_rec.location,
            },
        }
        .encrypt(&key.borrow())
    } else {
        TagScannedResponse { tag_info: TagInfo::Unknown }.encrypt(&key.borrow())
    }
}

fn handle_spool_staging_get(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState) -> String {
    let view_model = state.view_model.borrow();
    let filament_staging = view_model.filament_staging.borrow();
    let csv_record = filament_staging
        .spool_rec()
        .and_then(|spool_rec| state.store.get_spool_csv_by_id(&spool_rec.id))
        .unwrap_or_default();

    SpoolStagingResponse { csv_record }.encrypt(&key.borrow())
}

async fn handle_spools_delete(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, delete_spool: DeleteSpoolDTO) -> String {
    let store = state.store;
    let delete_spool_id = delete_spool.id;
    match store.delete_spool(&delete_spool_id).await {
        Ok(_) => match store.spool_ids_hash() {
            Ok(ids_hash) => DeleteSpoolDTOResponse {
                deleted_ids: vec![delete_spool_id],
                ids_hash,
            }
            .encrypt(&key.borrow()),
            Err(err) => {
                error!("Failed to generate response to spoole delete: {err}");
                "".to_string()
            }
        },
        Err(err) => {
            error!("Failed to delete spool {} : {err}", delete_spool_id);
            err.to_string()
        }
    }
}

async fn handle_spools_add_edit(key: &'static RefCell<Vec<u8>>, state: ConsoleAppState, add_spool: AddSpoolDTO) -> String {
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

    let add_spool_operation = add_spool.id.is_empty();
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
        added_time: if add_spool_operation { add_spool.added_time } else { None },
        encode_time: None,
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
        td: add_spool.td.filter(|td| td.is_finite() && *td >= 0.0),
    };

    let spool_id = if add_spool_operation {
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

    let mut changed_ids = vec![spool_id.clone()];

    if let Some(mut split_spool) = split_spool {
        let split_spool_id = split_spool.id.clone();
        split_spool.spools_count -= add_spool.spools_count;
        if let Err(err) = store.update_spool(split_spool, None).await {
            error!("Critical: Added new splitted spool/stock but failed to update splitted stock : {err}");
            return err.to_string();
        }
        changed_ids.push(split_spool_id);
    }

    let changed_records = match store.spool_csv_rows_by_id(&changed_ids) {
        Ok(changed_records) => changed_records,
        Err(err) => {
            error!("Failed to generate changed spool records response: {err}");
            return "Failed to generate changed spool records response".to_string();
        }
    };

    match store.spool_ids_hash() {
        Ok(ids_hash) => AddSpoolDTOResponse {
            id: spool_id,
            changed_records,
            ids_hash,
        }
        .encrypt(&key.borrow()),
        Err(err) => {
            error!("Failed to generate response to spoole query: {err}");
            "Failed to generate response to spoole query".to_string()
        }
    }
}

async fn handle_spool_kinfo<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    app_state: ConsoleAppState,
) -> Result<ResponseSent, W::Error> {
    let get_spool_kinfo = match extract_web_app_request::<GetSpoolKInfoDTO, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let store = app_state.view_model.borrow_mut().store.clone();
    match store.get_spool_ext_by_id(&get_spool_kinfo.id).await {
        Ok(spool_rec_ext) => {
            write_ok(
                request.body_connection,
                response_writer,
                GetSpoolKInfoDTOResponse {
                    k_info: spool_rec_ext.k_info,
                }
                .encrypt(&key.borrow()),
            )
            .await
        }
        Err(_) => write_empty_response(request.body_connection, response_writer, StatusCode::new(404)).await,
    }
}

async fn handle_store_restore_delete<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
) -> Result<ResponseSent, W::Error> {
    let RestoreDeleteDTO {} = match extract_web_app_request::<RestoreDeleteDTO, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    cleanup_restore_upload_temp(&state.framework.0).await;
    write_ok(
        request.body_connection,
        response_writer,
        GenericResponse {
            text: "Deleted uploaded backup files".to_string(),
            error: None,
        }
        .encrypt(&key.borrow()),
    )
    .await
}

async fn handle_storage_config_post<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
) -> Result<ResponseSent, W::Error> {
    let storage_config = match extract_web_app_request::<StorageConfig, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let store = state.more_state.store.clone();
    let payload = match store.set_storage_config(storage_config).await {
        Ok(storage_config_str) => encrypt(&key.borrow(), &storage_config_str),
        Err(err) => {
            error!("Failed to store Storage Configuration : {err}");
            err.to_string()
        }
    };
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_dashboard_config_post<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
) -> Result<ResponseSent, W::Error> {
    let dashboard_config = match extract_web_app_request::<DashboardConfigDTO, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let store = state.more_state.store.clone();
    let payload = match &dashboard_config.dashboard_config_json {
        Some(dashboard_config_json) => match store.set_dashboard_config_json(dashboard_config_json).await {
            Ok(_) => DashboardConfigDTO {
                dashboard_config_json: Some(dashboard_config_json.clone()),
            }
            .encrypt(&key.borrow()),
            Err(err) => {
                error!("Failed to store dashboard configuration: {err}");
                DashboardConfigDTO { dashboard_config_json: None }.encrypt(&key.borrow())
            }
        },
        None => DashboardConfigDTO { dashboard_config_json: None }.encrypt(&key.borrow()),
    };
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_dashboard_config_get<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
    app_state: ConsoleAppState,
) -> Result<ResponseSent, W::Error> {
    let store = app_state.store;
    let payload = match store.get_dashboard_config_json().await {
        Ok(dashboard_config_json) => DashboardConfigDTO { dashboard_config_json }.encrypt(&key.borrow()),
        Err(err) => {
            error!("Failed to load dashboard configuration: {err}");
            DashboardConfigDTO { dashboard_config_json: None }.encrypt(&key.borrow())
        }
    };
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_set_tag_location<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
) -> Result<ResponseSent, W::Error> {
    let set_tag_location = match extract_web_app_request::<SetTagLocationDTO, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let store = state.more_state.store.clone();
    let payload = if set_tag_location.location.is_empty() {
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
    };
    write_ok(request.body_connection, response_writer, payload).await
}

async fn handle_spool_location<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &WebAppState<ConsoleAppState>,
    mut request: Request<'_, R>,
    response_writer: W,
    key: &'static RefCell<Vec<u8>>,
) -> Result<ResponseSent, W::Error> {
    let set_spool_location = match extract_web_app_request::<SetSpoolLocationDTO, _, _>(state, request.parts, request.body_connection.body()).await {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let store = state.more_state.store.clone();
    let payload = if let Some(mut spool_rec) = store.get_spool_by_id(&set_spool_location.spool_id) {
        match set_spool_location.location_type {
            LocationType::Assigned => spool_rec.assigned_location = set_spool_location.location,
            LocationType::Actual => spool_rec.actual_location = set_spool_location.location,
        }
        match store.update_spool(spool_rec, None).await {
            Ok(_) => GenericResponse {
                text: format!("Spool {} updated", set_spool_location.spool_id),
                error: None,
            }
            .encrypt(&key.borrow()),
            Err(err) => GenericResponse {
                text: format!("Tried to update Spool {}", set_spool_location.spool_id),
                error: Some(format!("Failed to update Spool {} : {err}", set_spool_location.spool_id)),
            }
            .encrypt(&key.borrow()),
        }
    } else {
        GenericResponse {
            text: format!("Tried to update Spool {}", set_spool_location.spool_id),
            error: Some(format!("No Spool {} in store", set_spool_location.spool_id)),
        }
        .encrypt(&key.borrow())
    };
    write_ok(request.body_connection, response_writer, payload).await
}

struct StoreRestoreUploadService;

async fn cleanup_restore_upload_temp(framework: &Rc<RefCell<Framework>>) {
    let file_store = framework.borrow().file_store();
    let mut file_store = file_store.lock().await;
    let _ = file_store.delete_file("/store.bak").await;
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
                    text: "Data store backup upload failed".to_string(),
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
                    text: "Data store backup upload failed".to_string(),
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
                    text: "Data store backup upload failed".to_string(),
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
            text: "Data store backup upload completed to /store.bak".to_string(),
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
                text: "Data store backup upload failed".to_string(),
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

#[derive(Debug)]
enum ScreenshotError {
    SecurityKey,
    Snapshot(DisplaySnapshotError),
}

impl From<DisplaySnapshotError> for ScreenshotError {
    fn from(value: DisplaySnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

impl ScreenshotError {
    fn message(&self) -> String {
        match self {
            Self::SecurityKey => "Security key error".to_string(),
            Self::Snapshot(err) => err.message(),
        }
    }
}

struct ScreenshotChunks {
    screenshot: Result<DisplaySnapshotBmp, ScreenshotError>,
}

impl picoserve::response::chunked::Chunks for ScreenshotChunks {
    fn content_type(&self) -> &'static str {
        if self.screenshot.is_ok() {
            DisplaySnapshotBmp::content_type()
        } else {
            "text/plain"
        }
    }

    async fn write_chunks<W: picoserve::io::Write>(self, mut chunk_writer: ChunkWriter<W>) -> Result<ChunksWritten, W::Error> {
        match self.screenshot {
            Ok(screenshot) => screenshot.write_to(&mut chunk_writer).await?,
            Err(err) => {
                let message = format!("Screenshot failed: {}", err.message());
                chunk_writer.write_chunk(message.as_bytes()).await?;
            }
        }
        chunk_writer.finalize().await
    }
}

struct StoreBackupChunks {
    backup: StoreBackupGenerator,
    key: &'static RefCell<Vec<u8>>,
}

struct EncryptedStoreBackupSink<'a, W: picoserve::io::Write> {
    chunk_writer: &'a mut ChunkWriter<W>,
    key: &'static RefCell<Vec<u8>>,
}

impl<W: picoserve::io::Write> StoreBackupPlaintextSink for EncryptedStoreBackupSink<'_, W> {
    type Error = W::Error;

    async fn write_backup_chunk(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
        let encrypted = {
            let key = self.key.borrow();
            encrypt_bytes(&key, chunk)
        };
        self.chunk_writer.write_chunk(encrypted.as_bytes()).await?;
        self.chunk_writer.write_chunk("|".as_bytes()).await
    }
}

impl picoserve::response::chunked::Chunks for StoreBackupChunks {
    fn content_type(&self) -> &'static str {
        "text/plain"
    }

    async fn write_chunks<W: picoserve::io::Write>(self, mut chunk_writer: ChunkWriter<W>) -> Result<ChunksWritten, W::Error> {
        let mut sink = EncryptedStoreBackupSink {
            chunk_writer: &mut chunk_writer,
            key: self.key,
        };
        self.backup.write_plaintext(&mut sink).await?;
        let res = chunk_writer.finalize().await;
        res
    }
}
#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfigDTO {
    name: Option<String>,
    #[serde(flatten)]
    driver: PrinterDriverConfigDTO,
}
encrypted_input!(PrinterConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "driver_kind", content = "driver_config")]
enum PrinterDriverConfigDTO {
    Bambu(BambuPrinterConfigDTO),
    Fake(FakePrinterConfigDTO),
}

#[derive(serde::Deserialize, serde::Serialize)]
struct BambuPrinterConfigDTO {
    ip: Option<String>,
    serial: Option<String>,
    access_code: Option<String>,
    log_filter: Option<log::LevelFilter>,
    #[serde(default)]
    auto_restore_k: bool,
    #[serde(default)]
    track_print_consume: bool,
    fetch_3mf: Option<String>,
    #[serde(default)]
    ignore_certificates: bool,
    #[serde(default)]
    printer_mode: PrinterMode,
    #[serde(default)]
    use_ams_scan: UseAmsScan,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct FakePrinterConfigDTO {
    unique_id: String,
    #[serde(default = "default_fake_slot_count_dto")]
    slot_count: u8,
}

fn default_fake_slot_count_dto() -> u8 {
    4
}

impl From<BambuPrinterConfigDTO> for BambuPrinterConfig {
    fn from(v: BambuPrinterConfigDTO) -> Self {
        Self {
            ip: v.ip.and_then(|s| s.parse::<Ipv4Addr>().ok()),
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
            use_ams_scan: v.use_ams_scan,
        }
    }
}

impl From<&BambuPrinterConfig> for BambuPrinterConfigDTO {
    fn from(v: &BambuPrinterConfig) -> Self {
        Self {
            ip: v.ip.map(|ip| ip.to_string()),
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
            use_ams_scan: v.use_ams_scan,
        }
    }
}

impl From<FakePrinterConfigDTO> for FakePrinterConfig {
    fn from(v: FakePrinterConfigDTO) -> Self {
        Self {
            unique_id: v.unique_id,
            slot_count: v.slot_count,
        }
    }
}

impl From<&FakePrinterConfig> for FakePrinterConfigDTO {
    fn from(v: &FakePrinterConfig) -> Self {
        Self {
            unique_id: v.unique_id.clone(),
            slot_count: v.slot_count,
        }
    }
}

impl From<PrinterConfigDTO> for PrinterConfig {
    fn from(v: PrinterConfigDTO) -> Self {
        match v.driver {
            PrinterDriverConfigDTO::Bambu(bambu_config) => Self::bambu(v.name, bambu_config.into()),
            PrinterDriverConfigDTO::Fake(fake_config) => Self::fake(v.name, fake_config.into()),
        }
    }
}
impl From<&PrinterConfig> for PrinterConfigDTO {
    fn from(v: &PrinterConfig) -> Self {
        let driver = match &v.driver {
            PrinterDriverConfig::Bambu(bambu_config) => PrinterDriverConfigDTO::Bambu(BambuPrinterConfigDTO::from(bambu_config)),
            PrinterDriverConfig::Fake(fake_config) => PrinterDriverConfigDTO::Fake(FakePrinterConfigDTO::from(fake_config)),
        };
        Self {
            name: v.name.clone(),
            driver,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrintersConfigDTO {
    printers: Vec<PrinterConfigDTO>,
    default_printer_id: Option<String>,
    #[serde(default)]
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
            default_printer_id: None,
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
struct DashboardConfigDTO {
    dashboard_config_json: Option<String>,
}
encrypted_input!(DashboardConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct ScaleConfigDTO {
    available: bool,
    #[serde(default)]
    local_scale_available: bool,
    #[serde(default)]
    preferred_scale_source: ScaleSourceConfig,
    name: Option<String>,
    ip: Option<String>,
    key: Option<String>,
}
encrypted_input!(ScaleConfigDTO);

impl From<ScaleConfigDTO> for ScaleConfig {
    fn from(v: ScaleConfigDTO) -> Self {
        Self {
            available: v.available,
            local_scale_available: v.local_scale_available,
            preferred_scale_source: v.preferred_scale_source,
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
            local_scale_available: v.local_scale_available,
            preferred_scale_source: v.preferred_scale_source.clone(),
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
pub struct ApiTokensResponse {
    tokens: Vec<ApiTokenMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateApiTokenDTO {
    name: String,
    created_at: i32,
}
encrypted_input!(CreateApiTokenDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateApiTokenResponse {
    token: Option<String>,
    token_metadata: Option<ApiTokenMetadata>,
    error_text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteApiTokenDTO {
    id: String,
}
encrypted_input!(DeleteApiTokenDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateDeviceCertificateDTO {
    sans: Vec<String>,
    created_at: i32,
    ca_not_before: String,
    ca_not_after: String,
    ca_expires_at: i32,
    leaf_not_before: String,
    leaf_not_after: String,
    leaf_expires_at: i32,
}
encrypted_input!(CreateDeviceCertificateDTO);

impl From<CreateDeviceCertificateDTO> for DeviceCertificateGenerationRequest {
    fn from(v: CreateDeviceCertificateDTO) -> Self {
        Self {
            sans: v.sans,
            created_at: v.created_at,
            ca_not_before: v.ca_not_before,
            ca_not_after: v.ca_not_after,
            ca_expires_at: v.ca_expires_at,
            leaf_not_before: v.leaf_not_before,
            leaf_not_after: v.leaf_not_after,
            leaf_expires_at: v.leaf_expires_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateDeviceCertificateLeafDTO {
    sans: Vec<String>,
    created_at: i32,
    leaf_not_before: String,
    leaf_not_after: String,
    leaf_expires_at: i32,
}
encrypted_input!(UpdateDeviceCertificateLeafDTO);

impl From<UpdateDeviceCertificateLeafDTO> for DeviceCertificateLeafRequest {
    fn from(v: UpdateDeviceCertificateLeafDTO) -> Self {
        Self {
            sans: v.sans,
            created_at: v.created_at,
            leaf_not_before: v.leaf_not_before,
            leaf_not_after: v.leaf_not_after,
            leaf_expires_at: v.leaf_expires_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetDeviceCertificateEnabledDTO {
    enabled: bool,
}
encrypted_input!(SetDeviceCertificateEnabledDTO);

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceCertificateCaCertResponse {
    ca_cert_pem: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConsoleInfoResponse {
    ai_providers: Vec<AiProviderAvailability>,
    device_name: Option<String>,
    device_ip: Option<String>,
    device_certificate: DeviceCertificateStatus,
    backup_config: BackupConfig,
    backup_status: BackupStatus,
    store_initialized: bool,
    console_errors: Vec<String>,
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
pub struct DeleteSpoolDTOResponse {
    pub deleted_ids: Vec<String>,
    pub ids_hash: String,
}

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
    pub added_time: Option<i32>,
    #[serde(default)]
    pub td: Option<f32>,
}
encrypted_input!(AddSpoolDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub struct AddSpoolDTOResponse {
    pub id: String,
    pub changed_records: Vec<String>,
    pub ids_hash: String,
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

encrypted_input!(BackupConfig);

#[derive(Debug, Serialize, Deserialize)]
pub struct MarkBackupCompletedDTO {
    date_time: i64,
}
encrypted_input!(MarkBackupCompletedDTO);

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

const STYLES_CSS_FILE: picoserve::response::File =
    picoserve::response::File::with_content_type_and_headers("text/css", include_bytes_gz!("static/styles.css"), &[("Content-Encoding", "gzip")]);

const FAVICON_FILE: picoserve::response::File =
    picoserve::response::File::with_content_type("image/png", include_bytes!("../static/favicon-48x48.png"));

const L1_INDEX_HTML_FILE: picoserve::response::File = picoserve::response::File::with_content_type_and_headers(
    "text/html",
    include_bytes_gz!("static/consoletag/index.html"),
    &[("Content-Encoding", "gzip")],
);

const APP_INDEX_HTML_FILE: picoserve::response::File = picoserve::response::File::with_content_type_and_headers(
    "text/html",
    include_bytes_gz!("static/app/index.html"),
    &[("Content-Encoding", "gzip")],
);

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
