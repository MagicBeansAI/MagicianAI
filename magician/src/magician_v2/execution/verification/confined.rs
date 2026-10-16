//! Mandatory local boundary used by controller-owned artifact checks.
use super::policy::{NetworkPolicy, ResolvedPolicy};
use anyhow::{ensure, Context, Result};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub fn launcher_pin() -> Result<(PathBuf, String)> {
    let path = if cfg!(target_os = "macos") {
        PathBuf::from("/usr/bin/sandbox-exec")
    } else if cfg!(target_os = "linux") {
        runtime_core::process::resolve_program_str("bwrap", None)
    } else {
        anyhow::bail!("mandatory verification confinement unavailable on this host")
    };
    let path = path
        .canonicalize()
        .context("verification sandbox launcher unavailable")?;
    Ok((
        path.clone(),
        blake3::hash(&std::fs::read(path)?).to_hex().to_string(),
    ))
}

pub(crate) fn command(
    program: &OsStr,
    args: &[OsString],
    workspace: &Path,
    read_roots: &[PathBuf],
    policy: &ResolvedPolicy,
) -> Result<tokio::process::Command> {
    ensure!(
        matches!(policy.sandbox.network, NetworkPolicy::Denied)
            && !policy.sandbox.allow_secrets
            && policy.sandbox.cache_mounts.is_empty()
            && policy.sandbox.writable_roots.is_empty(),
        "controller checks require denied network, no secrets and workspace-only writes"
    );
    let (launcher, _) = launcher_pin()?;
    let workspace = workspace.canonicalize()?;
    let read_roots = read_roots
        .iter()
        .map(|root| root.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut command = tokio::process::Command::new(launcher);
    #[cfg(target_os = "macos")]
    {
        let escape = |path: &Path| {
            path.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        };
        let mut profile = String::from("(version 1)\n(allow default)\n(deny network*)\n(deny file-read*)\n(deny file-write*)\n");
        // Apple's dyld-support profile requires opening the root directory as
        // libignition's openat anchor. This grants no reads of its descendants.
        profile.push_str("(allow file-read* (literal \"/\"))\n");
        for root in [
            "/System",
            "/usr",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/private/var/db/dyld",
        ] {
            profile.push_str(&format!("(allow file-read* (subpath \"{root}\"))\n"));
        }
        for root in read_roots.iter().chain(std::iter::once(&workspace)) {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                escape(root)
            ));
        }
        profile.push_str(&format!("(allow file-read* (literal \"{}\"))\n(allow file-write* (subpath \"{}\"))\n(allow file-read* file-write* (literal \"/dev/null\"))\n", escape(Path::new(program)), escape(&workspace)));
        command
            .args([OsStr::new("-p"), OsStr::new(&profile), program])
            .args(args);
    }
    #[cfg(target_os = "linux")]
    {
        command.args(["--unshare-all", "--die-with-parent", "--new-session"]);
        for root in [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/etc/ld.so.cache",
        ] {
            if Path::new(root).exists() {
                command.args(["--ro-bind", root, root]);
            }
        }
        for root in &read_roots {
            command.arg("--ro-bind").arg(root).arg(root);
        }
        command
            .arg("--bind")
            .arg(&workspace)
            .arg(&workspace)
            .args(["--dev", "/dev", "--proc", "/proc"]);
        // A launcher outside system/toolchain mounts is deliberately unavailable.
        command.arg("--").arg(program).args(args);
    }
    Ok(command)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::verification::policy::SandboxPolicy;

    #[tokio::test]
    async fn confined_loader_bootstraps_without_outside_file_or_network_access() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("synthetic-private.txt");
        let forbidden_write = outside.path().join("forbidden.txt");
        std::fs::write(&secret, "synthetic secret must remain unreadable").unwrap();
        let policy = ResolvedPolicy {
            required: vec![],
            advisory: vec![],
            total_timeout_secs: 10,
            sandbox: SandboxPolicy::default(),
            baseline_ref: None,
            refused_weakenings: vec![],
            added_by_repository: vec![],
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let script = format!(
            "test 7 = 7 || exit 11; if /bin/cat '{}' >/dev/null 2>&1; then exit 12; fi; if echo forbidden >'{}' 2>/dev/null; then exit 13; fi; if /usr/bin/curl --max-time 1 -s http://127.0.0.1:{}/ >/dev/null 2>&1; then exit 14; fi; echo verified",
            secret.display(), forbidden_write.display(), listener.local_addr().unwrap().port(),
        );
        let mut child = command(
            OsStr::new("/bin/sh"),
            &["-c".into(), script.into()],
            workspace.path(),
            &[],
            &policy,
        )
        .unwrap();
        child.env_clear().current_dir(workspace.path());
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.output())
            .await
            .unwrap()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "verified");
        assert!(!forbidden_write.exists());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
