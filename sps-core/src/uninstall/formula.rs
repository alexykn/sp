// sps-core/src/uninstall/formula.rs
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use tracing::{debug, error, warn};

use crate::check::installed::InstalledPackageInfo;
use crate::install; // For install::bottle::link
use crate::uninstall::common::{remove_filesystem_artifact, UninstallOptions};

pub fn uninstall_formula_artifacts(
    info: &InstalledPackageInfo,
    config: &Config,
    _options: &UninstallOptions, /* options currently unused for formula but kept for signature
                                  * consistency */
) -> Result<()> {
    debug!(
        "Uninstalling Formula artifacts for {} version {}",
        info.name, info.version
    );

    // 1. Unlink artifacts
    // This function should handle removal of symlinks from /opt/sps/bin, /opt/sps/lib etc.
    // and the /opt/sps/opt/formula_name link.
    install::bottle::link::unlink_formula_artifacts(&info.name, &info.version, config)?;

    // 2. Remove the keg directory
    if info.path.exists() {
        debug!("Removing formula keg directory: {}", info.path.display());
        // For formula kegs, we generally expect them to be owned by the user or sps,
        // but sudo might be involved if permissions were changed manually or during a problematic
        // install. Setting use_sudo to true provides a fallback, though ideally it's not
        // needed for user-owned kegs.
        let use_sudo = true;
        if !remove_filesystem_artifact(&info.path, use_sudo) {
            // Check if it still exists after the removal attempt
            if info.path.exists() {
                error!(
                    "Failed remove keg {}: Check logs for sudo errors or other filesystem issues.",
                    info.path.display()
                );
                return Err(SpsError::InstallError(format!(
                    "Failed to remove keg directory: {}",
                    info.path.display()
                )));
            } else {
                // It means remove_filesystem_artifact returned false but the dir is gone
                // (possibly removed by sudo, or a race condition if another process removed it)
                debug!("Keg directory successfully removed (possibly with sudo).");
            }
        }
    } else {
        warn!(
            "Keg directory {} not found during uninstall. It might have been already removed.",
            info.path.display()
        );
    }
    Ok(())
}
