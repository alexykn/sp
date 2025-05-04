// sps-common/src/model/artifact.rs
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Represents an item installed or managed by sps, recorded in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)] // Added Hash
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstalledArtifact {
    /// The main application bundle (e.g., in /Applications).
    AppBundle { path: PathBuf },
    /// A command-line binary symlinked into the prefix's bin dir.
    BinaryLink {
        link_path: PathBuf,
        target_path: PathBuf,
    },
    /// A man page symlinked into the prefix's man dir.
    ManpageLink {
        link_path: PathBuf,
        target_path: PathBuf,
    },
    /// A resource moved to a standard system/user location (e.g., Font, PrefPane).
    MovedResource { path: PathBuf },
    /// A macOS package receipt ID managed by pkgutil.
    PkgUtilReceipt { id: String },
    /// A launchd service (Agent/Daemon).
    Launchd {
        label: String,
        path: Option<PathBuf>,
    }, // Path is the plist file
    /// A symlink created within the Caskroom pointing to the actual installed artifact.
    /// Primarily for internal reference and potentially easier cleanup if needed.
    CaskroomLink {
        link_path: PathBuf,
        target_path: PathBuf,
    },
    /// A file copied *into* the Caskroom (e.g., a .pkg installer).
    CaskroomReference { path: PathBuf },
}

// Optional: Helper methods if needed
// impl InstalledArtifact { ... }
