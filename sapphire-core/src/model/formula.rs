// brew-rs-client/src/model/formula.rs
// This module defines structures and logic related to Formulas.
// Formulas are typically recipes for building software from source.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// TODO: Define a struct `Formula` to represent the data parsed from the API.
// This might include fields like: name, full_name, desc, version, homepage, urls, dependencies, etc.

// Defines the Formula struct based on Homebrew API JSON structure.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub tap: Option<String>,

    #[serde(default)]
    pub oldnames: Vec<String>,

    #[serde(default)]
    pub aliases: Vec<String>,

    #[serde(default)]
    pub versioned_formulae: Vec<String>,

    #[serde(alias = "desc")] // Allow "desc" or "description"
    pub description: Option<String>,

    pub homepage: Option<String>,
    pub license: Option<String>, // License can be null

    // Capture the versions object directly
    #[serde(default)]
    pub versions: FormulaVersions,

    // Capture the urls object directly
    #[serde(default)]
    pub urls: UrlMap,

    #[serde(default)]
    pub revision: u32,

    #[serde(default)]
    pub version_scheme: u32,

    // Bottle information
    #[serde(default)]
    pub bottle: BottleMap,

    #[serde(default)]
    pub pour_bottle_only_if: Option<serde_json::Value>,

    #[serde(default)]
    pub keg_only: bool,

    pub keg_only_reason: Option<serde_json::Value>,

    // Dependencies
    #[serde(default)]
    pub dependencies: Vec<String>,

    #[serde(default)]
    pub build_dependencies: Vec<String>,

    #[serde(default)]
    pub test_dependencies: Vec<String>,

    #[serde(default)]
    pub recommended_dependencies: Vec<String>,

    #[serde(default)]
    pub optional_dependencies: Vec<String>,

    #[serde(default)]
    pub uses_from_macos: Vec<serde_json::Value>,

    #[serde(default)]
    pub uses_from_macos_bounds: Vec<serde_json::Value>,

    #[serde(default)]
    pub conflicts_with: Vec<String>,

    #[serde(default)]
    pub conflicts_with_reasons: Vec<String>,

    #[serde(default)]
    pub link_overwrite: Vec<String>,

    // Other fields
    pub caveats: Option<String>,

    #[serde(default)]
    pub installed: Vec<serde_json::Value>, // Installation information

    pub linked_keg: Option<String>,

    #[serde(default)]
    pub pinned: bool,

    #[serde(default)]
    pub outdated: bool,

    // Deprecation and disablement info
    #[serde(default)]
    pub deprecated: bool,

    pub deprecation_date: Option<String>,
    pub deprecation_reason: Option<String>,
    pub deprecation_replacement: Option<String>,

    #[serde(default)]
    pub disabled: bool,

    pub disable_date: Option<String>,
    pub disable_reason: Option<String>,
    pub disable_replacement: Option<String>,

    // Metadata
    #[serde(default)]
    pub post_install_defined: bool,

    pub service: Option<serde_json::Value>,
    pub tap_git_head: Option<String>,
    pub ruby_source_path: Option<String>,

    pub ruby_source_checksum: Option<HashMap<String, String>>,

    #[serde(default)]
    pub variations: HashMap<String, serde_json::Value>,
    /// Whether this formula requires C++11 (Homebrew DSL: `needs :cxx11`)
    #[serde(default)]
    pub requires_cpp11: Option<bool>,

    /// The C++ standard library to use, e.g. "libc++" or "libstdc++"
    #[serde(default)]
    pub stdlib: Option<String>,
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
