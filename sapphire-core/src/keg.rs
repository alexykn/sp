use crate::utils::config::Config;
use semver::Version; // Changed from crate::model::version::Version
use crate::utils::error::{Result, SapphireError};
// Removed: use crate::build; // No longer needed here
use std::fs;
use std::path::{Path, PathBuf};

/// Represents information about an installed package (Keg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledKeg {
    pub name: String,
    pub version: Version, // Use semver::Version
    pub path: PathBuf,    // Path to the versioned installation directory (e.g., Cellar/foo/1.2.3)
    pub revision: u32,    // Store revision separately
}

/// Manages querying installed packages in the Cellar.
#[derive(Debug)]
pub struct KegRegistry {
    config: Config, // Holds paths like cellar and prefix
}

impl KegRegistry {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Gets the path to the directory containing all versions for a formula.
    fn formula_cellar_path(&self, name: &str) -> PathBuf {
        self.config.cellar.join(name)
    }

    /// Calculates the conventional 'opt' path for a formula (e.g., /opt/homebrew/opt/foo).
    /// This path typically points to the currently linked/active version.
    pub fn get_opt_path(&self, name: &str) -> PathBuf {
        self.config.prefix.join("opt").join(name)
    }


    /// Checks if a formula is installed and returns its Keg info if it is.
    /// If multiple versions are installed, returns the latest version (considering revisions).
    pub fn get_installed_keg(&self, name: &str) -> Result<Option<InstalledKeg>> {
        let formula_dir = self.formula_cellar_path(name);

        if !formula_dir.is_dir() {
            return Ok(None);
        }

        let mut latest_keg: Option<InstalledKeg> = None;

        for entry_result in fs::read_dir(&formula_dir).map_err(SapphireError::Io)? {
            let entry = entry_result.map_err(SapphireError::Io)?;
            let path = entry.path();

            if path.is_dir() {
                if let Some(version_str_full) = path.file_name().and_then(|n| n.to_str()) {
                     // Separate version and revision
                     let mut parts = version_str_full.splitn(2, '_');
                     let version_part = parts.next().unwrap_or(version_str_full);
                     let revision = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

                     // Attempt to parse the version part (pad if necessary)
                     let version_str_padded = if version_part.split('.').count() < 3 {
                         let v_parts: Vec<&str> = version_part.split('.').collect();
                         match v_parts.len() {
                             1 => format!("{}.0.0", v_parts[0]),
                             2 => format!("{}.{}.0", v_parts[0], v_parts[1]),
                             _ => version_part.to_string(),
                         }
                     } else {
                         version_part.to_string()
                     };

                     if let Ok(version) = Version::parse(&version_str_padded) {
                         let current_keg = InstalledKeg {
                             name: name.to_string(),
                             version: version.clone(),
                             revision,
                             path: path.clone(),
                         };

                         // Compare with the latest found so far
                         match latest_keg {
                             Some(ref latest) => {
                                 if version > latest.version || (version == latest.version && revision > latest.revision) {
                                     latest_keg = Some(current_keg);
                                 }
                             }
                             None => {
                                 latest_keg = Some(current_keg);
                             }
                         }
                     }
                     // else: Ignore directories that don't parse as versions
                }
            }
        }

        Ok(latest_keg)
    }

    /// Returns the root path of the Cellar.
    pub fn cellar_path(&self) -> &Path {
        &self.config.cellar
    }

    /// Returns the path for a *specific* versioned keg (whether installed or not).
    /// Includes revision in the path name if revision > 0.
    pub fn get_keg_path(&self, name: &str, version: &Version, revision: u32) -> PathBuf {
         let version_string = if revision > 0 {
             format!("{}_{}", version, revision)
         } else {
             version.to_string()
         };
         self.formula_cellar_path(name).join(version_string)
    }
}