//! Phase 3: qualify a Grok ACP launch from initialize + session/new evidence.
//!
//! Request handlers never call this. They only read a cached receipt that
//! matches the current filesystem identity. Live ACP probes wait for the
//! background tick; this module is the fail-closed classifier those probes
//! (and tests) feed. Missing MCP/tool lists stay Unqualified (retry), never
//! Ready. The probe session id is not stored on the receipt.

use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::{
    discovery::{
        grok_is_selectable, GrokReadiness, GrokReadinessSnapshot, GROK_REASON_READY,
        GROK_REASON_UNQUALIFIED_ISOLATION, GROK_REASON_UNQUALIFIED_UNATTESTED_LISTS,
    },
    grok_contract::{
        attest_grok_isolation_roots, grok_version_meets_minimum, GrokIsolationError,
        GrokIsolationVerdict,
    },
};

const RECEIPT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, Default)]
pub struct GrokQualifyEvidence {
    pub identity: String,
    pub initialize: Value,
    pub session: Value,
    pub session_id: Option<String>,
    pub cancelled: bool,
    pub updates: Vec<Value>,
    pub canary_mcp_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokQualificationReceipt {
    pub identity: String,
    pub readiness: GrokReadiness,
    pub reason: String,
    pub attestation_digest: Option<String>,
    pub qualified_at_ms: i64,
    pub expires_at_ms: i64,
}

impl GrokQualificationReceipt {
    pub fn expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

pub fn qualify_from_grok_evidence(evidence: GrokQualifyEvidence) -> GrokQualificationReceipt {
    let now = now_ms();
    let mut receipt = GrokQualificationReceipt {
        identity: evidence.identity.clone(),
        readiness: GrokReadiness::Unqualified,
        reason: GROK_REASON_UNQUALIFIED_ISOLATION.to_string(),
        attestation_digest: None,
        qualified_at_ms: now,
        expires_at_ms: now.saturating_add(RECEIPT_TTL_MS),
    };

    if evidence.initialize.is_null() || evidence.session.is_null() {
        receipt.reason = "qualification session did not complete".to_string();
        return receipt;
    }

    if evidence.session_id.is_some() && !evidence.cancelled {
        receipt.reason =
            "qualification session was created but not cancelled; readiness stays unpublished"
                .to_string();
        return receipt;
    }

    match attest_grok_isolation_roots(
        std::iter::once(&evidence.initialize)
            .chain(std::iter::once(&evidence.session))
            .chain(evidence.updates.iter()),
        evidence.canary_mcp_name.as_deref(),
    ) {
        GrokIsolationVerdict::Isolated(attestation) => {
            receipt.attestation_digest = Some(attestation.digest);
            receipt.readiness = GrokReadiness::Ready;
            receipt.reason = GROK_REASON_READY.to_string();
        },
        GrokIsolationVerdict::Unattested => {
            receipt.readiness = GrokReadiness::Unqualified;
            receipt.reason = GROK_REASON_UNQUALIFIED_UNATTESTED_LISTS.to_string();
        },
        GrokIsolationVerdict::Incompatible(error) => {
            receipt.readiness = GrokReadiness::Incompatible;
            receipt.reason = isolation_reason(error);
        },
    }
    receipt
}

fn isolation_reason(error: GrokIsolationError) -> String {
    match error {
        GrokIsolationError::McpServersAdvertised => {
            "Grok advertised MCP servers in the ACP session".to_string()
        },
        GrokIsolationError::McpGatewayAdvertised => {
            "Grok advertises its MCP gateway tools (search_tool / use_tool), which can reach MCP servers outside Magician's control".to_string()
        },
        GrokIsolationError::WebSearchAdvertised => {
            "Grok advertised web search after --disable-web-search".to_string()
        },
        GrokIsolationError::HooksAdvertised => {
            "Grok advertised hooks in the ACP session".to_string()
        },
        GrokIsolationError::PluginsAdvertised => {
            "Grok advertised plugins in the ACP session".to_string()
        },
    }
}

/// Version + auth already look usable, but Ready is gated on a matching
/// isolation receipt. Unqualified (waiting) and isolation Incompatible retry.
pub fn grok_needs_acp_attestation(snapshot: &GrokReadinessSnapshot) -> bool {
    let Some(version) = snapshot.version.as_deref() else {
        return false;
    };
    if !grok_version_meets_minimum(version) {
        return false;
    }
    matches!(
        snapshot.readiness,
        GrokReadiness::Unqualified | GrokReadiness::Incompatible
    )
}

pub fn cache_grok_receipt(receipt: GrokQualificationReceipt) {
    grok_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(receipt.identity.clone(), receipt);
}

pub fn cached_grok_receipt(identity: &str) -> Option<GrokQualificationReceipt> {
    grok_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(identity)
        .cloned()
}

pub fn invalidate_grok_receipt(identity: &str) {
    grok_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(identity);
}

pub fn overlay_grok_receipt(
    mut snapshot: GrokReadinessSnapshot,
    receipt: &GrokQualificationReceipt,
    now_ms: i64,
) -> GrokReadinessSnapshot {
    if receipt.identity != snapshot.identity() || receipt.expired_at(now_ms) {
        return demote_unattested_ready(snapshot);
    }
    snapshot.readiness = receipt.readiness;
    snapshot.reason = receipt.reason.clone();
    snapshot.selectable = grok_is_selectable(receipt.readiness);
    snapshot
}

pub(crate) fn demote_unattested_ready(
    mut snapshot: GrokReadinessSnapshot,
) -> GrokReadinessSnapshot {
    if snapshot.readiness == GrokReadiness::Ready {
        snapshot.readiness = GrokReadiness::Unqualified;
        snapshot.selectable = false;
        snapshot.reason = GROK_REASON_UNQUALIFIED_ISOLATION.to_string();
    }
    snapshot
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn grok_receipt_slot() -> &'static Mutex<BTreeMap<String, GrokQualificationReceipt>> {
    static SLOT: OnceLock<Mutex<BTreeMap<String, GrokQualificationReceipt>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn clean_session() -> Value {
        json!({
            "sessionId": "sess-probe",
            "mcpServers": [],
            "tools": ["read_file", "bash", "grep_search", "list_dir"],
        })
    }

    fn clean_evidence() -> GrokQualifyEvidence {
        GrokQualifyEvidence {
            identity: "bin-1".to_string(),
            initialize: json!({ "protocolVersion": 1 }),
            session: clean_session(),
            session_id: Some("sess-probe".to_string()),
            cancelled: true,
            ..GrokQualifyEvidence::default()
        }
    }

    #[test]
    fn incomplete_session_evidence_stays_unqualified() {
        let receipt = qualify_from_grok_evidence(GrokQualifyEvidence {
            identity: "bin-1".to_string(),
            ..GrokQualifyEvidence::default()
        });
        assert_eq!(receipt.readiness, GrokReadiness::Unqualified);
        assert!(
            receipt.reason.contains("did not complete"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn fake_initialize_with_mcp_servers_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.initialize = json!({
            "protocolVersion": 1,
            "mcpServers": [{ "name": "github", "command": "npx" }]
        });
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Incompatible);
        assert!(!grok_is_selectable(receipt.readiness));
        assert!(receipt.reason.contains("MCP"), "{}", receipt.reason);
        assert!(!receipt.reason.contains("github"), "{}", receipt.reason);
        assert!(!receipt.reason.contains("npx"), "{}", receipt.reason);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
        assert!(receipt.attestation_digest.is_none());
    }

    #[test]
    fn empty_mcp_and_expected_tools_are_ready() {
        let receipt = qualify_from_grok_evidence(clean_evidence());
        assert_eq!(receipt.readiness, GrokReadiness::Ready);
        assert!(grok_is_selectable(receipt.readiness));
        assert!(receipt.attestation_digest.is_some());
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[test]
    fn missing_tool_list_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.session = json!({
            "sessionId": "sess-probe",
            "mcpServers": [],
        });
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Unqualified);
        assert!(receipt.attestation_digest.is_none());
    }

    #[test]
    fn missing_mcp_servers_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.session = json!({
            "sessionId": "sess-probe",
            "tools": ["read_file", "bash", "search_replace", "grep"],
        });
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Unqualified);
        assert!(!grok_is_selectable(receipt.readiness));
        assert!(receipt.attestation_digest.is_none());
        assert!(
            receipt.reason.contains("unattested") || receipt.reason.contains("did not advertise"),
            "{}",
            receipt.reason
        );
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[test]
    fn web_search_in_tools_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.session = json!({
            "sessionId": "sess-probe",
            "mcpServers": [],
            "tools": ["read_file", "web_search"],
        });
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Incompatible);
        assert!(!grok_is_selectable(receipt.readiness));
        assert!(receipt.reason.contains("web search"), "{}", receipt.reason);
    }

    #[test]
    fn drained_update_lists_can_make_ready() {
        let evidence = GrokQualifyEvidence {
            identity: "bin-1".to_string(),
            initialize: json!({ "protocolVersion": 1 }),
            session: json!({ "sessionId": "sess-probe" }),
            session_id: Some("sess-probe".to_string()),
            cancelled: true,
            updates: vec![json!({
                "method": "session/update",
                "params": {
                    "mcpServers": [],
                    "tools": ["read_file", "bash", "grep"],
                }
            })],
            canary_mcp_name: None,
        };
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Ready);
        assert!(receipt.attestation_digest.is_some());
    }

    #[test]
    fn canary_mcp_name_in_session_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.canary_mcp_name = Some("magician-grok-qualify-canary-xyz".to_string());
        evidence.session = json!({
            "sessionId": "sess-probe",
            "mcpServers": [{ "name": "magician-grok-qualify-canary-xyz" }],
            "tools": ["read_file"],
        });
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Incompatible);
        assert!(!receipt.reason.contains("xyz"), "{}", receipt.reason);
    }

    #[test]
    fn uncancelled_probe_session_stays_unqualified() {
        let mut evidence = clean_evidence();
        evidence.cancelled = false;
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Unqualified);
        assert!(
            receipt.reason.contains("not cancelled"),
            "{}",
            receipt.reason
        );
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[test]
    fn matching_fresh_receipt_overlays_ready() {
        let snapshot = GrokReadinessSnapshot::unqualified_for_test("bin-1");
        let mut receipt = qualify_from_grok_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        let overlaid = overlay_grok_receipt(snapshot, &receipt, receipt.qualified_at_ms);
        assert_eq!(overlaid.readiness, GrokReadiness::Ready);
        assert!(overlaid.selectable);
    }

    #[test]
    fn expired_or_mismatched_receipt_does_not_overlay() {
        let snapshot = GrokReadinessSnapshot::ready_for_test("bin-1");
        let mut receipt = qualify_from_grok_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.expires_at_ms = 1;
        let kept = overlay_grok_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, GrokReadiness::Unqualified);
        assert!(!kept.selectable);
        receipt.expires_at_ms = 10;
        receipt.identity = "other".to_string();
        let kept = overlay_grok_receipt(snapshot, &receipt, 2);
        assert_eq!(kept.readiness, GrokReadiness::Unqualified);
    }

    #[test]
    fn grok_needs_acp_when_versioned_and_unqualified() {
        let waiting = GrokReadinessSnapshot::unqualified_for_test("bin-1");
        assert!(grok_needs_acp_attestation(&waiting));
        let ready = GrokReadinessSnapshot::ready_for_test("bin-1");
        assert!(!grok_needs_acp_attestation(&ready));
        let auth = GrokReadinessSnapshot::auth_required_for_test("bin-1");
        assert!(!grok_needs_acp_attestation(&auth));
    }
}
