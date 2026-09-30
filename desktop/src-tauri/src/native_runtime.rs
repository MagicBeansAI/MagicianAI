//! Desktop-owned native Magician backend installation and lifecycle.
//!
//! The artifact contract is shared with `make package-release`: Desktop does
//! not invent another package layout or checksum format. A local development
//! package is supplied through `MAGICIAN_PACKAGE`; releases can place the same
//! directory or tarball in the Tauri `native-backend` resource directory.

use flate2::read::GzDecoder;
use ring::digest::{Context as DigestContext, SHA256};
use serde::Deserialize;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use tar::Archive;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

pub const RUNTIME_NAME: &str = "Native services";
pub const SERVICE_ID: &str = "ai.magicbeans.magican.backend";
const PACKAGE_ENV: &str = "MAGICIAN_PACKAGE";
#[cfg(target_os = "windows")]
const WINDOWS_TASK_NAME: &str = "Magican Backend";

#[derive(Debug, Clone, Deserialize)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub target: String,
    #[serde(default)]
    pub signing: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PackageSource {
    Directory(PathBuf),
    Archive(PathBuf),
    Installed(PathBuf),
}

impl PackageSource {
    pub fn display(&self) -> String {
        match self {
            Self::Directory(path) | Self::Archive(path) | Self::Installed(path) => {
                path.display().to_string()
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Availability {
    pub source: Option<PackageSource>,
    pub reason: Option<String>,
}

impl Availability {
    pub fn available(&self) -> bool {
        self.source.is_some()
    }
}

pub struct PreparedPackage {
    root: PathBuf,
    _temporary: Option<tempfile::TempDir>,
    pub manifest: PackageManifest,
}

pub fn install_prefix() -> PathBuf {
    std::env::var_os("MAGICIAN_PREFIX")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".magician")))
        .unwrap_or_else(|| PathBuf::from(".magician"))
}

pub fn install_prefix_pre_exists() -> bool {
    let prefix = install_prefix();
    if !prefix.exists() {
        return false;
    }
    if prefix.is_file() {
        return true;
    }
    fs::read_dir(prefix)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(true)
}

pub fn service_registration_exists() -> bool {
    #[cfg(target_os = "macos")]
    {
        return macos_service_path().is_ok_and(|path| path.is_file());
    }
    #[cfg(target_os = "linux")]
    {
        return linux_service_path().is_ok_and(|path| path.is_file());
    }
    #[cfg(target_os = "windows")]
    {
        return windows_task_exists();
    }
}

pub fn platform_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Err(
            "Magician needs a Mac with Apple Silicon (M1 or later) on macOS 14 or newer; Intel Macs are not supported"
                .to_string(),
        ),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!(
            "Native Magician packages are not published for {os}/{arch} yet"
        )),
    }
}

pub fn availability(app: &AppHandle) -> Availability {
    let target = match platform_target() {
        Ok(target) => target,
        Err(reason) => {
            return Availability {
                source: None,
                reason: Some(reason),
            };
        },
    };

    if let Some(raw) = std::env::var_os(PACKAGE_ENV).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(raw);
        return if path.is_dir() {
            Availability {
                source: Some(PackageSource::Directory(path)),
                reason: None,
            }
        } else if path.is_file() {
            Availability {
                source: Some(PackageSource::Archive(path)),
                reason: None,
            }
        } else {
            Availability {
                source: None,
                reason: Some(format!(
                    "{PACKAGE_ENV} points to {}, but that package does not exist",
                    path.display()
                )),
            }
        };
    }

    if let Ok(resource_dir) = app.path().resource_dir() {
        let bundled = resource_dir.join("native-backend");
        if bundled.join("MANIFEST.yaml").is_file() {
            return Availability {
                source: Some(PackageSource::Directory(bundled)),
                reason: None,
            };
        }
        if let Some(archive) = matching_archive(&bundled, target) {
            return Availability {
                source: Some(PackageSource::Archive(archive)),
                reason: None,
            };
        }
    }

    let installed = install_prefix();
    if installed.join("MANIFEST.yaml").is_file() && required_binaries_exist(&installed) {
        return Availability {
            source: Some(PackageSource::Installed(installed)),
            reason: None,
        };
    }

    Availability {
        source: None,
        reason: Some(format!(
            "No native backend package for {target} is bundled. For a local development package, run `make package-release` and launch Desktop with {PACKAGE_ENV}=<package>."
        )),
    }
}

