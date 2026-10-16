//! Machine registry: which Magicians this owner runs, each behind its own
//! Cloudflare tunnel (`scripts/ensure-machine-tunnel.sh`).
//!
//! The list lives in `$MAGICIAN_ROOT_DIR/remote-machines.yaml`, owner data that
//! is never in git (sample: `magician_data_v3/remote-machines.example.yaml`). It
//! is a separate file because `magician-config.yaml` rejects unknown top-level
//! keys. Apps read it through `GET /api/magician/v2/remote-machines` to show
//! which machine they use and to switch between machines they have paired.
//! The response carries no secrets and no webhook origins.
//!
//! The file is re-read when its mtime changes, so edits apply without a
//! restart. One bad entry is dropped and logged; it never empties the list.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::device_pairing_api::{normalized_public_origin, scope, MobileEnrollmentConfig};

/// The registry file under the runtime root.
pub const REGISTRY_FILE_NAME: &str = "remote-machines.yaml";
/// Bound on how many machines one registry may list.
const MAX_MACHINES: usize = 32;
/// Bound on the registry file read from disk.
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteMachine {
    pub name: String,
    pub api_origin: String,
    pub ui_origin: String,
}

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    machines: Vec<RegistryEntry>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    name: String,
    api_origin: String,
    ui_origin: String,
    // Written by the script for the operator; never served to apps.
    #[serde(default)]
    #[allow(dead_code)]
    webhook_origin: Option<String>,
}

/// Lowercase letters, digits and dashes, 1–31 characters, not starting with a dash.
pub fn valid_machine_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 31
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parse and validate a registry document. Invalid or duplicate entries are
/// dropped with a warning; parse failure yields an empty list.
pub fn parse_registry(text: &str) -> Vec<RemoteMachine> {
    let file: RegistryFile = match serde_yaml::from_str(text) {
        Ok(file) => file,
        Err(error) => {
            warn!("[REMOTE-MACHINES] registry is not valid YAML; serving no machines: {error}");
            return Vec::new();
        },
    };
    let mut names = HashSet::new();
    let mut machines = Vec::new();
    for entry in file.machines {
        if machines.len() == MAX_MACHINES {
            warn!("[REMOTE-MACHINES] registry lists more than {MAX_MACHINES} machines; the rest are ignored");
            break;
        }
        if !valid_machine_name(&entry.name) {
            warn!("[REMOTE-MACHINES] dropping an entry with an invalid machine name");
            continue;
        }
        let (Ok(api_origin), Ok(ui_origin)) = (
            normalized_public_origin(&entry.api_origin),
            normalized_public_origin(&entry.ui_origin),
        ) else {
            warn!(
                "[REMOTE-MACHINES] dropping {}: its origins must be bare https origins",
                entry.name
            );
            continue;
        };
        if !names.insert(entry.name.clone()) {
            warn!(
                "[REMOTE-MACHINES] dropping a duplicate entry for {}",
                entry.name
            );
            continue;
        }
        machines.push(RemoteMachine {
            name: entry.name,
            api_origin,
            ui_origin,
        });
    }
    machines
}

/// Which entry is this machine: the one serving the configured public origin,
/// else the one `MAGICIAN_MACHINE_NAME` names.
pub fn resolve_self<'a>(
    machines: &'a [RemoteMachine],
    public_origin: Option<&str>,
    machine_name: Option<&str>,
) -> Option<&'a RemoteMachine> {
    public_origin
        .and_then(|origin| machines.iter().find(|m| m.api_origin == origin))
        .or_else(|| machine_name.and_then(|name| machines.iter().find(|m| m.name == name)))
}

/// This machine's name from the runtime env, when it is a valid name.
pub fn configured_machine_name() -> Option<String> {
    std::env::var("MAGICIAN_MACHINE_NAME")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| valid_machine_name(name))
}

/// The registry file plus an mtime-keyed cache of its parsed contents.
pub struct RemoteMachineRegistry {
    path: PathBuf,
    cache: Mutex<Option<(SystemTime, Arc<Vec<RemoteMachine>>)>>,
}

impl RemoteMachineRegistry {
    pub fn new(runtime_root: &Path) -> Self {
        Self::at(runtime_root.join(REGISTRY_FILE_NAME))
    }

    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }

    /// The current machines; an absent file is an empty registry.
    pub fn machines(&self) -> Arc<Vec<RemoteMachine>> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            *self.cache.lock() = None;
            return Arc::new(Vec::new());
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if let Some((cached_at, machines)) = self.cache.lock().as_ref() {
            if *cached_at == modified {
                return Arc::clone(machines);
            }
        }
        let machines = if metadata.len() > MAX_REGISTRY_BYTES {
            warn!("[REMOTE-MACHINES] registry exceeds {MAX_REGISTRY_BYTES} bytes; serving no machines");
            Vec::new()
        } else {
            std::fs::read_to_string(&self.path)
                .map(|text| parse_registry(&text))
                .unwrap_or_else(|error| {
                    warn!("[REMOTE-MACHINES] registry could not be read: {error}");
                    Vec::new()
                })
        };
        let machines = Arc::new(machines);
        *self.cache.lock() = Some((modified, Arc::clone(&machines)));
        machines
    }
}

