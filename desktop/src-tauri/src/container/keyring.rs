//! Shared installer/desktop custody preparation. No credential bytes are
//! returned by this helper or put in the container environment.

use super::ContainerConfig;
#[cfg(not(target_os = "windows"))]
use std::ffi::OsStr;
#[cfg(not(target_os = "windows"))]
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
const PROVISIONER: &str = include_str!("../../../../scripts/prepare-container-keyring.py");

pub(super) async fn prepare(
    config: &ContainerConfig,
    runtime: &str,
    engine: &str,
) -> Result<Vec<String>, String> {
    prepare_using_home(config, runtime, engine, None).await
}

async fn prepare_using_home(
    config: &ContainerConfig,
    runtime: &str,
    engine: &str,
    custody_home: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    validate_managed_config(config)?;
    #[cfg(target_os = "windows")]
    {
        let _ = engine;
        return prepare_windows(config, runtime, custody_home).await;
    }
    #[cfg(not(target_os = "windows"))]
    {
        prepare_unix(config, runtime, engine, custody_home).await
    }
}

fn validate_managed_config(config: &ContainerConfig) -> Result<(), String> {
    if config
        .env_vars
        .keys()
        .any(|key| key.starts_with("MAGICIAN_KEYRING_"))
        || config
            .env_vars
            .get("MAGICIAN_ROOT_DIR")
            .is_some_and(|value| value != "/data")
    {
        return Err(
            "Managed containers require the provisioned keyring mounts and /data runtime root"
                .into(),
        );
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
async fn prepare_unix(
    config: &ContainerConfig,
    runtime: &str,
    engine: &str,
    custody_home: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|error| format!("Cannot prepare runtime root: {error}"))?;
    let path = std::env::var_os("PATH");
    let python = runtime_core::process::resolve_program(OsStr::new("python3"), path.as_deref());
    let engine = runtime_core::process::resolve_program(OsStr::new(engine), path.as_deref());
    let mut command = tokio::process::Command::new(python);
    command
        .args(["-c", PROVISIONER, "--data-dir"])
        .arg(&config.data_dir)
        .args(["--runtime", runtime, "--engine"])
        .arg(engine)
        .args(["--container", &config.name, "--image", &config.image])
        .kill_on_drop(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;

        let canonical = std::fs::canonicalize(&config.data_dir).map_err(|error| {
            format!("Cannot resolve runtime root for keyring migration: {error}")
        })?;
        let bytes = canonical.as_os_str().as_bytes();
        let mut anchor = blake3::Hasher::new();
        anchor.update(b"magician.paired-devices.anchor-account.v1\0");
        anchor.update(&(bytes.len() as u64).to_le_bytes());
        anchor.update(bytes);
        command
            .arg("--pairing-anchor-account")
            .arg(format!("generation-{}", anchor.finalize().to_hex()));
    }
    if let Some(home) = custody_home {
        command.arg("--custody-home").arg(home);
    }
    let output = tokio::time::timeout(Duration::from_secs(150), command.output())
        .await
        .map_err(|_| "Container keyring preflight timed out; existing container was not changed")?
        .map_err(|_| "Container keyring provisioning requires Python 3 on the desktop host")?;
    if !output.status.success() {
        // The embedded helper reports only bounded, credential-free diagnostics.
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    #[derive(serde::Deserialize)]
    struct Plan {
        launch_args: Vec<String>,
    }
    serde_json::from_slice::<Plan>(&output.stdout)
        .map(|plan| plan.launch_args)
        .map_err(|_| "Invalid container keyring provisioning response".into())
}

#[cfg(target_os = "windows")]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct WindowsCustodyManifest {
    version: u8,
    runtime_root: String,
    runtime: String,
    secret_sha256: String,
}

#[cfg(target_os = "windows")]
async fn prepare_windows(
    config: &ContainerConfig,
    runtime: &str,
    custody_home: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    use ring::rand::{SecureRandom, SystemRandom};
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    fn ensure_plain_path(path: &Path, directory: bool) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            format!("Cannot inspect keyring custody {}: {error}", path.display())
        })?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("Keyring custody paths must not be links or reparse points".to_string());
        }
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            return Err("Invalid keyring custody file type".to_string());
        }
        Ok(())
    }

    fn ordinary_windows_path(path: &Path) -> String {
        let value = path.to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else {
            value
                .strip_prefix(r"\\?\")
                .unwrap_or(value.as_ref())
                .to_string()
        }
    }

    fn normalized(path: &Path) -> String {
        ordinary_windows_path(path)
            .replace('\\', "/")
            .to_lowercase()
    }

    fn is_within(child: &Path, parent: &Path) -> bool {
        let child = normalized(child);
        let mut parent = normalized(parent);
        if !parent.ends_with('/') {
            parent.push('/');
        }
        child == parent.trim_end_matches('/') || child.starts_with(&parent)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn windows_mount_path(path: &Path) -> Result<String, String> {
        let value = ordinary_windows_path(path).replace('\\', "/");
        if value
            .chars()
            .any(|character| matches!(character, '\0' | '\n' | '\r'))
            || value
                .match_indices(':')
                .any(|(index, _)| index != 1 || !value.as_bytes()[0].is_ascii_alphabetic())
        {
            return Err(format!(
                "Container mount path is not supported: {}",
                path.display()
            ));
        }
        Ok(value)
    }

    async fn current_user_sid() -> Result<String, String> {
        let whoami = runtime_core::process::resolve_program(
            std::ffi::OsStr::new("whoami.exe"),
            std::env::var_os("PATH").as_deref(),
        );
        let output = tokio::process::Command::new(whoami)
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .await
            .map_err(|error| format!("Cannot identify the Windows desktop user: {error}"))?;
        if !output.status.success() {
            return Err("Cannot identify the Windows desktop user for keyring custody".to_string());
        }
        let row = String::from_utf8_lossy(&output.stdout);
        let sid = row
            .trim()
            .trim_matches('"')
            .split("\",\"")
            .nth(1)
            .map(|value| value.trim_matches('"').trim())
            .filter(|value| value.starts_with("S-1-"))
            .ok_or("Windows returned an invalid desktop user identity")?;
        Ok(sid.to_string())
    }

    async fn secure_acl(path: &Path, directory: bool) -> Result<(), String> {
        let sid = current_user_sid().await?;
        let icacls = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .map(|root| root.join("System32").join("icacls.exe"))
            .unwrap_or_else(|| {
                runtime_core::process::resolve_program(
                    std::ffi::OsStr::new("icacls.exe"),
                    std::env::var_os("PATH").as_deref(),
                )
            });
        let mut command = tokio::process::Command::new(icacls);
        let permission = if directory { "(OI)(CI)F" } else { "F" };
        command
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("*{sid}:{permission}"))
            .arg(format!("*S-1-5-18:{permission}"));
        let output = command
            .output()
            .await
            .map_err(|error| format!("Cannot protect Windows keyring custody: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Windows could not restrict keyring custody permissions: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    std::fs::create_dir_all(&config.data_dir)
        .map_err(|error| format!("Cannot prepare runtime root: {error}"))?;
    let data = std::fs::canonicalize(&config.data_dir)
        .map_err(|error| format!("Cannot resolve runtime root for keyring custody: {error}"))?;
    ensure_plain_path(&data, true)?;

    let custody = match custody_home {
        Some(path) => path.to_path_buf(),
        None => match std::env::var_os("MAGICIAN_CONTAINER_KEYRING_HOME") {
            Some(path) => PathBuf::from(path),
            None => dirs::data_local_dir()
                .ok_or("Windows local application data directory is unavailable")?
                .join("Magican")
                .join("container-keyrings"),
        },
    };
    std::fs::create_dir_all(&custody)
        .map_err(|error| format!("Cannot create Windows keyring custody: {error}"))?;
    let custody = std::fs::canonicalize(&custody)
        .map_err(|error| format!("Cannot resolve Windows keyring custody: {error}"))?;
    ensure_plain_path(&custody, true)?;
    if is_within(&custody, &data) {
        return Err("Keep keyring custody outside the Magician runtime data folder".to_string());
    }
    secure_acl(&custody, true).await?;

    let anchor = sha256_hex(normalized(&data).as_bytes());
    let bundle = custody.join(anchor);
    let state = bundle.join("state");
    let secret = bundle.join("unlock.secret");
    let manifest = bundle.join("manifest.json");
    let lock = custody.join(format!(
        "{}.lock",
        bundle.file_name().unwrap().to_string_lossy()
    ));

    let _lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .open(&lock)
        .map_err(|error| {
            format!(
                "Another setup is changing this runtime's keyring custody, or the custody lock is invalid: {error}"
            )
        })?;
    ensure_plain_path(&lock, false)?;

    if !bundle.exists() {
        let system = data.join("system");
        if system.exists() {
            ensure_plain_path(&system, true)?;
            if std::fs::read_dir(&system)
                .map_err(|error| format!("Cannot inspect existing runtime state: {error}"))?
                .next()
                .is_some()
            {
                return Err(
                    "Runtime already has system state; restore or explicitly migrate its existing keyring before using a managed Windows container"
                        .to_string(),
                );
            }
        }

        std::fs::create_dir(&bundle)
            .map_err(|error| format!("Cannot create keyring custody bundle: {error}"))?;
        secure_acl(&bundle, true).await?;
        std::fs::create_dir(&state)
            .map_err(|error| format!("Cannot create keyring state directory: {error}"))?;

        let mut random = Zeroizing::new([0_u8; 32]);
        SystemRandom::new()
            .fill(random.as_mut())
            .map_err(|_| "Cannot generate the container keyring unlock secret")?;
        let password = Zeroizing::new(
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        let mut secret_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&secret)
            .map_err(|error| format!("Cannot create keyring unlock secret: {error}"))?;
        secret_file
            .write_all(password.as_bytes())
            .and_then(|_| secret_file.sync_all())
            .map_err(|error| format!("Cannot persist keyring unlock secret: {error}"))?;
        drop(secret_file);

        let record = WindowsCustodyManifest {
            version: 1,
            runtime_root: normalized(&data),
            runtime: runtime.to_string(),
            secret_sha256: sha256_hex(password.as_bytes()),
        };
        let encoded = serde_json::to_vec_pretty(&record)
            .map_err(|error| format!("Cannot encode keyring custody manifest: {error}"))?;
        let mut manifest_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&manifest)
            .map_err(|error| format!("Cannot create keyring custody manifest: {error}"))?;
        manifest_file
            .write_all(&encoded)
            .and_then(|_| manifest_file.write_all(b"\n"))
            .and_then(|_| manifest_file.sync_all())
            .map_err(|error| format!("Cannot persist keyring custody manifest: {error}"))?;
    }

    ensure_plain_path(&bundle, true)?;
    ensure_plain_path(&state, true)?;
    ensure_plain_path(&secret, false).map_err(|_| {
        "The Windows container keyring unlock secret is missing or invalid; restore the original instead of generating a replacement"
            .to_string()
    })?;
    ensure_plain_path(&manifest, false)?;
    secure_acl(&bundle, true).await?;
    secure_acl(&state, true).await?;
    secure_acl(&secret, false).await?;
    secure_acl(&manifest, false).await?;

    let manifest_bytes = std::fs::read(&manifest)
        .map_err(|error| format!("Cannot read keyring custody manifest: {error}"))?;
    if manifest_bytes.len() > 16 * 1024 {
        return Err("Keyring custody manifest exceeds its size limit".to_string());
    }
    let record: WindowsCustodyManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Invalid keyring custody manifest: {error}"))?;
    if record.version != 1 || record.runtime_root != normalized(&data) || record.runtime != runtime
    {
        return Err(
            "Keyring custody belongs to another runtime or an unsupported version".to_string(),
        );
    }

    let secret_file = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&secret)
        .map_err(|error| format!("Cannot open keyring unlock secret: {error}"))?;
    let mut password = Zeroizing::new(Vec::new());
    secret_file
        .take(4097)
        .read_to_end(&mut password)
        .map_err(|error| format!("Cannot read keyring unlock secret: {error}"))?;
    let valid = (32..=4096).contains(&password.len())
        && !password.contains(&0)
        && sha256_hex(&password) == record.secret_sha256;
    if !valid {
        return Err(
            "Keyring unlock secret changed; restore the original instead of rotating it"
                .to_string(),
        );
    }

    Ok(vec![
        "-v".to_string(),
        format!("{}:/data", windows_mount_path(&data)?),
        "-v".to_string(),
        format!("{}:/keyring", windows_mount_path(&state)?),
        "-v".to_string(),
        format!(
            "{}:/run/secrets/magician-keyring-password:ro",
            windows_mount_path(&secret)?
        ),
        "-e".to_string(),
        "MAGICIAN_KEYRING_STATE_DIR=/keyring".to_string(),
        "-e".to_string(),
        "MAGICIAN_KEYRING_PASSWORD_FILE=/run/secrets/magician-keyring-password".to_string(),
    ])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn container_routing_keyring_embedded_helper_preserves_credentials_and_refuses_loss() {
        let root = std::env::temp_dir().join(format!("desktop-keyring-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let engine = root.join("fixture-engine");
        std::fs::write(
            &engine,
            "#!/bin/sh\ncase \"$1\" in\nlist) printf '[]';;\nrun) exit 0;;\n*) exit 1;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = ContainerConfig {
            image: "fixture-image".into(),
            name: "fixture".into(),
            data_dir: root.join("runtime"),
            config_dir: root.join("runtime/config"),
            log_dir: root.join("runtime/logs"),
            ports: vec![],
            memory_limit: None,
            cpu_limit: None,
            env_vars: Default::default(),
        };
        let home = root.join("custody");
        let first = prepare_using_home(
            &config,
            "apple-container",
            engine.to_str().unwrap(),
            Some(&home),
        )
        .await
        .unwrap();
        let second = prepare_using_home(
            &config,
            "apple-container",
            engine.to_str().unwrap(),
            Some(&home),
        )
        .await
        .unwrap();
        assert_eq!(first, second);
        let mount = first
            .iter()
            .find(|arg| arg.ends_with(":/run/secrets/magician-keyring-password:ro"))
            .unwrap();
        let secret = mount
            .strip_suffix(":/run/secrets/magician-keyring-password:ro")
            .unwrap();
        let original = std::fs::read(secret).unwrap();
        assert!(!first
            .join(" ")
            .contains(std::str::from_utf8(&original).unwrap()));
        std::fs::rename(secret, root.join("saved-secret")).unwrap();
        let error = prepare_using_home(
            &config,
            "apple-container",
            engine.to_str().unwrap(),
            Some(&home),
        )
        .await
        .unwrap_err();
        assert!(error.contains("FileNotFoundError"));
        assert!(!std::path::Path::new(secret).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn windows_custody_reuses_the_secret_and_refuses_to_rotate_it() {
        let root = std::env::temp_dir().join(format!("desktop-keyring-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let config = ContainerConfig {
            image: "fixture-image".into(),
            name: "fixture".into(),
            data_dir: root.join("runtime"),
            config_dir: root.join("runtime/config"),
            log_dir: root.join("runtime/logs"),
            ports: vec![],
            memory_limit: None,
            cpu_limit: None,
            env_vars: Default::default(),
        };
        let home = root.join("custody");
        let first = prepare_using_home(&config, "docker", "docker", Some(&home))
            .await
            .unwrap();
        let second = prepare_using_home(&config, "docker", "docker", Some(&home))
            .await
            .unwrap();
        assert_eq!(first, second);
        let mount = first
            .iter()
            .find(|arg| arg.ends_with(":/run/secrets/magician-keyring-password:ro"))
            .unwrap();
        let secret = mount
            .strip_suffix(":/run/secrets/magician-keyring-password:ro")
            .unwrap();
        let original = std::fs::read(secret).unwrap();
        assert!(!first
            .join(" ")
            .contains(std::str::from_utf8(&original).unwrap()));
        std::fs::remove_file(secret).unwrap();
        let error = prepare_using_home(&config, "docker", "docker", Some(&home))
            .await
            .unwrap_err();
        assert!(error.contains("missing or invalid"));
        assert!(!std::path::Path::new(secret).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
