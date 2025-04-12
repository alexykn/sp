// brew-rs-client/src/model/formula.rs
// This module defines structures and logic related to Formulas.
// Formulas are typically recipes for building software from source.

use crate::dependency::{Dependency, Requirement, DependencyTag};
use crate::utils::error::Result;
use serde::{Deserialize, Serialize, Deserializer};
use serde::de;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use semver::Version; // Keep using semver::Version directly for now
use std::rc::Rc; // Import Rc

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

// Represents the versions object in the JSON
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct FormulaVersions {
    pub stable: Option<String>,
    pub head: Option<String>, // Keep head for potential future use
    #[serde(default)]
    pub bottle: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    #[serde(rename = "versions", deserialize_with = "deserialize_version")]
    pub version: Version, // Use semver::Version directly
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,

    /// Source download URL (stable version) - may be empty if only head exists
    #[serde(default)]
    pub url: String,
    /// SHA256 checksum for the source archive - may be empty
    #[serde(default)]
    pub sha256: String,

    /// Optional mirrors for the source archive
    #[serde(default)]
    pub mirrors: Vec<String>,

    /// Bottle information (pre-compiled binaries)
    #[serde(default)]
    pub bottle: BottleSpec,

    /// Parsed dependencies from the formula definition.
    #[serde(default, deserialize_with = "deserialize_dependencies")]
    pub dependencies: Vec<Dependency>,

    /// Parsed requirements from the formula definition.
    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<Requirement>,

    // --- Fields derived after loading ---
    /// Reference counted self - useful for sharing formula data
    #[serde(skip)]
    self_rc: Option<Rc<Formula>>, // To hold Rc<Self> after initial parse

    /// Installation path - determined *after* installation, not part of definition
    #[serde(skip)]
    install_keg_path: Option<PathBuf>,
}


// Custom deserialization logic
impl<'de> Deserialize<'de> for Formula {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawFormula {
            name: String,
            #[serde(default)]
            revision: u32,
            desc: Option<String>,
            homepage: Option<String>,
            versions: FormulaVersions, // Use the dedicated struct
            #[serde(default)]
            url: String, // Top level URL
            #[serde(default)]
            sha256: String, // Top level sha256
            #[serde(default)]
            mirrors: Vec<String>,
            #[serde(default)]
            bottle: BottleSpec,
            #[serde(default, deserialize_with = "deserialize_dependencies")]
            dependencies: Vec<Dependency>,
             #[serde(default, deserialize_with = "deserialize_requirements")]
            requirements: Vec<Requirement>,

            // Handle potential legacy/alternative structures if needed
             #[serde(default)]
             urls: Option<Value>,
        }

        let raw: RawFormula = RawFormula::deserialize(deserializer)?;

        // --- Version Logic ---
        // Prioritize versions.stable
        let version_str = raw.versions.stable.as_deref().ok_or_else(|| de::Error::missing_field("versions.stable"))?;
        // Ensure version string is valid semver (pad if necessary)
        let version_str_padded = if version_str.split('.').count() < 3 {
             // Handle cases like "1.2" -> "1.2.0"
             format!("{}.0", version_str)
        } else {
             version_str.to_string()
        };
        let version = Version::parse(&version_str_padded).map_err(|e| de::Error::custom(format!("Invalid stable version '{}': {}", version_str, e)))?;


        // --- URL/SHA256 Logic ---
        // Prefer top-level url/sha256 if present, otherwise look inside legacy `urls`
        let mut final_url = raw.url;
        let mut final_sha256 = raw.sha256;

        if final_url.is_empty() {
             if let Some(Value::Object(urls_map)) = raw.urls {
                 if let Some(Value::Object(stable_url_info)) = urls_map.get("stable") {
                     if let Some(Value::String(u)) = stable_url_info.get("url") {
                         final_url = u.clone();
                     }
                     if let Some(Value::String(s)) = stable_url_info.get("checksum").or_else(|| stable_url_info.get("sha256")) {
                          final_sha256 = s.clone();
                     }
                 }
             }
        }

