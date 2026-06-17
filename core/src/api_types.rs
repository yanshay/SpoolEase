use alloc::{string::String, vec::Vec};

use serde::Serialize;

use crate::printer::{PrinterDriverKind, SlotGroupKind};

#[derive(Debug, Serialize)]
pub struct ApiPrinterSlotsResponse {
    pub printers: Vec<ApiPrinterSlotsPrinter>,
}

#[derive(Debug, Serialize)]
pub struct ApiPrinterSlotsPrinter {
    pub id: String,
    pub native_id: String,
    pub kind: PrinterDriverKind,
    pub slot_groups: Vec<ApiPrinterSlotGroup>,
}

#[derive(Debug, Serialize)]
pub struct ApiPrinterSlotGroup {
    pub id: String,
    pub native_id: Option<String>,
    pub kind: SlotGroupKind,
    pub slots: Vec<ApiPrinterSlot>,
}

#[derive(Debug, Serialize)]
pub struct ApiPrinterSlot {
    pub id: String,
    pub native_id: Option<String>,
    pub spool_id: Option<String>,
    pub spool_brand: Option<String>,
    pub spool_material_type: Option<String>,
    pub spool_material_subtype: Option<String>,
    pub spool_color_name: Option<String>,
    pub weight_net: Option<f32>,
}
