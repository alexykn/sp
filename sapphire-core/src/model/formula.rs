// sapphire-core/src/model/formula.rs
// *** Corrected: Removed derive Deserialize from ResourceSpec, removed unused SapphireError import,
// added ResourceSpec struct and parsing ***

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tracing::{debug, error};

use crate::dependency::{Dependency, DependencyTag, Requirement};
use crate::utils::error::Result; // <-- Import only Result // Use log crate imports

// --- Resource Spec Struct ---
// *** Added struct definition, REMOVED #[derive(Deserialize)] ***
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceSpec {
    pub name: String,
    pub url: String,
    pub sha256: String,
    // Add other potential fields like version if needed later
}

// --- Bottle Related Structs (Original structure) ---
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

// --- Formula Version Struct (Original structure) ---
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FormulaVersions {
    pub stable: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub bottle: bool,
}

// --- Main Formula Struct ---
// *** Added 'resources' field ***
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    pub stable_version_str: String,
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
    #[serde(skip_deserializing)]
    pub dependencies: Vec<Dependency>,
    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<Requirement>,
    #[serde(skip_deserializing)] // Skip direct deserialization for this field
    pub resources: Vec<ResourceSpec>, // Stores parsed resources
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
        // *** Added 'resources' field to capture raw JSON Value ***
        #[derive(Deserialize, Debug)]
        struct RawFormulaData {
            name: String,
            #[serde(default)]
            revision: u32,
            desc: Option<String>,
            homepage: Option<String>,
            versions: FormulaVersions,
            #[serde(default)]
            url: String,
            #[serde(default)]
            sha256: String,
            #[serde(default)]
            mirrors: Vec<String>,
            #[serde(default)]
            bottle: BottleSpec,
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
            resources: Vec<Value>, // Capture resources as generic Value first
            #[serde(default)]
            urls: Option<Value>,
        }

        let raw: RawFormulaData = RawFormulaData::deserialize(deserializer)?;

        // --- Version Parsing (Original logic) ---
        let stable_version_str = raw
            .versions
            .stable
            .clone()
            .ok_or_else(|| de::Error::missing_field("versions.stable"))?;
        let version_semver = match crate::model::version::Version::parse(&stable_version_str) {
            Ok(v) => v.into(),
            Err(_) => {
                let mut majors = 0u32;
                let mut minors = 0u32;
                let mut patches = 0u32;
                let mut part_count = 0;
                for (i, part) in stable_version_str.split('.').enumerate() {
                    let numeric_part = part
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>();
                    if numeric_part.is_empty() && i > 0 {
                        break;
                    }
                    if numeric_part.len() < part.len() && i > 0 {
                        if let Ok(num) = numeric_part.parse::<u32>() {
                            match i {
                                0 => majors = num,
                                1 => minors = num,
                                2 => patches = num,
                                _ => {}
                            }
                            part_count += 1;
                        }
                        break;
                    }
                    if let Ok(num) = numeric_part.parse::<u32>() {
                        match i {
                            0 => majors = num,
                            1 => minors = num,
                            2 => patches = num,
                            _ => {}
                        }
                        part_count += 1;
                    }
                    if i >= 2 {
                        break;
                    }
                }
                let version_str_padded = match part_count {
                    1 => format!("{majors}.0.0"),
                    2 => format!("{majors}.{minors}.0"),
                    _ => format!("{majors}.{minors}.{patches}"),
                };
                match Version::parse(&version_str_padded) {
                    Ok(v) => v,
                    Err(_) => {
                        error!( "Warning: Could not parse version '{}' (sanitized to '{}') for formula '{}'. Using 0.0.0.", stable_version_str, version_str_padded, raw.name );
                        Version::new(0, 0, 0)
                    }
                }
            }
        };

        // --- URL/SHA256 Logic (Original logic) ---
        let mut final_url = raw.url;
        let mut final_sha256 = raw.sha256;
        if final_url.is_empty() {
            if let Some(Value::Object(urls_map)) = raw.urls {
                if let Some(Value::Object(stable_url_info)) = urls_map.get("stable") {
                    if let Some(Value::String(u)) = stable_url_info.get("url") {
                        final_url = u.clone();
                    }
                    if let Some(Value::String(s)) = stable_url_info
                        .get("checksum")
                        .or_else(|| stable_url_info.get("sha256"))
                    {
                        final_sha256 = s.clone();
                    }
                }
            }
        }
        if final_url.is_empty() && raw.versions.head.is_none() {
            debug!("Warning: Formula '{}' has no stable URL defined.", raw.name);
        }

        // --- Dependency Processing (Original logic) ---
        let mut combined_dependencies: Vec<Dependency> = Vec::new();
        let mut seen_deps: HashMap<String, DependencyTag> = HashMap::new();
        let mut process_list = |deps: &[String], tag: DependencyTag| {
            for name in deps {
                *seen_deps
                    .entry(name.clone())
                    .or_insert(DependencyTag::empty()) |= tag;
            }
        };
        process_list(&raw.dependencies, DependencyTag::RUNTIME);
        process_list(&raw.build_dependencies, DependencyTag::BUILD);
        process_list(&raw.test_dependencies, DependencyTag::TEST);
        process_list(
            &raw.recommended_dependencies,
            DependencyTag::RECOMMENDED | DependencyTag::RUNTIME,
        );
        process_list(
            &raw.optional_dependencies,
            DependencyTag::OPTIONAL | DependencyTag::RUNTIME,
        );
        for (name, tags) in seen_deps {
            combined_dependencies.push(Dependency::new_with_tags(name, tags));
        }

        // --- Resource Processing ---
        // *** Added parsing logic for the 'resources' field ***
        let mut combined_resources: Vec<ResourceSpec> = Vec::new();
        for res_val in raw.resources {
            // Homebrew API JSON format puts resource spec inside a keyed object
            // e.g., { "resource_name": { "url": "...", "sha256": "..." } }
            if let Value::Object(map) = res_val {
                // Assume only one key-value pair per object in the array
                if let Some((res_name, res_spec_val)) = map.into_iter().next() {
                    // Use the manual Deserialize impl for ResourceSpec
                    match ResourceSpec::deserialize(res_spec_val.clone()) {
                        // Use ::deserialize
                        Ok(mut res_spec) => {
                            // Inject the name from the key if missing
                            if res_spec.name.is_empty() {
                                res_spec.name = res_name;
                            } else if res_spec.name != res_name {
                                debug!("Resource name mismatch in formula '{}': key '{}' vs spec '{}'. Using key.", raw.name, res_name, res_spec.name);
                                res_spec.name = res_name; // Prefer key name
                            }
                            // Ensure required fields are present
                            if res_spec.url.is_empty() || res_spec.sha256.is_empty() {
                                debug!("Resource '{}' for formula '{}' is missing URL or SHA256. Skipping.", res_spec.name, raw.name);
                                continue;
                            }
                            debug!(
                                "Parsed resource '{}' for formula '{}'",
                                res_spec.name, raw.name
                            );
                            combined_resources.push(res_spec);
                        }
                        Err(e) => {
                            // Use display for the error which comes from serde::de::Error::custom
                            debug!("Failed to parse resource spec value for key '{}' in formula '{}': {}. Value: {:?}", res_name, raw.name, e, res_spec_val);
                        }
                    }
                } else {
                    debug!("Empty resource object found in formula '{}'.", raw.name);
                }
            } else {
                debug!("Unexpected format for resource entry in formula '{}': expected object, got {:?}", raw.name, res_val);
            }
        }

        Ok(Self {
            name: raw.name,
            stable_version_str,
            version_semver,
            revision: raw.revision,
            desc: raw.desc,
            homepage: raw.homepage,
            url: final_url,
            sha256: final_sha256,
            mirrors: raw.mirrors,
            bottle: raw.bottle,
            dependencies: combined_dependencies,
            requirements: raw.requirements,
            resources: combined_resources, // Assign parsed resources
            install_keg_path: None,
        })
    }
}

