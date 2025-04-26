pub mod artifacts;
pub mod dmg;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use infer;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;
use tracing::{debug, error};

use crate::build::extract;
use crate::model::cask::{Cask, Sha256Field, UrlField};
use crate::utils::cache::Cache;
use crate::utils::config::Config;
use crate::utils::error::{Result, SpmError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstalledArtifact {
    App {
        path: PathBuf,
    },
    CaskroomLink {
        link_path: PathBuf,
        target_path: PathBuf,
    },
    BinaryLink {
        link_path: PathBuf,
        target_path: PathBuf,
    },
    PkgUtilReceipt {
        id: String,
    },
    Launchd {
        label: String,
        path: Option<PathBuf>,
    },
    CaskroomReference {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskInstallManifest {
    pub manifest_format_version: String,
    pub token: String,
    pub version: String,
    pub installed_at: u64,
    pub artifacts: Vec<InstalledArtifact>,
}

pub fn get_cask_version_path(cask: &Cask, config: &Config) -> PathBuf {
    let version = cask.version.clone().unwrap_or_else(|| "latest".to_string());
    config.cask_version_path(&cask.token, &version)
}

pub async fn download_cask(cask: &Cask, cache: &Cache) -> Result<PathBuf> {
    let url_field = cask
        .url
        .as_ref()
        .ok_or_else(|| SpmError::Generic(format!("Cask {} has no URL", cask.token)))?;
    let url_str = match url_field {
        UrlField::Simple(u) => u.as_str(),
        UrlField::WithSpec { url, .. } => url.as_str(),
    };

    if url_str.is_empty() {
        return Err(SpmError::Generic(format!(
            "Cask {} has an empty URL",
            cask.token
        )));
    }

    debug!("Downloading cask from {}", url_str);
    let parsed = Url::parse(url_str)
        .map_err(|e| SpmError::Generic(format!("Invalid URL '{url_str}': {e}")))?;
    crate::fetch::validation::validate_url(parsed.as_str())?;
    let file_name = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            debug!("URL has no filename component, using fallback name for cache based on token.");
            format!("cask-{}-download.tmp", cask.token.replace('/', "_"))
        });
    let cache_key = format!("cask-{}-{}", cask.token, file_name);
    let cache_path = cache.get_dir().join(&cache_key);

    if cache_path.exists() {
        debug!("Using cached download: {}", cache_path.display());
        return Ok(cache_path);
    }

    let client = reqwest::Client::new();
    let response = client
        .get(parsed.clone())
        .send()
        .await
        .map_err(|e| SpmError::Http(std::sync::Arc::new(e)))?;
    if !response.status().is_success() {
        return Err(SpmError::DownloadError(
            cask.token.clone(),
            url_str.to_string(),
            format!("HTTP status {}", response.status()),
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| SpmError::Http(std::sync::Arc::new(e)))?;
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(&cache_path)?;
    file.write_all(&bytes)?;
    let expected_sha256 = match cask.sha256.as_ref() {
        Some(Sha256Field::Hex(s)) => s.as_str(),
        _ => "",
    };
    if !expected_sha256.is_empty() {
        match crate::fetch::validation::verify_checksum(&cache_path, expected_sha256) {
            Ok(_) => {
                tracing::debug!("Cask download checksum verified: {}", cache_path.display());
            }
            Err(e) => {
                tracing::error!(
                    "Cask download checksum mismatch ({}). Deleting cached file.",
                    e
                );
                let _ = fs::remove_file(&cache_path);
                return Err(e);
            }
        }
    } else {
        tracing::warn!(
            "Skipping checksum verification for cask {} - none provided.",
            cache_path.display()
        );
    }
    debug!("Download completed: {}", cache_path.display());
    Ok(cache_path)
}

pub fn install_cask(cask: &Cask, download_path: &Path, config: &Config) -> Result<()> {
    debug!("Installing cask: {}", cask.token);
    let cask_version_install_path = get_cask_version_path(cask, config);
    if !cask_version_install_path.exists() {
        fs::create_dir_all(&cask_version_install_path).map_err(|e| {
            SpmError::Io(std::sync::Arc::new(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed create cask dir {}: {}",
                    cask_version_install_path.display(),
                    e
                ),
            )))
        })?;
        debug!(
            "Created cask version directory: {}",
            cask_version_install_path.display()
        );
    }
    let mut detected_extension = download_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let non_extensions = ["stable", "latest", "download", "bin", ""];
    if non_extensions.contains(&detected_extension.as_str()) {
        debug!(
            "Download path '{}' has no definite extension ('{}'), attempting content detection.",
            download_path.display(),
            detected_extension
        );
        match infer::get_from_path(download_path) {
            Ok(Some(kind)) => {
                detected_extension = kind.extension().to_string();
                debug!("Detected file type via content: {}", detected_extension);
            }
            Ok(None) => {
                error!(
                    "Could not determine file type from content for: {}",
                    download_path.display()
                );
                return Err(SpmError::Generic(format!(
                    "Could not determine file type for download: {}",
                    download_path.display()
                )));
            }
            Err(e) => {
                error!(
                    "Error reading file for type detection {}: {}",
                    download_path.display(),
                    e
                );
                return Err(SpmError::Io(std::sync::Arc::new(e)));
            }
        }
    } else {
        debug!(
            "Using file extension for type detection: {}",
            detected_extension
        );
    }
    if detected_extension == "pkg" || detected_extension == "mpkg" {
        debug!("Detected PKG installer, running directly");
        match artifacts::pkg::install_pkg_from_path(
            cask,
            download_path,
            &cask_version_install_path,
            config,
        ) {
            Ok(installed_artifacts) => {
                debug!("Writing PKG install manifest");
                write_cask_manifest(cask, &cask_version_install_path, installed_artifacts)?;
                debug!("Successfully installed PKG cask: {}", cask.token);
                return Ok(());
            }
            Err(e) => {
                error!("PKG installation failed: {}", e);
                let _ = fs::remove_dir_all(&cask_version_install_path);
                return Err(e);
            }
        }
    }
    let stage_dir = TempDir::new().map_err(|e| {
        SpmError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to create staging directory: {e}"),
        )))
    })?;
    let stage_path = stage_dir.path();
    debug!("Created staging directory: {}", stage_path.display());
    // Determine expected extension (this might need refinement)
    // Option 1: Parse from URL
    let expected_ext_from_url = download_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    // Option 2: A new field in Cask JSON definition (preferred)
    // let expected_ext = cask.expected_extension.as_deref().unwrap_or(expected_ext_from_url);
    let expected_ext = expected_ext_from_url; // Use URL for now

    if !expected_ext.is_empty()
        && crate::build::formula::source::RECOGNISED_SINGLE_FILE_EXTENSIONS.contains(&expected_ext)
    {
        // Check if it's an archive/installer type we handle
        tracing::debug!(
            "Verifying content type for {} against expected extension '{}'",
            download_path.display(),
            expected_ext
        );
        if let Err(e) = crate::fetch::validation::verify_content_type(download_path, expected_ext) {
            tracing::error!("Content type verification failed: {}", e);
            // Attempt cleanup?
            let _ = fs::remove_dir_all(&cask_version_install_path);
            return Err(e);
        }
    } else {
        tracing::debug!(
            "Skipping content type verification for {} (unknown/no expected extension: '{}')",
            download_path.display(),
            expected_ext
        );
    }
    match detected_extension.as_str() {
        "dmg" => {
            debug!(
                "Extracting DMG {} to stage {}...",
                download_path.display(),
                stage_path.display()
            );
            dmg::extract_dmg_to_stage(download_path, stage_path)?;
            debug!("Successfully extracted DMG to staging area.");
        }
        "zip" => {
            debug!(
                "Extracting ZIP {} to stage {}...",
                download_path.display(),
                stage_path.display()
            );
            extract::extract_archive(download_path, stage_path, 0, "zip")?;
            debug!("Successfully extracted ZIP to staging area.");
        }
        "gz" | "bz2" | "xz" | "tar" => {
            let archive_type_for_extraction = detected_extension.as_str();
            debug!(
                "Extracting TAR archive ({}) {} to stage {}...",
                archive_type_for_extraction,
                download_path.display(),
                stage_path.display()
            );
            extract::extract_archive(download_path, stage_path, 0, archive_type_for_extraction)?;
            debug!("Successfully extracted TAR archive to staging area.");
        }
        _ => {
            error!(
                "Unsupported container/installer type '{}' for staged installation derived from {}",
                detected_extension,
                download_path.display()
            );
            return Err(SpmError::Generic(format!(
                "Unsupported file type for staged installation: {detected_extension}"
            )));
        }
    }
    let mut all_installed_artifacts: Vec<InstalledArtifact> = Vec::new();
    let mut artifact_install_errors = Vec::new();
    if let Some(artifacts_def) = &cask.artifacts {
        debug!(
            "Processing {} declared artifacts from staging area...",
            artifacts_def.len()
        );
        for artifact_value in artifacts_def.iter() {
            if let Some(artifact_obj) = artifact_value.as_object() {
                if let Some((key, value)) = artifact_obj.iter().next() {
                    debug!("Processing artifact type: {}", key);
                    let result: Result<Vec<InstalledArtifact>> = match key.as_str() {
                        "app" => {
                            let mut app_artifacts = vec![];
                            if let Some(app_names) = value.as_array() {
                                for app_name_val in app_names {
                                    if let Some(app_name) = app_name_val.as_str() {
                                        let staged_app_path = stage_path.join(app_name);
                                        debug!(
                                            "Attempting to install app artifact: {}",
                                            staged_app_path.display()
                                        );
                                        match artifacts::app::install_app_from_staged(
                                            cask,
                                            &staged_app_path,
                                            &cask_version_install_path,
                                            config,
                                        ) {
                                            Ok(mut artifacts) => {
                                                app_artifacts.append(&mut artifacts)
                                            }
                                            Err(e) => {
                                                return Err(e);
                                            }
                                        }
                                    } else {
                                        debug!(
                                            "Non-string value found in 'app' artifact array: {:?}",
                                            app_name_val
                                        );
                                    }
                                }
                            } else {
                                debug!("'app' artifact value is not an array: {:?}", value);
                            }
                            Ok(app_artifacts)
                        }
                        "pkg" => {
                            let mut installed_pkgs = vec![];
                            if let Some(pkg_names) = value.as_array() {
                                for pkg_val in pkg_names {
                                    if let Some(pkg_name) = pkg_val.as_str() {
                                        let staged_pkg_path = stage_path.join(pkg_name);
                                        debug!(
                                            "Attempting to install staged pkg artifact: {}",
                                            staged_pkg_path.display()
                                        );
                                        match artifacts::pkg::install_pkg_from_path(
                                            cask,
                                            &staged_pkg_path,
                                            &cask_version_install_path,
                                            config,
                                        ) {
                                            Ok(mut artifacts) => {
                                                installed_pkgs.append(&mut artifacts)
                                            }
                                            Err(e) => {
                                                return Err(e);
                                            }
                                        }
                                    } else {
                                        debug!(
                                            "Non-string value found in 'pkg' artifact array: {:?}",
                                            pkg_val
                                        );
                                    }
                                }
                            } else {
                                debug!("'pkg' artifact value is not an array: {:?}", value);
                            }
                            Ok(installed_pkgs)
                        }
                        "suite" => artifacts::suite::install_suite(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "installer" => artifacts::installer::run_installer(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "binary" => artifacts::binary::install_binary(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "manpage" => artifacts::manpage::install_manpage(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "colorpicker" => artifacts::colorpicker::install_colorpicker(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "dictionary" => artifacts::dictionary::install_dictionary(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "font" => artifacts::font::install_font(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "input_method" => artifacts::input_method::install_input_method(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "internet_plugin" => artifacts::internet_plugin::install_internet_plugin(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "keyboard_layout" => artifacts::keyboard_layout::install_keyboard_layout(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "prefpane" => artifacts::prefpane::install_prefpane(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "qlplugin" => artifacts::qlplugin::install_qlplugin(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "mdimporter" => artifacts::mdimporter::install_mdimporter(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "screen_saver" => artifacts::screen_saver::install_screen_saver(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "service" => artifacts::service::install_service(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "audio_unit_plugin" => {
                            artifacts::audio_unit_plugin::install_audio_unit_plugin(
                                cask,
                                stage_path,
                                &cask_version_install_path,
                                config,
                            )
                        }
                        "vst_plugin" => artifacts::vst_plugin::install_vst_plugin(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "vst3_plugin" => artifacts::vst3_plugin::install_vst3_plugin(
                            cask,
                            stage_path,
                            &cask_version_install_path,
                            config,
                        ),
                        "zap" => artifacts::zap::install_zap(cask, config),
                        "preflight" => {
                            artifacts::preflight::run_preflight(cask, stage_path, config)
                        }
                        "uninstall" => artifacts::uninstall::record_uninstall(cask),
                        other => {
                            debug!("Artifact type '{}' not supported yet — skipping.", other);
                            Ok(vec![])
                        }
                    };
                    match result {
                        Ok(installed) => {
                            if !installed.is_empty() {
                                debug!(
                                    "Successfully processed artifact '{}', added {} items.",
                                    key,
                                    installed.len()
                                );
                                all_installed_artifacts.extend(installed);
                            } else {
                                debug!("Artifact handler for '{}' completed successfully but returned no artifacts.", key);
                            }
                        }
                        Err(e) => {
                            error!("Error processing artifact '{}': {}", key, e);
                            artifact_install_errors.push(e);
                        }
                    }
                } else {
                    debug!("Empty artifact object found: {:?}", artifact_obj);
                }
            } else {
                debug!(
                    "Unexpected non-object artifact found in list: {:?}",
                    artifact_value
                );
            }
        }
    } else {
        error!("Cask {} definition is missing the required 'artifacts' array. Cannot determine what to install.", cask.token);
        return Err(SpmError::InstallError(format!(
            "Cask '{}' has no artifacts defined.",
            cask.token
        )));
    }
    if !artifact_install_errors.is_empty() {
        error!(
            "Encountered {} errors installing artifacts for cask '{}'. Installation incomplete.",
            artifact_install_errors.len(),
            cask.token
        );
        let _ = fs::remove_dir_all(&cask_version_install_path);
        return Err(artifact_install_errors.remove(0));
    }
    let actual_install_count = all_installed_artifacts
        .iter()
        .filter(|a| {
            !matches!(
                a,
                InstalledArtifact::PkgUtilReceipt { .. } | InstalledArtifact::Launchd { .. }
            )
        })
        .count();
    if actual_install_count == 0 {
        debug!("No installable artifacts (like app, pkg, binary, etc.) were processed for cask '{}' from the staged content. Check cask definition.", cask.token);
        write_cask_manifest(cask, &cask_version_install_path, all_installed_artifacts)?;
    } else {
        debug!("Writing cask installation manifest");
        write_cask_manifest(cask, &cask_version_install_path, all_installed_artifacts)?;
    }
    debug!("Successfully installed cask: {}", cask.token);
    Ok(())
}

#[deprecated(note = "Use write_cask_manifest with detailed InstalledArtifact enum instead")]
pub fn write_receipt(
    cask: &Cask,
    cask_version_install_path: &Path,
    artifacts: Vec<String>,
) -> Result<()> {
    let receipt_path = cask_version_install_path.join("INSTALL_RECEIPT.json");
    debug!("Writing legacy cask receipt: {}", receipt_path.display());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| SpmError::Generic(format!("System time error: {e}")))?
        .as_secs();
    let receipt_data = json!({
        "token": cask.token,
        "name": cask.name.as_ref().and_then(|n| n.first()).cloned(),
        "version": cask.version.as_ref().unwrap_or(&"latest".to_string()),
        "installed_at": timestamp,
        "artifacts_installed": artifacts
    });
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent).map_err(|e| SpmError::Io(std::sync::Arc::new(e)))?;
    }
    let mut file =
        fs::File::create(&receipt_path).map_err(|e| SpmError::Io(std::sync::Arc::new(e)))?;
    serde_json::to_writer_pretty(&mut file, &receipt_data)
        .map_err(|e| SpmError::Json(std::sync::Arc::new(e)))?;
    debug!(
        "Successfully wrote legacy receipt with {} artifact entries.",
        artifacts.len()
    );
    Ok(())
}

pub fn write_cask_manifest(
    cask: &Cask,
    cask_version_install_path: &Path,
    artifacts: Vec<InstalledArtifact>,
) -> Result<()> {
    let manifest_path = cask_version_install_path.join("CASK_INSTALL_MANIFEST.json");
    debug!("Writing cask manifest: {}", manifest_path.display());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| SpmError::Generic(format!("System time error: {e}")))?
        .as_secs();
    let manifest_data = CaskInstallManifest {
        manifest_format_version: "1.0".to_string(),
        token: cask.token.clone(),
        version: cask.version.clone().unwrap_or_else(|| "latest".to_string()),
        installed_at: timestamp,
        artifacts,
    };
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            SpmError::Io(std::sync::Arc::new(std::io::Error::new(
                e.kind(),
                format!("Failed create parent dir {}: {}", parent.display(), e),
            )))
        })?;
    }
    let file = fs::File::create(&manifest_path).map_err(|e| {
        SpmError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed create manifest {}: {}", manifest_path.display(), e),
        )))
    })?;
    let writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &manifest_data).map_err(|e| {
        error!(
            "Failed to serialize cask manifest JSON for {}: {}",
            cask.token, e
        );
        SpmError::Json(std::sync::Arc::new(e))
    })?;
    debug!(
        "Successfully wrote cask manifest with {} artifact entries.",
        manifest_data.artifacts.len()
    );
    Ok(())
}