        // Basic validation: Ensure URL is present if not a head-only formula (heuristic)
        if final_url.is_empty() && raw.versions.head.is_none() {
             // Allow empty URL for now, build process should handle this later
             // return Err(de::Error::custom("Missing stable URL"));
              println!("Warning: Formula '{}' has no stable URL defined.", raw.name);
        }
        // SHA256 might be missing for head or other reasons
        // if final_sha256.is_empty() && !final_url.is_empty() && raw.versions.head.is_none() {
        //     return Err(de::Error::custom("Missing SHA256 checksum for stable URL"));
        // }


        Ok(Formula {
            name: raw.name,
            version,
            revision: raw.revision,
            desc: raw.desc,
            homepage: raw.homepage,
            url: final_url,
            sha256: final_sha256,
            mirrors: raw.mirrors,
            bottle: raw.bottle,
            dependencies: raw.dependencies,
            requirements: raw.requirements,
            self_rc: None, // Initialize as None
            install_keg_path: None,
        })
    }
}


impl Formula {
    /// Creates a basic Formula instance. Primarily for testing or manual creation.
    #[allow(dead_code)]
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
            self_rc: None,
            install_keg_path: None,
        })
    }

    /// Creates a dummy formula instance for testing purposes.
    #[allow(dead_code)]
    pub fn new_dummy(name: &str) -> Self {
        let (version_str, url, sha) = match name {
            "curl" => ("8.7.1", "https://curl.se/download/curl-8.7.1.tar.gz", "EXAMPLE_SHA_CURL"),
            "openssl" => ("3.3.0", "https://www.openssl.org/source/openssl-3.3.0.tar.gz", "EXAMPLE_SHA_OPENSSL"),
            "pkg-config" => ("0.29.2", "https://pkgconfig.freedesktop.org/releases/pkg-config-0.29.2.tar.gz", "EXAMPLE_SHA_PKGCONF"),
            "ca-certificates" => ("2024-03-11", "https://curl.se/ca/cacert-2024-03-11.pem", "EXAMPLE_SHA_CACERTS"),
            "autoconf" => ("2.71", "https://ftp.gnu.org/gnu/autoconf/autoconf-2.71.tar.gz", "EXAMPLE_SHA_AUTOCONF"),
            "m4" => ("1.4.19", "https://ftp.gnu.org/gnu/m4/m4-1.4.19.tar.gz", "EXAMPLE_SHA_M4"),
            "htop" => ("3.3.0", "https://github.com/htop-dev/htop/releases/download/3.3.0/htop-3.3.0.tar.xz", "EXAMPLE_SHA_HTOP"),
            "ncurses" => ("6.4", "https://ftp.gnu.org/gnu/ncurses/ncurses-6.4.tar.gz", "EXAMPLE_SHA_NCURSES"),
            _ => ("1.0.0", "http://example.com/dummy-1.0.0.tar.gz", "EXAMPLE_SHA_DUMMY")
        };

        let version = Version::parse(version_str).unwrap_or_else(|_| Version::new(0, 0, 0)); // Basic fallback

        let mut f = Self {
             name: name.to_string(),
             version,
             revision: 0,
             desc: Some(format!("Dummy description for {}", name)),
             homepage: Some("http://example.com".to_string()),
             url: url.to_string(),
             sha256: sha.to_string(),
             mirrors: Vec::new(),
             bottle: BottleSpec::default(),
             dependencies: Vec::new(),
             requirements: Vec::new(),
             self_rc: None,
             install_keg_path: None,
        };

        // Add some dummy dependencies
        if name == "curl" {
            f.dependencies.push(Dependency::new_runtime("openssl"));
            f.dependencies.push(Dependency::new_with_tags("pkg-config", DependencyTag::BUILD));
        } else if name == "openssl" {
            f.dependencies.push(Dependency::new_runtime("ca-certificates"));
        } else if name == "htop" {
             f.dependencies.push(Dependency::new_runtime("ncurses"));
             f.dependencies.push(Dependency::new_with_tags("autoconf", DependencyTag::BUILD));
             f.dependencies.push(Dependency::new_with_tags("automake", DependencyTag::BUILD));
             f.dependencies.push(Dependency::new_with_tags("libtool", DependencyTag::BUILD));
             f.dependencies.push(Dependency::new_with_tags("pkg-config", DependencyTag::BUILD));
        } else if name == "autoconf" {
             f.dependencies.push(Dependency::new_runtime("m4"));
        }
        f
    }

    /// Returns a clone of the defined dependencies.
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

    /// Gets the full version string including revision (e.g., "1.2.3_1").
    pub fn version_str_full(&self) -> String {
        if self.revision > 0 {
            format!("{}_{}", self.version, self.revision)
        } else {
            self.version.to_string()
        }
    }

    // --- Accessors ---
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

     /// Returns a reference-counted pointer to this formula.
     /// Initializes it if not already done.
     pub fn get_rc(mut self) -> Rc<Self> {
         if self.self_rc.is_none() {
             self.self_rc = Some(Rc::new(self));
         }
         // This clone bumps the reference count, doesn't deep clone the Formula
         self.self_rc.as_ref().unwrap().clone()
     }
}


