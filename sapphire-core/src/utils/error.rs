// brew-rs-client/src/error.rs
// Defines the custom error types for the brew-rs-client library.

use thiserror::Error;

// Define a top-level error enum for the application using thiserror
#[derive(Error, Debug)]
pub enum SapphireError {
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP Request Error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON Parsing Error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Configuration Error: {0}")]
    Config(String),

    #[error("API Error: {0}")]
    Api(String),

    #[error("Semantic Versioning Error: {0}")]
    SemVer(#[from] semver::Error),

    #[error("DownloadError: {0}")]
    DownloadError(String, String, String),

    #[error("Cache Error: {0}")]
    Cache(String),

    #[error("Resource Not Found: {0}")]
    NotFound(String),

    #[error("Installation Error: {0}")]
    InstallError(String),

    #[error("Generic Error: {0}")]
    Generic(String),

    #[error("Parsing Error in {0}: {1}")]
    ParseError(&'static str, String),

    #[error("Version error: {0}")]
    VersionError(String),

    #[error("Dependency Error: {0}")]
    DependencyError(String),

    #[error("Build environment setup failed: {0}")]
    BuildEnvError(String),

    #[error("Failed to execute command: {0}")]
    CommandExecError(String),
}

// Define a convenience Result type alias using our custom error
pub type Result<T> = std::result::Result<T, SapphireError>;

// Manual implementations of Error, Display, and From are no longer needed
// as they are handled by thiserror using the #[derive(Error)] and #[from] attributes.
