use crate::utils::config::Config;
use crate::model::version::Version; // Use the new Version type
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::{Path, PathBuf};

/// Represents information about an installed package (Keg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledKeg {
    pub name: String,
    pub version: Version, // Use Version type
    pub path: PathBuf,    // Path to the versioned installation directory
                          // Add other info: linked status, installed options from receipt?
}

/// Manages querying installed packages in the Cellar.
#[derive(Debug)]
pub struct KegRegistry {
    config: Config, // Holds paths like cellar
}

impl KegRegistry {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Gets the path to the directory containing all versions for a formula.
    fn formula_cellar_path(&self, name: &str) -> PathBuf {
        self.config.cellar.join(name)
    }

    /// Checks if a formula is installed and returns its Keg info if it is.
    /// If multiple versions are installed, returns the latest version unless specified.
    /// TODO: Add optional version parameter to get a specific installed version.
    pub fn get_installed_keg(&self, name: &str) -> Result<Option<InstalledKeg>> {
        let formula_dir = self.formula_cellar_path(name);
        println!("Checking for installed kegs in: {}", formula_dir.display());

        if !formula_dir.is_dir() {
            println!("Formula directory does not exist. Not installed.");
            return Ok(None);
        }

        let mut latest_keg: Option<InstalledKeg> = None;

        for entry_result in fs::read_dir(&formula_dir)
            .map_err(|e| SapphireError::Generic(format!("Failed to read cellar dir '{}': {}", formula_dir.display(), e)))? {
            let entry = entry_result
                 .map_err(|e| SapphireError::Generic(format!("Failed to read entry in cellar dir '{}': {}", formula_dir.display(), e)))?;
            let path = entry.path();

            if path.is_dir() {
                if let Some(version_str) = path.file_name().and_then(|n| n.to_str()) {
                     match Version::parse(version_str) {
                         Ok(version) => {
                             println!("Found potential keg: {} version {}", name, version_str);
                             let current_keg = InstalledKeg {
                                 name: name.to_string(),
                                 version: version.clone(),
                                 path: path.clone(),
                             };

                             // Compare with the latest found so far
                             if let Some(ref latest) = latest_keg {
                                 if version > latest.version {
                                      println!("Updating latest keg to version {}", version_str);
                                     latest_keg = Some(current_keg);
                                 }
                             } else {
                                  println!("Setting initial latest keg to version {}", version_str);
                                 latest_keg = Some(current_keg);
                             }
                         },
                         Err(_) => {
                             println!("Ignoring non-version directory in {}: {}", formula_dir.display(), path.display());
                         }
                     }
                }
            }
        }

        if latest_keg.is_some() {
             println!("Latest installed keg for '{}': {:?}", name, latest_keg);
        } else {
             println!("No valid installed keg versions found for '{}'.", name);
        }

        Ok(latest_keg)
    }

    /// Returns the root path of the Cellar.
    pub fn cellar_path(&self) -> &Path {
        &self.config.cellar
    }

    /// Returns the path for a *specific* versioned keg (whether installed or not).
    pub fn get_keg_path(&self, name: &str, version: &Version) -> PathBuf {
         self.formula_cellar_path(name).join(version.to_string())
    }
}