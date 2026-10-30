//! Bounded, owner-started login for a declaratively reviewed CLI. The CLI
//! owns the callback, token exchange and scoped credential format.
use super::process::OwnedProcess;
use std::{
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

struct StartupScratch(Option<std::path::PathBuf>);
impl Drop for StartupScratch {
    fn drop(&mut self) {
        if let (Some(path), Ok(runtime)) = (self.0.take(), tokio::runtime::Handle::try_current()) {
            runtime.spawn(async move {
                let _ = tokio::fs::remove_dir_all(path).await;
            });
        }
    }
}

pub struct CliLogin {
    pub url: Zeroizing<String>,
    pub state: Arc<AtomicU8>, // 0 waiting, 1 CLI-confirmed login, 2 failed/expired
    cancel: CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_local_sign_in_page_inside_scratch_yields_its_allowed_link() {
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let page = "<a href=\"https://clerk.example.ai/oauth/authorize?client_id=c&amp;redirect_uri=http%3A%2F%2Flocalhost%3A8765%2Fcallback&amp;state=s\">Sign in</a>";
        let inside = scratch.path().join("auth").join("sign-in.html");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, page).unwrap();
        std::fs::write(outside.path().join("sign-in.html"), page).unwrap();
        let hosts = vec!["clerk.example.ai".to_string()];
        let printed = |path: &Path| {
            format!(
                "If browser does not open, open this file: {}\n",
                url::Url::from_file_path(path).unwrap()
            )
        };
        assert_eq!(
            sign_in_page_url(&printed(&inside), &hosts, scratch.path()).as_deref(),
            Some("https://clerk.example.ai/oauth/authorize?client_id=c&redirect_uri=http%3A%2F%2Flocalhost%3A8765%2Fcallback&state=s")
        );
        // Outside the login's scratch directory, or a host that is not allowed: ignored.
        assert!(sign_in_page_url(
            &printed(&outside.path().join("sign-in.html")),
            &hosts,
            scratch.path()
        )
        .is_none());
        assert!(sign_in_page_url(
            &printed(&inside),
            &["other.example".to_string()],
            scratch.path()
        )
        .is_none());
    }

    #[test]
    fn replacing_a_cli_viewer_keeps_login_alive_but_login_stop_revokes_viewers() {
        let login = CliLogin {
            url: Zeroizing::new("https://provider.example/authorize".into()),
            state: Arc::new(AtomicU8::new(0)),
            cancel: CancellationToken::new(),
        };
        let first = login.viewer_cancellation();
        let second = login.viewer_cancellation();
        first.cancel();
        assert!(!second.is_cancelled());
        login.cancel();
        assert!(second.is_cancelled());
    }
    #[test]
    fn only_the_reviewed_cli_login_host_can_be_opened() {
        let hosts = vec!["provider.example".to_string()];
        assert_eq!(
            authorization_url("Open https://provider.example/login?state=opaque\n", &hosts)
                .as_deref(),
            Some("https://provider.example/login?state=opaque")
        );
        for value in [
            "https://provider.example.attacker.test/login",
            "http://provider.example/login",
            "https://user:password@provider.example/login",
            "file:///tmp/login",
        ] {
            assert!(authorization_url(value, &hosts).is_none());
        }
    }
}
impl Drop for CliLogin {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl CliLogin {
    /// A login another owner runs (a managed bot's supervisor login, e.g.
    /// `gws auth login`): no process here, only the viewer lifecycle and a
    /// result the owner's watcher stores into `state`.
    pub fn external(url: Zeroizing<String>) -> Self {
        Self {
            url,
            state: Arc::new(AtomicU8::new(0)),
            cancel: CancellationToken::new(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn viewer_cancellation(&self) -> CancellationToken {
        self.cancel.child_token()
    }
}

pub(super) fn saved_credential(path: &Path, secure: bool) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let Ok(file) = options.open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid reads this process's identity and takes no pointers.
        let owner = unsafe { libc::geteuid() };
        if metadata.nlink() != 1 || metadata.uid() != owner {
            return false;
        }
        if secure {
            if file
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .is_err()
            {
                return false;
            }
        } else if metadata.mode() & 0o077 != 0 {
            return false;
        }
    }
    #[cfg(not(unix))]
    let _ = secure;
    true
}

fn authorization_url(text: &str, hosts: &[String]) -> Option<String> {
    for part in text.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>')) {
        let Ok(url) = url::Url::parse(part) else {
            continue;
        };
        if url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some_and(|host| {
                hosts
                    .iter()
                    .any(|allowed| host.eq_ignore_ascii_case(allowed))
            })
            && part.len() <= 64 * 1024
        {
            return Some(part.to_owned());
        }
    }
    None
}

/// Some CLIs (Higgsfield ≥ the 2026-10 release) no longer print the
/// authorization URL: they write a local `sign-in.html` that links to it and
/// print a `file://` path. Follow such a path only when it names a regular,
/// owned HTML file inside this login's private scratch directory (the CLI's
/// `TMPDIR`), and accept only an allowed-host link found in it.
fn sign_in_page_url(text: &str, hosts: &[String], scratch: &Path) -> Option<String> {
    let root = std::fs::canonicalize(scratch).ok()?;
    for part in text.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>')) {
        if !part.starts_with("file://") {
            continue;
        }
        let Ok(page_url) = url::Url::parse(part) else {
            continue;
        };
        let Ok(path) = page_url.to_file_path() else {
            continue;
        };
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
        {
            continue;
        }
        let Ok(resolved) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !resolved.starts_with(&root) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            continue;
        }
        let Ok(page) = std::fs::read(&resolved) else {
            continue;
        };
        // The link sits in an HTML attribute, so `&` arrives as `&amp;`.
        let page = Zeroizing::new(String::from_utf8_lossy(&page).replace("&amp;", "&"));
        if let Some(url) = authorization_url(&page, hosts) {
            return Some(url);
        }
    }
    None
}

