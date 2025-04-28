// src/model/mod.rs
// Declares the modules within the model directory.
use std::sync::Arc;

pub mod cask;
pub mod formula;
pub mod version;

// Re-export
pub use cask::Cask;
pub use formula::Formula;

#[derive(Debug, Clone)]
pub enum InstallTargetIdentifier {
    Formula(Arc<Formula>),
    Cask(Arc<Cask>),
}
