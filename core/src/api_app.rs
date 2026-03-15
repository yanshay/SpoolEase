use core::future::ready;

use alloc::{string::String, string::ToString};
use picoserve::routing::get;

use crate::api_server::ApiServerState;

pub fn build_app() -> picoserve::Router<impl picoserve::routing::PathRouter<ApiServerState>, ApiServerState> {
    picoserve::Router::new().route(
        "/api/hello",
        get(|| ready(JsonStringResponse::new(r#"{"message":"hello from ApiServer"}"#.to_string()))),
    )
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
