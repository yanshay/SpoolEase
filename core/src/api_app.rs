use core::future::ready;

use alloc::{string::String, string::ToString};
use framework::framework_web_app::{BodyReadRejection, read_limited_body};
use picoserve::{
    ResponseSent,
    extract::{FromRequest, State},
    io::Read,
    request::{RequestBody, RequestParts},
    response::{IntoResponse, ResponseWriter, StatusCode},
    routing::{Layer, Next, get, post},
};
use serde::Deserialize;

use crate::api_server::ApiServerState;

pub fn build_app() -> picoserve::Router<impl picoserve::routing::PathRouter<ApiServerState>, ApiServerState> {
    picoserve::Router::new()
        .route(
            "/api/hello",
            get(|| ready(JsonStringResponse::new(r#"{"message":"hello from ApiServer"}"#.to_string()))),
        )
        .route(
            "/api/internal/printers/slots",
            get(|state: State<ApiServerState>| {
                ready({
                    let response = state.0.view_model.borrow().get_api_printer_slots();
                    let json = serde_json::to_string(&response).unwrap_or_else(|_| r#"{"error":"serialization_failed"}"#.to_string());
                    JsonStringResponse::new(json)
                })
            }),
        )
        .route(
            "/api/internal/filaments-config",
            post(
                |state: State<ApiServerState>, FilamentsConfigRequest { custom_filaments }: FilamentsConfigRequest| {
                    ready(match state.0.app_config.borrow_mut().set_filaments(custom_filaments) {
                        Ok(_) => (StatusCode::OK, JsonStringResponse::new(r#"{"success":true}"#.to_string())),
                        Err(_) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            JsonStringResponse::new(r#"{"error":"set_filaments_failed"}"#.to_string()),
                        ),
                    })
                },
            ),
        )
        .layer(ApiTokenAuthLayer)
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

struct ApiTokenAuthLayer;

impl<PathParameters> Layer<ApiServerState, PathParameters> for ApiTokenAuthLayer {
    type NextPathParameters = PathParameters;
    type NextState = ApiServerState;

    async fn call_layer<'a, R: Read + 'a, NextLayer: Next<'a, R, Self::NextState, Self::NextPathParameters>, W: ResponseWriter<Error = R::Error>>(
        &self,
        next: NextLayer,
        state: &ApiServerState,
        path_parameters: PathParameters,
        request_parts: RequestParts<'_>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let authorized_token_name = bearer_token_from_parts(&request_parts).and_then(|token| state.app_config.borrow().verify_api_token(token));

        if authorized_token_name.is_some() {
            return next.run(state, path_parameters, response_writer).await;
        }

        (
            StatusCode::UNAUTHORIZED,
            ("WWW-Authenticate", "Bearer realm=\"SpoolEase API\""),
            JsonStringResponse::new(r#"{"error":"unauthorized"}"#.to_string()),
        )
            .write_to(next.into_connection().await?, response_writer)
            .await
    }
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
