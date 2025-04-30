// sps-aio/src/lib.rs
//! Asynchronous IO operations for sps (filesystem, json, git, checksums, process, uninstall)

// Declare modules
pub mod checksum;
pub mod extract; // Added extract module
pub mod fs;
pub mod git2;
pub mod json_io;
pub mod process; // Added process module
pub mod uninstall;

// Re-export the primary async functions
pub use checksum::verify_checksum_async;
pub use extract::extract_archive_async; // Export async extract
pub use fs::*; /* Exports all functions from fs (both sync and
                                          * async for now) */
pub use git2::update_repo_async; // Export async git update
pub use json_io::{read_json_async, write_json_async}; // Export async json ops
pub use process::run_command_async; // Export async command execution
pub use uninstall::*; // Exports all functions from uninstall (both sync and async for now)
