//! Desktop-local CuaDriver installation used by first-run and maintenance setup.
//!
//! The backend may live in a container or on another computer, so this action
//! deliberately runs in Magican Desktop's signed-in graphical session.
//! It installs exactly the CuaDriver release pinned in the built-in components
//! setup catalog, from that release's tag, with every script hash-checked.

use futures_util::StreamExt;
use magician_components::setup::{CuaDriverInstallerFile, CuaDriverRelease};
use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// A script block or `-File` run of install.ps1 would otherwise resolve its
/// module relative to itself; point it at the verified copy explicitly.
const PS_MODULE_ANCHOR: &str = "-LocalDir $PSScriptRoot";

fn installer_files<'a>(
    os: &str,
    release: &'a CuaDriverRelease,
) -> Result<&'a [CuaDriverInstallerFile], String> {
    match os {
        "macos" | "linux" => Ok(release.unix_installer.as_slice()),
        "windows" => Ok(release.windows_installer.as_slice()),
        other => Err(format!("CuaDriver setup is unsupported on {other}")),
    }
}

/// Only tag-pinned trycua/cua files are fetched; their bytes are then checked
/// against the catalog hash, so a redirect target cannot change what runs.
fn installer_url(url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| format!("CuaDriver installer URL is invalid: {error}"))?;
    let pinned_source = match parsed.host_str() {
        Some("github.com") => parsed.path().starts_with("/trycua/cua/releases/download/"),
        Some("raw.githubusercontent.com") => parsed.path().starts_with("/trycua/cua/"),
        _ => false,
    };
    if parsed.scheme() != "https" || !pinned_source {
        return Err(format!(
            "CuaDriver installer must come from a pinned trycua/cua GitHub tag: {url}"
        ));
    }
    Ok(parsed)
}

async fn download_installer(url: &str) -> Result<Vec<u8>, String> {
    let parsed = installer_url(url)?;
    // GitHub release downloads redirect to its asset CDN. Follow a few HTTPS
    // hops only; integrity comes from the pinned hash, not from the host.
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            attempt.error("CuaDriver installer redirected away from HTTPS")
        } else if attempt.previous().len() >= 5 {
            attempt.error("CuaDriver installer redirected too many times")
        } else {
            attempt.follow()
        }
    });
    let client = reqwest::Client::builder()
        .redirect(redirects)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| format!("Could not create the CuaDriver download client: {error}"))?;
    let response = client
        .get(parsed)
        .send()
        .await
        .map_err(|error| format!("Could not download CuaDriver: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "CuaDriver installer download returned {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length == 0 || length > MAX_INSTALLER_BYTES as u64)
    {
        return Err("CuaDriver installer has an unexpected size".to_string());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| format!("Could not read CuaDriver installer: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_INSTALLER_BYTES {
            return Err("CuaDriver installer exceeded the download limit".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err("CuaDriver installer download was empty".to_string());
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Refuse bytes that differ from the pinned release before they touch disk.
fn verify_installer(
    file: &CuaDriverInstallerFile,
    version: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if sha256_hex(bytes) != file.sha256 {
        return Err(format!(
            "CuaDriver installer {} does not match the pinned {version} hash",
            file.name
        ));
    }
    Ok(())
}

/// Point install.ps1's module import at the verified copy in `folder`.
fn anchor_windows_module(script: &str, folder: &Path) -> Result<String, String> {
    let script = script.strip_prefix('\u{feff}').unwrap_or(script);
    if script.matches(PS_MODULE_ANCHOR).count() != 1 {
        return Err("Pinned install.ps1 no longer loads its module from $PSScriptRoot".to_string());
    }
    let literal = folder.display().to_string().replace('\'', "''");
    Ok(script.replacen(PS_MODULE_ANCHOR, &format!("-LocalDir '{literal}'"), 1))
}

/// The first `x.y.z[-pre]` token of `cua-driver --version`.
fn parse_driver_version(stdout: &str) -> Option<String> {
    stdout.split_whitespace().find_map(|token| {
        let token = token
            .trim_start_matches('v')
            .trim_end_matches(|character: char| !character.is_ascii_alphanumeric());
        let core = token.split('-').next()?;
        let parts: Vec<_> = core.split('.').collect();
        (parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())))
        .then(|| token.to_string())
    })
}

async fn installed_version(binary: &Path) -> Option<String> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(30), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_driver_version(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "windows")]
fn installer_command(path: &Path) -> Result<Command, String> {
    use std::os::windows::process::CommandExt;

    let system_root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Windows did not provide SystemRoot".to_string())?;
    let powershell = system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !powershell.is_file() {
        return Err(format!(
            "Windows PowerShell is missing at {}",
            powershell.display()
        ));
    }
    let mut command = Command::new(powershell);
    command.as_std_mut().creation_flags(0x0800_0000);
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]);
    command.arg(path);
    Ok(command)
}