// --- BuildEnvironment Dependency Interface ---

/// Trait defining the interface expected by BuildEnvironment for formula objects.
/// This primarily provides identification and target path calculation.
pub trait FormulaDependencies {
    /// Returns the formula's name (for temp dir, logging, etc).
    fn name(&self) -> &str;

    /// Returns the *full* install prefix path for this formula version.
    /// Requires the cellar root path to be provided.
    fn install_prefix(&self, cellar_path: &std::path::Path) -> Result<PathBuf>;

    // DEPRECATED: Dependency path resolution is now handled externally by the resolver.
    // These methods should not be relied upon by BuildEnvironment.

    /// **DEPRECATED:** Use dependency resolver results instead.
    fn resolved_runtime_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        println!("Warning: Formula::resolved_runtime_dependency_paths() called - this is deprecated.");
        Ok(Vec::new()) // Return empty vec to satisfy trait temporarily
    }

    /// **DEPRECATED:** Use dependency resolver results instead.
    fn resolved_build_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        println!("Warning: Formula::resolved_build_dependency_paths() called - this is deprecated.");
        Ok(Vec::new()) // Return empty vec to satisfy trait temporarily
    }

    /// **DEPRECATED:** Use dependency resolver results instead.
    fn all_resolved_dependency_paths(&self) -> Result<Vec<PathBuf>> {
        println!("Warning: Formula::all_resolved_dependency_paths() called - this is deprecated.");
        Ok(Vec::new()) // Return empty vec to satisfy trait temporarily
    }
}

impl FormulaDependencies for Formula {
    fn name(&self) -> &str {
        &self.name
    }

    /// Calculates the installation prefix based on the provided cellar path.
    fn install_prefix(&self, cellar_path: &std::path::Path) -> Result<PathBuf> {
        let version_string = self.version_str_full();
        Ok(cellar_path.join(self.name()).join(version_string))
    }
}


// --- Deserialization Helpers ---

/// Custom deserializer for dependencies that handles both Vec<String> and Vec<Dependency>
fn deserialize_dependencies<'de, D>(deserializer: D) -> std::result::Result<Vec<Dependency>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor, MapAccess};
    use std::fmt;

    struct DependenciesVisitor;

    impl<'de> Visitor<'de> for DependenciesVisitor {
        type Value = Vec<Dependency>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a list of dependencies as strings or objects with name and tags")
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut deps = Vec::new();
            while let Some(value) = seq.next_element::<Value>()? {
                 match value {
                    Value::String(s) => {
                        // Assume string dependency is runtime by default
                        deps.push(Dependency::new_runtime(s));
                    }
                    Value::Object(map) => {
                         // Expect keys like "name" and "tags"
                         let name = map.get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| de::Error::missing_field("name in dependency object"))?
                            .to_string();

                         let tags_vec = map.get("tags")
                             .and_then(Value::as_array)
                             .ok_or_else(|| de::Error::missing_field("tags in dependency object"))?;

                         let mut dep_tags = DependencyTag::empty();
                         for tag_val in tags_vec {
                             if let Some(tag_str) = tag_val.as_str() {
                                 match tag_str {
                                     "build" => dep_tags |= DependencyTag::BUILD,
                                     "test" => dep_tags |= DependencyTag::TEST,
                                     "optional" => dep_tags |= DependencyTag::OPTIONAL,
                                     "recommended" => dep_tags |= DependencyTag::RECOMMENDED,
                                     // Assume other tags might imply runtime? Or ignore unknown tags?
                                     // For now, let's explicitly require 'runtime' or imply it if no other major tag present.
                                     "runtime" => dep_tags |= DependencyTag::RUNTIME,
                                     _ => { /* Ignore unknown tags */ }
                                 }
                             }
                         }
                          // If no specific tags imply non-runtime, assume runtime.
                         if !dep_tags.intersects(DependencyTag::BUILD | DependencyTag::TEST | DependencyTag::OPTIONAL | DependencyTag::RECOMMENDED) {
                              dep_tags |= DependencyTag::RUNTIME;
                         }


                         deps.push(Dependency::new_with_tags(name, dep_tags));

                    }
                    _ => return Err(de::Error::invalid_type(de::Unexpected::Other("non-string/object dependency"), &self)),
                 }
            }
            Ok(deps)
        }
    }

    deserializer.deserialize_seq(DependenciesVisitor)
}


