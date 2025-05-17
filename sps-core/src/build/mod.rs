// ===== sps-core/src/build/mod.rs =====
// Main module for build functionality
// Removed deprecated functions and re-exports.

use std::path::PathBuf;

use sps_common::config::Config;
use sps_common::model::formula::Formula;

// --- Submodules ---
pub mod cask;
pub mod devtools;
pub mod env;
pub mod extract;
pub mod formula; // <-- Declare the extract module

// Re-export relevant functions from formula submodule
pub use formula::{get_formula_cellar_path, write_receipt};

// --- Path helpers using Config ---
pub fn get_formula_opt_path(formula: &Formula, config: &Config) -> PathBuf {
    // Use new Config method
    config.formula_opt_path(formula.name())
}

// --- DEPRECATED EXTRACTION FUNCTIONS REMOVED ---