// --- Formula impl Methods ---
impl Formula {
    // dependencies() and requirements() are unchanged
    pub fn dependencies(&self) -> Result<Vec<Dependency>> {
        Ok(self.dependencies.clone())
    }
    pub fn requirements(&self) -> Result<Vec<Requirement>> {
        Ok(self.requirements.clone())
    }

    // *** Added: Returns a clone of the defined resources. ***
    pub fn resources(&self) -> Result<Vec<ResourceSpec>> {
        Ok(self.resources.clone())
    }

    // Other methods (set_keg_path, version_str_full, accessors) are unchanged
    pub fn set_keg_path(&mut self, path: PathBuf) {
        self.install_keg_path = Some(path);
    }
    pub fn version_str_full(&self) -> String {
        if self.revision > 0 {
            format!("{}_{}", self.stable_version_str, self.revision)
        } else {
            self.stable_version_str.clone()
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &Version {
        &self.version_semver
    }
    pub fn source_url(&self) -> &str {
        &self.url
    }
    pub fn source_sha256(&self) -> &str {
        &self.sha256
    }
    pub fn get_bottle_spec(&self, bottle_tag: &str) -> Option<&BottleFileSpec> {
        self.bottle.stable.as_ref()?.files.get(bottle_tag)
    }
}

// --- BuildEnvironment Dependency Interface (Unchanged) ---
pub trait FormulaDependencies {
    fn name(&self) -> &str;
    fn install_prefix(&self, cellar_path: &Path) -> Result<PathBuf>;
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>>;
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>>;
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>>;
}
impl FormulaDependencies for Formula {
    fn name(&self) -> &str {
        &self.name
    }
    fn install_prefix(&self, cellar_path: &Path) -> Result<PathBuf> {
        let version_string = self.version_str_full();
        Ok(cellar_path.join(self.name()).join(version_string))
    }
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

// --- Deserialization Helpers ---
// deserialize_requirements remains unchanged
fn deserialize_requirements<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<Requirement>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize, Debug)]
    struct ReqWrapper {
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        cask: Option<String>,
        #[serde(default)]
        download: Option<String>,
    }
    let raw_reqs: Vec<Value> = Deserialize::deserialize(deserializer)?;
    let mut requirements = Vec::new();
    for req_val in raw_reqs {
        if let Ok(req_obj) = serde_json::from_value::<ReqWrapper>(req_val.clone()) {
            match req_obj.name.as_str() {
                "macos" => {
                    requirements.push(Requirement::MacOS(
                        req_obj.version.unwrap_or_else(|| "any".to_string()),
                    ));
                }
                "xcode" => {
                    requirements.push(Requirement::Xcode(
                        req_obj.version.unwrap_or_else(|| "any".to_string()),
                    ));
                }
                "cask" => {
                    requirements.push(Requirement::Other(format!(
                        "Cask Requirement: {}",
                        req_obj.cask.unwrap_or_else(|| "?".to_string())
                    )));
                }
                "download" => {
                    requirements.push(Requirement::Other(format!(
                        "Download Requirement: {}",
                        req_obj.download.unwrap_or_else(|| "?".to_string())
                    )));
                }
                _ => requirements.push(Requirement::Other(format!(
                    "Unknown requirement type: {req_obj:?}"
                ))),
            }
        } else if let Value::String(req_str) = req_val {
            match req_str.as_str() {
                "macos" => requirements.push(Requirement::MacOS("latest".to_string())),
                "xcode" => requirements.push(Requirement::Xcode("latest".to_string())),
                _ => {
                    requirements.push(Requirement::Other(format!("Simple requirement: {req_str}")))
                }
            }
        } else {
            debug!("Warning: Could not parse requirement: {:?}", req_val);
            requirements.push(Requirement::Other(format!(
                "Unparsed requirement: {req_val:?}"
            )));
        }
    }
    Ok(requirements)
}

// Manual impl Deserialize for ResourceSpec (unchanged, this is needed)
impl<'de> Deserialize<'de> for ResourceSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default)]
            name: String, // name is often the key, not in the value
            url: String,
            sha256: String,
        }
        let helper = Helper::deserialize(deserializer)?;
        // Note: The actual resource name comes from the key in the map during Formula
        // deserialization
        Ok(Self {
            name: helper.name,
            url: helper.url,
            sha256: helper.sha256,
        })
    }
}
