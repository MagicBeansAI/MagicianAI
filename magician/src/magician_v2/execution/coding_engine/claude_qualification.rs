//! Phase C2/C3: qualify a Claude Code launch from `system/init` evidence.
//!
//! Request handlers never call this. They only read a cached receipt that
//! matches the current filesystem identity **and** CLI version. Live probes
//! wait for the background tick; this module is the fail-closed classifier
//! those probes (and tests) feed. Missing MCP/tool lists stay Unqualified
//! (retry with backoff), never Ready. The probe session id is not stored
//! on the receipt.

use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::{
    claude_contract::{
        claude_init_api_key_source_leak, claude_init_isolation_leak, claude_version_meets_minimum,
    },
    discovery::{
        claude_is_selectable, ClaudeReadiness, ClaudeReadinessSnapshot, CLAUDE_REASON_READY,
        CLAUDE_REASON_UNQUALIFIED_ISOLATION, CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS,
    },
};

const RECEIPT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, Default)]
pub struct ClaudeQualifyEvidence {
    pub identity: String,
    pub version: Option<String>,
    pub use_api_key: bool,
    pub init: Value,
    pub cancelled: bool,
    pub canary_mcp_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeQualificationReceipt {
    pub identity: String,
    pub version: Option<String>,
    pub readiness: ClaudeReadiness,
    pub reason: String,
    pub attestation_digest: Option<String>,
    pub qualified_at_ms: i64,
    pub expires_at_ms: i64,
}

impl ClaudeQualificationReceipt {
    pub fn expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

pub fn qualify_from_claude_evidence(evidence: ClaudeQualifyEvidence) -> ClaudeQualificationReceipt {
    let now = now_ms();
    let mut receipt = ClaudeQualificationReceipt {
        identity: evidence.identity.clone(),
        version: evidence.version.clone(),
        readiness: ClaudeReadiness::Unqualified,
        reason: CLAUDE_REASON_UNQUALIFIED_ISOLATION.to_string(),
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
        receipt.readiness = ClaudeReadiness::Incompatible;
        receipt.reason = "Claude loaded a cwd MCP canary after --strict-mcp-config".to_string();
        return receipt;
    }

    if let Some(leak) = claude_init_isolation_leak(&evidence.init) {
        match leak {
            "mcp_servers_unattested" | "tools_unattested" => {
                receipt.readiness = ClaudeReadiness::Unqualified;
                receipt.reason = CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS.to_string();
            },
            "mcp_servers" => {
                receipt.readiness = ClaudeReadiness::Incompatible;
                receipt.reason =
                    "Claude advertised MCP servers in the headless session".to_string();
            },
            "plugins" => {
                receipt.readiness = ClaudeReadiness::Incompatible;
                receipt.reason = "Claude advertised plugins in the headless session".to_string();
            },
            "forbidden_tool" => {
                receipt.readiness = ClaudeReadiness::Incompatible;
                receipt.reason =
                    "Claude advertised a forbidden tool after --disallowedTools".to_string();
            },
            other => {
                receipt.readiness = ClaudeReadiness::Incompatible;
                receipt.reason = format!("Claude isolation leak: {other}");
            },
        }
        return receipt;
    }

    if evidence
        .init
        .get("apiKeySource")
        .and_then(Value::as_str)
        .is_none()
        && !evidence.use_api_key
    {
        receipt.readiness = ClaudeReadiness::Unqualified;
        receipt.reason = CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS.to_string();
        return receipt;
    }
    if let Some(leak) = claude_init_api_key_source_leak(&evidence.init, evidence.use_api_key) {
        receipt.readiness = ClaudeReadiness::Incompatible;
        receipt.reason = format!("Claude isolation leak: {leak}");
        return receipt;
    }

    receipt.attestation_digest = Some(attestation_digest(&evidence.init));
    receipt.readiness = ClaudeReadiness::Ready;
    receipt.reason = CLAUDE_REASON_READY.to_string();
    receipt
}

fn canary_appeared(evidence: &ClaudeQualifyEvidence) -> bool {
    let Some(name) = evidence
        .canary_mcp_name
        .as_deref()
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    evidence
        .init
        .get("mcp_servers")
        .map(|servers| servers.to_string().contains(name))
        .unwrap_or(false)
}

fn attestation_digest(init: &Value) -> String {
    let tools = init.get("tools").cloned().unwrap_or(Value::Null);
    let mcp = init.get("mcp_servers").cloned().unwrap_or(Value::Null);
    let plugins = init.get("plugins").cloned().unwrap_or(Value::Null);
    let payload = serde_json::json!({
        "tools": tools,
        "mcp_servers": mcp,
        "plugins": plugins,
    });
    blake3::hash(payload.to_string().as_bytes())
        .to_hex()
        .to_string()
}

/// Version + auth already look usable, but Ready is gated on a matching
/// isolation receipt. Incompatible isolation is cached and not re-probed
/// every tick (a live `-p` probe can bill Max).
pub fn claude_needs_isolation_attestation(snapshot: &ClaudeReadinessSnapshot) -> bool {
    let Some(version) = snapshot.version.as_deref() else {
        return false;
    };
    if !claude_version_meets_minimum(version) {
        return false;
    }
    matches!(snapshot.readiness, ClaudeReadiness::Unqualified)
}

pub fn cache_claude_receipt(receipt: ClaudeQualificationReceipt) {
    claude_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(receipt.identity.clone(), receipt);
}

pub fn cached_claude_receipt(identity: &str) -> Option<ClaudeQualificationReceipt> {
    claude_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(identity)
        .cloned()
}

pub fn invalidate_claude_receipt(identity: &str) {
    claude_receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(identity);
}

pub fn overlay_claude_receipt(
    mut snapshot: ClaudeReadinessSnapshot,
    receipt: &ClaudeQualificationReceipt,
    now_ms: i64,
) -> ClaudeReadinessSnapshot {
    if receipt.identity != snapshot.identity()
        || receipt.version.as_deref() != snapshot.version.as_deref()
        || receipt.expired_at(now_ms)
    {
        return demote_unattested_ready(snapshot);
    }
    snapshot.readiness = receipt.readiness;
    snapshot.reason = receipt.reason.clone();
    snapshot.selectable = claude_is_selectable(receipt.readiness);
    snapshot
}

pub(crate) fn demote_unattested_ready(
    mut snapshot: ClaudeReadinessSnapshot,
) -> ClaudeReadinessSnapshot {
    if snapshot.readiness == ClaudeReadiness::Ready {
        snapshot.readiness = ClaudeReadiness::Unqualified;
        snapshot.selectable = false;
        snapshot.reason = CLAUDE_REASON_UNQUALIFIED_ISOLATION.to_string();
    }
    snapshot
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn claude_receipt_slot() -> &'static Mutex<BTreeMap<String, ClaudeQualificationReceipt>> {
    static SLOT: OnceLock<Mutex<BTreeMap<String, ClaudeQualificationReceipt>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn clean_init() -> Value {
        json!({
            "type": "system",
            "subtype": "init",
            "session_id": "sess-probe",
            "tools": ["Read", "Bash", "Edit"],
            "mcp_servers": [],
            "plugins": [],
            "apiKeySource": "none"
        })
    }

    fn clean_evidence() -> ClaudeQualifyEvidence {
        ClaudeQualifyEvidence {
            identity: "bin-1".to_string(),
            version: Some("2.1.229".to_string()),
            use_api_key: false,
            init: clean_init(),
            cancelled: true,
            canary_mcp_name: None,
        }
    }

    #[test]
    fn incomplete_session_evidence_stays_unqualified() {
        let receipt = qualify_from_claude_evidence(ClaudeQualifyEvidence {
            identity: "bin-1".to_string(),
            ..ClaudeQualifyEvidence::default()
        });
        assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
        assert!(
            receipt.reason.contains("did not complete"),
            "{}",
            receipt.reason
        );
    }

    fn builtin(name: &str) -> Value {
        json!({ "name": name, "path": "builtin", "source": format!("{name}@builtin") })
    }

    #[test]
    fn the_cli_bundled_builtin_plugins_do_not_break_isolation() {
        // Claude 2.1.281 reports these two in every headless init, even with
        // --setting-sources "" and --safe-mode. Treating them as a leak made
        // Claude Code permanently Incompatible and invisible in VibeDev.
        let mut evidence = clean_evidence();
        evidence.init["plugins"] = json!([builtin("agents-md"), builtin("telemetry")]);
        let receipt = qualify_from_claude_evidence(evidence);
        assert!(claude_is_selectable(receipt.readiness), "{receipt:?}");
    }

    #[test]
    fn any_other_plugin_still_fails_closed() {
        let unreviewed_builtin = builtin("new-bundled-thing");
        let user_plugin = json!({ "name": "agents-md", "path": "/Users/x/.claude/plugins/agents-md", "source": "agents-md@marketplace" });
        let spoofed_source =
            json!({ "name": "telemetry", "path": "builtin", "source": "evil@builtin" });
        let unnamed = json!({ "path": "builtin" });
        for plugin in [unreviewed_builtin, user_plugin, spoofed_source, unnamed] {
            let mut evidence = clean_evidence();
            evidence.init["plugins"] = json!([builtin("agents-md"), plugin.clone()]);
            let receipt = qualify_from_claude_evidence(evidence);
            assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible, "{plugin}");
        }
    }

