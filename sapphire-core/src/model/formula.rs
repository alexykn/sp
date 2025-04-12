// brew-rs-client/src/model/formula.rs
// This module defines structures and logic related to Formulas.
// Formulas are typically recipes for building software from source.

use crate::dependency::{Dependency, Requirement, DependencyTag};
use crate::model::version::Version;
use crate::utils::error::{Result, SapphireError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BottleFileSpec {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct BottleSpec {
    pub stable: Option<BottleStableSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct BottleStableSpec {
    pub rebuild: u32,
    #[serde(default)]
    pub files: HashMap<String, BottleFileSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Formula {
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,

    /// Source download URL (stable version)
    pub url: String,
    /// SHA256 checksum for the source archive
    pub sha256: String,

    /// Optional mirrors for the source archive
    #[serde(default)]
    pub mirrors: Vec<String>,

    /// Bottle information (pre-compiled binaries)
    #[serde(default)]
    pub bottle: BottleSpec,

    /// Parsed dependencies from the formula definition.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,

    /// Parsed requirements from the formula definition.
    #[serde(default)]
    pub requirements: Vec<Requirement>,

    /// Installation path - determined *after* installation, not part of definition
    #[serde(skip)]
    install_keg_path: Option<PathBuf>,
}

impl Formula {
    pub fn new(name: impl Into<String>, version_str: &str, url: String, sha256: String) -> Result<Self> {
        Ok(Self {
            name: name.into(),
            version: Version::parse(version_str)?,
            revision: 0,
            desc: None,
            homepage: None,
            url,
            sha256,
            mirrors: Vec::new(),
            bottle: BottleSpec::default(),
            dependencies: Vec::new(),
            requirements: Vec::new(),
            install_keg_path: None,
        })
    }

    pub fn new_dummy(name: &str) -> Self {
        let (version, url, sha) = match name {
            "curl" => ("8.7.1", "https://curl.se/download/curl-8.7.1.tar.gz", "EXAMPLE_SHA_CURL"),
            "openssl" => ("3.3.0", "https://www.openssl.org/source/openssl-3.3.0.tar.gz", "EXAMPLE_SHA_OPENSSL"),
            "pkg-config" => ("0.29.2", "https://pkgconfig.freedesktop.org/releases/pkg-config-0.29.2.tar.gz", "EXAMPLE_SHA_PKGCONF"),
            "ca-certificates" => ("2024-03-11", "https://curl.se/ca/cacert-2024-03-11.pem", "EXAMPLE_SHA_CACERTS"),
            _ => ("1.0.0", "http://example.com/dummy-1.0.0.tar.gz", "EXAMPLE_SHA_DUMMY")
        };

        let mut f = Self::new(name, version, url.to_string(), sha.to_string()).expect("Dummy creation failed");

        if name == "curl" {
            f.dependencies.push(Dependency::new_runtime("openssl"));
            f.dependencies.push(Dependency::new_with_tags("pkg-config", DependencyTag::BUILD));
        } else if name == "openssl" {
            f.dependencies.push(Dependency::new_runtime("ca-certificates"));
        }
        f
    }

    pub fn dependencies(&self) -> Result<Vec<Dependency>> {
        Ok(self.dependencies.clone())
    }

    pub fn requirements(&self) -> Result<Vec<Requirement>> {
        Ok(self.requirements.clone())
    }

    pub fn set_keg_path(&mut self, path: PathBuf) {
        self.install_keg_path = Some(path);
    }

    pub fn version_str_full(&self) -> String {
        if self.revision > 0 {
            format!("{}_{}", self.version, self.revision)
        } else {
            self.version.to_string()
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Gets the primary source download URL.
    pub fn source_url(&self) -> &str {
        &self.url
    }

    /// Gets the expected SHA256 for the source download.
    pub fn source_sha256(&self) -> &str {
        &self.sha256
    }

    /// Gets the bottle information for a specific tag (e.g., "arm64_sonoma").
    pub fn get_bottle_spec(&self, bottle_tag: &str) -> Option<&BottleFileSpec> {
        self.bottle.stable.as_ref()?.files.get(bottle_tag)
    }
}

// TODO: Define a struct `Formula` to represent the data parsed from the API.
// This might include fields like: name, full_name, desc, version, homepage, urls, dependencies, etc.

// Defines the Formula struct based on Homebrew API JSON structure.

// --- BuildEnvironment Dependency Interface ---


/// Trait defining the interface expected by BuildEnvironment for formula objects.
pub trait FormulaDependencies {
    /// Returns the formula's name (for temp dir, logging, etc).
    fn name(&self) -> &str;

    /// Returns the install prefix for this formula.
    fn install_prefix(&self) -> Result<PathBuf>;

    /// Returns the resolved installation paths (keg roots) for runtime dependencies.
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>>;

    /// Returns the resolved installation paths (keg roots) for build-time dependencies.
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>>;

    /// Returns all resolved dependency paths (runtime + build).
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        let mut runtime = self.resolved_runtime_dependency_paths()?;
        let build = self.resolved_build_dependency_paths()?;
        runtime.extend(build);
        runtime.sort();
        runtime.dedup();
        Ok(runtime)
    }
}

impl FormulaDependencies for Formula {
    fn name(&self) -> &str {
        &self.name
    }

    fn install_prefix(&self) -> Result<PathBuf> {
        // Placeholder: In a real implementation, this would compute the install prefix
        // based on the formula's name, tap, and the Sapphire/Homebrew prefix.
        // For now, return an error to indicate this needs to be implemented.
        Err(SapphireError::BuildEnvError(
            "install_prefix() not yet implemented for Formula".to_string(),
        ))
    }

    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        // Placeholder: Would resolve runtime dependencies to their keg paths.
        Ok(Vec::new())
    }

    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        // Placeholder: Would resolve build dependencies to their keg paths.
        Ok(Vec::new())
    }
}

// Represents the versions object in the JSON
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct FormulaVersions {
    pub stable: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub bottle: bool,
}

// Represents the urls object containing different URL types (like "stable")
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct UrlMap {
    #[serde(flatten)]
    pub urls: HashMap<String, UrlInfo>,
}

// Represents the details for a specific URL entry (e.g., the "stable" url)
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct UrlInfo {
    pub url: Option<String>,
    pub tag: Option<String>,
    pub revision: Option<String>,
    pub using: Option<String>,
    pub checksum: Option<String>,
}

// Represents the map of OS/stability tags (like "stable") -> BottleInfo
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct BottleMap {
    #[serde(flatten)] // Flatten the map entries into this struct
    pub bottles: HashMap<String, BottleInfo>,
}

// Represents the details for a specific bottle definition (e.g., the "stable" bottle)
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct BottleInfo {
    // Fields directly within the "stable" object (or similar)
    #[serde(default)]
    pub rebuild: u32,

    pub root_url: Option<String>, // Root URL can be optional

    // The nested map of architecture -> file details
    #[serde(default)]
    pub files: HashMap<String, BottleFile>,
}

// Represents the details for a specific bottle file (for a specific architecture)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BottleFile {
    pub cellar: Option<String>,
    pub url: Option<String>,
    pub sha256: Option<String>,
}

// TODO: Implement functions for:
// - Parsing formula data from JSON
// - Searching for formulas
// - Getting formula info
// - Resolving dependencies

// Placeholder function removed as fetching/processing logic belongs elsewhere.
