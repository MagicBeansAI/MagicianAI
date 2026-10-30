//! Uninstall / cleanup logic for Magician Desktop.
//!
//! Accepts a *mode* that controls what is removed:
//!
//! | Mode             | Container | Image | Data dirs | Launch Agent | Installed tools | Manifest |
//! |------------------|-----------|-------|-----------|--------------|-----------------|----------|
//! | tools-and-data   | yes       | yes   | yes       | yes          | yes             | yes      |
//! | only-tools       | yes       | yes   | no        | yes          | yes             | no       |
//! | only-data        | yes       | no    | yes       | no           | no              | no       |
//!
//! Order matters: a running service is stopped or unregistered before its
//! data folder is removed (see [`cleanup_plan`]). With Desktop's own native
//! service, "only-data" stops it, removes the data, re-seeds the root the
//! service uses and starts it again.

use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tracing::{info, warn};

use crate::container::ContainerRuntime;
use crate::manifest::InstallManifest;
use crate::AppState;

/// One side-effecting cleanup step, in the order [`cleanup_plan`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupStep {
    StopRemoveContainer,
    RemoveImage,
    RemoveLaunchAgent,
    /// Unregisters the native service and removes its install prefix.
    RemoveInstalledTools,
    /// The desktop app's clean-quit marker (magic-supervisor's desktop watch).
    RemoveQuitMarker,
    StopNativeService,
    WarnForeignNativeService,
    RemoveDataDirs,
    ReseedAndStartNative,
    RemoveManifest,
}

/// The steps for `mode`. Any service writing into the data folder is stopped
/// or unregistered before the data is removed.
fn cleanup_plan(
    mode: &str,
    native: bool,
    native_service_ours: bool,
) -> Result<Vec<CleanupStep>, String> {
    use CleanupStep::*;
    Ok(match mode {
        "tools-and-data" => vec![
            StopRemoveContainer,
            RemoveImage,
            RemoveLaunchAgent,
            RemoveInstalledTools,
            RemoveQuitMarker,
            RemoveDataDirs,
            RemoveManifest,
        ],
        "only-tools" => vec![
            StopRemoveContainer,
            RemoveImage,
            RemoveLaunchAgent,
            RemoveInstalledTools,
            RemoveQuitMarker,
        ],
        "only-data" if native && native_service_ours => vec![
            StopRemoveContainer,
            StopNativeService,
            RemoveDataDirs,
            ReseedAndStartNative,
        ],
        "only-data" if native => vec![
            StopRemoveContainer,
            WarnForeignNativeService,
            RemoveDataDirs,
        ],
        "only-data" => vec![StopRemoveContainer, RemoveDataDirs],
        other => {
            return Err(format!(
                "Unknown cleanup mode \"{other}\". Expected: tools-and-data, only-tools, only-data"
            ))
        },
    })
}

/// Whether `root` (or its canonical form, taken before the deletion) is a
/// directory this run actually removed, or inside one. A refused or failed
/// delete never counts, so nothing is seeded into an intact root.
fn root_was_removed(
    root: &std::path::Path,
    canonical_root: Option<&std::path::Path>,
    removed: &[std::path::PathBuf],
) -> bool {
    removed.iter().any(|dir| {
        root.starts_with(dir) || canonical_root.is_some_and(|canonical| canonical.starts_with(dir))
    })
}

