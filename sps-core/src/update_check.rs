// sps-core/src/update_check.rs
use std::collections::HashMap;
use std::sync::Arc;

use sps_common::cache::Cache;
use sps_common::error::{Result, SpsError};
use sps_common::model::InstallTargetIdentifier;
use sps_common::model::cask::Cask;
use sps_common::model::formula::Formula;
use sps_common::model::version::Version;
use sps_net::fetch::api;
use tracing::{debug, warn};

use crate::installed::{InstalledPackageInfo, PackageType};

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub name: String,
    pub installed_version: String,
    pub available_version: String,
    pub pkg_type: PackageType,
    pub target_definition: InstallTargetIdentifier, // Contains Arc<Formula/Cask>
}

async fn load_or_fetch_json(
    cache: &Cache,
    filename: &str,
    api_fetcher: impl std::future::Future<Output = Result<String>>,
) -> Result<Vec<serde_json::Value>> {
    match cache.load_raw(filename) {
        Ok(data) => {
            debug!("Loaded {} from cache.", filename);
            serde_json::from_str(&data).map_err(|e| {
                warn!("Failed to parse cached {}: {}", filename, e);
                SpsError::Cache(format!("Failed parse cached {filename}: {e}"))
            })
        }
        Err(_) => {
            debug!("Cache miss for {}, fetching from API...", filename);
            let raw_data = api_fetcher.await?;
            if let Err(cache_err) = cache.store_raw(filename, &raw_data) {
                warn!(
                    "Failed to cache {} data after fetching: {}",
                    filename, cache_err
                );
            } else {
                debug!("Successfully cached {} after fetching.", filename);
            }
            serde_json::from_str(&raw_data).map_err(|e| SpsError::Json(Arc::new(e)))
        }
    }
}

pub async fn check_for_updates(
    installed_packages: &[InstalledPackageInfo],
    cache: &Cache,
) -> Result<Vec<UpdateInfo>> {
    let (formula_values_res, cask_values_res) = tokio::join!(
        load_or_fetch_json(cache, "formula.json", api::fetch_all_formulas()),
        load_or_fetch_json(cache, "cask.json", api::fetch_all_casks())
    );

    let formulae_map: HashMap<String, Arc<Formula>> = match formula_values_res {
        Ok(values) => values
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Formula>(v).ok())
            .map(|f| (f.name.clone(), Arc::new(f)))
            .collect(),
        Err(e) => {
            warn!("Failed to load formula map for update check: {}", e);
            HashMap::new()
        }
    };

    let casks_map: HashMap<String, Arc<Cask>> = match cask_values_res {
        Ok(values) => values
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Cask>(v).ok())
            .map(|c| (c.token.clone(), Arc::new(c)))
            .collect(),
        Err(e) => {
            warn!("Failed to load cask map for update check: {}", e);
            HashMap::new()
        }
    };

    let mut updates_available = Vec::new();

    for installed in installed_packages {
        match installed.pkg_type {
            PackageType::Formula => {
                if let Some(latest_formula_arc) = formulae_map.get(&installed.name) {
                    let latest_version_str = latest_formula_arc.version_str_full();
                    let installed_v_res = Version::parse(&installed.version);
                    let latest_v_res = Version::parse(&latest_version_str);
                    let installed_revision = installed
                        .version
                        .split('_')
                        .nth(1)
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    let needs_update = match (installed_v_res, latest_v_res) {
                        (Ok(iv), Ok(lv)) => {
                            lv > iv
                                || (lv == iv && latest_formula_arc.revision > installed_revision)
                        }
                        _ => installed.version != latest_version_str,
                    };
                    if needs_update {
                        debug!(
                            "Update found for Formula {}: {} -> {}",
                            installed.name, installed.version, latest_version_str
                        );
                        updates_available.push(UpdateInfo {
                            name: installed.name.clone(),
                            installed_version: installed.version.clone(),
                            available_version: latest_version_str,
                            pkg_type: PackageType::Formula,
                            target_definition: InstallTargetIdentifier::Formula(
                                latest_formula_arc.clone(),
                            ),
                        });
                    }
                } else {
                    warn!(
                        "Installed formula '{}' not found in latest API data.",
                        installed.name
                    );
                }
            }
            PackageType::Cask => {
                if let Some(latest_cask_arc) = casks_map.get(&installed.name) {
                    if let Some(available_version) = latest_cask_arc.version.as_ref() {
                        if &installed.version != available_version {
                            debug!(
                                "Update found for Cask {}: {} -> {}",
                                installed.name, installed.version, available_version
                            );
                            updates_available.push(UpdateInfo {
                                name: installed.name.clone(),
                                installed_version: installed.version.clone(),
                                available_version: available_version.clone(),
                                pkg_type: PackageType::Cask,
                                target_definition: InstallTargetIdentifier::Cask(
                                    latest_cask_arc.clone(),
                                ),
                            });
                        }
                    } else {
                        warn!(
                            "Latest cask definition for '{}' has no version string.",
                            installed.name
                        );
                    }
                } else {
                    warn!(
                        "Installed cask '{}' not found in latest API data.",
                        installed.name
                    );
                }
            }
        }
    }
    Ok(updates_available)
}
