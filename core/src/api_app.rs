use alloc::{string::String, string::ToString};
use framework::{
    box_route_future, debug,
    framework_web_app::{ApiMethod, BodyReadRejection, read_limited_body, write_rejection},
};
use picoserve::{
    ResponseSent,
    extract::FromRequest,
    io::Read,
    request::{Path, Request, RequestBody, RequestParts},
    response::{IntoResponse, ResponseWriter, StatusCode},
    routing::PathRouterService,
};
use serde::Deserialize;

use crate::api_server::ApiServerState;

pub fn build_app() -> picoserve::Router<impl picoserve::routing::PathRouter<ApiServerState>, ApiServerState> {
    picoserve::Router::from_service(ApiServerWebService)
}

struct ApiServerWebService;

impl PathRouterService<ApiServerState> for ApiServerWebService {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        state: &ApiServerState,
        (): (),
        path: Path<'_>,
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let method_log = request.parts.method().to_string();
        let path_log = path.encoded().to_string();
        debug!("Api-Server request started: {method_log} {path_log}");

        if !request_authorized(state, &request.parts) {
            let result = box_route_future!(write_unauthorized(request, response_writer).await);
            debug!(
                "Api-Server request completed: {method_log} {path_log} {}",
                if result.is_ok() { "ok" } else { "error" }
            );
            return result;
        }

        let method = ApiMethod::from(request.parts.method());
        let result = match (method, path.encoded()) {
            (ApiMethod::Get, "/api/hello") => {
                box_route_future!(handle_hello(request, response_writer).await)
            }
            (ApiMethod::Get, "/api/internal/printers/slots") => {
                box_route_future!(handle_printer_slots_get(state, request, response_writer).await)
            }
            (ApiMethod::Post, "/api/internal/filaments-config") => {
                box_route_future!(handle_filaments_config_post(state, request, response_writer).await)
            }
            _ => box_route_future!(write_not_found(request, response_writer).await),
        };

        debug!(
            "Api-Server request completed: {method_log} {path_log} {}",
            if result.is_ok() { "ok" } else { "error" }
        );
        result
    }
}

#[derive(Deserialize)]
struct FilamentsConfigRequest {
    custom_filaments: Option<String>,
}

enum ApiRequestRejection {
    BodyRead(BodyReadRejection),
    DeserializationError,
}

impl From<BodyReadRejection> for ApiRequestRejection {
    fn from(value: BodyReadRejection) -> Self {
        Self::BodyRead(value)
    }
}

impl IntoResponse for ApiRequestRejection {
    async fn write_to<R: Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Self::BodyRead(error) => error.write_to(connection, response_writer).await,
            Self::DeserializationError => {
                (StatusCode::BAD_REQUEST, JsonStringResponse::new(r#"{"error":"bad_json"}"#.to_string()))
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

impl<'r> FromRequest<'r, ApiServerState> for FilamentsConfigRequest {
    type Rejection = ApiRequestRejection;

    async fn from_request<R: Read>(
        state: &'r ApiServerState,
        _request_parts: RequestParts<'r>,
        request_body: RequestBody<'r, R>,
    ) -> Result<Self, Self::Rejection> {
        let body = read_limited_body(request_body, state.request_body_max_bytes).await?;
        serde_json::from_slice(&body).map_err(|_| ApiRequestRejection::DeserializationError)
    }
}

fn request_authorized(state: &ApiServerState, request_parts: &RequestParts<'_>) -> bool {
    bearer_token_from_parts(request_parts)
        .and_then(|token| state.app_config.borrow().verify_api_token(token))
        .is_some()
}

fn bearer_token_from_parts<'r>(request_parts: &RequestParts<'r>) -> Option<&'r str> {
    request_parts
        .headers()
        .get("authorization")
        .and_then(|value| value.as_str().ok())
        .map(str::trim)
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

async fn extract_api_request<T, R: Read>(
    state: &ApiServerState,
    request_parts: RequestParts<'_>,
    request_body: RequestBody<'_, R>,
) -> Result<T, ApiRequestRejection>
where
    T: for<'r> FromRequest<'r, ApiServerState, Rejection = ApiRequestRejection>,
{
    T::from_request(state, request_parts, request_body).await
}

async fn handle_hello<R: Read, W: ResponseWriter<Error = R::Error>>(request: Request<'_, R>, response_writer: W) -> Result<ResponseSent, W::Error> {
    JsonStringResponse::new(r#"{"message":"hello from ApiServer"}"#.to_string())
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

async fn handle_printer_slots_get<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &ApiServerState,
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let response = state.view_model.borrow().get_api_printer_slots();
    let json = serde_json::to_string(&response).unwrap_or_else(|_| r#"{"error":"serialization_failed"}"#.to_string());
    JsonStringResponse::new(json)
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

async fn handle_filaments_config_post<R: Read, W: ResponseWriter<Error = R::Error>>(
    state: &ApiServerState,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let FilamentsConfigRequest { custom_filaments } =
        match extract_api_request::<FilamentsConfigRequest, _>(state, request.parts, request.body_connection.body()).await {
            Ok(value) => value,
            Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
        };

    let response = match state.app_config.borrow_mut().set_filaments(custom_filaments) {
        Ok(_) => (StatusCode::OK, JsonStringResponse::new(r#"{"success":true}"#.to_string())),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            JsonStringResponse::new(r#"{"error":"set_filaments_failed"}"#.to_string()),
        ),
    };
    response.write_to(request.body_connection.finalize().await?, response_writer).await
}

async fn write_unauthorized<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    (
        StatusCode::UNAUTHORIZED,
        ("WWW-Authenticate", "Bearer realm=\"SpoolEase API\""),
        JsonStringResponse::new(r#"{"error":"unauthorized"}"#.to_string()),
    )
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

async fn write_not_found<R: Read, W: ResponseWriter<Error = R::Error>>(
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    (StatusCode::NOT_FOUND, JsonStringResponse::new(r#"{"error":"not_found"}"#.to_string()))
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

struct JsonStringResponse {
    json: String,
}

impl JsonStringResponse {
    fn new(json: String) -> Self {
        Self { json }
    }
}

impl picoserve::response::Content for JsonStringResponse {
    fn content_type(&self) -> &'static str {
        "application/json; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        self.json.len()
    }

    async fn write_content<W: embedded_io_async::Write>(self, writer: W) -> Result<(), W::Error> {
        self.json.as_bytes().write_content(writer).await
    }
}
