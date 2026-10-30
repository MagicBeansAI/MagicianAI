//! Standing approvals for browser form submissions ("Approve Always").
//!
//! A grant is per site origin and lasts until the owner revokes it
//! (`DELETE /api/magician/v2/browser/submission-grants`). It lives in the
//! scope's runtime tree, so it survives restarts and is never inferred: only
//! an owner's answer on a `browser_submission_approval` card writes one. An
//! unreadable file is read as "no grants", so a damaged store asks again
//! rather than letting a submission through.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// Serialises read-modify-write cycles on the grants file within the process.
static FILE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionGrant {
    pub origin: String,
    pub granted_at_ms: i64,
    pub request_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    grants: Vec<SubmissionGrant>,
}

/// Where a scope keeps its grants.
pub fn grants_path(workspace: &ArtifactV2Workspace, principal: &str, scope: &str) -> PathBuf {
    workspace
        .runtime_root(principal, scope)
        .join("browser")
        .join("submission_grants.json")
}

/// `scheme://host[:port]` of an http(s) page, lower-cased; `None` for anything
/// else (a grant is never made for a page without a clear site).
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.to_ascii_lowercase();
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

fn read(path: &Path) -> GrantFile {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write(path: &Path, file: &GrantFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    magician_core::durable_io::write_bytes_durably_sync(path, &serde_json::to_vec_pretty(file)?)
}

pub fn list(path: &Path) -> Vec<SubmissionGrant> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read(path).grants
}

pub fn is_granted(path: &Path, origin: &str) -> bool {
    list(path).iter().any(|grant| grant.origin == origin)
}

/// Record a grant; one per origin, the newest answer replacing an older one.
pub fn grant(path: &Path, grant: SubmissionGrant) -> std::io::Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = read(path);
    file.grants
        .retain(|existing| existing.origin != grant.origin);
    file.grants.push(grant);
    write(path, &file)
}

/// Remove the grant for `origin`, or every grant when `origin` is `None`.
/// Returns how many were removed.
pub fn revoke(path: &Path, origin: Option<&str>) -> std::io::Result<usize> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = read(path);
    let before = file.grants.len();
    file.grants
        .retain(|existing| origin.is_some_and(|origin| existing.origin != origin));
    let removed = before - file.grants.len();
    if removed > 0 {
        write(path, &file)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(origin: &str) -> SubmissionGrant {
        SubmissionGrant {
            origin: origin.to_owned(),
            granted_at_ms: 1,
            request_id: "req-1".to_owned(),
            agent_id: None,
        }
    }

    #[test]
    fn origin_is_scheme_and_host_only() {
        assert_eq!(
            origin_of("https://X.com/compose/post?a=1#f").as_deref(),
            Some("https://x.com")
        );
        assert_eq!(
            origin_of("http://user:pw@localhost:3000/x").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("file:///etc/passwd"), None);
        assert_eq!(origin_of(""), None);
    }

    #[test]
    fn a_grant_persists_and_revokes_one_or_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("browser").join("g.json");
        assert!(!is_granted(&path, "https://x.com"));
        grant(&path, entry("https://x.com")).unwrap();
        grant(&path, entry("https://x.com")).unwrap();
        grant(&path, entry("https://linkedin.com")).unwrap();
        assert_eq!(list(&path).len(), 2);
        assert!(is_granted(&path, "https://x.com"));
        assert_eq!(revoke(&path, Some("https://x.com")).unwrap(), 1);
        assert!(!is_granted(&path, "https://x.com"));
        assert_eq!(revoke(&path, None).unwrap(), 1);
        assert!(list(&path).is_empty());
    }

    #[test]
    fn a_damaged_store_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(!is_granted(&path, "https://x.com"));
    }
}
