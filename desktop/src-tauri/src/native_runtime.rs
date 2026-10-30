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
    Checkout(PathBuf),
}

impl PackageSource {
    pub fn display(&self) -> String {
        match self {
            Self::Directory(path)
            | Self::Archive(path)
            | Self::Installed(path)
            | Self::Checkout(path) => path.display().to_string(),
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
        macos_service_path().is_ok_and(|path| path.is_file())
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

    // A development Desktop can build its backend from the exact checkout.
    // A packaged Desktop with no checkout keeps using verified release artifacts.
    if cfg!(target_os = "macos") {
        if let Some(checkout) = source_checkout() {
            return Availability {
                source: Some(PackageSource::Checkout(checkout)),
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

fn source_checkout() -> Option<PathBuf> {
    let compiled = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf);
    [
        std::env::var_os("MAGICIAN_SOURCE_DIR").map(PathBuf::from),
        compiled,
    ]
    .into_iter()
    .flatten()
    .find(|path| magician_components::source_install::is_checkout(path))
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
        PackageSource::Checkout(root) => {
            let contents = fs::read_to_string(root.join("magician/Cargo.toml"))
                .map_err(|error| format!("Could not inspect source checkout: {error}"))?;
            let manifest: toml::Value = toml::from_str(&contents)
                .map_err(|error| format!("Could not parse source version: {error}"))?;
            let version = manifest
                .get("package")
                .and_then(|package| package.get("version"))
                .and_then(toml::Value::as_str)
                .ok_or("Source checkout has no package version")?;
            Ok((version.into(), install_prefix_pre_exists()))
        },
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

#[cfg(unix)]
struct SourceProcessGroup(Option<u32>);

#[cfg(unix)]
impl Drop for SourceProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.filter(|pid| *pid > 1) {
            // The installer is the leader of its own group. A timeout or task
            // cancellation must also stop its builds and package subprocesses.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

async fn prepare_source_install(
    plan: &magician_components::source_install::SourceInstall,
    timeout: std::time::Duration,
) -> Result<PackageManifest, String> {
    fs::create_dir_all(plan.data_root.join("logs"))
        .map_err(|error| format!("Could not prepare source build logs: {error}"))?;
    let log_path = plan.data_root.join("logs/source-install.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("Could not open source build log: {error}"))?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("Could not prepare source build log: {error}"))?;
    let mut command = Command::new("/bin/bash");
    command
        .args(plan.arguments(
            magician_components::source_install::Stage::Prepare,
            false,
            true,
        ))
        .current_dir(&plan.checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start source installation: {error}"))?;
    #[cfg(unix)]
    let mut group = SourceProcessGroup(child.id());
    let status = tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| format!("Source installation timed out; see {}", log_path.display()))?
        .map_err(|error| format!("Could not wait for source installation: {error}"))?;
    #[cfg(unix)]
    {
        group.0 = None;
    }
    if !status.success() {
        return Err(format!(
            "Source installation failed; see {}",
            log_path.display()
        ));
    }
    let manifest = read_package_manifest(&plan.prefix)?;
    validate_installed_dir(&plan.prefix, &manifest)?;
    Ok(manifest)
}

pub async fn install_package(
    source: PackageSource,
    runtime_root: &Path,
) -> Result<PackageManifest, String> {
    if let PackageSource::Checkout(checkout) = &source {
        let plan = magician_components::source_install::SourceInstall::new(
            checkout,
            runtime_root,
            &install_prefix(),
        )?;
        return prepare_source_install(&plan, std::time::Duration::from_secs(3600)).await;
    }
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
    #[cfg(target_os = "macos")]
    if prefix.join("scripts/native-runtime-service.py").is_file() {
        let script = prefix.join("scripts/native-runtime-service.py");
        let mut command = Command::new(runtime_core::process::resolve_program(
            std::ffi::OsStr::new("python3"),
            None,
        ));
        command
            .arg(&script)
            .args(["start", "--prefix"])
            .arg(&prefix)
            .arg("--data-dir")
            .arg(runtime_root);
        // An older installed package's script does not know the flag and
        // would fail argparse; register without supervisor arguments then.
        if script_supports(&script, "--supervisor-args-json") {
            command
                .arg("--supervisor-args-json")
                .arg(supervisor_args_json(runtime_root)?);
        }
        return command_success(
            &mut command,
            "register and start the native backend with the reviewed data folder",
        )
        .await;
    }
    if service_registration_exists() {
        ensure_service_started().await
    } else {
        register_and_start_service(&prefix, runtime_root).await
    }
}

fn prepare_package(source: PackageSource) -> Result<PreparedPackage, String> {
    match source {
        PackageSource::Checkout(_) => {
            Err("Source checkout must be built through the shared source installer".into())
        },
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
    // Same list as scripts/cleanup.sh's native_reseed_root; the decision
    // engine reads decision-engine.yaml, and native-runtime-service.py
    // `start` requires it. Never `.env.development` from `.env.example`: both
    // env loaders read it first and never overwrite, so its empty `KEY=`
    // lines would shadow the owner's keys in `.env`.
    for (source_name, destination_name) in [
        ("decision-engine.yaml", "decision-engine.yaml"),
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
    // Re-registering keeps what the existing registration records (merged by
    // the same rule as the launch-time sync).
    let candidate = current_supervisor_args(runtime_root);
    let args = fs::read_to_string(&service_path)
        .ok()
        .and_then(|existing| recorded_fallback_args(&existing, prefix, runtime_root))
        .map(|recorded| merge_with_this_app(&recorded, &candidate))
        .unwrap_or(candidate);
    fs::write(
        &service_path,
        render_macos_service(prefix, runtime_root, &args),
    )
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

/// `<X>.app` when `executable` runs from `<X>.app/Contents/MacOS/<bin>` at a
/// stable location. A translocated copy (Gatekeeper's randomized
/// `/private/var/folders/.../AppTranslocation/...`), a mounted image
/// (`/Volumes/...`) or a bare binary is not a place to relaunch from.
#[cfg(any(target_os = "macos", test))]
fn app_bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let text = bundle.to_string_lossy();
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        return None;
    }
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app"))
    .then(|| bundle.to_path_buf())
}

/// Whether an installed `native-runtime-service.py` knows `feature` (a flag
/// or action), so an older package's script is never handed one it rejects.
#[cfg(target_os = "macos")]
fn script_supports(script: &Path, feature: &str) -> bool {
    fs::read_to_string(script).is_ok_and(|text| text.contains(feature))
}

/// `--flag value`, `--flag=value` and bare `--flag` items, in order.
#[cfg(any(target_os = "macos", test))]
fn split_flags(args: &[String]) -> Vec<(String, Vec<String>)> {
    let mut items = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        let flag = token.split('=').next().unwrap_or_default().to_string();
        let paired = token.starts_with("--")
            && !token.contains('=')
            && args
                .get(index + 1)
                .is_some_and(|next| !next.starts_with("--"));
        if paired {
            items.push((flag, vec![token.clone(), args[index + 1].clone()]));
            index += 2;
        } else {
            items.push((flag, vec![token.clone()]));
            index += 1;
        }
    }
    items
}

/// An app installed in `/Applications` or `~/Applications`.
#[cfg(any(target_os = "macos", test))]
fn is_applications_bundle(bundle: &Path, home: Option<&Path>) -> bool {
    bundle.starts_with("/Applications")
        || home.is_some_and(|home| bundle.starts_with(home.join("Applications")))
}

/// The supervisor arguments to record, given the recorded ones and this
/// launch's candidate. The config path follows the candidate when it has one.
/// The candidate's `--desktop-app` (and log) wins when it runs from
/// `/Applications` or `~/Applications`, or when no existing bundle is
/// recorded; otherwise the recorded one is kept, so a worktree debug build or
/// a stale copy never steals the binding and an installed app reclaims it.
/// Every other recorded flag (a hand-added `--manage-desktop=0`, a future
/// flag) is kept. `scripts/native-runtime-service.py` applies the same rule.
#[cfg(any(target_os = "macos", test))]
fn merge_supervisor_args(
    recorded: &[String],
    candidate: &[String],
    bundle_exists: impl Fn(&Path) -> bool,
    preferred: impl Fn(&Path) -> bool,
) -> Vec<String> {
    let value = |args: &[String], flag: &str| {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].clone())
    };
    let config =
        value(candidate, "--magician-config").or_else(|| value(recorded, "--magician-config"));
    let recorded_bundle = value(recorded, "--desktop-app");
    let candidate_bundle = value(candidate, "--desktop-app");
    let candidate_wins = candidate_bundle.as_deref().is_some_and(|bundle| {
        preferred(Path::new(bundle))
            || recorded_bundle
                .as_deref()
                .is_none_or(|recorded| !bundle_exists(Path::new(recorded)))
    });
    let (bundle, log) = if candidate_wins {
        (candidate_bundle, value(candidate, "--desktop-log"))
    } else {
        (recorded_bundle, value(recorded, "--desktop-log"))
    };
    let mut merged = Vec::new();
    if let Some(config) = config {
        merged.extend(["--magician-config".to_string(), config]);
    }
    if let Some(bundle) = bundle {
        merged.extend(["--desktop-app".to_string(), bundle]);
        if let Some(log) = log {
            merged.extend(["--desktop-log".to_string(), log]);
        }
    }
    for (flag, tokens) in split_flags(recorded) {
        if !matches!(
            flag.as_str(),
            "--magician-config" | "--desktop-app" | "--desktop-log"
        ) {
            merged.extend(tokens);
        }
    }
    merged
}

/// The recorded supervisor arguments of the fallback plist, when it runs this
/// installation's `magic-supervisor.bin` for `runtime_root`.
#[cfg(target_os = "macos")]
fn recorded_fallback_args(
    existing: &str,
    prefix: &Path,
    runtime_root: &Path,
) -> Option<Vec<String>> {
    let bound_root = format!(
        "<key>MAGICIAN_ROOT_DIR</key><string>{}</string>",
        xml_escape(&runtime_root.display().to_string())
    );
    let (_, recorded) = plist_program_arguments(existing)?;
    let binary = prefix.join("magic-supervisor.bin").display().to_string();
    (existing.contains(&bound_root) && recorded.first() == Some(&binary))
        .then(|| recorded[1..].to_vec())
}

#[cfg(target_os = "macos")]
fn merge_with_this_app(recorded: &[String], candidate: &[String]) -> Vec<String> {
    let home = dirs::home_dir();
    merge_supervisor_args(
        recorded,
        candidate,
        |bundle| bundle.is_dir(),
        |bundle| is_applications_bundle(bundle, home.as_deref()),
    )
}

/// The `<string>` items of the plist's ProgramArguments array.
#[cfg(any(target_os = "macos", test))]
fn plist_program_arguments(plist: &str) -> Option<(String, Vec<String>)> {
    let after_key = plist.split_once("<key>ProgramArguments</key>")?.1;
    let array_start = after_key.find("<array>")?;
    let array_end = after_key.find("</array>")?;
    let array = &after_key[array_start..array_end + "</array>".len()];
    let items = array
        .split("<string>")
        .skip(1)
        .filter_map(|item| {
            item.split_once("</string>")
                .map(|(value, _)| xml_unescape(value))
        })
        .collect();
    Some((array.to_string(), items))
}

#[cfg(any(target_os = "macos", test))]
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// magic-supervisor arguments for the macOS LaunchAgent: the runtime config
/// it reads `supervisor.manage_desktop` from, and, when this app runs from a
/// bundle, that bundle and its stdout/stderr log, so the supervisor can
/// relaunch the app after a crash (never after its Quit). No preflight
/// script in production.
#[cfg(any(target_os = "macos", test))]
fn supervisor_args(runtime_root: &Path, bundle: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "--magician-config".to_string(),
        runtime_root
            .join("magician-config.yaml")
            .display()
            .to_string(),
    ];
    if let Some(bundle) = bundle {
        args.extend([
            "--desktop-app".to_string(),
            bundle.display().to_string(),
            "--desktop-log".to_string(),
            runtime_root.join("logs/desktop.log").display().to_string(),
        ]);
    }
    args
}

#[cfg(target_os = "macos")]
fn current_supervisor_args(runtime_root: &Path) -> Vec<String> {
    let bundle = std::env::current_exe()
        .ok()
        .map(|exe| fs::canonicalize(&exe).unwrap_or(exe))
        .and_then(|exe| app_bundle_of(&exe));
    supervisor_args(runtime_root, bundle.as_deref())
}

#[cfg(target_os = "macos")]
fn supervisor_args_json(runtime_root: &Path) -> Result<String, String> {
    serde_json::to_string(&current_supervisor_args(runtime_root))
        .map_err(|error| format!("Could not encode the supervisor arguments: {error}"))
}

/// macOS, on Desktop launch: record current supervisor arguments in an
/// existing LaunchAgent (the runtime config path; this bundle and its log
/// unless another existing bundle is already recorded). Only the plist file is
/// rewritten, never re-bootstrapped: the running backend is not restarted, and
/// launchd loads the new arguments at the next login or re-registration.
/// Skipped when this app runs from no stable bundle (translocated, a mounted
/// image, a bare binary), when the registration belongs to another
/// installation or data folder, or when the installed script predates the
/// action. Returns whether it was rewritten. Other platforms: no-op.
pub async fn sync_supervisor_arguments(runtime_root: &Path) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let candidate = current_supervisor_args(runtime_root);
        if !candidate.iter().any(|arg| arg == "--desktop-app") {
            return Ok(false);
        }
        let prefix = install_prefix();
        let script = prefix.join("scripts/native-runtime-service.py");
        if script.is_file() {
            if !script_supports(&script, "sync-supervisor-args") {
                static NOTED: std::sync::Once = std::sync::Once::new();
                NOTED.call_once(|| {
                    tracing::debug!(
                        "Installed native-runtime-service.py predates sync-supervisor-args; leaving the LaunchAgent as is"
                    )
                });
                return Ok(false);
            }
            let output = Command::new(runtime_core::process::resolve_program(
                std::ffi::OsStr::new("python3"),
                None,
            ))
            .arg(&script)
            .args(["sync-supervisor-args", "--prefix"])
            .arg(&prefix)
            .arg("--data-dir")
            .arg(runtime_root)
            .arg("--supervisor-args-json")
            .arg(supervisor_args_json(runtime_root)?)
            .output()
            .await
            .map_err(|error| format!("Could not update the backend LaunchAgent: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "Could not update the backend LaunchAgent: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            return Ok(String::from_utf8_lossy(&output.stdout).trim() == "updated");
        }
        let service_path = macos_service_path()?;
        let Ok(existing) = fs::read_to_string(&service_path) else {
            return Ok(false);
        };
        let Some(recorded) = recorded_fallback_args(&existing, &prefix, runtime_root) else {
            return Ok(false);
        };
        let Some((array, _)) = plist_program_arguments(&existing) else {
            return Ok(false);
        };
        let binary = prefix.join("magic-supervisor.bin").display().to_string();
        let merged = merge_with_this_app(&recorded, &candidate);
        if merged == recorded {
            return Ok(false);
        }
        let new_array = format!(
            "<array>{}</array>",
            std::iter::once(&binary)
                .chain(merged.iter())
                .map(|arg| format!("<string>{}</string>", xml_escape(arg)))
                .collect::<String>()
        );
        fs::write(&service_path, existing.replacen(&array, &new_array, 1))
            .map_err(|error| format!("Could not write {}: {error}", service_path.display()))?;
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = runtime_root;
        Ok(false)
    }
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
        Err(format!(
            "Could not stop the native Magician backend: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
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

/// A data root that is a symlink whose target was removed (cleanup deletes
/// the canonical target and leaves the link): recreate the target directory
/// so seeding writes through the owner's chosen layout. The link is kept.
fn ensure_root_target(runtime_root: &Path) -> Result<(), String> {
    let Ok(meta) = fs::symlink_metadata(runtime_root) else {
        return Ok(());
    };
    if !meta.file_type().is_symlink() || runtime_root.exists() {
        return Ok(());
    }
    let link = fs::read_link(runtime_root).map_err(|error| {
        format!(
            "Could not read the link {}: {error}",
            runtime_root.display()
        )
    })?;
    let target = if link.is_absolute() {
        link
    } else {
        runtime_root
            .parent()
            .map(|parent| parent.join(&link))
            .unwrap_or(link)
    };
    fs::create_dir_all(&target)
        .map_err(|error| format!("Could not recreate {}: {error}", target.display()))
}

/// Clear a `launchctl disable` override left by `stop_service` (macOS).
async fn clear_disable_override() {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, SERVICE_ID);
        let _ = Command::new("launchctl")
            .args(["enable", &target])
            .output()
            .await;
    }
}

/// Re-seed a runtime root whose files were removed under the native service
/// (Desktop's "only data" cleanup) and start the service again: the native
/// launcher does not seed a missing `magician-config.yaml`, so the backend
/// would crash-loop on an empty root. A seeding failure is reported, the
/// disable override is always cleared, and the service is still started when
/// a usable config exists.
pub async fn reseed_and_start(runtime_root: &Path) -> Result<(), String> {
    let seeded = ensure_root_target(runtime_root)
        .and_then(|()| seed_runtime_root(&install_prefix(), runtime_root));
    clear_disable_override().await;
    let started = if seeded.is_ok() || runtime_root.join("magician-config.yaml").is_file() {
        ensure_service_started().await
    } else {
        Err("not started: the runtime root has no magician-config.yaml".to_string())
    };
    match (seeded, started) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(seeding), Ok(())) => Err(format!(
            "seeding failed ({seeding}); the backend was started with the existing config"
        )),
        (Ok(()), Err(start)) => Err(start),
        (Err(seeding), Err(start)) => Err(format!("seeding failed ({seeding}); {start}")),
    }
}

/// The runtime root the registered native service actually uses: the
/// LaunchAgent's `MAGICIAN_ROOT_DIR` (macOS), the systemd unit's
/// `Environment` (Linux), or the logon-task launcher's `$env:` value
/// (Windows). None when nothing is registered or it cannot be read.
pub fn registered_runtime_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let text = fs::read_to_string(macos_service_path().ok()?).ok()?;
        root_from_macos_plist(&text)
    }
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string(linux_service_path().ok()?).ok()?;
        root_from_systemd_unit(&text)
    }
    #[cfg(target_os = "windows")]
    {
        let text = fs::read_to_string(windows_launcher_path(&install_prefix())).ok()?;
        root_from_windows_launcher(&text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// `MAGICIAN_ROOT_DIR` from a LaunchAgent plist, as written by Desktop or by
/// `native-runtime-service.py` (plistlib indents the `<string>`).
#[cfg(any(target_os = "macos", test))]
fn root_from_macos_plist(text: &str) -> Option<PathBuf> {
    let after_key = text.split_once("<key>MAGICIAN_ROOT_DIR</key>")?.1;
    let after_open = after_key.trim_start().strip_prefix("<string>")?;
    let value = after_open.split_once("</string>")?.0;
    (!value.is_empty()).then(|| PathBuf::from(xml_unescape(value)))
}

/// `MAGICIAN_ROOT_DIR` from `Environment="MAGICIAN_ROOT_DIR=..."` in the unit.
#[cfg(any(target_os = "linux", test))]
fn root_from_systemd_unit(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("Environment=\"MAGICIAN_ROOT_DIR=")?
            .strip_suffix('"')?;
        (!value.is_empty())
            .then(|| PathBuf::from(value.replace("\\\"", "\"").replace("\\\\", "\\")))
    })
}

/// `MAGICIAN_ROOT_DIR` from `$env:MAGICIAN_ROOT_DIR = '...'` in the launcher.
#[cfg(any(target_os = "windows", test))]
fn root_from_windows_launcher(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("$env:MAGICIAN_ROOT_DIR = '")?
            .strip_suffix('\'')?;
        (!value.is_empty()).then(|| PathBuf::from(value.replace("''", "'")))
    })
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
        // stop_service left a `launchctl disable` override; clear it so a
        // later install is not silently disabled.
        #[cfg(target_os = "macos")]
        {
            let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, SERVICE_ID);
            let _ = Command::new("launchctl")
                .args(["enable", &target])
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

fn render_macos_service(prefix: &Path, runtime_root: &Path, supervisor_args: &[String]) -> String {
    let executable = xml_escape(&prefix.join("magic-supervisor.bin").display().to_string());
    let program_arguments = std::iter::once(executable)
        .chain(supervisor_args.iter().map(|arg| xml_escape(arg)))
        .map(|arg| format!("<string>{arg}</string>"))
        .collect::<String>();
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
  <key>ProgramArguments</key><array>{program_arguments}</array>
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

    #[cfg(unix)]
    #[tokio::test]
    async fn source_build_failures_and_missing_packages_never_report_installed() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("source checkout");
        let root = temporary.path().join("my notes");
        let prefix = temporary.path().join("software");
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        let script = checkout.join("scripts/install-source-checkout.sh");
        let plan =
            magician_components::source_install::SourceInstall::new(&checkout, &root, &prefix)
                .unwrap();
        fs::write(&script, "echo fixture-build-failed; exit 7\n").unwrap();
        let error = prepare_source_install(&plan, std::time::Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(error.contains("failed"));
        assert!(error.contains("source-install.log"));
        assert!(fs::read_to_string(root.join("logs/source-install.log"))
            .unwrap()
            .contains("fixture-build-failed"));
        assert!(!prefix.exists());
        fs::write(script, "exit 0\n").unwrap();
        assert!(
            prepare_source_install(&plan, std::time::Duration::from_secs(5))
                .await
                .is_err(),
            "zero exit without a valid package is incomplete"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_timeout_stops_background_build_children() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("source");
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        fs::write(
            checkout.join("scripts/install-source-checkout.sh"),
            "sleep 60 &\necho $! > child.pid\nwait\n",
        )
        .unwrap();
        let plan = magician_components::source_install::SourceInstall::new(
            &checkout,
            &temporary.path().join("notes"),
            &temporary.path().join("software"),
        )
        .unwrap();
        let error = prepare_source_install(&plan, std::time::Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(error.contains("timed out"));
        let pid: i32 = fs::read_to_string(checkout.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "background build child survived timeout"
        );
    }

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
        let macos = render_macos_service(prefix, root, &[]);
        assert!(macos.contains(SERVICE_ID));
        assert!(macos.contains("magic-supervisor.bin"));
        assert!(macos.contains("MAGICIAN_ROOT_DIR"));
        let with_desktop = render_macos_service(
            prefix,
            root,
            &supervisor_args(root, Some(Path::new("/Applications/Magican & Co.app"))),
        );
        assert!(with_desktop.contains(
            "<array><string>/Users/Test Person/.magician/magic-supervisor.bin</string><string>--magician-config</string><string>/Users/Test Person/MagicianNotes/magician-config.yaml</string><string>--desktop-app</string><string>/Applications/Magican &amp; Co.app</string><string>--desktop-log</string><string>/Users/Test Person/MagicianNotes/logs/desktop.log</string></array>"
        ));
        assert_ne!(macos, with_desktop, "changed arguments are plist drift");
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
    fn unstable_locations_are_never_recorded_as_the_desktop_bundle() {
        assert_eq!(
            app_bundle_of(Path::new(
                "/private/var/folders/x/AppTranslocation/ABC/d/Magican.app/Contents/MacOS/magician-desktop"
            )),
            None
        );
        assert_eq!(
            app_bundle_of(Path::new(
                "/Volumes/Magican/Magican.app/Contents/MacOS/magician-desktop"
            )),
            None
        );
    }

    #[test]
    fn another_bundle_never_steals_an_existing_binding() {
        let s = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        let recorded = s(&[
            "--magician-config",
            "/data/magician-config.yaml",
            "--desktop-app",
            "/Applications/Magican.app",
            "--desktop-log",
            "/data/logs/desktop.log",
        ]);
        let debug = s(&[
            "--magician-config",
            "/data/magician-config.yaml",
            "--desktop-app",
            "/repo/.local/Magican-Debug.app",
            "--desktop-log",
            "/data/logs/desktop.log",
        ]);
        assert_eq!(
            merge_supervisor_args(&recorded, &debug, |_| true, |_| false),
            recorded,
            "the recorded bundle still exists: kept"
        );
        assert_eq!(
            merge_supervisor_args(&recorded, &debug, |_| false, |_| false),
            debug,
            "the recorded bundle is gone: this one takes over"
        );
        assert_eq!(
            merge_supervisor_args(&s(&[]), &debug, |_| true, |_| false),
            debug,
            "nothing recorded yet"
        );
        let mut stale = recorded.clone();
        stale[3] = "/Users/me/Downloads/Magican.app".to_string();
        stale.extend(s(&[
            "--manage-desktop=0",
            "--desktop-launch-at-boot",
            "always",
        ]));
        let installed = merge_supervisor_args(
            &stale,
            &recorded,
            |_| true,
            |bundle| is_applications_bundle(bundle, Some(Path::new("/Users/me"))),
        );
        assert_eq!(
            installed[..6],
            recorded[..],
            "an /Applications app reclaims the binding"
        );
        assert_eq!(
            installed[6..],
            s(&["--manage-desktop=0", "--desktop-launch-at-boot", "always"])[..],
            "hand-added and future flags survive"
        );
        assert_eq!(
            merge_supervisor_args(&stale, &[], |_| true, |_| false),
            stale,
            "no candidate keeps everything, including --magician-config"
        );
        assert!(is_applications_bundle(
            Path::new("/Users/me/Applications/Magican.app"),
            Some(Path::new("/Users/me"))
        ));
    }

    #[test]
    fn only_the_program_arguments_are_compared_and_replaced() {
        let prefix = Path::new("/Users/Test Person/.magician");
        let root = Path::new("/Users/Test Person/MagicianNotes");
        let plist = render_macos_service(
            prefix,
            root,
            &supervisor_args(root, Some(Path::new("/Applications/Magican & Co.app"))),
        );
        let (array, items) = plist_program_arguments(&plist).expect("arguments");
        assert!(array.starts_with("<array>") && array.ends_with("</array>"));
        assert_eq!(
            items[0],
            "/Users/Test Person/.magician/magic-supervisor.bin"
        );
        assert_eq!(items[4], "/Applications/Magican & Co.app", "unescaped");
    }

    #[test]
    fn the_desktop_bundle_is_passed_only_when_running_from_one() {
        assert_eq!(
            app_bundle_of(Path::new(
                "/Applications/Magican.app/Contents/MacOS/magician-desktop"
            )),
            Some(PathBuf::from("/Applications/Magican.app"))
        );
        assert_eq!(
            app_bundle_of(Path::new("/repo/target/debug/magician-desktop")),
            None
        );
        assert_eq!(
            app_bundle_of(Path::new("/repo/Contents/MacOS/magician-desktop")),
            None
        );
        let root = Path::new("/data");
        assert_eq!(
            supervisor_args(root, None),
            vec!["--magician-config", "/data/magician-config.yaml"]
        );
        assert_eq!(
            supervisor_args(root, Some(Path::new("/Applications/Magican.app")))[2..],
            [
                "--desktop-app",
                "/Applications/Magican.app",
                "--desktop-log",
                "/data/logs/desktop.log"
            ]
        );
    }

    #[test]
    fn reseeding_restores_every_shipped_seed_without_overwriting() {
        let temporary = tempfile::tempdir().expect("temp dir");
        let prefix = temporary.path().join("prefix");
        let seed = prefix.join("share/seed");
        fs::create_dir_all(&seed).expect("seed dir");
        fs::write(seed.join("decision-engine.yaml"), "engine: seed\n").expect("write");
        fs::write(seed.join(".env.example"), "KEY=seed\n").expect("write");
        fs::write(
            seed.join("operator-config.template.yaml"),
            "operator: seed\n",
        )
        .expect("write");
        let root = temporary.path().join("root");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join(".env"), "KEY=owner\n").expect("write");
        seed_runtime_root(&prefix, &root).expect("seeded");
        for name in [
            "magician-config.yaml",
            "llm-router.yaml",
            "decision-engine.yaml",
            "operator-config.yaml",
        ] {
            assert!(root.join(name).is_file(), "{name} seeded");
        }
        assert!(
            !root.join(".env.development").exists(),
            "an empty-keyed .env.development would shadow the owner's .env"
        );
        assert!(root.join("logs").is_dir());
        assert_eq!(
            fs::read_to_string(root.join(".env")).expect("read"),
            "KEY=owner\n",
            "existing files are kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_root_link_gets_its_target_back_and_keeps_the_link() {
        let temporary = tempfile::tempdir().expect("temp dir");
        let target = temporary.path().join("ssd/MagicianNotes");
        let link = temporary.path().join("MagicianNotes");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        assert!(!link.exists(), "dangling");
        ensure_root_target(&link).expect("recreated");
        assert!(target.is_dir());
        assert!(fs::symlink_metadata(&link)
            .expect("meta")
            .file_type()
            .is_symlink());
        ensure_root_target(temporary.path()).expect("a plain directory is left alone");
    }

    #[test]
    fn the_registered_runtime_root_is_read_back_from_each_service_format() {
        let prefix = Path::new("/Users/Test Person/.magician");
        let root = Path::new("/Users/Test Person/Magician & \"Notes\"");
        assert_eq!(
            root_from_macos_plist(&render_macos_service(prefix, root, &[])),
            Some(root.to_path_buf())
        );
        let plistlib_style =
            "<dict>\n\t\t<key>MAGICIAN_ROOT_DIR</key>\n\t\t<string>/data/notes</string>\n</dict>";
        assert_eq!(
            root_from_macos_plist(plistlib_style),
            Some(PathBuf::from("/data/notes"))
        );
        assert_eq!(
            root_from_systemd_unit(&render_linux_service(prefix, root)),
            Some(root.to_path_buf())
        );
        let windows_root = Path::new(r"C:\Users\O'Brien\MagicianNotes");
        assert_eq!(
            root_from_windows_launcher(&render_windows_launcher(
                Path::new(r"C:\Users\O'Brien\.magician"),
                windows_root
            )),
            Some(windows_root.to_path_buf())
        );
        assert_eq!(root_from_macos_plist("<plist></plist>"), None);
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
