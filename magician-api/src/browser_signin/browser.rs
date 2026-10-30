//! A private Chromium process for human sign-in. It is never registered with
//! Magicutor, agent browser discovery, screen recording or API mining.
use super::process::OwnedProcess;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{net::TcpStream, process::Command};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type Result<T> = std::result::Result<T, &'static str>;
pub const WIDTH: u32 = 390;
pub const HEIGHT: u32 = 720;

pub struct PrivateBrowser {
    child: Option<OwnedProcess>,
    socket: Socket,
    profile: PathBuf,
    session: String,
    target: String,
    root_target: String,
    next_id: u64,
    stop: CancellationToken,
    /// This engine's own user agent without the `HeadlessChrome` token.
    user_agent: Option<String>,
}

impl Drop for PrivateBrowser {
    fn drop(&mut self) {
        let mut child = self.child.take();
        let profile = std::mem::take(&mut self.profile);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(ref mut child) = child {
                    child.terminate().await;
                }
                let _ = tokio::fs::remove_dir_all(profile).await;
            });
        } else if let Some(ref mut child) = child {
            let _ = child.start_kill();
        }
    }
}

/// Also cleans startup failures or a disconnected owner cancelling startup.
struct StartupProfile(Option<PathBuf>);
impl Drop for StartupProfile {
    fn drop(&mut self) {
        if let (Some(path), Ok(runtime)) = (self.0.take(), tokio::runtime::Handle::try_current()) {
            runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let _ = tokio::fs::remove_dir_all(path).await;
            });
        }
    }
}

fn executable(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = configured {
        return (path.is_absolute() && path.is_file())
            .then(|| path.to_owned())
            .ok_or("signin_browser_not_installed");
    }
    let mut candidates = vec![
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ];
    for name in ["google-chrome", "chromium", "chromium-browser", "msedge"] {
        candidates.push(runtime_core::process::resolve_program(
            std::ffi::OsStr::new(name),
            std::env::var_os("PATH").as_deref(),
        ));
    }
    if let Some(base) = std::env::var_os("PROGRAMFILES") {
        candidates.push(PathBuf::from(base).join("Google/Chrome/Application/chrome.exe"));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok())
        .ok_or("signin_browser_not_installed")
}

pub fn allowed_page(raw: &str) -> bool {
    if raw == "about:blank" {
        return true;
    }
    url::Url::parse(raw).is_ok_and(|url| {
        url.username().is_empty()
            && url.password().is_none()
            && (url.scheme() == "https"
                || (url.scheme() == "http"
                    && url.host_str().is_some_and(|host| {
                        host == "localhost"
                            || host
                                .trim_matches(['[', ']'])
                                .parse::<std::net::IpAddr>()
                                .is_ok_and(|ip| ip.is_loopback())
                    })))
    })
}

