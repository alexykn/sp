// build/mod.rs - Main module for build functionality

pub mod formula;
pub mod cask;
pub mod devtools;
pub mod env;

// Re-export common functionality
pub use formula::{get_cellar_path, get_formula_cellar_path, extract_archive, write_receipt};

// Helper function to get Homebrew prefix (used by build modules)
pub fn get_homebrew_prefix() -> std::path::PathBuf {
    if std::path::Path::new("/opt/homebrew").exists() {
        std::path::PathBuf::from("/opt/homebrew")
    } else if std::path::Path::new("/usr/local").exists() {
        std::path::PathBuf::from("/usr/local")
    } else {
        std::path::PathBuf::from("/opt/homebrew") // default to ARM64 location
    }
}

// Path helpers
pub fn get_formula_opt_path(formula: &crate::model::formula::Formula) -> std::path::PathBuf {
    let prefix = get_homebrew_prefix();
    prefix.join("opt").join(&formula.name)
}
