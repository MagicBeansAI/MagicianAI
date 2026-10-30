//! Executing a [`ProbeSpec`] against the running machine.
//!
//! Behind the `probe` feature, because the model and resolver are I/O-free and
//! most callers only reason about a [`Report`](crate::Report) someone else
//! produced.
//!
//! # What a probe is allowed to do
//!
//! Answer "is this component usable from here, right now", cheaply enough to run
//! on a schedule. That rules out anything that costs a third party a request. A
//! configured API key reports as **present when it is set**, not when it has been
//! validated against the provider: validation is a round trip to someone else's
//! service on every poll, and a rate limit or an outage there would read as the
//! operator's component being broken. Checking a key really works is a separate,
//! explicit action a person takes, not a background probe.
//!
//! Every probe is fail-soft. A probe that cannot reach a thing reports
//! [`Observed::Absent`]; a probe that cannot answer at all reports
//! [`Observed::Unknown`], which is a real answer and not an error — macOS gives
//! no way to read microphone or screen-recording permission from a process that
//! does not hold it.

// Only the network path needs this, and that path is feature-gated.
#[cfg(feature = "probe")]
use std::time::Duration;

use crate::{Observed, ProbeSpec};

/// Explicit local-Desktop attestation for terminal setup. Generic engine
/// `DesktopPermission` probes still remain unknown: only the signed host can
/// answer for its native grants.
pub async fn desktop_permissions(url: &str) -> std::collections::BTreeMap<String, Observed> {
    match request(url, 15000).await {
        Ok((200, body)) => decode_desktop_permissions(&body),
        _ => Default::default(),
    }
}

fn decode_desktop_permissions(body: &str) -> std::collections::BTreeMap<String, Observed> {
    #[derive(serde::Deserialize)]
    struct Permission {
        key: String,
        state: String,
    }
    // JSON is a YAML subset; keep the shared crate's existing dependency set.
    let Ok(permissions) = serde_yaml::from_str::<Vec<Permission>>(body) else {
        return Default::default();
    };
    let mut observed = std::collections::BTreeMap::new();
    for permission in permissions {
        let state = match permission.state.as_str() {
            "granted" => {
                Observed::Present(format!("{} access verified by Desktop", permission.key))
            },
            "missing" => {
                Observed::Absent(format!("{} access is missing in Desktop", permission.key))
            },
            _ => Observed::Unknown(format!(
                "Desktop could not verify {} access",
                permission.key
            )),
        };
        if observed.insert(permission.key, state).is_some() {
            return Default::default();
        }
    }
    observed
}