#[cfg(not(target_os = "windows"))]
fn installer_command(path: &Path) -> Result<Command, String> {
    if !Path::new("/bin/bash").is_file() {
        return Err("CuaDriver installation requires /bin/bash".to_string());
    }
    let mut command = Command::new("/bin/bash");
    command.arg(path);
    Ok(command)
}

fn bounded_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(&String::from_utf8_lossy(stdout));
    }
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(stderr));
    }
    if combined.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !combined.is_char_boundary(end) {
            end -= 1;
        }
        combined.truncate(end);
        combined.push_str("\n[installer output truncated]");
    }
    combined.trim().to_string()
}

/// Install exactly the catalog's CuaDriver release. Any other installed
/// version, older or newer, is replaced; success means `cua-driver --version`
/// reports the pin afterwards.
pub(crate) async fn install(release: &CuaDriverRelease) -> Result<String, String> {
    let version = release.version.as_str();
    if let Some(binary) = runtime_core::cua::driver_binary() {
        if installed_version(&binary).await.as_deref() == Some(version) {
            return Ok(format!(
                "CuaDriver {version} is already installed; no download was needed."
            ));
        }
    }
    let files = installer_files(std::env::consts::OS, release)?;
    let entry = files
        .first()
        .ok_or_else(|| "The CuaDriver pin lists no installer".to_string())?;
    // Fetch and verify every script before anything is written or run.
    let mut verified = Vec::with_capacity(files.len());
    for file in files {
        let bytes = download_installer(&file.url).await?;
        verify_installer(file, version, &bytes)?;
        verified.push((file.name.as_str(), bytes));
    }
    // The helpers sit beside the entry script in one private folder, so the
    // official installer uses them from disk instead of fetching cua.ai's
    // moving copies.
    let directory = tempfile::tempdir()
        .map_err(|error| format!("Could not create a temporary CuaDriver folder: {error}"))?;
    for (name, bytes) in verified {
        let bytes = if cfg!(target_os = "windows") && name == entry.name {
            let script = String::from_utf8(bytes)
                .map_err(|_| "Pinned install.ps1 is not UTF-8 text".to_string())?;
            // Keep a BOM so Windows PowerShell reads the rewrite as UTF-8.
            format!(
                "\u{feff}{}",
                anchor_windows_module(&script, directory.path())?
            )
            .into_bytes()
        } else {
            bytes
        };
        tokio::fs::write(directory.path().join(name), bytes)
            .await
            .map_err(|error| format!("Could not save the CuaDriver installer: {error}"))?;
    }
    let path = directory.path().join(&entry.name);

    let mut command = installer_command(&path)?;
    command
        // The installer's own exact pin: without it, it resolves the latest
        // release. The program path is absolute and PATH is inherited as-is.
        .env("CUA_DRIVER_RS_VERSION", version)
        .env_remove("CUA_DRIVER_VERSION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(300), command.output())
        .await
        .map_err(|_| "CuaDriver installation timed out after five minutes".to_string())?
        .map_err(|error| format!("Could not run the CuaDriver installer: {error}"))?;
    let report = bounded_output(&output.stdout, &output.stderr);
    if !output.status.success() {
        return Err(if report.is_empty() {
            format!("CuaDriver installer exited with {}", output.status)
        } else {
            format!("CuaDriver installer failed: {report}")
        });
    }
    let binary = runtime_core::cua::driver_binary().ok_or_else(|| {
        "CuaDriver installer completed, but Desktop could not find cua-driver".to_string()
    })?;
    let installed = installed_version(&binary).await;
    if installed.as_deref() != Some(version) {
        return Err(format!(
            "{} reports {} after install; Magician requires CuaDriver {version}. Remove the other copy or set MAGICIAN_CUA_DRIVER_BIN to the pinned binary.",
            binary.display(),
            installed.as_deref().unwrap_or("no version")
        ));
    }
    Ok(if report.is_empty() {
        format!("CuaDriver {version} installed successfully.")
    } else {
        report
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> CuaDriverRelease {
        let file = |name: &str| CuaDriverInstallerFile {
            name: name.to_string(),
            url: format!(
                "https://github.com/trycua/cua/releases/download/cua-driver-rs-v1.2.3/{name}"
            ),
            sha256: sha256_hex(name.as_bytes()),
        };
        CuaDriverRelease {
            version: "1.2.3".to_string(),
            unix_installer: vec![file("install.sh"), file("_install-rust.sh")],
            windows_installer: vec![file("install.ps1")],
        }
    }

    #[test]
    fn selects_the_installer_for_the_desktop_os() {
        let release = release();
        assert_eq!(
            installer_files("macos", &release).unwrap()[0].name,
            "install.sh"
        );
        assert_eq!(installer_files("linux", &release).unwrap().len(), 2);
        assert_eq!(
            installer_files("windows", &release).unwrap()[0].name,
            "install.ps1"
        );
        assert!(installer_files("freebsd", &release).is_err());
    }

    #[test]
    fn the_desktop_installs_the_catalog_pin() {
        let pin = magician_components::setup::cua_driver_release().expect("catalog pin");
        for file in pin.unix_installer.iter().chain(&pin.windows_installer) {
            installer_url(&file.url).expect("pinned GitHub source");
        }
    }

    #[test]
    fn installers_come_only_from_pinned_github_sources() {
        assert!(installer_url("https://cua.ai/driver/install.sh").is_err());
        assert!(
            installer_url("http://github.com/trycua/cua/releases/download/x/install.sh").is_err()
        );
        assert!(
            installer_url("https://github.com/someone/cua/releases/download/x/install.sh").is_err()
        );
        assert!(installer_url(
            "https://raw.githubusercontent.com/trycua/cua/cua-driver-rs-v1.2.3/libs/cua-driver/scripts/_install-common.sh"
        )
        .is_ok());
    }

    #[test]
    fn tampered_installer_bytes_are_refused() {
        let release = release();
        let entry = &release.unix_installer[0];
        assert!(verify_installer(entry, "1.2.3", b"install.sh").is_ok());
        let error = verify_installer(entry, "1.2.3", b"install.sh; curl evil").unwrap_err();
        assert!(error.contains("pinned 1.2.3 hash"), "{error}");
    }

    #[test]
    fn windows_installer_loads_the_verified_module() {
        let folder = Path::new("C:/Temp/it's here");
        let script = anchor_windows_module(
            "\u{feff}Import-Bootstrap -LocalDir $PSScriptRoot -Url x",
            folder,
        )
        .unwrap();
        assert!(!script.contains("$PSScriptRoot"));
        assert!(script.contains("-LocalDir 'C:/Temp/it''s here'"));
        assert!(anchor_windows_module("Import-Bootstrap -Url x", folder).is_err());
    }

    #[test]
    fn driver_version_is_read_from_the_version_line() {
        assert_eq!(
            parse_driver_version("cua-driver 0.28.2\n").as_deref(),
            Some("0.28.2")
        );
        assert_eq!(
            parse_driver_version("cua-driver 0.28.5-nightly.20260925.36094786616").as_deref(),
            Some("0.28.5-nightly.20260925.36094786616")
        );
        assert_eq!(parse_driver_version("v0.1.9").as_deref(), Some("0.1.9"));
        assert_eq!(parse_driver_version("cua-driver"), None);
    }

    #[test]
    fn installer_output_is_bounded() {
        let output = bounded_output(&vec![b'a'; MAX_OUTPUT_BYTES + 10], b"failure");
        assert!(output.ends_with("[installer output truncated]"));
        assert!(output.len() < MAX_OUTPUT_BYTES + 64);
    }
}
