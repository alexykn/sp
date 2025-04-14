// sapphire-core/src/model/formula.rs
// *** This is the corrected version that parses build_dependencies ***

use crate::dependency::{Dependency, Requirement, DependencyTag};
use crate::utils::error::Result;
use serde::{Deserialize, Serialize, Deserializer};
use serde::de;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use semver::Version; // Keep using semver::Version

// --- Bottle Related Structs ---
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

// --- Formula Version Struct ---
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FormulaVersions {
    pub stable: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub bottle: bool,
}

// --- Main Formula Struct ---
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    /// The raw stable version string from the API (e.g., "1.1", "1.2.3")
    pub stable_version_str: String,
    /// Parsed semver version (mainly for comparison, may be padded)
    #[serde(rename = "versions")]
    pub version_semver: Version,
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub bottle: BottleSpec,

    /// Combined list of all parsed dependencies with appropriate tags.
    #[serde(skip_deserializing)] // Skip direct deserialization for this field
    pub dependencies: Vec<Dependency>,

    /// Parsed requirements from the formula definition.
    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<Requirement>,

    #[serde(skip)]
    install_keg_path: Option<PathBuf>,
}


// Custom deserialization logic for Formula
impl<'de> Deserialize<'de> for Formula {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Temporary struct reflecting the JSON structure more closely
        #[derive(Deserialize, Debug)]
        struct RawFormulaData {
            name: String,
            #[serde(default)]
            revision: u32,
            desc: Option<String>,
            homepage: Option<String>,
            versions: FormulaVersions, // Keep FormulaVersions struct here
            #[serde(default)]
            url: String,
            #[serde(default)]
            sha256: String,
            #[serde(default)]
            mirrors: Vec<String>,
            #[serde(default)]
            bottle: BottleSpec,

            // Explicitly capture all dependency types from JSON
            #[serde(default)]
            dependencies: Vec<String>,
            #[serde(default)]
            build_dependencies: Vec<String>,
            #[serde(default)]
            test_dependencies: Vec<String>,
            #[serde(default)]
            recommended_dependencies: Vec<String>,
            #[serde(default)]
            optional_dependencies: Vec<String>,

            #[serde(default, deserialize_with = "deserialize_requirements")]
            requirements: Vec<Requirement>,
             #[serde(default)]
             urls: Option<Value>, // Keep for legacy URL handling
        }

        let raw: RawFormulaData = RawFormulaData::deserialize(deserializer)?;

        // --- Version Parsing ---
        // Store the original stable version string
        let stable_version_str = raw.versions.stable.clone().ok_or_else(|| de::Error::missing_field("versions.stable"))?;

        // Parse into semver::Version for comparisons, padding if necessary
        // (Use the logic from your previous version.rs or similar robust parsing)
        let version_semver = match crate::model::version::Version::parse(&stable_version_str) {
            Ok(v) => v.into(), // Convert our Version wrapper back to semver::Version
            Err(_) => {
                 // Fallback parsing/padding logic as before
                 let mut majors = 0u32;
                 let mut minors = 0u32;
                 let mut patches = 0u32;
                 let mut part_count = 0;
                 for (i, part) in stable_version_str.split('.').enumerate() {
                     let numeric_part = part.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
                     if numeric_part.is_empty() && i > 0 { break; }
                     if numeric_part.len() < part.len() && i > 0 {
                         if let Ok(num) = numeric_part.parse::<u32>() {
                             match i { 0 => majors = num, 1 => minors = num, 2 => patches = num, _ => {} }
                             part_count += 1;
                         } break;
                     }
                     if let Ok(num) = numeric_part.parse::<u32>() {
                         match i { 0 => majors = num, 1 => minors = num, 2 => patches = num, _ => {} }
                         part_count += 1;
                     }
                     if i >= 2 { break; }
                 }
                 let version_str_padded = match part_count {
                     1 => format!("{}.0.0", majors), 2 => format!("{}.{}.0", majors, minors), _ => format!("{}.{}.{}", majors, minors, patches),
                 };
                 match Version::parse(&version_str_padded) {
                     Ok(v) => v,
                     Err(_) => {
                          eprintln!( "Warning: Could not parse version '{}' (sanitized to '{}') for formula '{}'. Using 0.0.0.", stable_version_str, version_str_padded, raw.name );
                          Version::new(0, 0, 0)
                     }
                 }
            }
        };


        // --- URL/SHA256 Logic ---
        let mut final_url = raw.url;
        let mut final_sha256 = raw.sha256;
        if final_url.is_empty() {
             if let Some(Value::Object(urls_map)) = raw.urls {
                 if let Some(Value::Object(stable_url_info)) = urls_map.get("stable") {
                     if let Some(Value::String(u)) = stable_url_info.get("url") { final_url = u.clone(); }
                     if let Some(Value::String(s)) = stable_url_info.get("checksum").or_else(|| stable_url_info.get("sha256")) { final_sha256 = s.clone(); }
                 }
             }
        }
        if final_url.is_empty() && raw.versions.head.is_none() { println!("Warning: Formula '{}' has no stable URL defined.", raw.name); }


        // --- Dependency Processing ---
        let mut combined_dependencies: Vec<Dependency> = Vec::new();
        // Use a temporary map to merge tags for dependencies appearing in multiple lists
        let mut seen_deps: HashMap<String, DependencyTag> = HashMap::new();

