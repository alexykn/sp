// sps-core/src/uninstall/mod.rs

pub mod cask;
pub mod common;
pub mod formula;

// Re-export key functions and types
pub use cask::{uninstall_cask_artifacts, zap_cask_artifacts};
pub use common::UninstallOptions;
pub use formula::uninstall_formula_artifacts;
