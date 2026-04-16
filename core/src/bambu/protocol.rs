// TODO: Finish implementation of increasing sequence_id

use core::cell::RefCell;

use alloc::{
    borrow::Cow,
    rc::Rc,
    string::{String, ToString},
};
use framework::debug;
use once_cell::sync::Lazy;
use serde::Serialize;

use crate::bambu::{BambuPrinter, bambu_api::MqttCommand};

pub(super) struct ProtocolState {
    sequence_id: u32,
}

impl ProtocolState {
    pub fn new() -> Self {
        Self { sequence_id: 1 }
    }
}

// static CLEAN_CRLF_WHITESPACE_RE: Lazy<regex::bytes::Regex> = Lazy::new(|| regex::bytes::Regex::new(r"\s*[\r\n]\s*").unwrap());
static CLEAN_CRLF_WHITESPACE_RE: Lazy<regex::bytes::Regex> = Lazy::new(|| regex::bytes::Regex::new(r"[ \t\r\n]*[\r\n][ \t\r\n]*").unwrap());
pub fn clean_message_bytes_to_log(input: &[u8]) -> String {
    // Step 1: Replace in &[u8] without converting to &str first
    let replaced: Cow<[u8]> = CLEAN_CRLF_WHITESPACE_RE.replace_all(input, b" " as &[u8]);

    // Step 2: Convert result into String
    match replaced {
        Cow::Borrowed(b) => String::from_utf8_lossy(b).into_owned(), // No change → borrow
        Cow::Owned(b) => String::from_utf8(b).expect("invalid UTF-8"),
    }
}

#[allow(dead_code)]
impl BambuPrinter {
    pub(crate) async fn init_protocol(bambu_printer: &Rc<RefCell<BambuPrinter>>) {
        bambu_printer.borrow_mut().protocol_state.sequence_id = 10001;
    }

    pub(crate) fn printer_message<T: Serialize + MqttCommand>(&mut self, value: &mut T) -> String {
        let sequence_id = self.protocol_state.sequence_id.to_string();
        self.protocol_state.sequence_id += 1;
        value.set_sequence_id(sequence_id);
        // take care for increasing sequential id later on, need to probably add trait on the T to set sequence_id
        serde_json::to_string(value).unwrap()
    }

    pub(super) fn pre_message_send(&self, payload: &mut String) {
        //first log, only then fix
        if self.log_filter >= log::Level::Debug {
            debug!("[{}] MQTT Publish: {}", self.printer_number, payload);
        }
    }
}
