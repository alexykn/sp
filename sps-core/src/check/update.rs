// sps-core/src/check/update.rs

use std::collections::HashMap;
use std::sync::Arc;

// Imports from sps-common
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::formulary::Formulary; // Using the shared Formulary
use sps_common::model::version::Version as PkgVersion;
use sps_common::model::Cask; // Using the Cask and Formula from sps-common
// Use the Cask and Formula structs from sps_common::model
// Ensure InstallTargetIdentifier is correctly pathed if it's also in sps_common::model
use sps_common::model::InstallTargetIdentifier;
// Imports from sps-net
use sps_net::api;

// Imports from sps-core
use crate::check::installed::{InstalledPackageInfo, PackageType};

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub name: String,
    pub installed_version: String,
    pub available_version: String,
    pub pkg_type: PackageType,
    pub target_definition: InstallTargetIdentifier,
}

/// Ensures that the raw JSON data for formulas and casks exists in the main disk cache,
/// fetching from the API if necessary.
async fn ensure_api_data_cached(cache: &Cache) -> Result<()> {
    let formula_check = cache.load_raw("formula.json");
    let cask_check = cache.load_raw("cask.json");

    // Determine if fetches are needed
    let mut fetch_formulas = false;
    if formula_check.is_err() {
        tracing::debug!("Local formula.json cache missing or unreadable. Scheduling fetch.");
        fetch_formulas = true;
    }

    let mut fetch_casks = false;
    if cask_check.is_err() {
        tracing::debug!("Local cask.json cache missing or unreadable. Scheduling fetch.");
        fetch_casks = true;
    }

    // Perform fetches concurrently if needed
    match (fetch_formulas, fetch_casks) {
        (true, true) => {
            tracing::info!("Populating missing formula.json and cask.json from API...");
            let (formula_res, cask_res) = tokio::join!(
                async {
                    let data = api::fetch_all_formulas().await?;
                    cache.store_raw("formula.json", &data)?;
                    Ok::<(), SpsError>(())
                },
                async {
                    let data = api::fetch_all_casks().await?;
                    cache.store_raw("cask.json", &data)?;
                    Ok::<(), SpsError>(())
                }
            );
            formula_res?;
            cask_res?;
            tracing::info!("Formula.json and cask.json populated from API.");
        }
        (true, false) => {
            tracing::info!("Populating missing formula.json from API...");
            let data = api::fetch_all_formulas().await?;
            cache.store_raw("formula.json", &data)?;
            tracing::info!("Formula.json populated from API.");
        }
        (false, true) => {
            tracing::info!("Populating missing cask.json from API...");
            let data = api::fetch_all_casks().await?;
            cache.store_raw("cask.json", &data)?;
            tracing::info!("Cask.json populated from API.");
        }
        (false, false) => {
            tracing::debug!("Local formula.json and cask.json caches are present.");
        }
    }
    Ok(())
}

pub async fn check_for_updates(
    installed_packages: &[InstalledPackageInfo],
    cache: &Cache,
    config: &Config,
) -> Result<Vec<UpdateInfo>> {
    // 1. Ensure the underlying JSON files in the main cache are populated.
    ensure_api_data_cached(cache)
        .await
        .map_err(|e| {
            tracing::error!(
                "Failed to ensure API data is cached for update check: {}",
                e
            );
            // Decide if this is a fatal error for update checking or if we can proceed with
            // potentially stale/missing data. For now, let's make it non-fatal but log
            // an error, as Formulary/cask parsing might still work with older cache.
            SpsError::Generic(format!(
                "Failed to ensure API data in cache, update check might be unreliable: {e}"
            ))
        })
        .ok(); // Continue even if this pre-fetch step fails, rely on Formulary/cask loading to handle actual
               // errors.

    // 2. Instantiate Formulary. It uses the `cache` (from sps-common) to load `formula.json`.
    let formulary = Formulary::new(config.clone());

    // 3. For Casks: Load the entire `cask.json` from cache and parse it robustly into Vec<Cask>.
    //    This uses the Cask definition from sps_common::model::cask.
    let casks_map: HashMap<String, Arc<Cask>> = match cache.load_raw("cask.json") {
        Ok(json_str) => {
            // Parse directly into Vec<Cask> using the definition from sps-common::model::cask
            match serde_json::from_str::<Vec<Cask>>(&json_str) {
                Ok(casks) => casks
                    .into_iter()
                    .map(|c| (c.token.clone(), Arc::new(c)))
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse full cask.json string into Vec<Cask> (from sps-common): {}. Cask update checks may be incomplete.",
                        e
                    );
                    HashMap::new()
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to load cask.json string from cache: {}. Cask update checks will be based on no remote data.", e
            );
            HashMap::new()
        }
    };

    let mut updates_available = Vec::new();

    for installed in installed_packages {
        match installed.pkg_type {
            PackageType::Formula => {
                match formulary.load_formula(&installed.name) {
                    // Uses sps-common::formulary::Formulary
                    Ok(latest_formula_obj) => {
                        // Returns sps_common::model::formula::Formula
                        let latest_formula_arc = Arc::new(latest_formula_obj);

                        let latest_version_str = latest_formula_arc.version_str_full();
                        let installed_v_res = PkgVersion::parse(&installed.version);
                        let latest_v_res = PkgVersion::parse(&latest_version_str);
                        let installed_revision = installed
                            .version
                            .split('_')
                            .nth(1)
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);

                        let needs_update = match (installed_v_res, latest_v_res) {
                            (Ok(iv), Ok(lv)) => {
                                lv > iv
                                    || (lv == iv
                                        && latest_formula_arc.revision > installed_revision)
                            }
                            _ => installed.version != latest_version_str,
                        };

                        if needs_update {
                            updates_available.push(UpdateInfo {
                                name: installed.name.clone(),
                                installed_version: installed.version.clone(),
                                available_version: latest_version_str,
                                pkg_type: PackageType::Formula,
                                target_definition: InstallTargetIdentifier::Formula(
                                    // From sps-common
                                    latest_formula_arc.clone(),
                                ),
                            });
                        }
                    }
                    Err(_e) => {
                        tracing::debug!(
                                "Installed formula '{}' not found via Formulary. It might have been removed from the remote. Source error: {}",
                                installed.name, _e
                            );
                    }
                }
            }
            PackageType::Cask => {
                if let Some(latest_cask_arc) = casks_map.get(&installed.name) {
                    // latest_cask_arc is Arc<sps_common::model::cask::Cask>
                    if let Some(available_version) = latest_cask_arc.version.as_ref() {
                        if &installed.version != available_version {
                            updates_available.push(UpdateInfo {
                                name: installed.name.clone(),
                                installed_version: installed.version.clone(),
                                available_version: available_version.clone(),
                                pkg_type: PackageType::Cask,
                                target_definition: InstallTargetIdentifier::Cask(
                                    // From sps-common
                                    latest_cask_arc.clone(),
                                ),
                            });
                        }
                    } else {
                        tracing::warn!(
                            "Latest cask definition for '{}' from casks_map has no version string.",
                            installed.name
                        );
                    }
                } else {
                    tracing::warn!(
                        "Installed cask '{}' not found in the fully parsed casks_map.",
                        installed.name
                    );
                }
            }
        }
    }
    Ok(updates_available)
}