impl PrivateBrowser {
    pub async fn start(
        authorization_url: &str,
        configured: Option<&Path>,
        stop: CancellationToken,
    ) -> Result<Self> {
        if stop.is_cancelled() {
            return Err("signin_session_expired");
        }
        if !allowed_page(authorization_url) || authorization_url == "about:blank" {
            return Err("signin_url_invalid");
        }
        let executable = executable(configured)?;
        let profile =
            std::env::temp_dir().join(format!("magican-signin-{}", Uuid::new_v4().simple()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&profile)
            .map_err(|_| "signin_profile_unavailable")?;
        let mut cleanup = StartupProfile(Some(profile.clone()));
        let mut command = Command::new(executable);
        command.env_clear();
        for name in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "SYSTEMROOT",
            "WINDIR",
            "TMP",
            "TEMP",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
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
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--headless=new",
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
                "--disable-extensions",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--window-size=390,720",
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if stop.is_cancelled() {
            return Err("signin_session_expired");
        }
        let child = OwnedProcess::spawn(&mut command).map_err(|_| "signin_browser_unavailable");
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&profile).await;
                return Err(error);
            },
        };
        let port = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if child
                    .try_wait()
                    .map_err(|_| "signin_browser_unavailable")?
                    .is_some()
                {
                    return Err("signin_browser_unavailable");
                }
                if let Ok(text) =
                    tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await
                {
                    let mut lines = text.lines();
                    if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                        if let Ok(port) = port.parse::<u16>() {
                            if port != 0
                                && path.starts_with("/devtools/browser/")
                                && !path.contains(['?', '#', '\r', '\n'])
                            {
                                return Ok(format!("ws://127.0.0.1:{port}{path}"));
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or(Err("signin_browser_timeout"));
        let ws = match port {
            Ok(ws) => ws,
            Err(error) => {
                child.terminate().await;
                let _ = tokio::fs::remove_dir_all(&profile).await;
                return Err(error);
            },
        };
        let (socket, _) =
            match tokio::time::timeout(Duration::from_secs(5), connect_async(ws)).await {
                Ok(Ok(connected)) => connected,
                _ => {
                    child.terminate().await;
                    let _ = tokio::fs::remove_dir_all(&profile).await;
                    return Err("signin_browser_unavailable");
                },
            };
        let mut browser = Self {
            child: Some(child),
            socket,
            profile,
            session: String::new(),
            target: String::new(),
            root_target: String::new(),
            next_id: 1,
            stop,
            user_agent: None,
        };
        cleanup.0.take();
        // Headless Chromium announces itself as `HeadlessChrome/…`, and Google
        // refuses sign-in from browsers it takes for automation. Present this
        // same engine's ordinary desktop user agent instead (`attach` applies
        // it to every page). Client hints still carry the real brand list.
        if let Ok(version) = browser
            .command("Browser.getVersion", json!({}), false)
            .await
        {
            browser.user_agent = version["userAgent"]
                .as_str()
                .map(|agent| agent.replace("HeadlessChrome/", "Chrome/"))
                .filter(|agent| !agent.is_empty());
        }
        browser
            .command(
                "Browser.setDownloadBehavior",
                json!({"behavior":"deny"}),
                false,
            )
            .await?;
        let target = browser
            .command("Target.createTarget", json!({"url":"about:blank"}), false)
            .await?;
        browser.target = target["targetId"]
            .as_str()
            .ok_or("signin_browser_unavailable")?
            .to_owned();
        browser.root_target = browser.target.clone();
        browser.attach().await?;
        browser
            .command("Page.navigate", json!({"url":authorization_url}), true)
            .await?;
        Ok(browser)
    }

    async fn command(&mut self, method: &str, params: Value, page: bool) -> Result<Value> {
        if self.stop.is_cancelled() {
            return Err("signin_session_expired");
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut call = json!({"id":id,"method":method,"params":params});
        if page {
            call["sessionId"] = json!(self.session);
        }
        let stop = self.stop.clone();
        tokio::select! { biased;
        _ = stop.cancelled() => Err("signin_session_expired"),
        result = tokio::time::timeout(Duration::from_secs(12), async {
            self.socket
                .send(Message::Text(call.to_string()))
                .await
                .map_err(|_| "signin_browser_disconnected")?;
            while let Some(message) = self.socket.next().await {
                match message.map_err(|_| "signin_browser_disconnected")? {
                    Message::Text(text) => {
                        if text.len() > 8 * 1024 * 1024 {
                            return Err("signin_frame_too_large");
                        }
                        let value: Value = serde_json::from_str(&text)
                            .map_err(|_| "signin_browser_protocol_error")?;
                        if value["id"].as_u64() == Some(id) {
                            return if value.get("error").is_some() {
                                Err("signin_browser_command_failed")
                            } else {
                                Ok(value["result"].clone())
                            };
                        }
                    },
                    Message::Ping(data) => {
                        self.socket
                            .send(Message::Pong(data))
                            .await
                            .map_err(|_| "signin_browser_disconnected")?;
                    },
                    Message::Close(_) => return Err("signin_browser_disconnected"),
                    _ => {},
                }
            }
            Err("signin_browser_disconnected")
        }) => result.unwrap_or(Err("signin_browser_timeout"))
        }
    }

    async fn attach(&mut self) -> Result<()> {
        let previous = std::mem::take(&mut self.session);
        if !previous.is_empty() {
            let _ = self
                .command(
                    "Target.detachFromTarget",
                    json!({"sessionId":previous}),
                    false,
                )
                .await;
        }
        let reply = self
            .command(
                "Target.attachToTarget",
                json!({"targetId":self.target,"flatten":true}),
                false,
            )
            .await?;
        self.session = reply["sessionId"]
            .as_str()
            .ok_or("signin_browser_unavailable")?
            .to_owned();
        self.command(
            "Emulation.setDeviceMetricsOverride",
            json!({"width":WIDTH,"height":HEIGHT,"deviceScaleFactor":1,"mobile":false}),
            true,
        )
        .await?;
        if let Some(user_agent) = self.user_agent.clone() {
            // Best effort: a page that keeps the headless agent still works
            // for every provider that does not check it.
            let _ = self
                .command(
                    "Emulation.setUserAgentOverride",
                    json!({"userAgent":user_agent}),
                    true,
                )
                .await;
        }
        Ok(())
    }

    async fn current_origin(&mut self) -> Result<String> {
        // The entire process is private to this sign-in. Follow provider
        // popups, including an opener closing itself after sign-in.
        let result = self.command("Target.getTargets", json!({}), false).await?;
        let pages: Vec<&Value> = result["targetInfos"]
            .as_array()
            .ok_or("signin_browser_unavailable")?
            .iter()
            .filter(|value| value["type"] == "page")
            .collect();
        let page = pages
            .iter()
            .rev()
            .find(|page| page["openerId"].as_str() == Some(self.target.as_str()))
            .or_else(|| {
                pages
                    .iter()
                    .find(|page| page["targetId"].as_str() == Some(self.target.as_str()))
            })
            .or_else(|| {
                pages
                    .iter()
                    .find(|page| page["targetId"].as_str() == Some(self.root_target.as_str()))
            })
            .or_else(|| pages.last())
            .ok_or("signin_browser_closed")?;
        let target = page["targetId"]
            .as_str()
            .ok_or("signin_browser_unavailable")?;
        let raw_url = page["url"].as_str().unwrap_or_default();
        if raw_url.is_empty() {
            return Err("signin_page_loading");
        }
        if !allowed_page(raw_url) {
            return Err("signin_navigation_not_supported");
        }
        let origin = if raw_url == "about:blank" {
            "Loading…".into()
        } else {
            url::Url::parse(raw_url)
                .map(|url| url.origin().ascii_serialization())
                .unwrap_or_else(|_| "Loading…".into())
        };
        if target != self.target {
            self.target = target.to_owned();
            self.attach().await?;
        }
        Ok(origin)
    }

    pub async fn frame(&mut self) -> Result<Value> {
        self.current_origin().await?;
        let (revision, origin) = self.document().await?;
        let frame = self
            .command(
                "Page.captureScreenshot",
                json!({"format":"jpeg","quality":65,"captureBeyondViewport":false}),
                true,
            )
            .await?;
        // Navigation can happen between target discovery, the frame-tree
        // reply and the screenshot. Never label pixels with an older origin.
        if self.document().await? != (revision.clone(), origin.clone()) {
            return Err("signin_page_loading");
        }
        let image = frame["data"]
            .as_str()
            .filter(|image| image.len() <= 3 * 1024 * 1024)
            .ok_or("signin_frame_unavailable")?;
        Ok(
            json!({"image":image,"width":WIDTH,"height":HEIGHT,"origin":origin,"status":"waiting","frame_revision":revision}),
        )
    }

    async fn document(&mut self) -> Result<(String, String)> {
        let tree = self.command("Page.getFrameTree", json!({}), true).await?;
        document_identity(&self.target, &tree)
    }

    pub async fn input(&mut self, expected_revision: &str, input: HumanInput) -> Result<()> {
        let ready = |error| {
            if error == "signin_page_loading" {
                "signin_page_changed"
            } else {
                error
            }
        };
        self.current_origin().await.map_err(ready)?;
        if self.document().await.map_err(ready)?.0 != expected_revision {
            return Err("signin_page_changed");
        }
        self.dispatch(input).await
    }

    async fn dispatch(&mut self, input: HumanInput) -> Result<()> {
        match input {
            HumanInput::Tap { x, y }
                if x.is_finite()
                    && y.is_finite()
                    && (0.0..=1.0).contains(&x)
                    && (0.0..=1.0).contains(&y) =>
            {
                for kind in ["mousePressed", "mouseReleased"] {
                    self.command("Input.dispatchMouseEvent", json!({"type":kind,"x":x * WIDTH as f64,"y":y * HEIGHT as f64,"button":"left","buttons":if kind == "mousePressed" {1} else {0},"clickCount":1}), true).await?;
                }
            },
            HumanInput::Scroll { delta } if delta.is_finite() && delta.abs() <= 1000.0 => {
                self.command(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseWheel","x":195,"y":360,"deltaX":0,"deltaY":delta}),
                    true,
                )
                .await?;
            },
            HumanInput::Text { value }
                if !value.is_empty() && value.len() <= 4096 && !value.contains('\0') =>
            {
                self.command("Input.insertText", json!({"text":value}), true)
                    .await?;
            },
            HumanInput::Key { key } => {
                let (code, key_code) = match key.as_str() {
                    "Enter" => ("Enter", 13),
                    "Tab" => ("Tab", 9),
                    "Backspace" => ("Backspace", 8),
                    "Escape" => ("Escape", 27),
                    "ArrowLeft" => ("ArrowLeft", 37),
                    "ArrowRight" => ("ArrowRight", 39),
                    _ => return Err("signin_input_invalid"),
                };
                for kind in ["down", "up"] {
                    let mut params = json!({"type":if kind == "up" {"keyUp"} else if key == "Enter" {"keyDown"} else {"rawKeyDown"},"key":key,"code":code,"windowsVirtualKeyCode":key_code});
                    if kind == "down" && key == "Enter" {
                        params["text"] = json!("\r");
                        params["unmodifiedText"] = json!("\r");
                    }
                    #[cfg(target_os = "macos")]
                    if kind == "down" {
                        let command = match key.as_str() {
                            "Backspace" => Some("deleteBackward"),
                            "ArrowLeft" => Some("moveLeft"),
                            "ArrowRight" => Some("moveRight"),
                            _ => None,
                        };
                        if let Some(command) = command {
                            params["commands"] = json!([command]);
                        }
                    }
                    self.command("Input.dispatchKeyEvent", params, true).await?;
                }
            },
            _ => return Err("signin_input_invalid"),
        }
        Ok(())
    }
}

fn document_identity(target: &str, tree: &Value) -> Result<(String, String)> {
    let frame = &tree["frameTree"]["frame"];
    let loader = frame["loaderId"].as_str().ok_or("signin_page_loading")?;
    let raw = frame["url"].as_str().ok_or("signin_page_loading")?;
    if raw.is_empty() || raw == "about:blank" || loader.is_empty() {
        return Err("signin_page_loading");
    }
    if !allowed_page(raw) {
        return Err("signin_navigation_not_supported");
    }
    let origin = url::Url::parse(raw)
        .map_err(|_| "signin_navigation_not_supported")?
        .origin()
        .ascii_serialization();
    Ok((format!("{target}:{loader}"), origin))
}

// No Debug: entered text must never be formatted into diagnostics.
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HumanInput {
    Tap { x: f64, y: f64 },
    Scroll { delta: f64 },
    Text { value: String },
    Key { key: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_signin_never_opens_files_or_insecure_remote_pages() {
        for url in [
            "https://provider.example/login",
            "http://127.0.0.1:8765/callback",
            "http://[::1]:8765/callback",
            "about:blank",
        ] {
            assert!(allowed_page(url));
        }
        for url in [
            "file:///etc/passwd",
            "chrome://settings",
            "http://provider.example",
            "http://localhost.attacker.example",
            "https://user:password@provider.example",
            "javascript:alert(1)",
        ] {
            assert!(!allowed_page(url));
        }
    }
    #[test]
    fn pixels_and_the_site_label_use_the_same_document_snapshot() {
        let tree = json!({"frameTree":{"frame":{"loaderId":"document-a","url":"https://login.example/consent?opaque=secret"}}});
        assert_eq!(
            document_identity("tab", &tree).unwrap(),
            ("tab:document-a".into(), "https://login.example".into())
        );
        assert!(document_identity(
            "tab",
            &json!({"frameTree":{"frame":{"loaderId":"document-b","url":"file:///etc/passwd"}}})
        )
        .is_err());
        assert!(document_identity(
            "tab",
            &json!({"frameTree":{"frame":{"loaderId":"document-b","url":""}}})
        )
        .is_err());
    }

    #[test]
    fn viewer_cannot_supply_arbitrary_cdp_commands() {
        assert!(
            serde_json::from_value::<HumanInput>(json!({"kind":"tap","x":0.5,"y":0.5})).is_ok()
        );
        for value in [
            json!({"kind":"cdp","method":"Runtime.evaluate"}),
            json!({"kind":"text","value":"hello","method":"Runtime.evaluate"}),
        ] {
            assert!(serde_json::from_value::<HumanInput>(value).is_err());
        }
    }
}
