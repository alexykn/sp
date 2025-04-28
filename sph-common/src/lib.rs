// sph-common/src/lib.rs
pub mod cache;
pub mod config;
pub mod dependency;
pub mod error;
pub mod formulary;
pub mod keg;
pub mod model;
// Optional: pub mod dependency_def;

// Re-export key types
pub use cache::Cache;
pub use config::Config;
pub use error::{Result, SphError};
pub use model::{Cask, Formula}; // etc.
// Optional: pub use dependency_def::{Dependency, DependencyTag};
