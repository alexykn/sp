// sapphire-core/src/lib.rs
// This is the main library file for the sapphire-core crate.
// It declares and re-exports the public modules and types.

// Declare the top-level modules within the library crate
// These are directories with their own mod.rs files
pub mod build;
pub mod fetch;
pub mod model;
pub mod tap;
pub mod keg;
pub mod formulary;
pub mod utils;
pub mod dependency;

// Re-export key types for easier use by the CLI crate
pub use model::formula::Formula;
pub use model::cask::Cask;
pub use utils::error::{SapphireError, Result};
pub use utils::config::Config;

// No need to redefine the Error type since we're re-exporting the existing one