/// Run one spec. Never panics and never propagates an error: an unreachable
/// component is a fact about the stack, not a failure of the check.
pub async fn observe(spec: &ProbeSpec) -> Observed {
    match spec {
        ProbeSpec::HttpConfigured {
            origin_key,
            files,
            path,
            headers,
            timeout_ms,
        } => observe_configured_http(origin_key, files, path, headers, *timeout_ms).await,
        ProbeSpec::HttpOk { url, timeout_ms } => match request(url, *timeout_ms).await {
            Ok((status, _)) if (200..300).contains(&status) => {
                Observed::Present(format!("{url} answered {status}"))
            },
            Ok((status, _)) => Observed::Absent(format!("{url} answered {status}")),
            Err(e) => Observed::Absent(format!("{url} unreachable ({e})")),
        },

        // Any HTTP response proves the hop routed through to a live upstream.
        // The Kapso webhook is POST-only, so a GET legitimately answers 4xx and
        // treating that as down would report a working tunnel as broken.
        ProbeSpec::HttpAnyResponse { url, timeout_ms } => match request(url, *timeout_ms).await {
            Ok((status, _)) => Observed::Present(format!("{url} answered {status}")),
            Err(e) => Observed::Absent(format!("{url} unreachable ({e})")),
        },

        ProbeSpec::HttpBodyContains {
            url,
            needle,
            timeout_ms,
        } => match request(url, *timeout_ms).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                if body.contains(needle.as_str()) {
                    Observed::Present(format!("{url} answered {status}"))
                } else {
                    Observed::Absent(format!("{url} answered {status} without {needle:?}"))
                }
            },
            Ok((status, _)) => Observed::Absent(format!("{url} answered {status}")),
            Err(e) => Observed::Absent(format!("{url} unreachable ({e})")),
        },

        ProbeSpec::ConfiguredOllamaModelPresent {
            url,
            config_path,
            key_path,
            timeout_ms,
        } => {
            let selected = match selected_yaml_string(config_path, key_path) {
                Ok(selected) => selected,
                Err(error) => return Observed::Unknown(error),
            };
            match request(url, *timeout_ms).await {
                Ok((status, body)) if (200..300).contains(&status) => match ollama_models(&body) {
                    Ok(models) if models.iter().any(|model| model_matches(model, &selected)) => {
                        Observed::Present(format!("{selected} is installed in Ollama"))
                    },
                    Ok(_) => Observed::Absent(format!(
                        "selected local model {selected} is not installed in Ollama"
                    )),
                    Err(error) => Observed::Unknown(format!(
                        "{url} returned an unreadable model catalog ({error})"
                    )),
                },
                Ok((status, _)) => Observed::Absent(format!("{url} answered {status}")),
                Err(error) => Observed::Absent(format!("{url} unreachable ({error})")),
            }
        },

        ProbeSpec::FileExists { path } => {
            if std::path::Path::new(path).exists() {
                Observed::Present(format!("{path} exists"))
            } else {
                Observed::Absent(format!("{path} is not there"))
            }
        },

        ProbeSpec::FileInventory { path } => observe_inventory(path),

        ProbeSpec::FileReadable { path, because } => {
            // A read attempt, not a stat. This is the same test the desktop app
            // settled on for Full Disk Access, and for the same reason: TCC
            // refuses the open, not the lookup.
            match std::fs::File::open(path) {
                Ok(_) => Observed::Present(format!("{path} is readable")),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    Observed::Absent(because.clone())
                },
                // Not there at all. Whether the permission would be granted is
                // unanswerable from here, and guessing either way is wrong.
                Err(e) => Observed::Unknown(format!("{path} could not be read: {e}")),
            }
        },

        ProbeSpec::EnvKeyPresent { keys, files } => observe_key(keys, files),

        ProbeSpec::EnvKeysAllPresent { keys, files } => observe_all_keys(keys, files),

        ProbeSpec::DesktopPermission { key } => {
            Observed::Unknown(format!("Verify {key} access in the connected desktop app"))
        },

        // Nothing to run. The caller has to ask a person, and saying so is more
        // useful than guessing and being confidently wrong.
        ProbeSpec::Manual { hint } => Observed::Unknown(hint.clone()),

        ProbeSpec::AnyOf { probes } => observe_any(probes).await,
    }
}

fn configured_value(key: &str, files: &[String]) -> Option<String> {
    // Fresh installer file values win over startup env, including explicit empties.
    for file in files {
        if let Ok(contents) = std::fs::read_to_string(file) {
            for line in contents.lines() {
                let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
                let Some((name, value)) = line.split_once('=') else {
                    continue;
                };
                if name.trim() == key {
                    return Some(value.trim().trim_matches(['"', '\'']).to_string());
                }
            }
        }
    }
    std::env::var(key).ok()
}

#[cfg(feature = "probe")]
async fn observe_configured_http(
    origin_key: &str,
    files: &[String],
    path: &str,
    headers: &std::collections::BTreeMap<String, String>,
    timeout_ms: u64,
) -> Observed {
    let Some(origin) = configured_value(origin_key, files).filter(|value| !value.is_empty()) else {
        return Observed::Absent(format!("{origin_key} is not configured"));
    };
    let Ok(mut url) = reqwest::Url::parse(&origin) else {
        return Observed::Absent("configured origin is invalid".into());
    };
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#'])
    {
        return Observed::Absent(
            "health checks require an HTTPS origin and an absolute path".into(),
        );
    }
    url.set_path(path);
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    else {
        return Observed::Unknown("health client unavailable".into());
    };
    let mut request = client.get(url);
    for (name, key) in headers {
        let Some(value) = configured_value(key, files).filter(|value| !value.is_empty()) else {
            return Observed::Absent(format!("{key} is not configured"));
        };
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            return Observed::Unknown("invalid health header name".into());
        };
        let Ok(mut value) = reqwest::header::HeaderValue::from_str(&value) else {
            return Observed::Absent("invalid configured health credential".into());
        };
        value.set_sensitive(true);
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(response) if response.status().is_success() => {
            Observed::Present("configured origin passed its authenticated health check".into())
        },
        Ok(response) => Observed::Absent(format!(
            "configured origin answered {} (redirects are refused)",
            response.status().as_u16()
        )),
        Err(_) => Observed::Absent("configured origin health check failed or timed out".into()),
    }
}

#[cfg(not(feature = "probe"))]
async fn observe_configured_http(
    _origin_key: &str,
    _files: &[String],
    _path: &str,
    _headers: &std::collections::BTreeMap<String, String>,
    _timeout_ms: u64,
) -> Observed {
    Observed::Unknown("built without the `probe` feature".into())
}

