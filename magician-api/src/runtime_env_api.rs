//! Runtime environment file management for the web UI.
//!
//! The Skills page's Environment tab reads and writes the runtime's
//! `.env` / `.env.development` files — the value layer under
//! `magician-config.yaml` (which only NAMES env keys via `api_key_env`).
//! Everything goes through the sanctioned seam,
//! `magician_v2::runtime_settings`, so writes are atomic, 0600, and
//! re-synced into the live process env exactly the way the vibedev deploy
//! settings do.
//!
//! # Secret posture
//!
//! Values are **write-only**: `GET` returns key names with set/unset status
//! and never echoes values. This replaces interactive use of
//! `scripts/setup-identity.sh` / `ensure-ios-access.sh` / the setup TUI for
//! operators who prefer the UI; the scripts remain the CLI path.
//!
//! # Lockout guard
//!
//! A few keys can sever the operator's own access when set wrong
//! (`MAGICIAN_ADMIN_SECRET`, `MAGICIAN_BEARER_TOKEN`, the Cloudflare Access
//! pair). Writing those requires `acknowledge_lockout_risk: true`.

use std::collections::HashMap;
use std::path::PathBuf;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};

use magician::magician_v2::runtime_settings::{
    read_env_file_values, runtime_settings_paths, update_env_file,
};

use crate::secret_vault_api::SecretVaultApi;

/// Keys whose misconfiguration can lock the operator out of this very API.
const LOCKOUT_RISK_KEYS: [&str; 4] = [
    "MAGICIAN_ADMIN_SECRET",
    "MAGICIAN_BEARER_TOKEN",
    "CF_ACCESS_CLIENT_ID",
    "CF_ACCESS_CLIENT_SECRET",
];

#[derive(Clone)]
pub struct RuntimeEnvApi {
    /// Repo root, for locating `magician_data_v3/.env.example` (the key
    /// catalog: sections, notes, and which keys exist at all).
    repo_root: PathBuf,
}

impl RuntimeEnvApi {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }

    fn example_path(&self) -> PathBuf {
        self.repo_root.join("magician_data_v3/.env.example")
    }
}

#[derive(Debug, Serialize)]
struct EnvKeyStatus {
    set: bool,
    /// The real process env carries this key — file edits won't take effect
    /// for it until restart (real env beats both files).
    process_env_set: bool,
}

#[derive(Debug, Serialize)]
struct EnvFileStatus {
    keys: HashMap<String, EnvKeyStatus>,
}

#[derive(Debug, Serialize)]
struct CatalogKey {
    key: String,
    section: String,
    /// Heuristic by name (KEY/SECRET/TOKEN/PASSWORD): drives the UI's
    /// "secret — write-only" hint. Editing is write-only regardless.
    secret: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeEnvStatus {
    active_mode: String,
    env: EnvFileStatus,
    env_development: EnvFileStatus,
    catalog: Vec<CatalogKey>,
}

fn file_status(path: &std::path::Path) -> EnvFileStatus {
    let values = read_env_file_values(path);
    let mut keys = HashMap::new();
    for key in values.keys() {
        keys.insert(
            key.clone(),
            EnvKeyStatus {
                set: true,
                process_env_set: std::env::var(key)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false),
            },
        );
    }
    EnvFileStatus { keys }
}

fn looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
}

fn comment_body(line: &str) -> &str {
    line.trim().strip_prefix('#').map(str::trim).unwrap_or("")
}

fn is_banner_body(body: &str) -> bool {
    body.starts_with("=====")
}

/// Parse `.env.example` into a sectioned key catalog. Sections are the
/// title line bracketed by two `# =====` banner lines; keys appear as
/// `KEY=…` lines, commented or not.
fn parse_example_catalog(source: &str) -> Vec<CatalogKey> {
    let mut catalog: Vec<CatalogKey> = Vec::new();
    let mut section = String::from("GENERAL");
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<&str> = source.lines().collect();

    let push_key = |catalog: &mut Vec<CatalogKey>,
                    seen: &mut std::collections::HashSet<String>,
                    section: &str,
                    key: &str| {
        if seen.insert(key.to_string()) {
            catalog.push(CatalogKey {
                key: key.to_string(),
                section: section.to_string(),
                secret: looks_secret(key),
            });
        }
    };

    for (index, line) in lines.iter().enumerate() {
        let body = comment_body(line);
        if body.is_empty() {
            // Uncommented `KEY=…` line.
            if let Some(key) = rest_key(line.trim()) {
                push_key(&mut catalog, &mut seen, &section, key);
            }
            continue;
        }
        if is_banner_body(body) {
            continue;
        }
        // A plain comment bracketed by banners is the section title…
        let prev_is_banner = index > 0 && is_banner_body(comment_body(lines[index - 1]));
        let next_is_banner = lines
            .get(index + 1)
            .map(|next| is_banner_body(comment_body(next)))
            .unwrap_or(false);
        if prev_is_banner && next_is_banner && rest_key(body).is_none() {
            section = body.to_string();
            continue;
        }
        // …anything else is a commented `# KEY=…` example key.
        if let Some(key) = rest_key(body) {
            push_key(&mut catalog, &mut seen, &section, key);
        }
    }
    catalog
}

fn rest_key(line: &str) -> Option<&str> {
    let key = line.split('=').next()?.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let first = key.chars().next()?;
    if !first.is_ascii_uppercase() && !first.is_ascii_digit() {
        return None;
    }
    Some(key)
}

