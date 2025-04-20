// src/cli/mod.rs
// Declares the modules within the cli directory.

pub mod args;

// Re-export the main structs/enums needed by main.rs
pub use args::{CliArgs as Cli, Commands};
