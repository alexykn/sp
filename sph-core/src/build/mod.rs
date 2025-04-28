// ===== sph-core/src/build/mod.rs =====
// Main module for build functionality
// Removed deprecated functions and re-exports.

use std::path::PathBuf;

use sph_common::config::Config;
use sph_common::model::formula::Formula;

// --- Submodules ---
pub mod cask;
pub mod devtools;
pub mod env;
pub mod extract;
pub mod formula; // <-- Declare the extract module

// --- Re-exports ---
pub use extract::extract_archive; // <-- Re-export the main function from extract.rs
// Re-export relevant functions from formula submodule
pub use formula::{get_formula_cellar_path, write_receipt};

// --- Path helpers using Config ---
pub fn get_formula_opt_path(formula: &Formula, config: &Config) -> PathBuf {
    // Use Config method
    config.formula_opt_link_path(formula.name())
}

// --- DEPRECATED EXTRACTION FUNCTIONS REMOVED ---