fn matching_archive(dir: &Path, target: &str) -> Option<PathBuf> {
    let mut archives = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("-{target}.tar.gz")))
        })
        .collect::<Vec<_>>();
    archives.sort();
    archives.pop()
}

pub fn installed_by_desktop() -> bool {
    crate::manifest::read_manifest().is_some_and(|manifest| manifest.runtime == RUNTIME_NAME)
        && install_prefix().join("MANIFEST.yaml").is_file()
        && required_binaries_exist(&install_prefix())
}

pub fn package_summary(source: &PackageSource) -> Result<(String, bool), String> {
    match source {
        PackageSource::Installed(root) => {
            let manifest = read_package_manifest(root)?;
            validate_installed_dir(root, &manifest)?;
            Ok((manifest.version, true))
        },
        PackageSource::Directory(root) => {
            let manifest = read_package_manifest(root)?;
            validate_package_dir(root, &manifest)?;
            Ok((manifest.version, install_prefix_pre_exists()))
        },
        PackageSource::Archive(path) => {
            let prepared = prepare_archive(path)?;
            validate_package_dir(&prepared.root, &prepared.manifest)?;
            Ok((prepared.manifest.version, install_prefix_pre_exists()))
        },
    }
}

pub async fn install_package(source: PackageSource) -> Result<PackageManifest, String> {
    let prepared = tokio::task::spawn_blocking(move || prepare_package(source))
        .await
        .map_err(|error| format!("Native package preparation task failed: {error}"))??;

    #[cfg(target_os = "windows")]
    if prepared.root != install_prefix() && service_registration_exists() {
        stop_service().await?;
    }
    if prepared.root != install_prefix() {
        run_package_installer(&prepared.root, &install_prefix()).await?;
    }
    Ok(prepared.manifest)
}

pub async fn prepare_runtime_and_start(runtime_root: &Path) -> Result<(), String> {
    let prefix = install_prefix();
    let manifest = read_package_manifest(&prefix)?;
    validate_installed_dir(&prefix, &manifest)?;
    seed_runtime_root(&prefix, runtime_root)?;
    if service_registration_exists() {
        ensure_service_started().await
    } else {
        register_and_start_service(&prefix, runtime_root).await
    }
}

fn prepare_package(source: PackageSource) -> Result<PreparedPackage, String> {
    match source {
        PackageSource::Directory(root) => {
            let manifest = read_package_manifest(&root)?;
            validate_package_dir(&root, &manifest)?;
            Ok(PreparedPackage {
                root,
                _temporary: None,
                manifest,
            })
        },
        PackageSource::Installed(root) => {
            let manifest = read_package_manifest(&root)?;
            validate_installed_dir(&root, &manifest)?;
            Ok(PreparedPackage {
                root,
                _temporary: None,
                manifest,
            })
        },
        PackageSource::Archive(path) => prepare_archive(&path),
    }
}

