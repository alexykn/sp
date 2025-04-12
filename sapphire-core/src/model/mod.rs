// src/model/mod.rs
// Declares the modules within the model directory.

pub mod cask;
pub mod version;
pub mod formula;

// Re-export
pub use cask::Cask;
pub use formula::Formula;
