use alloc::string::String;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use crate::csvdb::CsvDbId;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct StorageConfig {
    pub rack_config: HashMap<String, RackConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RackConfig {
    pub name: String,
    pub num_bays: i32,
    pub num_shelves: i32,
    pub num_positions: i32,
    pub num_containers: i32,
    pub bay_numbering_order: BayNumOrder,
    pub shelf_numbering_order: ShelfNumOrder,
    pub position_numbering_order: PosNumOrder,
    pub container_numbering_order: ContainerNumOrder,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BayNumOrder {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ShelfNumOrder {
    TopDown,
    BottomUp,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PosNumOrder {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ContainerNumOrder {
    Unordered,
    TopDown,
    BottomUp,
    FrontToBack,
    BackToFront,
    LeftToRight,
    RightToLeft,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Default)]
pub struct TagLocationRecord {
    pub tag_id_hex: String,
    pub location: String,
}

impl CsvDbId for TagLocationRecord {
    fn id(&self) -> &String {
        &self.tag_id_hex
    }
}