fn prepare_archive(path: &Path) -> Result<PreparedPackage, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("Could not open native package {}: {error}", path.display()))?;
    let temporary = tempfile::tempdir()
        .map_err(|error| format!("Could not create native package staging directory: {error}"))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    for item in archive
        .entries()
        .map_err(|error| format!("Could not read native package {}: {error}", path.display()))?
    {
        let mut item = item.map_err(|error| format!("Could not read package entry: {error}"))?;
        let entry_type = item.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err("Native package contains an unsupported link or special file".to_string());
        }
        let relative = item
            .path()
            .map_err(|error| format!("Native package has an invalid path: {error}"))?;
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return Err("Native package contains a path outside its package root".to_string());
        }
        let unpacked = item
            .unpack_in(temporary.path())
            .map_err(|error| format!("Could not unpack native package: {error}"))?;
        if !unpacked {
            return Err("Native package tried to write outside its staging directory".to_string());
        }
    }

    let mut roots = fs::read_dir(temporary.path())
        .map_err(|error| format!("Could not inspect native package: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| entry.is_dir() && entry.join("MANIFEST.yaml").is_file())
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err(
            "Native package must contain exactly one Magician package directory".to_string(),
        );
    }
    let root = roots.remove(0);
    let manifest = read_package_manifest(&root)?;
    validate_package_dir(&root, &manifest)?;
    Ok(PreparedPackage {
        root,
        _temporary: Some(temporary),
        manifest,
    })
}

fn read_package_manifest(root: &Path) -> Result<PackageManifest, String> {
    let path = root.join("MANIFEST.yaml");
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    serde_yaml::from_str(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))
}

fn validate_package_manifest(manifest: &PackageManifest) -> Result<(), String> {
    if manifest.name != "magician" {
        return Err(format!(
            "Native package identifies itself as '{}', not 'magician'",
            manifest.name
        ));
    }
    let expected = platform_target()?;
    if manifest.target != expected {
        return Err(format!(
            "Native package target '{}' does not match this computer ({expected})",
            manifest.target
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err("Native package has no version".to_string());
    }
    if !matches!(
        manifest.signing.as_deref(),
        None | Some("none" | "adhoc" | "developer-id")
    ) {
        return Err(format!(
            "Native package has an unknown signing declaration: {}",
            manifest.signing.as_deref().unwrap_or_default()
        ));
    }
    Ok(())
}

fn validate_package_dir(root: &Path, manifest: &PackageManifest) -> Result<(), String> {
    validate_package_manifest(manifest)?;
    let mut required = vec!["tool-runtime-config.yaml", "MANIFEST.yaml", "SHA256SUMS"];
    required.extend(binary_names_for_target(&manifest.target));
    required.push(installer_name_for_target(&manifest.target));
    for required in required {
        if !root.join(required).is_file() {
            return Err(format!(
                "Native package is incomplete: {} is missing",
                root.join(required).display()
            ));
        }
    }
    verify_package_checksums(root)?;
    Ok(())
}

fn validate_installed_dir(root: &Path, manifest: &PackageManifest) -> Result<(), String> {
    validate_package_manifest(manifest)?;
    let mut required = vec!["tool-runtime-config.yaml", "MANIFEST.yaml"];
    required.extend(binary_names_for_target(&manifest.target));
    for required in required {
        if !root.join(required).is_file() {
            return Err(format!(
                "Installed native backend is incomplete: {} is missing",
                root.join(required).display()
            ));
        }
    }
    Ok(())
}

fn required_binaries_exist(root: &Path) -> bool {
    platform_target().is_ok_and(|target| {
        binary_names_for_target(target)
            .iter()
            .all(|name| root.join(name).is_file())
    })
}

fn binary_names_for_target(target: &str) -> &'static [&'static str; 3] {
    if target.contains("-windows-") {
        &["magician.exe", "magicutor.exe", "magic-supervisor.exe"]
    } else {
        &["magician.bin", "magicutor.bin", "magic-supervisor.bin"]
    }
}

fn installer_name_for_target(target: &str) -> &'static str {
    if target.contains("-windows-") {
        "install.ps1"
    } else {
        "install.sh"
    }
}

