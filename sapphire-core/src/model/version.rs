// **File:** sapphire-core/src/model/version.rs (New file)
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::utils::error::{Result, SapphireError};

/// Wrapper around semver::Version for formula versions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(semver::Version);

impl Version {
    pub fn parse(s: &str) -> Result<Self> {
        // Attempt standard semver parse first
        semver::Version::parse(s).map(Version).or_else(|_| {
            // Homebrew often uses versions like "1.2.3_1" (revision) or just "123"
            // Try to handle these by stripping suffixes or padding
            // This is a simplified handling, Homebrew's PkgVersion is complex
            let cleaned = s.split('_').next().unwrap_or(s); // Take part before _
            let parts: Vec<&str> = cleaned.split('.').collect();
            let padded = match parts.len() {
                1 => format!("{}.0.0", parts[0]),
                2 => format!("{}.{}.0", parts[0], parts[1]),
                _ => cleaned.to_string(), // Use original if 3+ parts
            };
            semver::Version::parse(&padded).map(Version).map_err(|e| {
                SapphireError::VersionError(format!(
                    "Failed to parse version '{}' (tried '{}'): {}",
                    s, padded, e
                ))
            })
        })
    }
}

impl FromStr for Version {
    type Err = SapphireError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Version::parse(s)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: Preserve original format if possible? PkgVersion complexity.
        // For now, display the parsed semver representation.
        write!(f, "{}", self.0)
    }
}

// Manual Serialize/Deserialize to handle the Version<->String conversion
impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl AsRef<Version> for Version {
    fn as_ref(&self) -> &Version {
        self
    }
}

// Removed redundant ToString implementation as it conflicts with the blanket implementation in std.

impl From<Version> for semver::Version {
    fn from(version: Version) -> Self {
        version.0
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Version::from_str(&s).map_err(serde::de::Error::custom)
    }
}

// Add to sapphire-core/src/utils/error.rs:
// #[error("Version error: {0}")]
// VersionError(String),

// Add to sapphire-core/Cargo.toml:
// [dependencies]
// semver = "1.0"