    #[test]
    fn fake_init_with_mcp_servers_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "type": "system",
            "subtype": "init",
            "tools": ["Read"],
            "mcp_servers": [{ "name": "github", "command": "npx" }]
        });
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible);
        assert!(!claude_is_selectable(receipt.readiness));
        assert!(receipt.reason.contains("MCP"), "{}", receipt.reason);
        assert!(!receipt.reason.contains("github"), "{}", receipt.reason);
        assert!(!receipt.reason.contains("npx"), "{}", receipt.reason);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
        assert!(receipt.attestation_digest.is_none());
    }

    #[test]
    fn empty_mcp_and_expected_tools_are_ready() {
        let receipt = qualify_from_claude_evidence(clean_evidence());
        assert_eq!(receipt.readiness, ClaudeReadiness::Ready);
        assert!(claude_is_selectable(receipt.readiness));
        assert!(receipt.attestation_digest.is_some());
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[test]
    fn missing_tool_list_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "type": "system",
            "subtype": "init",
            "mcp_servers": [],
        });
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
        assert!(receipt.attestation_digest.is_none());
        assert_eq!(receipt.reason, CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS);
    }

    #[test]
    fn missing_mcp_servers_is_not_ready() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "type": "system",
            "subtype": "init",
            "tools": ["Read", "Bash"],
        });
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
        assert!(!claude_is_selectable(receipt.readiness));
        assert!(receipt.attestation_digest.is_none());
        assert_eq!(receipt.reason, CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[test]
    fn web_search_in_tools_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.init = json!({
            "type": "system",
            "subtype": "init",
            "tools": ["Read", "WebSearch"],
            "mcp_servers": [],
        });
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible);
        assert!(!claude_is_selectable(receipt.readiness));
        assert!(
            receipt.reason.contains("forbidden tool"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn api_key_source_without_use_api_key_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.use_api_key = false;
        evidence.init["apiKeySource"] = json!("ANTHROPIC_API_KEY");
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible);
        assert!(
            receipt.reason.contains("api_key_source"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn canary_mcp_name_in_init_is_incompatible() {
        let mut evidence = clean_evidence();
        evidence.canary_mcp_name = Some("magician-claude-qualify-canary-xyz".to_string());
        evidence.init = json!({
            "type": "system",
            "subtype": "init",
            "mcp_servers": [{ "name": "magician-claude-qualify-canary-xyz" }],
            "tools": ["Read"],
        });
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible);
        assert!(!receipt.reason.contains("xyz"), "{}", receipt.reason);
    }

    #[test]
    fn uncancelled_probe_session_stays_unqualified() {
        let mut evidence = clean_evidence();
        evidence.cancelled = false;
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
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
        let snapshot = ClaudeReadinessSnapshot::unqualified_for_test("bin-1");
        let mut receipt = qualify_from_claude_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.version = snapshot.version.clone();
        let overlaid = overlay_claude_receipt(snapshot, &receipt, receipt.qualified_at_ms);
        assert_eq!(overlaid.readiness, ClaudeReadiness::Ready);
        assert!(overlaid.selectable);
    }

    #[test]
    fn expired_mismatched_or_version_changed_receipt_does_not_overlay() {
        let snapshot = ClaudeReadinessSnapshot::ready_for_test("bin-1");
        let mut receipt = qualify_from_claude_evidence(clean_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.version = snapshot.version.clone();
        receipt.expires_at_ms = 1;
        let kept = overlay_claude_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, ClaudeReadiness::Unqualified);
        assert!(!kept.selectable);
        receipt.expires_at_ms = 10;
        receipt.identity = "other".to_string();
        let kept = overlay_claude_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, ClaudeReadiness::Unqualified);
        receipt.identity = "bin-1".to_string();
        receipt.version = Some("9.9.9".to_string());
        let kept = overlay_claude_receipt(snapshot, &receipt, 2);
        assert_eq!(kept.readiness, ClaudeReadiness::Unqualified);
    }

    #[test]
    fn missing_api_key_source_without_use_api_key_is_unattested() {
        let mut evidence = clean_evidence();
        evidence
            .init
            .as_object_mut()
            .unwrap()
            .remove("apiKeySource");
        let receipt = qualify_from_claude_evidence(evidence);
        assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
        assert!(!claude_is_selectable(receipt.readiness));
    }

    #[test]
    fn claude_needs_isolation_when_versioned_and_unqualified() {
        let waiting = ClaudeReadinessSnapshot::unqualified_for_test("bin-1");
        assert!(claude_needs_isolation_attestation(&waiting));
        let ready = ClaudeReadinessSnapshot::ready_for_test("bin-1");
        assert!(!claude_needs_isolation_attestation(&ready));
        let auth = ClaudeReadinessSnapshot::auth_required_for_test("bin-1");
        assert!(!claude_needs_isolation_attestation(&auth));
    }
}