fn verify_package_checksums(root: &Path) -> Result<(), String> {
    let checksum_path = root.join("SHA256SUMS");
    let checksums = fs::read_to_string(&checksum_path)
        .map_err(|error| format!("Could not read {}: {error}", checksum_path.display()))?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("Could not resolve package root {}: {error}", root.display()))?;
    for (index, line) in checksums.lines().enumerate() {
        let mut fields = line.splitn(2, char::is_whitespace);
        let expected = fields.next().unwrap_or_default();
        let relative = fields
            .next()
            .map(str::trim)
            .map(|value| value.strip_prefix('*').unwrap_or(value))
            .map(|value| value.strip_prefix("./").unwrap_or(value))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Malformed SHA256SUMS entry on line {}", index + 1))?;
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "Malformed SHA256 digest on line {} of {}",
                index + 1,
                checksum_path.display()
            ));
        }
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(format!(
                "Checksum path escapes the package root: {relative}"
            ));
        }
        let path = root.join(relative_path);
        let canonical = fs::canonicalize(&path).map_err(|error| {
            format!(
                "Package checksum file {} is missing: {error}",
                path.display()
            )
        })?;
        if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
            return Err(format!("Checksum path is not a package file: {relative}"));
        }
        let mut file = fs::File::open(&canonical)
            .map_err(|error| format!("Could not verify {}: {error}", canonical.display()))?;
        let mut context = DigestContext::new(&SHA256);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("Could not verify {}: {error}", canonical.display()))?;
            if count == 0 {
                break;
            }
            context.update(&buffer[..count]);
        }
        let actual = context
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("Native package checksum mismatch: {relative}"));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
async fn run_package_installer(root: &Path, prefix: &Path) -> Result<(), String> {
    let output = Command::new("bash")
        .arg(root.join("install.sh"))
        .arg("--yes")
        .arg("--prefix")
        .arg(prefix)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| format!("Could not run the native package installer: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "Native package installer failed: {}",
        stderr
            .lines()
            .chain(stdout.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("unknown installer error")
    ))
}

#[cfg(target_os = "windows")]
async fn run_package_installer(root: &Path, prefix: &Path) -> Result<(), String> {
    let root = root.to_path_buf();
    let prefix = prefix.to_path_buf();
    tokio::task::spawn_blocking(move || install_windows_package(&root, &prefix))
        .await
        .map_err(|error| format!("Native package installation task failed: {error}"))?
}

