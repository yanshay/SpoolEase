use alloc::{string::String, vec::Vec};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct StorageConfig {
    rack_config: HashMap<String, RackConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RackConfig {
    pub num_shelves: Option<i32>,
    pub num_positions_per_shelf: Option<i32>,
    pub num_bins_per_position: Option<i32>,
    pub num_spools_per_bin: Option<i32>,
    pub shelf_numbering_order: ShelfNumOrder,
    pub position_numbering_order: PosNumOrder,
    pub bins_numbering_order: BinNumOrder,
    pub shelf_overrides: HashMap<i32, ShelfOverride>,
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
pub enum BinNumOrder {
    TopDown,
    BottomUp,
    FrontToBack,
    BackToFront,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShelfOverride {
    num_positions: Option<i32>,
    num_bins_per_position: Option<i32>,
    num_spools_per_bin: Option<i32>,
}