/// Run cleanup/uninstall according to the requested `mode`.
///
/// Returns a human-readable summary of everything that was done.
pub async fn run_cleanup(app: &AppHandle, mode: &str) -> Result<String, String> {
    let manifest = crate::manifest::read_manifest()
        .ok_or_else(|| "No install manifest found — nothing to uninstall.".to_string())?;

    let native = manifest.runtime == crate::native_runtime::RUNTIME_NAME;
    let plan = cleanup_plan(mode, native, !manifest.pre_existing.native_service)?;
    let mut summary: Vec<String> = Vec::new();
    // Read before the data goes: the root the native service really uses,
    // re-seeded afterwards only when this run actually removed it.
    let registered_root = crate::native_runtime::registered_runtime_root();
    let canonical_root = registered_root
        .as_ref()
        .and_then(|root| root.canonicalize().ok());
    let mut removed: Vec<std::path::PathBuf> = Vec::new();

    for step in plan {
        match step {
            CleanupStep::StopRemoveContainer => {
                stop_and_remove_container(app, &manifest, &mut summary).await
            },
            CleanupStep::RemoveImage => remove_container_image(&manifest, &mut summary).await,
            CleanupStep::RemoveLaunchAgent => remove_launch_agent(&manifest, &mut summary).await,
            CleanupStep::RemoveInstalledTools => {
                remove_installed_tools(&manifest, &mut summary).await
            },
            CleanupStep::RemoveQuitMarker => {
                let marker = crate::config::desktop_quit_marker_path();
                match std::fs::remove_file(&marker) {
                    Ok(()) => summary.push(format!(
                        "Removed the desktop clean-quit marker {}.",
                        marker.display()
                    )),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => summary.push(format!(
                        "Warning: could not remove {} — {error}",
                        marker.display()
                    )),
                }
            },
            CleanupStep::StopNativeService => match crate::native_runtime::stop_service().await {
                Ok(()) => summary
                    .push("Stopped the native backend before removing its data.".to_string()),
                Err(error) => summary.push(format!(
                    "Warning: could not stop the native backend before removing its data — {error}"
                )),
            },
            CleanupStep::WarnForeignNativeService => summary.push(
                "Warning: the native backend service existed before Desktop setup and was left running; its data was removed under it."
                    .to_string(),
            ),
            CleanupStep::RemoveDataDirs => removed = remove_data_dirs(&manifest, &mut summary),
            CleanupStep::ReseedAndStartNative => match registered_root
                .as_deref()
                .filter(|root| root_was_removed(root, canonical_root.as_deref(), &removed))
            {
                Some(root) => match crate::native_runtime::reseed_and_start(root).await {
                    Ok(()) => summary.push(format!(
                        "Re-seeded {} and started the native backend again.",
                        root.display()
                    )),
                    Err(error) => summary.push(format!(
                        "Warning: could not re-seed {} and start the native backend — {error}",
                        root.display()
                    )),
                },
                None => match crate::native_runtime::ensure_service_started().await {
                    Ok(()) => summary.push("Started the native backend again.".to_string()),
                    Err(error) => summary.push(format!(
                        "Warning: could not start the native backend again — {error}"
                    )),
                },
            },
            CleanupStep::RemoveManifest => {
                if let Err(e) = crate::manifest::remove_manifest() {
                    warn!("Failed to remove manifest: {e}");
                    summary.push(format!("Warning: could not remove manifest — {e}"));
                } else {
                    summary.push("Removed install manifest.".to_string());
                }
            },
        }
    }

    if summary.is_empty() {
        summary.push("Nothing to clean up.".to_string());
    }

    Ok(summary.join("\n"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Stop and remove the container using the runtime from `AppState`.
async fn stop_and_remove_container(
    app: &AppHandle,
    manifest: &InstallManifest,
    summary: &mut Vec<String>,
) {
    if manifest.runtime == crate::native_runtime::RUNTIME_NAME {
        return;
    }
    let runtime: Option<Arc<dyn ContainerRuntime>> = {
        let state = app.state::<AppState>();
        let guard = state.runtime.lock().await;
        guard.clone()
    };

    let Some(rt) = runtime else {
        summary.push("No container runtime available — skipping container removal.".to_string());
        return;
    };

    let name = &manifest.installed_by_us.container_name;

    // Try to stop first (ignore errors — container may already be stopped or missing).
    match rt.stop(name).await {
        Ok(()) => {
            info!("Stopped container {name}");
            summary.push(format!("Stopped container \"{name}\"."));
        },
        Err(e) => {
            info!("Could not stop container {name} (may already be stopped): {e}");
        },
    }

    match rt.remove(name).await {
        Ok(()) => {
            info!("Removed container {name}");
            summary.push(format!("Removed container \"{name}\"."));
        },
        Err(e) => {
            info!("Could not remove container {name}: {e}");
        },
    }
}

/// Remove the container image. Tries `docker rmi`, falls back to `container image remove`.
async fn remove_container_image(manifest: &InstallManifest, summary: &mut Vec<String>) {
    let image = &manifest.installed_by_us.container_image;
    if image.is_empty() {
        return;
    }

    // Try docker rmi first.
    let docker_result = tokio::process::Command::new("docker")
        .args(["rmi", image])
        .output()
        .await;

    match docker_result {
        Ok(output) if output.status.success() => {
            info!("Removed image {image} via docker rmi");
            summary.push(format!("Removed container image \"{image}\"."));
            return;
        },
        _ => {
            info!("docker rmi failed or unavailable, trying `container image remove`");
        },
    }

    // Fallback: Apple container CLI.
    let apple_result = tokio::process::Command::new("container")
        .args(["image", "remove", image])
        .output()
        .await;

    match apple_result {
        Ok(output) if output.status.success() => {
            info!("Removed image {image} via `container image remove`");
            summary.push(format!("Removed container image \"{image}\"."));
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to remove image {image}: {stderr}");
            summary.push(format!(
                "Warning: could not remove image \"{image}\" — {}",
                stderr.trim()
            ));
        },
        Err(e) => {
            warn!("Failed to remove image {image}: {e}");
            summary.push(format!("Warning: could not remove image \"{image}\" — {e}"));
        },
    }
}

/// `path` is inside `home` and is not `home` itself (nor, therefore, any
/// ancestor of it); mirrors `scripts/cleanup.sh`'s `"$HOME"/*` pattern.
fn is_strictly_under(path: &std::path::Path, home: &std::path::Path) -> bool {
    path.starts_with(home) && path != home
}

/// Remove data directories listed in the manifest; returns the resolved
/// paths actually removed.
///
/// Safety: each path is validated to be under the user's home directory and
/// free of `..` components before deletion, guarding against a tampered manifest.
fn remove_data_dirs(
    manifest: &InstallManifest,
    summary: &mut Vec<String>,
) -> Vec<std::path::PathBuf> {
    let mut removed = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h.canonicalize().unwrap_or(h),
        None => {
            warn!("Cannot determine home directory — refusing to remove data dirs");
            summary.push(
                "Warning: cannot determine home directory — skipped data dir removal.".to_string(),
            );
            return removed;
        },
    };

    for dir in &manifest.data_dirs {
        let path = std::path::Path::new(dir);

        // Canonicalize to resolve symlinks; fall back to the raw path if it
        // doesn't exist yet (in which case there's nothing to delete anyway).
        let resolved = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                info!("Data dir {dir} does not exist or cannot be resolved, skipping");
                continue;
            },
        };

        // Reject paths containing ".." components (belt-and-suspenders after canonicalize).
        if dir.contains("..") {
            warn!("REFUSED to delete '{dir}': path contains '..' component");
            summary.push(format!(
                "REFUSED to delete \"{dir}\" (path contains \"..\")."
            ));
            continue;
        }

        // Reject paths that are not strictly under the user's home directory:
        // home itself (or an ancestor of it) is never a data folder.
        if !is_strictly_under(&resolved, &home) {
            warn!(
                "REFUSED to delete '{dir}': not under home directory {}",
                home.display()
            );
            summary.push(format!(
                "REFUSED to delete \"{dir}\" (not under home directory)."
            ));
            continue;
        }

        match std::fs::remove_dir_all(&resolved) {
            Ok(()) => {
                info!("Removed data directory {dir}");
                summary.push(format!("Removed data directory \"{dir}\"."));
                removed.push(path.to_path_buf());
                removed.push(resolved.clone());
            },
            Err(e) => {
                warn!("Failed to remove data directory {dir}: {e}");
                summary.push(format!("Warning: could not remove \"{dir}\" — {e}"));
            },
        }
    }
    removed
}