#[cfg(target_os = "windows")]
fn install_windows_package(root: &Path, prefix: &Path) -> Result<(), String> {
    fs::create_dir_all(prefix)
        .map_err(|error| format!("Could not create {}: {error}", prefix.display()))?;
    for directory in ["scripts", "share"] {
        let source = root.join(directory);
        if !source.is_dir() {
            continue;
        }
        let destination = prefix.join(directory);
        match fs::remove_dir_all(&destination) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(format!(
                    "Could not replace {}: {error}",
                    destination.display()
                ))
            },
        }
        copy_directory(&source, &destination)?;
    }
    for file in [
        "tool-runtime-config.yaml",
        "MANIFEST.yaml",
        "SHA256SUMS",
        "install.ps1",
        "uninstall.ps1",
    ] {
        let source = root.join(file);
        if source.is_file() {
            fs::copy(&source, prefix.join(file))
                .map_err(|error| format!("Could not install {}: {error}", source.display()))?;
        }
    }
    for binary in binary_names_for_target("x86_64-pc-windows-msvc") {
        let source = root.join(binary);
        fs::copy(&source, prefix.join(binary))
            .map_err(|error| format!("Could not install {}: {error}", source.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?
    {
        let entry = entry.map_err(|error| format!("Could not read package entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("Could not copy {}: {error}", target.display()))?;
        } else {
            return Err(format!(
                "Native package contains an unsupported file: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn seed_runtime_root(prefix: &Path, runtime_root: &Path) -> Result<(), String> {
    fs::create_dir_all(runtime_root)
        .map_err(|error| format!("Could not create {}: {error}", runtime_root.display()))?;
    crate::container::seed_runtime_config_if_missing(runtime_root)?;
    let seed = prefix.join("share/seed");
    for (source_name, destination_name) in [
        (".env.example", ".env"),
        ("operator-config.template.yaml", "operator-config.yaml"),
    ] {
        let source = seed.join(source_name);
        let destination = runtime_root.join(destination_name);
        if source.is_file() && !destination.exists() {
            fs::copy(&source, &destination).map_err(|error| {
                format!(
                    "Could not seed {} from {}: {error}",
                    destination.display(),
                    source.display()
                )
            })?;
        }
    }
    fs::create_dir_all(runtime_root.join("logs"))
        .map_err(|error| format!("Could not create native runtime logs: {error}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn register_and_start_service(prefix: &Path, runtime_root: &Path) -> Result<(), String> {
    let service_path = macos_service_path()?;
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    fs::write(&service_path, render_macos_service(prefix, runtime_root))
        .map_err(|error| format!("Could not write {}: {error}", service_path.display()))?;
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let target = format!("{domain}/{SERVICE_ID}");
    let _ = Command::new("launchctl")
        .args(["bootout", &target])
        .output()
        .await;
    command_success(
        Command::new("launchctl").args(["enable", &target]),
        "enable the Magician backend LaunchAgent",
    )
    .await?;
    command_success(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(&service_path),
        "register the Magician backend LaunchAgent",
    )
    .await?;
    command_success(
        Command::new("launchctl").args(["kickstart", "-k", &target]),
        "start the Magician backend LaunchAgent",
    )
    .await
}

#[cfg(target_os = "linux")]
async fn register_and_start_service(prefix: &Path, runtime_root: &Path) -> Result<(), String> {
    let service_path = linux_service_path()?;
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    fs::write(&service_path, render_linux_service(prefix, runtime_root))
        .map_err(|error| format!("Could not write {}: {error}", service_path.display()))?;
    command_success(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "reload the user service manager",
    )
    .await?;
    command_success(
        Command::new("systemctl").args(["--user", "enable", "--now", "magician-backend.service"]),
        "enable and start the Magician backend service",
    )
    .await
}

#[cfg(target_os = "windows")]
async fn register_and_start_service(prefix: &Path, runtime_root: &Path) -> Result<(), String> {
    let launcher = windows_launcher_path(prefix);
    fs::write(&launcher, render_windows_launcher(prefix, runtime_root))
        .map_err(|error| format!("Could not write {}: {error}", launcher.display()))?;
    let powershell = windows_powershell_path()?;
    let task_command = format!(
        "\"{}\" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\"",
        powershell.display(),
        launcher.display()
    );
    command_success(
        hidden_windows_command(windows_schtasks_path()?).args([
            "/Create",
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/TN",
            WINDOWS_TASK_NAME,
            "/TR",
            &task_command,
            "/F",
        ]),
        "register the Magician backend logon task",
    )
    .await?;
    ensure_service_started().await
}

pub async fn ensure_service_started() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        let target = format!("{domain}/{SERVICE_ID}");
        command_success(
            Command::new("launchctl").args(["enable", &target]),
            "enable the installed Magician backend",
        )
        .await?;
        let kicked = Command::new("launchctl")
            .args(["kickstart", &target])
            .output()
            .await
            .map_err(|error| format!("Could not start the installed Magician backend: {error}"))?;
        if kicked.status.success() {
            return Ok(());
        }
        let service_path = macos_service_path()?;
        command_success(
            Command::new("launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(&service_path),
            "register the installed Magician backend",
        )
        .await?;
        return command_success(
            Command::new("launchctl").args(["kickstart", &target]),
            "start the installed Magician backend",
        )
        .await;
    }
    #[cfg(target_os = "linux")]
    {
        return command_success(
            Command::new("systemctl").args(["--user", "start", "magician-backend.service"]),
            "start the installed Magician backend",
        )
        .await;
    }
    #[cfg(target_os = "windows")]
    {
        let supervisor = install_prefix().join("magic-supervisor.exe");
        if supervisor.is_file()
            && hidden_windows_command(supervisor.clone())
                .args(["client", "health"])
                .output()
                .await
                .is_ok_and(|output| output.status.success())
        {
            return Ok(());
        }
        command_success(
            hidden_windows_command(windows_schtasks_path()?).args([
                "/Run",
                "/TN",
                WINDOWS_TASK_NAME,
            ]),
            "start the Magician backend logon task",
        )
        .await
    }
}

pub async fn dispatch(action: &str) -> Result<(), String> {
    match action {
        "restart-magician" | "restart-magicutor" | "stop-magician" | "stop-magicutor" => {
            command_success(
                native_command(install_prefix().join(if cfg!(target_os = "windows") {
                    "magic-supervisor.exe"
                } else {
                    "magic-supervisor.bin"
                }))
                .arg("client")
                .arg(action),
                &format!("send {action} to the native supervisor"),
            )
            .await
        },
        "restart-supervisor" => restart_service().await,
        "stop-supervisor" => stop_service().await,
        _ => Err(format!("Unknown native service action: {action}")),
    }
}

pub async fn restart_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, SERVICE_ID);
        return command_success(
            Command::new("launchctl").args(["kickstart", "-k", &target]),
            "restart the native Magician backend",
        )
        .await;
    }
    #[cfg(target_os = "linux")]
    {
        return command_success(
            Command::new("systemctl").args(["--user", "restart", "magician-backend.service"]),
            "restart the native Magician backend",
        )
        .await;
    }
    #[cfg(target_os = "windows")]
    {
        stop_service().await?;
        ensure_service_started().await
    }
}

pub async fn stop_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, SERVICE_ID);
        let _ = Command::new("launchctl")
            .args(["disable", &target])
            .output()
            .await;
        let output = Command::new("launchctl")
            .args(["bootout", &target])
            .output()
            .await
            .map_err(|error| format!("Could not stop the native Magician backend: {error}"))?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("Could not find service")
        {
            return Ok(());
        }
        return Err(format!(
            "Could not stop the native Magician backend: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    #[cfg(target_os = "linux")]
    {
        return command_success(
            Command::new("systemctl").args(["--user", "stop", "magician-backend.service"]),
            "stop the native Magician backend",
        )
        .await;
    }
    #[cfg(target_os = "windows")]
    {
        let supervisor = install_prefix().join("magic-supervisor.exe");
        if supervisor.is_file() {
            let _ = hidden_windows_command(supervisor)
                .args(["client", "shutdown"])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if service_registration_exists() {
            let _ = hidden_windows_command(windows_schtasks_path()?)
                .args(["/End", "/TN", WINDOWS_TASK_NAME])
                .output()
                .await;
        }
        Ok(())
    }
}

pub async fn unregister_service() -> Result<(), String> {
    let _ = stop_service().await;
    #[cfg(target_os = "windows")]
    {
        if service_registration_exists() {
            command_success(
                hidden_windows_command(windows_schtasks_path()?).args([
                    "/Delete",
                    "/TN",
                    WINDOWS_TASK_NAME,
                    "/F",
                ]),
                "remove the Magician backend logon task",
            )
            .await?;
        }
        let launcher = windows_launcher_path(&install_prefix());
        match fs::remove_file(&launcher) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("Could not remove {}: {error}", launcher.display())),
        }
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let path = macos_service_path()?;
        #[cfg(target_os = "linux")]
        let path = linux_service_path()?;

        match fs::remove_file(&path) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("Could not remove {}: {error}", path.display())),
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output()
                .await;
        }
        Ok(())
    }
}

async fn command_success(command: &mut Command, purpose: &str) -> Result<(), String> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| format!("Could not {purpose}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = stderr
            .lines()
            .chain(stdout.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("unknown operating-system error");
        Err(format!("Could not {purpose}: {}", detail.trim()))
    }
}

#[cfg(target_os = "macos")]
fn macos_service_path() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| {
            home.join("Library/LaunchAgents")
                .join(format!("{SERVICE_ID}.plist"))
        })
        .ok_or_else(|| "Could not locate the home directory for LaunchAgents".to_string())
}

