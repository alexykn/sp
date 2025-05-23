// **File:** sps-core/src/dependency/dependency.rs // Should be in the model module
use std::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct DependencyTag: u8 {
        const RUNTIME     = 0b00000001;
        const BUILD       = 0b00000010;
        const TEST        = 0b00000100;
        const OPTIONAL    = 0b00001000;
        const RECOMMENDED = 0b00010000;
    }
}

impl Default for DependencyTag {
    fn default() -> Self {
        Self::RUNTIME
    }
}

impl fmt::Display for DependencyTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    #[serde(default)]
    pub tags: DependencyTag,
}

impl Dependency {
    pub fn new_runtime(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tags: DependencyTag::RUNTIME,
        }
    }

    pub fn new_with_tags(name: impl Into<String>, tags: DependencyTag) -> Self {
        Self {
            name: name.into(),
            tags,
        }
    }
}

pub trait DependencyExt {
    fn filter_by_tags(&self, include: DependencyTag, exclude: DependencyTag) -> Vec<&Dependency>;
    fn runtime(&self) -> Vec<&Dependency>;
    fn build_time(&self) -> Vec<&Dependency>;
}

impl DependencyExt for Vec<Dependency> {
    fn filter_by_tags(&self, include: DependencyTag, exclude: DependencyTag) -> Vec<&Dependency> {
        self.iter()
            .filter(|dep| dep.tags.contains(include) && !dep.tags.intersects(exclude))
            .collect()
    }

    fn runtime(&self) -> Vec<&Dependency> {
        // A dependency is runtime if its tags indicate it's needed at runtime.
        // This includes standard runtime, recommended, or optional dependencies.
        // Build-only or Test-only dependencies (without other runtime flags) are excluded.
        self.iter()
            .filter(|dep| {
                dep.tags.intersects(
                    DependencyTag::RUNTIME | DependencyTag::RECOMMENDED | DependencyTag::OPTIONAL,
                )
            })
            .collect()
    }

    fn build_time(&self) -> Vec<&Dependency> {
        self.filter_by_tags(DependencyTag::BUILD, DependencyTag::empty())
    }
}
