// sps-core/src/lib.rs

// Declare the top-level modules within the library crate
pub mod build;
pub mod installed; // New
pub mod tap;
pub mod uninstall; // New
pub mod update_check; // New
                      //pub mod utils;

// Re-export key types for easier use by the CLI crate
// Define InstallTargetIdentifier here or ensure it's public from cli/pipeline
// For simplicity, let's define it here for now:

pub use installed::{InstalledPackageInfo, PackageType}; // New
pub use uninstall::UninstallOptions; // New
pub use update_check::UpdateInfo; // New