#[cfg(target_os = "linux")]
fn linux_service_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|config| config.join("systemd/user/magician-backend.service"))
        .ok_or_else(|| "Could not locate the user configuration directory".to_string())
}

#[cfg(target_os = "windows")]
fn windows_system_root() -> Result<PathBuf, String> {
    std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Windows did not provide SystemRoot".to_string())
}

#[cfg(target_os = "windows")]
fn windows_schtasks_path() -> Result<PathBuf, String> {
    Ok(windows_system_root()?.join("System32/schtasks.exe"))
}

#[cfg(target_os = "windows")]
fn windows_powershell_path() -> Result<PathBuf, String> {
    let path = windows_system_root()?.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "Windows PowerShell is missing at {}",
            path.display()
        ))
    }
}

#[cfg(target_os = "windows")]
fn hidden_windows_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new(program);
    command.as_std_mut().creation_flags(0x0800_0000);
    command
}

#[cfg(target_os = "windows")]
fn native_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    hidden_windows_command(program)
}

#[cfg(not(target_os = "windows"))]
fn native_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
}

#[cfg(target_os = "windows")]
fn windows_task_exists() -> bool {
    windows_schtasks_path().is_ok_and(|program| {
        let mut command = std::process::Command::new(program);
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
        command
            .args(["/Query", "/TN", WINDOWS_TASK_NAME])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

#[cfg(target_os = "windows")]
fn windows_launcher_path(prefix: &Path) -> PathBuf {
    prefix.join("start-native.ps1")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn service_path_env(prefix: &Path) -> String {
    let mut paths = Vec::<PathBuf>::new();
    let mut push = |path: PathBuf| {
        let rendered = path.to_string_lossy();
        if !path.as_os_str().is_empty()
            && !rendered.contains('\n')
            && !rendered.contains('\r')
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    };
    push(prefix.to_path_buf());
    if let Some(home) = dirs::home_dir() {
        push(home.join(".local/bin"));
        push(home.join(".cargo/bin"));
    }
    if cfg!(target_os = "macos") {
        push(PathBuf::from("/opt/homebrew/bin"));
        push(PathBuf::from("/usr/local/bin"));
    }
    if let Some(current) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&current) {
            push(path);
        }
    }
    if !cfg!(target_os = "windows") {
        for path in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            push(PathBuf::from(path));
        }
    }
    std::env::join_paths(paths)
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
        .to_string_lossy()
        .into_owned()
}

#[cfg(any(target_os = "windows", test))]
fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(any(target_os = "windows", test))]
fn render_windows_launcher(prefix: &Path, runtime_root: &Path) -> String {
    let prefix_text = powershell_single_quote(&prefix.display().to_string());
    let runtime = powershell_single_quote(&runtime_root.display().to_string());
    let executable =
        powershell_single_quote(&prefix.join("magic-supervisor.exe").display().to_string());
    let path = powershell_single_quote(&service_path_env(prefix));
    let log = powershell_single_quote(
        &runtime_root
            .join("logs/native-supervisor.log")
            .display()
            .to_string(),
    );
    format!(
        "$ErrorActionPreference = 'Stop'\n\
$env:MAGICIAN_ROOT_DIR = '{runtime}'\n\
$env:MAGICIAN_HOST_GATEWAY_URL = 'http://127.0.0.1:3017'\n\
$env:PATH = '{path}'\n\
Set-Location -LiteralPath '{prefix_text}'\n\
& '{executable}' *>> '{log}'\n\
exit $LASTEXITCODE\n"
    )
}

