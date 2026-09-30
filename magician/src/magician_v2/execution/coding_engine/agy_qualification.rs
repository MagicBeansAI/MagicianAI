//! Phase A2/A3: qualify an Antigravity launch from `event: init` evidence.
//!
//! Request handlers never call this. They only read a cached receipt that
//! matches the current filesystem identity **and** CLI version. Live probes
//! wait for the background tick; this module is the fail-closed classifier
//! those probes (and tests) feed. Missing tool lists / `permission_mode`
//! stay Unqualified (retry with backoff), never Ready. Catalog names like
//! `search_web` are always advertised and are not Ready-incompatible;
//! runtime use is fail-closed in the adapter. The probe conversation id is
//! not stored on the receipt.

use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::{
    agy_contract::{agy_init_isolation_leak, agy_version_meets_minimum},
    discovery::{
        agy_is_selectable, AgyReadiness, AgyReadinessSnapshot, AGY_REASON_READY,
        AGY_REASON_UNQUALIFIED_ISOLATION, AGY_REASON_UNQUALIFIED_UNATTESTED_LISTS,
    },
};

const RECEIPT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, Default)]
pub struct AgyQualifyEvidence {
    pub identity: String,
    pub version: Option<String>,
    pub init: Value,
    pub cancelled: bool,
    pub canary_mcp_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyQualificationReceipt {
    pub identity: String,
    pub version: Option<String>,
    pub readiness: AgyReadiness,
    pub reason: String,
    pub attestation_digest: Option<String>,
    pub qualified_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AgyQualificationReceipt {
    pub fn expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

pub fn qualify_from_agy_evidence(evidence: AgyQualifyEvidence) -> AgyQualificationReceipt {
    let now = now_ms();
    let mut receipt = AgyQualificationReceipt {
        identity: evidence.identity.clone(),
        version: evidence.version.clone(),
        readiness: AgyReadiness::Unqualified,
        reason: AGY_REASON_UNQUALIFIED_ISOLATION.to_string(),
        attestation_digest: None,
        qualified_at_ms: now,
        expires_at_ms: now.saturating_add(RECEIPT_TTL_MS),
    };

    if evidence.init.is_null() {
        receipt.reason = "qualification session did not complete".to_string();
        return receipt;
    }

    if !evidence.cancelled {
        receipt.reason =
            "qualification session was created but not cancelled; readiness stays unpublished"
                .to_string();
        return receipt;
    }

    if canary_appeared(&evidence) {
        receipt.readiness = AgyReadiness::Incompatible;
        receipt.reason = "Agy loaded a cwd MCP canary in the headless session".to_string();
        return receipt;
    }

    if let Some(leak) = agy_init_isolation_leak(&evidence.init) {
        match leak {
            "tools_unattested" | "permission_mode_unattested" => {
                receipt.readiness = AgyReadiness::Unqualified;
                receipt.reason = AGY_REASON_UNQUALIFIED_UNATTESTED_LISTS.to_string();
            },
            other => {
                receipt.readiness = AgyReadiness::Incompatible;
                receipt.reason = format!("Agy isolation leak: {other}");
            },
        }
        return receipt;
    }

    receipt.attestation_digest = Some(attestation_digest(&evidence.init));
    receipt.readiness = AgyReadiness::Ready;
    receipt.reason = AGY_REASON_READY.to_string();
    receipt
}

fn canary_appeared(evidence: &AgyQualifyEvidence) -> bool {
    let Some(name) = evidence
        .canary_mcp_name
        .as_deref()
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    evidence.init.to_string().contains(name)
}

fn attestation_digest(init: &Value) -> String {
    let payload = init.get("init").unwrap_or(init);
    let tools = payload.get("tools").cloned().unwrap_or(Value::Null);
    let permission_mode = payload
        .get("permission_mode")
        .cloned()
        .unwrap_or(Value::Null);
    let body = serde_json::json!({
        "tools": tools,
        "permission_mode": permission_mode,
    });
    blake3::hash(body.to_string().as_bytes())
        .to_hex()
        .to_string()
}

/// Version + auth already look usable, but Ready is gated on a matching
/// isolation receipt. Incompatible isolation is cached and not re-probed
/// every tick.
pub fn agy_needs_isolation_attestation(snapshot: &AgyReadinessSnapshot) -> bool {
    let Some(version) = snapshot.version.as_deref() else {
        return false;
    };
    if !agy_version_meets_minimum(version) {
        return false;
    }
    matches!(snapshot.readiness, AgyReadiness::Unqualified)
}

pub fn cache_agy_receipt(receipt: AgyQualificationReceipt) {
    agy_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(receipt.identity.clone(), receipt);
}

pub fn cached_agy_receipt(identity: &str) -> Option<AgyQualificationReceipt> {
    agy_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(identity)
        .cloned()
}

pub fn invalidate_agy_receipt(identity: &str) {
    agy_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(identity);
}

pub fn overlay_agy_receipt(
    mut snapshot: AgyReadinessSnapshot,
    receipt: &AgyQualificationReceipt,
    now_ms: i64,
) -> AgyReadinessSnapshot {
    if receipt.identity != snapshot.identity()
        || receipt.version.as_deref() != snapshot.version.as_deref()
        || receipt.expired_at(now_ms)
    {
        return demote_unattested_ready(snapshot);
    }
    snapshot.readiness = receipt.readiness;
    snapshot.reason = receipt.reason.clone();
    snapshot.selectable = agy_is_selectable(receipt.readiness);
    snapshot
}

pub(crate) fn demote_unattested_ready(mut snapshot: AgyReadinessSnapshot) -> AgyReadinessSnapshot {
    if snapshot.readiness == AgyReadiness::Ready {
        snapshot.readiness = AgyReadiness::Unqualified;
        snapshot.selectable = false;
        snapshot.reason = AGY_REASON_UNQUALIFIED_ISOLATION.to_string();
    }
    snapshot
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn agy_receipt_slot() -> &'static Mutex<BTreeMap<String, AgyQualificationReceipt>> {
    static SLOT: OnceLock<Mutex<BTreeMap<String, AgyQualificationReceipt>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn clean_init() -> Value {
        json!({
            "event": "init",
            "conversation_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "init": {
                "tools": ["run_command", "view_file", "write_to_file", "search_web"],
                "permission_mode": "always-proceed"
            }
        })
    }

    fn clean_evidence() -> AgyQualifyEvidence {
        AgyQualifyEvidence {
            identity: "bin-1".to_string(),
            version: Some("1.1.19".to_string()),
            init: clean_init(),
            cancelled: true,
            canary_mcp_name: None,
        }
    }

    #[test]
    fn incomplete_session_evidence_stays_unqualified() {
        let receipt = qualify_from_agy_evidence(AgyQualifyEvidence {
            identity: "bin-1".to_string(),
            ..AgyQualifyEvidence::default()
        });
        assert_eq!(receipt.readiness, AgyReadiness::Unqualified);
        assert!(
            receipt.reason.contains("did not complete"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn catalog_search_web_is_still_ready() {
        let receipt = qualify_from_agy_evidence(clean_evidence());
        assert_eq!(receipt.readiness, AgyReadiness::Ready);
        assert!(agy_is_selectable(receipt.readiness));
        assert!(receipt.attestation_digest.is_some());
        let rendered = format!("{receipt:?}");
        assert!(
            !rendered.contains("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            "{rendered}"
        );
    }

    #[test]
    fn missing_tool_list_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "event": "init",
            "init": { "permission_mode": "always-proceed" }
        });
        let receipt = qualify_from_agy_evidence(evidence);
        assert_eq!(receipt.readiness, AgyReadiness::Unqualified);
        assert!(receipt.attestation_digest.is_none());
        assert_eq!(receipt.reason, AGY_REASON_UNQUALIFIED_UNATTESTED_LISTS);
    }

    #[test]
    fn missing_permission_mode_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "event": "init",
            "init": { "tools": ["run_command"] }
        });
        let receipt = qualify_from_agy_evidence(evidence);
        assert_eq!(receipt.readiness, AgyReadiness::Unqualified);
        assert!(!agy_is_selectable(receipt.readiness));
        assert_eq!(receipt.reason, AGY_REASON_UNQUALIFIED_UNATTESTED_LISTS);
    }

