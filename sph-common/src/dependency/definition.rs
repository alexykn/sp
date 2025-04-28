// **File:** sph-core/src/dependency/dependency.rs // Should be in the model module
use std::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Serialize}; // For derive macros and attributes

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    /// Tags associated with a dependency, mirroring Homebrew's concepts.
    pub struct DependencyTag: u8 {
        /// Standard runtime dependency, needed for the formula to function.
        const RUNTIME     = 0b00000001;
        /// Needed only at build time.
        const BUILD       = 0b00000010;
        /// Needed for running tests (`brew test`).
        const TEST        = 0b00000100;
        /// Optional dependency, installable via user flag (e.g., `--with-foo`).
        const OPTIONAL    = 0b00001000;
        /// Recommended dependency, installed by default but can be skipped (e.g., `--without-bar`).
        const RECOMMENDED = 0b00010000;
        // Add other tags as needed (e.g., :implicit)
    }
}

impl Default for DependencyTag {
    // By default, a dependency is considered runtime unless specified otherwise.
    fn default() -> Self {
        Self::RUNTIME
    }
}

impl fmt::Display for DependencyTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}") // Simple debug format for now
    }
}

/// Represents a dependency declared by a Formula.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    /// The name of the formula dependency.
    pub name: String,
    /// Tags associated with this dependency (e.g., build, optional).
    #[serde(default)] // Use default tags (RUNTIME) if missing in serialization
    pub tags: DependencyTag,
    // We could add requirements here later:
    // pub requirements: Vec<Requirement>,
}

impl Dependency {
    /// Creates a new runtime dependency.
    pub fn new_runtime(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tags: DependencyTag::RUNTIME,
        }
    }

    /// Creates a new dependency with specific tags.
    pub fn new_with_tags(name: impl Into<String>, tags: DependencyTag) -> Self {
        Self {
            name: name.into(),
            tags,
        }
    }
}

/// Extension trait for Vec<Dependency> for easier filtering.
pub trait DependencyExt {
    /// Filters dependencies based on included tags and excluded tags.
    /// For example, to get runtime dependencies that are *not* optional:
    /// `filter_by_tags(DependencyTag::RUNTIME, DependencyTag::OPTIONAL)`
    fn filter_by_tags(&self, include: DependencyTag, exclude: DependencyTag) -> Vec<&Dependency>;

    /// Get only runtime dependencies (excluding build, test).
    fn runtime(&self) -> Vec<&Dependency>;

    /// Get only build-time dependencies (includes :build, excludes others unless also :build).
    fn build_time(&self) -> Vec<&Dependency>;
}

impl DependencyExt for Vec<Dependency> {
    fn filter_by_tags(&self, include: DependencyTag, exclude: DependencyTag) -> Vec<&Dependency> {
        self.iter()
            .filter(|dep| dep.tags.contains(include) && !dep.tags.intersects(exclude))
            .collect()
    }

    fn runtime(&self) -> Vec<&Dependency> {
        // Runtime deps are those *not* exclusively build or test
        // (A dep could be both runtime and build, e.g., a compiler needed at runtime too)
        self.iter()
            .filter(|dep| {
                !dep.tags
                    .contains(DependencyTag::BUILD | DependencyTag::TEST)
                    || dep.tags.contains(DependencyTag::RUNTIME)
            })
            // Alternatively, be more explicit: include RUNTIME | RECOMMENDED | OPTIONAL
            // .filter(|dep| dep.tags.intersects(DependencyTag::RUNTIME | DependencyTag::RECOMMENDED
            // | DependencyTag::OPTIONAL))
            .collect()
    }

    fn build_time(&self) -> Vec<&Dependency> {
        self.filter_by_tags(DependencyTag::BUILD, DependencyTag::empty())
    }
}

// Required for bitflags!