/// `GET /api/magician/v2/remote-machines`
///
/// The machines this owner runs, with this one marked `is_self`. Origins only:
/// each app pairs with and authenticates to every machine separately.
pub async fn list_remote_machines_handler(
    req: HttpRequest,
    registry: web::Data<Arc<RemoteMachineRegistry>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> Result<HttpResponse> {
    if let Err(response) = scope(&req) {
        return Ok(response);
    }
    let machines = registry.machines();
    let machine_name = configured_machine_name();
    let self_name = resolve_self(
        &machines,
        enrollment_config.public_origin.as_deref(),
        machine_name.as_deref(),
    )
    .map(|machine| machine.name.clone());
    let listed: Vec<_> = machines
        .iter()
        .map(|machine| {
            json!({
                "name": machine.name,
                "api_origin": machine.api_origin,
                "ui_origin": machine.ui_origin,
                "is_self": self_name.as_deref() == Some(machine.name.as_str()),
            })
        })
        .collect();
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(json!({
            "self": self_name.or(machine_name),
            "machines": listed,
        })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
machines:
  - name: studio
    api_origin: https://studio.example.com/
    ui_origin: https://ui-studio.example.com
    webhook_origin: https://webhook-studio.example.com
  - name: laptop
    api_origin: https://laptop.example.com
    ui_origin: https://ui-laptop.example.com
";

    #[test]
    fn registry_parses_and_normalizes_origins() {
        let machines = parse_registry(SAMPLE);
        assert_eq!(machines.len(), 2);
        assert_eq!(machines[0].name, "studio");
        assert_eq!(machines[0].api_origin, "https://studio.example.com");
        assert_eq!(machines[1].ui_origin, "https://ui-laptop.example.com");
    }

    #[test]
    fn bad_entries_are_dropped_without_emptying_the_list() {
        let machines = parse_registry(
            "machines:
  - name: Bad_Name
    api_origin: https://a.example.com
    ui_origin: https://ui-a.example.com
  - name: plain-http
    api_origin: http://b.example.com
    ui_origin: https://ui-b.example.com
  - name: with-path
    api_origin: https://c.example.com/api
    ui_origin: https://ui-c.example.com
  - name: good
    api_origin: https://good.example.com
    ui_origin: https://ui-good.example.com
  - name: good
    api_origin: https://dup.example.com
    ui_origin: https://ui-dup.example.com
",
        );
        assert_eq!(
            machines.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["good"]
        );
        assert_eq!(machines[0].api_origin, "https://good.example.com");
        assert!(parse_registry("not: [valid").is_empty());
        assert!(parse_registry("").is_empty());
    }

    #[test]
    fn self_is_the_public_origin_then_the_configured_name() {
        let machines = parse_registry(SAMPLE);
        assert_eq!(
            resolve_self(
                &machines,
                Some("https://laptop.example.com"),
                Some("studio")
            )
            .map(|m| m.name.as_str()),
            Some("laptop"),
            "the origin this machine serves wins"
        );
        assert_eq!(
            resolve_self(&machines, Some("https://other.example.com"), Some("studio"))
                .map(|m| m.name.as_str()),
            Some("studio")
        );
        assert!(resolve_self(&machines, None, None).is_none());
    }

    #[test]
    fn machine_names_are_dns_labels() {
        assert!(valid_machine_name("macbookpro"));
        assert!(valid_machine_name("mac-studio-2"));
        assert!(!valid_machine_name(""));
        assert!(!valid_machine_name("-studio"));
        assert!(!valid_machine_name("Studio"));
        assert!(!valid_machine_name("a.b"));
        assert!(!valid_machine_name(&"a".repeat(32)));
    }

    #[test]
    fn registry_rereads_the_file_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = RemoteMachineRegistry::new(dir.path());
        assert!(
            registry.machines().is_empty(),
            "no file is an empty registry"
        );
        let path = dir.path().join(REGISTRY_FILE_NAME);
        std::fs::write(&path, SAMPLE).unwrap();
        assert_eq!(registry.machines().len(), 2);
        std::fs::write(
            &path,
            "machines:\n  - name: solo\n    api_origin: https://solo.example.com\n    ui_origin: https://ui-solo.example.com\n",
        )
        .unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(registry.machines()[0].name, "solo");
    }

    #[actix_web::test]
    async fn endpoint_lists_machines_and_marks_self_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(REGISTRY_FILE_NAME), SAMPLE).unwrap();
        let registry = web::Data::new(Arc::new(RemoteMachineRegistry::new(dir.path())));
        let config = web::Data::new(
            MobileEnrollmentConfig::resolve(Some("https://studio.example.com")).unwrap(),
        );
        let response = list_remote_machines_handler(
            actix_web::test::TestRequest::default()
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request(),
            registry,
            config,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["self"], "studio");
        assert_eq!(body["machines"][0]["is_self"], true);
        assert_eq!(body["machines"][1]["is_self"], false);
        assert_eq!(
            body["machines"][1]["api_origin"],
            "https://laptop.example.com"
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("webhook"));
    }
}
