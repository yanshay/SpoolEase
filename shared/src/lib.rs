// might need to put this under feature flag to compile with std
#![no_std]
#![feature(impl_trait_in_assoc_type)]
// might need to put this under feature flag to compile with std
extern crate alloc;

pub mod scale;

pub mod gcode_analysis;
pub mod gcode_analysis_task;
pub mod my_ftp;
pub mod nfc;
pub mod pn532_ext;
pub mod settings;
pub mod spool_tag;
pub mod threemf_extractor;
pub mod types;
pub mod utils;
