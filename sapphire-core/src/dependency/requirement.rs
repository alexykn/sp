// **File:** sapphire-core/src/dependency/requirement.rs (New file)
use serde::{Deserialize, Serialize};
use std::fmt;

/// Represents a requirement beyond a simple formula dependency.
/// Placeholder - This needs significant expansion based on Homebrew's Requirement system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Requirement {
    /// Minimum macOS version required.
    MacOS(String), // e.g., "12.0"
    /// Minimum Xcode version required.
    Xcode(String), // e.g., "14.1"
    // Add others: Arch, specific libraries, environment variables, etc.
    /// Placeholder for unparsed or complex requirements.
    Other(String),
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Requirement::MacOS(v) => write!(f, "macOS >= {}", v),
            Requirement::Xcode(v) => write!(f, "Xcode >= {}", v),
            Requirement::Other(s) => write!(f, "Requirement: {}", s),
        }
    }
}