/// Custom deserializer for requirements. Placeholder - adapt as needed.
fn deserialize_requirements<'de, D>(deserializer: D) -> std::result::Result<Vec<Requirement>, D::Error>
where
    D: serde::Deserializer<'de>,
{
     #[derive(Deserialize)]
     struct ReqWrapper {
         #[serde(default)]
         name: String,
         #[serde(default)]
         version: Option<String>,
         // Add other fields as needed based on JSON structure
     }

     let raw_reqs: Vec<Value> = Deserialize::deserialize(deserializer)?;
     let mut requirements = Vec::new();

     for req_val in raw_reqs {
         if let Ok(req_obj) = serde_json::from_value::<ReqWrapper>(req_val) {
             match req_obj.name.as_str() {
                 "macos" => {
                     if let Some(v) = req_obj.version {
                          requirements.push(Requirement::MacOS(v));
                     } else {
                         // Handle cases like ":macos => :catalina" if needed later
                          requirements.push(Requirement::Other(format!("macos requirement (unknown version): {:?}", req_obj)));
                     }
                 }
                 "xcode" => {
                      if let Some(v) = req_obj.version {
                          requirements.push(Requirement::Xcode(v));
                     } else {
                          requirements.push(Requirement::Other(format!("xcode requirement (unknown version): {:?}", req_obj)));
                     }
                 }
                 // Add cases for other requirement types (:arch, :x11, etc.)
                 _ => requirements.push(Requirement::Other(format!("Unknown requirement type: {:?}", req_obj))),
             }
         } else {
             // Handle simple string requirements if applicable, or log/error
              println!("Warning: Could not parse requirement: {:?}", req_val);
              requirements.push(Requirement::Other(format!("Unparsed requirement: {:?}", req_val)));
         }
     }

     Ok(requirements)
}


// Custom deserializer to extract the stable version from the "versions" map
// DEPRECATED - Version deserialization is now handled within Formula::deserialize
#[allow(dead_code)]
fn deserialize_version<'de, D>(deserializer: D) -> std::result::Result<Version, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Value = Deserialize::deserialize(deserializer)?;
    if let Value::Object(map) = v {
        if let Some(Value::String(stable)) = map.get("stable") {
            // Append ".0" if needed (e.g. "6.5" becomes "6.5.0")
            let fixed = if stable.split('.').count() < 3 {
                format!("{}.0", stable)
            } else {
                stable.to_string()
            };
            return Version::parse(&fixed).map_err(de::Error::custom);
        }
        return Err(de::Error::missing_field("stable"));
    }
    Err(de::Error::custom("expected versions as map"))
}


// --- Helper Structs for potential alternative JSON structures ---
// These might be needed if the API returns different layouts sometimes.

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[allow(dead_code)] // Kept for reference or potential future use
struct UrlMap {
    #[serde(flatten)]
    pub urls: HashMap<String, UrlInfo>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Kept for reference or potential future use
struct UrlInfo {
    pub url: Option<String>,
    pub tag: Option<String>,
    pub revision: Option<String>,
    pub using: Option<String>,
    pub checksum: Option<String>, // Can be sha256
     #[serde(alias = "checksum")] // Allow 'checksum' as alias for sha256
    pub sha256: Option<String>,
}