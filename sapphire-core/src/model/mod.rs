// src/model/mod.rs
// Declares the modules within the model directory.

pub mod cask;
pub mod formula;
pub mod version;

// Re-export
pub use cask::Cask;
pub use formula::Formula;