    #[test]
    fn canary_mcp_name_in_init_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.canary_mcp_name = Some("magician-agy-qualify-canary-xyz".to_string());
        evidence.init = json!({
            "event": "init",
            "init": {
                "tools": ["run_command", "magician-agy-qualify-canary-xyz"],
                "permission_mode": "always-proceed"
            }
        });
        let receipt = qualify_from_agy_evidence(evidence);
        assert_eq!(receipt.readiness, AgyReadiness::Incompatible);
        assert!(!receipt.reason.contains("xyz"), "{}", receipt.reason);
    }

    #[test]
    fn uncancelled_probe_session_stays_unqualified() {
        let mut evidence = clean_evidence();
        evidence.cancelled = false;
        let receipt = qualify_from_agy_evidence(evidence);
        assert_eq!(receipt.readiness, AgyReadiness::Unqualified);
        assert!(
            receipt.reason.contains("not cancelled"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn matching_fresh_receipt_overlays_ready() {
        let snapshot = AgyReadinessSnapshot::unqualified_for_test("bin-1");
        let mut receipt = qualify_from_agy_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.version = snapshot.version.clone();
        let overlaid = overlay_agy_receipt(snapshot, &receipt, receipt.qualified_at_ms);
        assert_eq!(overlaid.readiness, AgyReadiness::Ready);
        assert!(overlaid.selectable);
    }

    #[test]
    fn expired_mismatched_or_version_changed_receipt_does_not_overlay() {
        let snapshot = AgyReadinessSnapshot::ready_for_test("bin-1");
        let mut receipt = qualify_from_agy_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.version = snapshot.version.clone();
        receipt.expires_at_ms = 1;
        let kept = overlay_agy_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, AgyReadiness::Unqualified);
        assert!(!kept.selectable);
        receipt.expires_at_ms = 10;
        receipt.identity = "other".to_string();
        let kept = overlay_agy_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, AgyReadiness::Unqualified);
        receipt.identity = "bin-1".to_string();
        receipt.version = Some("9.9.9".to_string());
        let kept = overlay_agy_receipt(snapshot, &receipt, 2);
        assert_eq!(kept.readiness, AgyReadiness::Unqualified);
    }

    #[test]
    fn agy_needs_isolation_when_versioned_and_unqualified() {
        let waiting = AgyReadinessSnapshot::unqualified_for_test("bin-1");
        assert!(agy_needs_isolation_attestation(&waiting));
        let ready = AgyReadinessSnapshot::ready_for_test("bin-1");
        assert!(!agy_needs_isolation_attestation(&ready));
        let auth = AgyReadinessSnapshot::auth_required_for_test("bin-1");
        assert!(!agy_needs_isolation_attestation(&auth));
    }
}