        // Helper closure to process a list and update seen_deps
        let mut process_list = |deps: &[String], tag: DependencyTag| {
            for name in deps {
                *seen_deps.entry(name.clone()).or_insert(DependencyTag::empty()) |= tag;
            }
        };

        // Process each dependency type and add appropriate tags
        process_list(&raw.dependencies, DependencyTag::RUNTIME);
        process_list(&raw.build_dependencies, DependencyTag::BUILD);
        process_list(&raw.test_dependencies, DependencyTag::TEST);
        // Add RUNTIME tag along with RECOMMENDED/OPTIONAL as they usually imply runtime usage too
        process_list(&raw.recommended_dependencies, DependencyTag::RECOMMENDED | DependencyTag::RUNTIME);
        process_list(&raw.optional_dependencies, DependencyTag::OPTIONAL | DependencyTag::RUNTIME);

        // Convert the seen_deps map into the final Vec<Dependency>
        for (name, tags) in seen_deps {
            combined_dependencies.push(Dependency::new_with_tags(name, tags));
        }


        Ok(Formula {
            name: raw.name,
            stable_version_str, // Store the original string
            version_semver,     // Store the parsed semver
            revision: raw.revision,
            desc: raw.desc,
            homepage: raw.homepage,
            url: final_url,
            sha256: final_sha256,
            mirrors: raw.mirrors,
            bottle: raw.bottle,
            dependencies: combined_dependencies,
            requirements: raw.requirements,
            install_keg_path: None,
        })
    }
}


impl Formula {
    // --- Methods ---

    /// Returns a clone of the defined dependencies (now includes all types with tags).
    pub fn dependencies(&self) -> Result<Vec<Dependency>> {
        Ok(self.dependencies.clone())
    }

    /// Returns a clone of the defined requirements.
    pub fn requirements(&self) -> Result<Vec<Requirement>> {
        Ok(self.requirements.clone())
    }

    /// Sets the installation path for this specific instance.
    pub fn set_keg_path(&mut self, path: PathBuf) {
        self.install_keg_path = Some(path);
    }

    /// Gets the full version string including revision, using the *original* stable version.
    /// e.g., "1.1_5"
    pub fn version_str_full(&self) -> String {
        if self.revision > 0 {
            format!("{}_{}", self.stable_version_str, self.revision)
        } else {
            self.stable_version_str.clone()
        }
    }

    // --- Accessors ---
    pub fn name(&self) -> &str { &self.name }
    // Keep accessor for semver if needed for comparisons
    pub fn version(&self) -> &Version { &self.version_semver }
    pub fn source_url(&self) -> &str { &self.url }
    pub fn source_sha256(&self) -> &str { &self.sha256 }
    pub fn get_bottle_spec(&self, bottle_tag: &str) -> Option<&BottleFileSpec> {
        self.bottle.stable.as_ref()?.files.get(bottle_tag)
    }
}


// --- BuildEnvironment Dependency Interface ---
pub trait FormulaDependencies {
    fn name(&self) -> &str;
    fn install_prefix(&self, cellar_path: &Path) -> Result<PathBuf>;
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>>;
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>>;
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>>;
}
impl FormulaDependencies for Formula {
    fn name(&self) -> &str { &self.name }
    /// Use version_str_full() to get the standard keg name suffix.
    fn install_prefix(&self, cellar_path: &Path) -> Result<PathBuf> {
        let version_string = self.version_str_full(); // This now produces "1.1_5"
        Ok(cellar_path.join(self.name()).join(version_string))
    }
    // Placeholder implementations - These should be filled by the dependency resolver state
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>> { Ok(Vec::new()) }
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>> { Ok(Vec::new()) }
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>> { Ok(Vec::new()) }
}


// --- Deserialization Helpers ---
fn deserialize_requirements<'de, D>(deserializer: D) -> std::result::Result<Vec<Requirement>, D::Error>
where D: serde::Deserializer<'de>,
{
     #[derive(Deserialize, Debug)]
     struct ReqWrapper { #[serde(default)] name: String, #[serde(default)] version: Option<String>, #[serde(default)] cask: Option<String>, #[serde(default)] download: Option<String>, }
     let raw_reqs: Vec<Value> = Deserialize::deserialize(deserializer)?;
     let mut requirements = Vec::new();
     for req_val in raw_reqs {
         if let Ok(req_obj) = serde_json::from_value::<ReqWrapper>(req_val.clone()) { match req_obj.name.as_str() { "macos" => { requirements.push(Requirement::MacOS(req_obj.version.unwrap_or_else(|| "any".to_string()))); } "xcode" => { requirements.push(Requirement::Xcode(req_obj.version.unwrap_or_else(|| "any".to_string()))); } "cask" => { requirements.push(Requirement::Other(format!("Cask Requirement: {}", req_obj.cask.unwrap_or_else(|| "?".to_string())))); } "download" => { requirements.push(Requirement::Other(format!("Download Requirement: {}", req_obj.download.unwrap_or_else(|| "?".to_string())))); } _ => requirements.push(Requirement::Other(format!("Unknown requirement type: {:?}", req_obj))), } } else if let Value::String(req_str) = req_val { match req_str.as_str() { "macos" => requirements.push(Requirement::MacOS("latest".to_string())), "xcode" => requirements.push(Requirement::Xcode("latest".to_string())), _ => requirements.push(Requirement::Other(format!("Simple requirement: {}", req_str))), } } else { println!("Warning: Could not parse requirement: {:?}", req_val); requirements.push(Requirement::Other(format!("Unparsed requirement: {:?}", req_val))); }
     }
     Ok(requirements)
}