/// Remove the macOS launch agent plist (if we installed one).
async fn remove_launch_agent(manifest: &InstallManifest, summary: &mut Vec<String>) {
    if std::env::consts::OS != "macos" || !manifest.installed_by_us.launch_agent {
        return;
    }

    let plist = dirs::home_dir()
        .expect("cannot determine home directory")
        .join("Library/LaunchAgents/dev.magician.desktop.plist");

    if !plist.exists() {
        info!("Launch agent plist does not exist, skipping");
        return;
    }

    // Unload first (ignore errors — may already be unloaded).
    let plist_str = plist.to_string_lossy().to_string();
    let _ = tokio::process::Command::new("launchctl")
        .args(["unload", &plist_str])
        .output()
        .await;

    match std::fs::remove_file(&plist) {
        Ok(()) => {
            info!("Removed launch agent {}", plist.display());
            summary.push("Removed macOS launch agent.".to_string());
        },
        Err(e) => {
            warn!("Failed to remove launch agent: {e}");
            summary.push(format!("Warning: could not remove launch agent — {e}"));
        },
    }
}

/// Remove tools that we installed (container runtime via Homebrew or system package manager).
///
/// Uses the same privilege escalation as install: `osascript` on macOS, `pkexec` on Linux.
///
/// Safety rules:
/// - NEVER remove tools where `manifest.pre_existing.*` is true.
/// - NEVER auto-remove Homebrew — just inform the user.
async fn remove_installed_tools(manifest: &InstallManifest, summary: &mut Vec<String>) {
    if manifest.runtime == crate::native_runtime::RUNTIME_NAME {
        if manifest.pre_existing.native_service {
            summary.push(
                "Native backend startup service existed before Desktop setup — left in place."
                    .to_string(),
            );
        } else {
            match crate::native_runtime::unregister_service().await {
                Ok(()) => summary.push("Removed the native backend startup service.".to_string()),
                Err(error) => summary.push(format!(
                    "Warning: could not remove the native backend startup service — {error}"
                )),
            }
        }
        if manifest.pre_existing.native_backend {
            summary.push(
                "Native backend files existed before Desktop setup — left in place.".to_string(),
            );
            return;
        }
        let expected = crate::native_runtime::install_prefix();
        let recorded = std::path::PathBuf::from(&manifest.installed_by_us.native_prefix);
        if recorded != expected || !recorded.join("MANIFEST.yaml").is_file() {
            summary.push(format!(
                "REFUSED to remove native backend files at {} because the installation identity did not match.",
                recorded.display()
            ));
            return;
        }
        match std::fs::remove_dir_all(&recorded) {
            Ok(()) => summary.push(format!(
                "Removed native backend files from {}.",
                recorded.display()
            )),
            Err(error) => summary.push(format!(
                "Warning: could not remove native backend files from {} — {error}",
                recorded.display()
            )),
        }
        return;
    }

    // Only remove the container runtime if we installed it ourselves.
    if manifest.pre_existing.container_runtime {
        info!("Container runtime was pre-existing — not removing");
        summary.push("Container runtime was pre-existing — left in place.".to_string());
    } else {
        let runtime_id = &manifest.installed_by_us.container_runtime;
        match runtime_id.as_str() {
            "colima+docker" => {
                // Stop Colima first (no admin needed)
                let _ = tokio::process::Command::new("colima")
                    .arg("stop")
                    .output()
                    .await;

                // Uninstall via brew with admin privileges (macOS)
                let brew = crate::container::find_brew_path();
                let cmd = format!("{} uninstall colima docker", brew);
                match crate::container::run_with_admin(&cmd, "uninstall Colima and Docker").await {
                    Ok(_) => {
                        info!("Uninstalled Colima + Docker via Homebrew (admin)");
                        summary.push("Uninstalled Colima + Docker CLI via Homebrew.".to_string());
                    },
                    Err(e) => {
                        warn!("Failed to uninstall Colima/Docker: {e}");
                        summary.push(format!("Warning: could not uninstall Colima/Docker — {e}"));
                    },
                }
            },
            "apple-container" => {
                let brew = crate::container::find_brew_path();
                let cmd = format!("{} uninstall container", brew);
                match crate::container::run_with_admin(&cmd, "uninstall Apple Container CLI").await
                {
                    Ok(_) => {
                        info!("Uninstalled Apple Container CLI via Homebrew (admin)");
                        summary.push("Uninstalled Apple Container CLI via Homebrew.".to_string());
                    },
                    Err(e) => {
                        warn!("Failed to uninstall container CLI: {e}");
                        summary.push(format!(
                            "Warning: could not uninstall Apple Container CLI — {e}"
                        ));
                    },
                }
            },
            "docker" => {
                // Linux: Docker was installed via get.docker.com, needs admin to remove
                if std::env::consts::OS == "linux" {
                    match crate::container::run_with_admin_linux(
                        "apt-get remove -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin 2>/dev/null || yum remove -y docker-ce docker-ce-cli containerd.io 2>/dev/null || true",
                        "uninstall Docker"
                    ).await {
                        Ok(_) => {
                            info!("Uninstalled Docker via system package manager (admin)");
                            summary.push("Uninstalled Docker via system package manager.".to_string());
                        }
                        Err(e) => {
                            warn!("Failed to uninstall Docker: {e}");
                            summary.push(format!(
                                "Warning: could not uninstall Docker — {e}"
                            ));
                        }
                    }
                }
            },
            "none" | "" => {
                info!("No container runtime was installed by us");
            },
            other => {
                warn!("Unknown runtime '{other}', skipping tool removal");
                summary.push(format!(
                    "Warning: unknown runtime \"{other}\" — skipping removal."
                ));
            },
        }
    }

    // Homebrew itself: NEVER auto-remove, just inform.
    if !manifest.pre_existing.homebrew && manifest.installed_by_us.homebrew {
        summary.push(
            "Homebrew was installed by Magician but has NOT been removed (other apps may depend on it). \
             To remove it manually, run: /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/uninstall.sh)\""
                .to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::CleanupStep::*;
    use super::*;

    fn position(plan: &[CleanupStep], step: CleanupStep) -> usize {
        plan.iter()
            .position(|item| *item == step)
            .expect("step planned")
    }

    #[test]
    fn services_stop_before_their_data_is_removed() {
        for native in [false, true] {
            let plan = cleanup_plan("tools-and-data", native, true).expect("plan");
            assert_eq!(
                plan,
                vec![
                    StopRemoveContainer,
                    RemoveImage,
                    RemoveLaunchAgent,
                    RemoveInstalledTools,
                    RemoveQuitMarker,
                    RemoveDataDirs,
                    RemoveManifest
                ]
            );
            assert!(position(&plan, RemoveInstalledTools) < position(&plan, RemoveDataDirs));
        }
        assert_eq!(
            cleanup_plan("only-tools", true, true).expect("plan"),
            vec![
                StopRemoveContainer,
                RemoveImage,
                RemoveLaunchAgent,
                RemoveInstalledTools,
                RemoveQuitMarker
            ]
        );
        assert!(
            !cleanup_plan("only-data", true, true)
                .expect("plan")
                .contains(&RemoveQuitMarker),
            "data-only cleanup keeps the marker, like scripts/cleanup.sh"
        );
    }

    #[test]
    fn only_data_with_our_native_service_stops_reseeds_and_restarts_it() {
        assert_eq!(
            cleanup_plan("only-data", true, true).expect("plan"),
            vec![
                StopRemoveContainer,
                StopNativeService,
                RemoveDataDirs,
                ReseedAndStartNative
            ]
        );
        assert_eq!(
            cleanup_plan("only-data", true, false).expect("plan"),
            vec![
                StopRemoveContainer,
                WarnForeignNativeService,
                RemoveDataDirs
            ],
            "a service that predates Desktop is never touched"
        );
        assert_eq!(
            cleanup_plan("only-data", false, true).expect("plan"),
            vec![StopRemoveContainer, RemoveDataDirs]
        );
        assert!(cleanup_plan("everything", true, true).is_err());
    }

    #[test]
    fn home_itself_or_an_ancestor_is_never_a_data_folder() {
        use std::path::Path;
        let home = Path::new("/Users/me");
        assert!(is_strictly_under(
            Path::new("/Users/me/MagicianNotes"),
            home
        ));
        assert!(
            !is_strictly_under(Path::new("/Users/me"), home),
            "home itself is refused"
        );
        assert!(
            !is_strictly_under(Path::new("/Users"), home),
            "an ancestor is refused"
        );
        assert!(!is_strictly_under(Path::new("/"), home));
        assert!(!is_strictly_under(Path::new("/Users/meme"), home));
    }

    #[test]
    fn the_service_root_is_reseeded_only_when_it_was_actually_removed() {
        use std::path::{Path, PathBuf};
        let removed = vec![PathBuf::from("/Volumes/ssd/MagicianNotes")];
        assert!(root_was_removed(
            Path::new("/Volumes/ssd/MagicianNotes"),
            None,
            &removed
        ));
        assert!(root_was_removed(
            Path::new("/Volumes/ssd/MagicianNotes/sub"),
            None,
            &removed
        ));
        assert!(
            root_was_removed(
                Path::new("/Users/me/MagicianNotes"),
                Some(Path::new("/Volumes/ssd/MagicianNotes")),
                &removed
            ),
            "a symlinked root matches through its canonical target"
        );
        assert!(!root_was_removed(
            Path::new("/Users/me/MagicianNotesOld"),
            None,
            &removed
        ));
        assert!(
            !root_was_removed(Path::new("/Volumes/ssd/MagicianNotes"), None, &[]),
            "a refused or failed delete never leads to seeding"
        );
    }
}
