//! CuaDriver discovery shared by the backend and desktop gateway.
//! Installation is independent of Apple Events, Messages, and macOS TCC.

use std::path::PathBuf;

/// Resolve an absolute executable, including the official Windows installer
/// directory when the desktop app inherited PATH before installation.
pub fn driver_binary() -> Option<PathBuf> {
    let candidates = driver_candidates(
        std::env::consts::OS,
        std::env::var_os("MAGICIAN_CUA_DRIVER_BIN").map(PathBuf::from),
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default(),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    );
    candidates.into_iter().find_map(|path| {
        if !path.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.metadata().ok()?.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        // Keep symlinks: the official installer may use an executable shim.
        if path.is_absolute() {
            Some(path)
        } else {
            Some(std::env::current_dir().ok()?.join(path))
        }
    })
}

fn driver_candidates(
    os: &str,
    explicit: Option<PathBuf>,
    search: Vec<PathBuf>,
    home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
) -> Vec<PathBuf> {
    let name = if os == "windows" {
        "cua-driver.exe"
    } else {
        "cua-driver"
    };
    let mut candidates: Vec<_> = explicit.into_iter().collect();
    candidates.extend(search.into_iter().map(|dir| dir.join(name)));
    if os == "windows" {
        if let Some(root) = local_app_data {
            candidates.push(root.join("Programs/Cua/cua-driver/bin").join(name));
            candidates.push(root.join("Programs/trycua/cua-driver-rs/bin").join(name));
        }
    } else if let Some(home) = home {
        candidates.push(home.join(".local/bin").join(name));
    }
    candidates
}

/// Can this process start a driver in the user's desktop session? A headless
/// Linux service must relay to a desktop, even if a Linux driver is installed.
/// An already-running daemon may still be reachable without these env vars.
pub fn has_desktop_session() -> bool {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcessId() -> u32;
            fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
        }
        let mut session = 0;
        // Session 0 is a service/SSH session, not the signed-in user's desktop.
        return unsafe {
            ProcessIdToSessionId(GetCurrentProcessId(), &mut session) != 0 && session != 0
        };
    }
    #[cfg(not(target_os = "windows"))]
    desktop_environment(
        std::env::consts::OS,
        std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
    )
}

#[cfg(any(test, not(target_os = "windows")))]
fn desktop_environment(os: &str, x11: bool, wayland: bool) -> bool {
    os == "macos" || (os == "linux" && (x11 || wayland))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_finds_exe_and_installer_path_without_mac_fallback() {
        let paths = driver_candidates(
            "windows",
            Some("override.exe".into()),
            vec!["bin".into()],
            Some("home".into()),
            Some("local".into()),
        );
        assert_eq!(paths[0], PathBuf::from("override.exe"));
        assert_eq!(paths[1], PathBuf::from("bin/cua-driver.exe"));
        assert_eq!(
            paths[2],
            PathBuf::from("local/Programs/Cua/cua-driver/bin/cua-driver.exe")
        );
        assert!(paths
            .iter()
            .all(|path| !path.to_string_lossy().contains(".local")));
    }

    #[test]
    fn linux_and_mac_keep_official_user_bin_fallback() {
        for os in ["linux", "macos"] {
            assert_eq!(
                driver_candidates(os, None, vec![], Some("home".into()), None),
                vec![PathBuf::from("home/.local/bin/cua-driver")]
            );
        }
    }

    #[test]
    fn linux_installation_does_not_imply_a_desktop() {
        assert!(!desktop_environment("linux", false, false));
        assert!(desktop_environment("linux", true, false));
        assert!(desktop_environment("linux", false, true));
        assert!(desktop_environment("macos", false, false));
        assert!(!desktop_environment("freebsd", true, true));
    }
}
