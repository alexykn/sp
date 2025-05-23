// **File:** sps-core/src/dependency/requirement.rs (New file)
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Requirement {
    MacOS(String),
    Xcode(String),
    Other(String),
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MacOS(v) => write!(f, "macOS >= {v}"),
            Self::Xcode(v) => write!(f, "Xcode >= {v}"),
            Self::Other(s) => write!(f, "Requirement: {s}"),
        }
    }
}
