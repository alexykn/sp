use std::sync::Arc;

use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum SpsError {
    #[error("I/O Error: {0}")]
    Io(#[from] Arc<std::io::Error>),

    #[error("HTTP Request Error: {0}")]
    Http(#[from] Arc<reqwest::Error>),

    #[error("JSON Parsing Error: {0}")]
    Json(#[from] Arc<serde_json::Error>),

    #[error("Semantic Versioning Error: {0}")]
    SemVer(#[from] Arc<semver::Error>),

    #[error("Object File Error: {0}")]
    Object(#[from] Arc<object::read::Error>),

    #[error("Configuration Error: {0}")]
    Config(String),

    #[error("API Error: {0}")]
    Api(String),

    #[error("API Request Error: {0}")]
    ApiRequestError(String),

    #[error("DownloadError: Failed to download '{0}' from '{1}': {2}")]
    DownloadError(String, String, String),

    #[error("Cache Error: {0}")]
    Cache(String),

    #[error("Resource Not Found: {0}")]
    NotFound(String),

    #[error("Installation Error: {0}")]
    InstallError(String),

    #[error("Generic Error: {0}")]
    Generic(String),

    #[error("HttpError: {0}")]
    HttpError(String),

    #[error("Checksum Mismatch: {0}")]
    ChecksumMismatch(String),

    #[error("Validation Error: {0}")]
    ValidationError(String),

    #[error("Checksum Error: {0}")]
    ChecksumError(String),

    #[error("Parsing Error in {0}: {1}")]
    ParseError(&'static str, String),

    #[error("Version error: {0}")]
    VersionError(String),

    #[error("Dependency Error: {0}")]
    DependencyError(String),

    #[error("Build environment setup failed: {0}")]
    BuildEnvError(String),

    #[error("IoError: {0}")]
    IoError(String),

    #[error("Failed to execute command: {0}")]
    CommandExecError(String),

    #[error("Mach-O Error: {0}")]
    MachOError(String),

    #[error("Mach-O Modification Error: {0}")]
    MachOModificationError(String),

    #[error("Mach-O Relocation Error: Path too long - {0}")]
    PathTooLongError(String),

    #[error("Codesign Error: {0}")]
    CodesignError(String),
}

impl From<std::io::Error> for SpsError {
    fn from(err: std::io::Error) -> Self {
        SpsError::Io(Arc::new(err))
    }
}

impl From<reqwest::Error> for SpsError {
    fn from(err: reqwest::Error) -> Self {
        SpsError::Http(Arc::new(err))
    }
}

impl From<serde_json::Error> for SpsError {
    fn from(err: serde_json::Error) -> Self {
        SpsError::Json(Arc::new(err))
    }
}

impl From<semver::Error> for SpsError {
    fn from(err: semver::Error) -> Self {
        SpsError::SemVer(Arc::new(err))
    }
}

impl From<object::read::Error> for SpsError {
    fn from(err: object::read::Error) -> Self {
        SpsError::Object(Arc::new(err))
    }
}

pub type Result<T> = std::result::Result<T, SpsError>;