/// `GET /api/magician/v2/runtime/env`
///
/// Status-only view: set/unset per file, process-env overrides, active
/// target mode, and the key catalog from the template. No values, ever.
pub async fn get_runtime_env_handler(
    _req: HttpRequest,
    api: web::Data<RuntimeEnvApi>,
) -> HttpResponse {
    let paths = runtime_settings_paths();
    let catalog = std::fs::read_to_string(api.example_path())
        .map(|source| parse_example_catalog(&source))
        .unwrap_or_default();
    HttpResponse::Ok().json(RuntimeEnvStatus {
        active_mode: paths.env_target_mode.to_string(),
        env: file_status(&paths.env_path),
        env_development: file_status(&paths.env_development_path),
        catalog,
    })
}

#[derive(Debug, Deserialize)]
pub struct RuntimeEnvUpdateRequest {
    /// `".env"` or `".env.development"`.
    pub file: String,
    /// `KEY → value` to set/replace, `null` to delete.
    pub updates: HashMap<String, Option<String>>,
    #[serde(default)]
    pub acknowledge_lockout_risk: bool,
}

#[derive(Debug, Serialize)]
pub struct RuntimeEnvUpdateResponse {
    pub updated: Vec<String>,
    pub deleted: Vec<String>,
}

/// `PUT /api/magician/v2/runtime/env`
///
/// Admin (setup token). Writes through `runtime_settings::update_env_file`
/// (atomic, 0600) and re-syncs the touched keys into the live process env so
/// provider clients observe changes without a restart where possible.
pub async fn put_runtime_env_handler(
    req: HttpRequest,
    vault: web::Data<SecretVaultApi>,
    body: web::Json<RuntimeEnvUpdateRequest>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let paths = runtime_settings_paths();
    let target = match body.file.as_str() {
        ".env" => paths.env_path.clone(),
        ".env.development" => paths.env_development_path.clone(),
        other => return HttpResponse::BadRequest().json(serde_json::json!({
            "message": format!("unknown file {other:?} — expected \".env\" or \".env.development\"")
        })),
    };

    let mut updated: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut pairs: Vec<(&str, Option<String>)> = Vec::new();
    for (key, value) in &body.updates {
        if rest_key(key).is_none() {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "message": format!("invalid env key {key:?}")
            }));
        }
        if LOCKOUT_RISK_KEYS.contains(&key.as_str()) && !body.acknowledge_lockout_risk {
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "message": format!(
                    "writing {key} can lock you out of this API — retry with acknowledge_lockout_risk: true"
                )
            }));
        }
        match value {
            Some(v) if v.trim().is_empty() => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "message": format!("{key}: empty values are not allowed — send null to delete")
                }))
            },
            Some(_) => updated.push(key.clone()),
            None => deleted.push(key.clone()),
        }
        pairs.push((key.as_str(), value.clone()));
    }
    if pairs.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "message": "updates must contain at least one key"
        }));
    }

    if let Err(e) = update_env_file(&target, &pairs) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "message": format!("env write failed: {e}")
        }));
    }
    // Boot loads BOTH files (.env.development first, dev-wins); syncing from
    // the single written file would diverge the live env from any restart
    // (e.g. deleting a key from .env while .env.development still sets it).
    // Recompute each touched key across both files in the same precedence.
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
    let dev_values = read_env_file_values(&paths.env_development_path);
    let prod_values = read_env_file_values(&paths.env_path);
    for key in keys {
        let effective = dev_values
            .get(key)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| prod_values.get(key).filter(|v| !v.trim().is_empty()));
        if let Some(value) = effective {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    tracing::info!(
        file = %body.file,
        updated = ?updated,
        deleted = ?deleted,
        "runtime_env_api: env updated"
    );

    HttpResponse::Ok().json(RuntimeEnvUpdateResponse { updated, deleted })
}

/// Register the runtime env routes under `/api/magician/v2`.
pub fn configure_runtime_env_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/runtime/env", web::get().to(get_runtime_env_handler))
        .route("/runtime/env", web::put().to(put_runtime_env_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_catalog_sections_and_secret_flags() {
        let source = "# Environment variables\n\
            # =====================================================================\n\
            # OPERATOR IDENTITY  (the identity layer)\n\
            # =====================================================================\n\
            # MAGICIAN_OWNER_NAME=\"Alex\"\n\
            OPENAI_API_KEY=sk-your-openai-key\n\
            # =====================================================================\n\
            # ORCHESTRATION  (flags)\n\
            # =====================================================================\n\
            MAGICIAN_TUNNEL_MODE=local\n";
        let catalog = parse_example_catalog(source);
        let by_key: HashMap<String, &CatalogKey> =
            catalog.iter().map(|c| (c.key.clone(), c)).collect();
        assert_eq!(
            by_key["MAGICIAN_OWNER_NAME"].section,
            "OPERATOR IDENTITY  (the identity layer)"
        );
        assert!(by_key["OPENAI_API_KEY"].secret);
        assert!(!by_key["MAGICIAN_TUNNEL_MODE"].secret);
        assert_eq!(
            by_key["MAGICIAN_TUNNEL_MODE"].section,
            "ORCHESTRATION  (flags)"
        );
    }

    #[test]
    fn rest_key_rejects_non_env_keys() {
        assert_eq!(rest_key("OK_KEY=\"x\""), Some("OK_KEY"));
        // A raw comment line is not a key; the commented-key path strips "# " first.
        assert_eq!(rest_key("# COMMENTED_KEY=1"), None);
        assert_eq!(rest_key("lower_case=1"), None);
        assert_eq!(rest_key("no-equals"), None);
        assert_eq!(rest_key("WITH SPACE=1"), None);
    }
}
