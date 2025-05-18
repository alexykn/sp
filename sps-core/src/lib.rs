// sps-core/src/lib.rs

// Declare the top-level modules within the library crate
pub mod build;
pub mod check;
pub mod install;
pub mod pipeline;
pub mod uninstall;
pub mod upgrade; // New
#[cfg(target_os = "macos")]
pub mod utils; // New
               //pub mod utils;

// Re-export key types for easier use by the CLI crate
// Define InstallTargetIdentifier here or ensure it's public from cli/pipeline
// For simplicity, let's define it here for now:

// New
pub use uninstall::UninstallOptions; // New
                                     // New
