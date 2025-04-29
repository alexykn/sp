// sp-aio/src/lib.rs
//! IO operations for sp (cache, manifests, checksums, config loading)

// Declare modules
pub mod checksum;
pub mod fs;
pub mod git2;
pub mod json_io;
pub mod uninstall;

// Re-export key types/functions if desired
pub use checksum::verify_checksum;
pub use fs::*;
pub use git2::*;
pub use json_io::{read_json_sync, write_json_sync};
pub use uninstall::*;
