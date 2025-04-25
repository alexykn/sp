// spm-core/src/utils/error.rs
// *** Added MachO related error variants *** [cite: 142]

use thiserror::Error;

// Define a top-level error enum for the application using thiserror
#[derive(Error, Debug)]
pub enum SpmError {
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

    #[error("API Request Error: {0}")]
    ApiRequestError(String),

    #[error("Semantic Versioning Error: {0}")]
    SemVer(#[from] semver::Error),

    // Updated DownloadError to match previous structure if needed, or keep simple
    #[error("DownloadError: Failed to download '{0}' from '{1}': {2}")]
    DownloadError(String, String, String), // name, url, reason

    #[error("Cache Error: {0}")]
    Cache(String),

    #[error("Resource Not Found: {0}")]
    NotFound(String),

    #[error("Installation Error: {0}")]
    InstallError(String),

    #[error("Generic Error: {0}")]
    Generic(String),

    // Keep HttpError if distinct from Http(reqwest::Error) is needed
    #[error("HttpError: {0}")]
    HttpError(String),

    #[error("Checksum Mismatch: {0}")]
    ChecksumMismatch(String), // Keep if used distinctly from ChecksumError

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

    // Kept IoError if distinct from Io(std::io::Error) is useful
    #[error("IoError: {0}")]
    IoError(String),

    #[error("Failed to execute command: {0}")]
    CommandExecError(String),

    // --- Added Mach-O Relocation Errors (Based on Plan) --- [cite: 142]
    #[error("Mach-O Error: {0}")]
    MachOError(String), // General Mach-O processing error

    #[error("Mach-O Modification Error: {0}")]
    MachOModificationError(String), // Specific error during modification step

    #[error("Mach-O Relocation Error: Path too long - {0}")]
    PathTooLongError(String), /* Specifically for path length issues during patching [cite:
                               * 115, 142] */

    #[error("Codesign Error: {0}")]
    CodesignError(String), // For errors during re-signing on Apple Silicon [cite: 138, 142]

    // --- Added object crate error integration --- [cite: 142]
    #[error("Object File Error: {0}")]
    Object(#[from] object::read::Error), // Error from object crate parsing
}

// Define a convenience Result type alias using our custom error
pub type Result<T> = std::result::Result<T, SpmError>;