fn observe_inventory(path: &str) -> Observed {
    #[derive(serde::Deserialize)]
    struct Inventory {
        schema_version: u32,
        #[serde(default)]
        verification_revision: u32,
        files: Vec<InventoryFile>,
    }
    #[derive(serde::Deserialize)]
    struct InventoryFile {
        path: String,
        #[serde(default)]
        executable: bool,
        size_bytes: u64,
        modified_ns: u64,
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            return Observed::Absent(format!("runtime inventory {path} unavailable: {error}"))
        },
    };
    let inventory: Inventory = match serde_yaml::from_str(&contents) {
        Ok(inventory) => inventory,
        Err(error) => {
            return Observed::Absent(format!("runtime inventory {path} invalid: {error}"))
        },
    };
    if inventory.schema_version != 1 || inventory.files.is_empty() {
        return Observed::Absent(format!(
            "runtime inventory {path} has no supported file list"
        ));
    }
    if inventory.verification_revision != 2 {
        return Observed::Absent(format!(
            "runtime inventory {path} needs a fresh setup verification"
        ));
    }
    let root = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    for file in &inventory.files {
        let candidate = root.join(&file.path);
        let metadata = match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
            _ => {
                return Observed::Absent(format!(
                    "required runtime file {} missing or empty",
                    candidate.display()
                ))
            },
        };
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        if metadata.len() != file.size_bytes || modified_ns != Some(u128::from(file.modified_ns)) {
            return Observed::Absent(format!(
                "runtime file {} changed; rerun setup verification",
                candidate.display()
            ));
        }
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Observed::Absent(format!(
                    "runtime file {} is not executable",
                    candidate.display()
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = (metadata, file.executable);
    }
    Observed::Present(format!(
        "{} installed runtime files verified",
        inventory.files.len()
    ))
}

/// Routes run in order and the first present one wins. The rest are not run:
/// a working route answers the question, and the later ones are typically
/// slower network checks.
async fn observe_any(probes: &[ProbeSpec]) -> Observed {
    let mut absent = Vec::new();
    let mut unknown = Vec::new();
    for probe in probes {
        // Boxed: an async fn cannot hold itself inline.
        match Box::pin(observe(probe)).await {
            Observed::Present(detail) => return Observed::Present(detail),
            Observed::Absent(detail) => absent.push(detail),
            Observed::Unknown(detail) => unknown.push(detail),
        }
    }
    if probes.is_empty() {
        return Observed::Unknown("no probe routes are declared".to_string());
    }
    if absent.is_empty() {
        Observed::Unknown(unknown.join("; "))
    } else {
        absent.extend(unknown);
        Observed::Absent(absent.join("; "))
    }
}

fn selected_yaml_string(config_path: &str, key_path: &[String]) -> Result<String, String> {
    if key_path.is_empty() {
        return Err("configured-model probe has an empty key path".to_string());
    }
    let source = std::fs::read_to_string(config_path)
        .map_err(|error| format!("{config_path} could not be read: {error}"))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&source)
        .map_err(|error| format!("{config_path} is not valid YAML: {error}"))?;
    for key in key_path {
        value = value
            .as_mapping()
            .and_then(|mapping| mapping.get(&serde_yaml::Value::String(key.clone())))
            .cloned()
            .ok_or_else(|| format!("{config_path} has no {} setting", key_path.join(".")))?;
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not a model name", key_path.join(".")))
}

fn ollama_models(body: &str) -> Result<Vec<String>, String> {
    let value: serde_yaml::Value = serde_yaml::from_str(body).map_err(|error| error.to_string())?;
    Ok(value
        .as_mapping()
        .and_then(|mapping| mapping.get(&serde_yaml::Value::String("models".to_string())))
        .and_then(serde_yaml::Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let mapping = row.as_mapping()?;
            mapping
                .get(&serde_yaml::Value::String("name".to_string()))
                .or_else(|| mapping.get(&serde_yaml::Value::String("model".to_string())))
                .and_then(serde_yaml::Value::as_str)
                .map(str::to_string)
        })
        .collect())
}

fn model_matches(installed: &str, selected: &str) -> bool {
    installed == selected
        || installed
            .strip_suffix(":latest")
            .is_some_and(|installed| installed == selected)
}

/// What this machine is, for the `host` block on a component to be judged
/// against. Observation like any other probe, which is why it lives here rather
/// than in the resolver.
///
/// Architecture is normalised to one spelling. `uname -m` says `arm64` on macOS
/// and `aarch64` on Linux for the same silicon, and a graph that had to list
/// both would be stating a detail of two operating systems rather than a
/// requirement.
#[cfg(feature = "probe")]
pub fn detect_host() -> crate::Host {
    crate::Host {
        os: std::env::consts::OS.to_string(),
        arch: normalise_arch(std::env::consts::ARCH),
        memory_gb: physical_memory_gb(),
    }
}