async fn drain(
    mut output: impl tokio::io::AsyncRead + Unpin,
    hosts: Vec<String>,
    sender: watch::Sender<Option<String>>,
    scratch: std::path::PathBuf,
) {
    let find = |text: &str| {
        authorization_url(text, &hosts).or_else(|| sign_in_page_url(text, &hosts, &scratch))
    };
    let mut buffer = Zeroizing::new([0u8; 4096]);
    let mut carry = Zeroizing::new(String::new());
    loop {
        let Ok(count) = output.read(&mut buffer[..]).await else {
            break;
        };
        if count == 0 {
            if let Some(url) = find(&carry) {
                sender.send_replace(Some(url));
            }
            break;
        }
        carry.push_str(&String::from_utf8_lossy(&buffer[..count]));
        // CLI output can contain secrets. Parse in bounded private memory;
        // never log it or send it to task history.
        if let Some(end) = carry.rfind(char::is_whitespace) {
            if let Some(url) = find(&carry[..end]) {
                sender.send_replace(Some(url));
            }
        }
        if carry.len() > 64 * 1024 {
            carry.clear();
        }
    }
}

pub(super) async fn start(
    executable: &Path,
    args: &[String],
    hosts: &[String],
    home: &Path,
    credential: &Path,
    child_path: &std::ffi::OsStr,
    reservation: super::CliReservation,
) -> Result<Arc<CliLogin>, &'static str> {
    let scratch =
        std::env::temp_dir().join(format!("magican-cli-signin-{}", Uuid::new_v4().simple()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&scratch)
        .map_err(|_| "signin_cli_unavailable")?;
    let mut scratch_cleanup = StartupScratch(Some(scratch.clone()));
    let spool = scratch.join("authorization-url");
    let opener = scratch.join("open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = b"#!/bin/sh\numask 077\nfor arg in \"$@\"; do\n case \"$arg\" in https://*) temporary=\"$MAGICAN_SIGNIN_URL_FILE.$$\"; printf '%s' \"$arg\" > \"$temporary\" && /bin/mv \"$temporary\" \"$MAGICAN_SIGNIN_URL_FILE\"; exit $?;; esac\ndone\nexit 1\n";
        for name in ["open", "xdg-open", "sensible-browser", "x-www-browser"] {
            let path = scratch.join(name);
            if tokio::fs::write(&path, script).await.is_err() {
                let _ = tokio::fs::remove_dir_all(&scratch).await;
                return Err("signin_cli_unavailable");
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "signin_cli_unavailable")?;
        }
    }
    let mut paths = vec![scratch.clone()];
    paths.extend(std::env::split_paths(child_path));
    let path = std::env::join_paths(paths).map_err(|_| "signin_cli_unavailable")?;
    let program = runtime_core::process::resolve_program(executable.as_os_str(), Some(&path));
    let mut command = Command::new(program);
    command.env_clear();
    for name in [
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "USER",
        "LOGNAME",
        "SYSTEMROOT",
        "WINDIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(args)
        .env("PATH", &path)
        .env("BROWSER", &opener)
        .env("MAGICAN_SIGNIN_URL_FILE", &spool)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("APPDATA", home.join(".config"))
        .env("LOCALAPPDATA", home.join(".cache"))
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TMPDIR", &scratch)
        .env("TEMP", &scratch)
        .env("TMP", &scratch)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let spawned = OwnedProcess::spawn(&mut command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            let _ = tokio::fs::remove_dir_all(scratch).await;
            return Err("signin_cli_unavailable");
        },
    };
    let (sender, mut receiver) = watch::channel(None);
    let stdout = child.stdout.take().ok_or("signin_cli_unavailable")?;
    let stderr = child.stderr.take().ok_or("signin_cli_unavailable")?;
    let out = tokio::spawn(drain(
        stdout,
        hosts.to_vec(),
        sender.clone(),
        scratch.clone(),
    ));
    let err = tokio::spawn(drain(
        stderr,
        hosts.to_vec(),
        sender.clone(),
        scratch.clone(),
    ));
    let state = Arc::new(AtomicU8::new(0));
    let progress = Arc::clone(&state);
    let cancel = CancellationToken::new();
    let cancelled_on_drop = cancel.clone().drop_guard();
    let stopped = cancel.clone();
    let credential = credential.to_owned();
    let allowed_hosts = hosts.to_vec();
    scratch_cleanup.0.take();
    tokio::spawn(async move {
        // Fixed-port CLI logins stay exclusive until the actual process tree
        // has stopped, even when the viewer/HTTP request has disappeared.
        let _reservation = reservation;
        let result = tokio::select! {
            _ = stopped.cancelled() => None,
            _ = tokio::time::sleep(Duration::from_secs(600)) => None,
            result = child.wait() => result.ok(),
        };
        let successful =
            result.is_some_and(|status| status.success()) && saved_credential(&credential, true);
        if !successful {
            child.terminate().await;
        }
        progress.store(if successful { 1 } else { 2 }, Ordering::SeqCst);
        // Publish the outcome before stopping viewers, and stop them before
        // releasing the listener reservation to another workspace.
        stopped.cancel();
        out.abort();
        err.abort();
        drop(sender);
        let _ = tokio::fs::remove_dir_all(scratch).await;
    });
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if let Ok(text) = tokio::fs::read(&spool).await {
                if text.len() <= 64 * 1024 {
                    if let Some(url) =
                        authorization_url(&String::from_utf8_lossy(&text), &allowed_hosts)
                    {
                        return Ok(url);
                    }
                }
            }
            if let Some(url) = receiver.borrow_and_update().clone() {
                return Ok(url);
            }
            match state.load(Ordering::SeqCst) {
                1 => return Err("signin_already_connected"),
                2 => return Err("signin_cli_login_failed"),
                _ => {},
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(Err("signin_cli_url_unavailable"));
    match result {
        Ok(url) => {
            cancelled_on_drop.disarm();
            Ok(Arc::new(CliLogin {
                url: Zeroizing::new(url),
                state,
                cancel,
            }))
        },
        Err(error) => {
            cancel.cancel();
            Err(error)
        },
    }
}