fn render_macos_service(prefix: &Path, runtime_root: &Path) -> String {
    let executable = xml_escape(&prefix.join("magic-supervisor.bin").display().to_string());
    let working = xml_escape(&prefix.display().to_string());
    let runtime = xml_escape(&runtime_root.display().to_string());
    let path = xml_escape(&service_path_env(prefix));
    let log = xml_escape(
        &runtime_root
            .join("logs/native-supervisor.log")
            .display()
            .to_string(),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{SERVICE_ID}</string>
  <key>ProgramArguments</key><array><string>{executable}</string></array>
  <key>WorkingDirectory</key><string>{working}</string>
  <key>EnvironmentVariables</key><dict>
    <key>MAGICIAN_ROOT_DIR</key><string>{runtime}</string>
    <key>MAGICIAN_HOST_GATEWAY_URL</key><string>http://127.0.0.1:3017</string>
    <key>PATH</key><string>{path}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#
    )
}

#[cfg(any(target_os = "linux", test))]
fn systemd_escape(value: &Path) -> String {
    value
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(any(target_os = "linux", test))]
fn render_linux_service(prefix: &Path, runtime_root: &Path) -> String {
    let executable = systemd_escape(&prefix.join("magic-supervisor.bin"));
    let working = systemd_escape(prefix);
    let runtime = systemd_escape(runtime_root);
    let path = service_path_env(prefix)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "[Unit]\nDescription=Magician backend\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nWorkingDirectory=\"{working}\"\nExecStart=\"{executable}\"\nEnvironment=\"MAGICIAN_ROOT_DIR={runtime}\"\nEnvironment=\"MAGICIAN_HOST_GATEWAY_URL=http://127.0.0.1:3017\"\nEnvironment=\"PATH={path}\"\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manifest_must_match_the_current_target() {
        let manifest = PackageManifest {
            name: "magician".to_string(),
            version: "1.2.3".to_string(),
            target: platform_target().unwrap().to_string(),
            signing: None,
        };
        validate_package_manifest(&manifest).unwrap();

        let mut wrong = manifest;
        wrong.target = "definitely-another-target".to_string();
        assert!(validate_package_manifest(&wrong)
            .unwrap_err()
            .contains("does not match this computer"));
    }

    #[test]
    fn service_specs_keep_paths_and_runtime_ownership_explicit() {
        let prefix = Path::new("/Users/Test Person/.magician");
        let root = Path::new("/Users/Test Person/MagicianNotes");
        let macos = render_macos_service(prefix, root);
        assert!(macos.contains(SERVICE_ID));
        assert!(macos.contains("magic-supervisor.bin"));
        assert!(macos.contains("MAGICIAN_ROOT_DIR"));
        let linux = render_linux_service(prefix, root);
        assert!(linux.contains("[Service]"));
        assert!(linux.contains("Restart=on-failure"));
        assert!(linux.contains("MAGICIAN_HOST_GATEWAY_URL"));
        let windows = render_windows_launcher(
            Path::new(r"C:\Users\Test Person\.magician"),
            Path::new(r"C:\Users\Test Person\MagicianNotes"),
        );
        assert!(windows.contains("magic-supervisor.exe"));
        assert!(windows.contains("MAGICIAN_ROOT_DIR"));
        assert!(windows.contains("MAGICIAN_HOST_GATEWAY_URL"));
        assert!(windows.contains("native-supervisor.log"));
    }

    #[test]
    fn package_contract_uses_native_windows_executable_names() {
        assert_eq!(
            binary_names_for_target("x86_64-pc-windows-msvc"),
            &["magician.exe", "magicutor.exe", "magic-supervisor.exe"]
        );
        assert_eq!(
            binary_names_for_target("aarch64-apple-darwin"),
            &["magician.bin", "magicutor.bin", "magic-supervisor.bin"]
        );
        assert_eq!(
            installer_name_for_target("x86_64-pc-windows-msvc"),
            "install.ps1"
        );
    }

    #[test]
    fn archive_selection_is_target_exact() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path()
                .join("magician-1.0-x86_64-unknown-linux-gnu.tar.gz"),
            b"fixture",
        )
        .unwrap();
        fs::write(
            dir.path().join("magician-1.0-aarch64-apple-darwin.tar.gz"),
            b"fixture",
        )
        .unwrap();
        let selected = matching_archive(dir.path(), "aarch64-apple-darwin").unwrap();
        assert!(selected
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("aarch64-apple-darwin"));
    }
}