#[cfg(feature = "probe")]
fn normalise_arch(arch: &str) -> String {
    match arch {
        "aarch64" | "arm64" => "arm64".to_string(),
        "x86_64" | "amd64" => "x86_64".to_string(),
        other => other.to_string(),
    }
}

/// Physical memory in whole GiB, or 0 when it cannot be read.
///
/// Zero rather than a guess: every caller treats it as "does not meet the bar",
/// and guessing high would offer a model the machine cannot run and then blame
/// whoever accepted the offer.
#[cfg(feature = "probe")]
fn physical_memory_gb() -> u32 {
    let bytes: u64 = if cfg!(target_os = "macos") {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    } else {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("MemTotal:"))
                    .and_then(|v| v.split_whitespace().next().map(str::to_string))
            })
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    (bytes / 1024 / 1024 / 1024) as u32
}

/// Resolve every spec in a graph, in order.
///
/// Sequential rather than concurrent: these are localhost calls with short
/// timeouts, and a component list is small. Concurrency here would buy
/// milliseconds and cost the caller a runtime dependency.
#[cfg(feature = "probe")]
pub async fn observe_all(
    components: &[crate::Component],
) -> std::collections::BTreeMap<String, Observed> {
    let mut out = std::collections::BTreeMap::new();
    for component in components {
        out.insert(component.id.clone(), observe(&component.probe).await);
    }
    out
}

/// A key counts as configured when it is set in the process environment or
/// assigned a non-empty value in one of the given env files. The environment
/// wins, matching how every consumer resolves it.
fn observe_key(keys: &[String], files: &[String]) -> Observed {
    // Any-of. The environment is checked for every key before any file, because
    // that is the order every consumer resolves them in.
    for key in keys {
        if std::env::var(key)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            return Observed::Present(format!("{key} is set in the environment"));
        }
    }
    for file in files {
        let Ok(contents) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if !keys.iter().any(|k| k == name) {
                continue;
            }
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Observed::Present(format!("{name} is set in {file}"));
            }
        }
    }
    // "none of X" reads wrong for a single key, and most probes have one.
    let subject = match keys {
        [one] => format!("{one} is not set"),
        many => format!("none of {} is set", many.join(", ")),
    };
    Observed::Absent(format!(
        "{subject} in the environment or {}",
        describe(files)
    ))
}

fn observe_all_keys(keys: &[String], files: &[String]) -> Observed {
    let present = configured_keys(keys, files);
    let missing: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|key| !present.contains(*key))
        .collect();
    if missing.is_empty() {
        return Observed::Present(format!(
            "{} are set in the environment or configured files",
            keys.join(", ")
        ));
    }
    Observed::Absent(format!(
        "{} {} not set in the environment or {}",
        missing.join(", "),
        if missing.len() == 1 { "is" } else { "are" },
        describe(files)
    ))
}

fn configured_keys(keys: &[String], files: &[String]) -> std::collections::BTreeSet<String> {
    let mut present = std::collections::BTreeSet::new();
    for key in keys {
        if std::env::var(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            present.insert(key.clone());
        }
    }
    for file in files {
        let Ok(contents) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if !keys.iter().any(|key| key == name) {
                continue;
            }
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                present.insert(name.to_string());
            }
        }
    }
    present
}

fn describe(items: &[String]) -> String {
    match items {
        [] => "any file".to_string(),
        [one] => one.clone(),
        many => many.join(", "),
    }
}

#[cfg(feature = "probe")]
async fn request(url: &str, timeout_ms: u64) -> Result<(u16, String), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        // A component probe asks about *this* machine. Sending a localhost health
        // check through a corporate proxy is how a probe reports a service down
        // that is running fine three ports away.
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(short_error)?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok((status, body))
}

/// Without the feature there is no client, so every network spec is honestly
/// unanswerable rather than quietly absent.
#[cfg(not(feature = "probe"))]
async fn request(_url: &str, _timeout_ms: u64) -> Result<(u16, String), String> {
    Err("built without the `probe` feature".to_string())
}

/// reqwest's Display walks the whole source chain and produces a paragraph. A
/// status line has room for a phrase.
#[cfg(feature = "probe")]
fn short_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "connection refused".to_string()
    } else {
        "request failed".to_string()
    }
}

#[cfg(all(test, feature = "probe"))]
mod tests;
