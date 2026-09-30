//! Phase 7 custom-surface isolation and bridge-admission kernel.
//!
//! Custom HTML/JS is a separate workstream from declarative MUIJ. The
//! general Iframe default (`allow-scripts allow-same-origin`) is
//! reserved for host-owned embeds such as marimo and must not be reused
//! for app documents. Magician's UI event loop is not a killable
//! worker. Scripted surfaces run in a Magician-spawned OS child with
//! CPU/RSS/wall kill; the display iframe stays no-script. This kernel
//! does not pretend an iframe is that boundary.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use magician::magician_v2::apps::models::{
    validate_json_value, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
    AppName, AppReference, AppRevision, ValidateAppContract,
};

/// Sandbox tokens the existing general Iframe component uses. App
/// documents must never inherit this set.
pub const GENERAL_IFRAME_SANDBOX: &str = "allow-scripts allow-same-origin";

const BRIDGE_SCHEMA_VERSION: u8 = 1;
const DEFAULT_SESSION_TTL: Duration = Duration::minutes(15);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCustomSurfaceMode {
    DeclarativeNoScript,
    ScriptedRequiresKillableBoundary,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCustomSurfaceTeardown {
    Disable,
    Quarantine,
    Update,
    Revocation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppBridgeMethod {
    Query,
    Mutate,
    InvokeAction,
    Subscribe,
    GetActionRun,
    WaitActionRun,
    CancelActionRun,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceIsolation {
    pub mode: AppCustomSurfaceMode,
    pub sandbox: String,
    pub csp: String,
    pub allows_scripts: bool,
    pub allows_same_origin: bool,
    pub allows_network: bool,
    pub package_revision_ref: AppReference,
    pub isolation_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBridgeSession {
    pub session_ref: AppReference,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub surface_revision: AppRevision,
    pub grant_revision: AppRevision,
    pub host_session_ref: AppReference,
    pub nonce: AppReference,
    pub expires_at: DateTime<Utc>,
    seen_request_ids: BTreeSet<AppReference>,
    last_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppBridgeMessage {
    pub schema_version: u8,
    pub request_id: AppReference,
    pub sequence: u64,
    pub method: AppBridgeMethod,
    pub origin: String,
    pub session_ref: AppReference,
    pub nonce: AppReference,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub surface_revision: AppRevision,
    pub grant_revision: AppRevision,
    #[serde(default)]
    pub view_or_action: Option<AppName>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppSandboxError {
    #[error("app custom surfaces cannot reuse the general iframe sandbox")]
    GeneralIframeTokensRefused,
    #[error("scripted custom surfaces require a killable worker or WebView")]
    ScriptedWithoutKillableBoundary,
    #[error("allow-same-origin is refused when custom-surface scripts are enabled")]
    SameOriginWithScripts,
    #[error("custom-surface network is denied in the first release")]
    DirectNetworkDenied,
    #[error("bridge session has expired")]
    SessionExpired,
    #[error("bridge session was torn down")]
    SessionTornDown,
    #[error("bridge request replayed a used id")]
    Replay,
    #[error("bridge request sequence is not the next admitted sequence")]
    OutOfOrder,
    #[error("bridge request names a stale package, surface or grant revision")]
    StaleRevision,
    #[error("bridge request names an unknown method")]
    UnknownMethod,
    #[error("bridge request origin does not match the session")]
    ForgedOrigin,
    #[error("bridge request exceeds the admitted byte, node or depth ceiling")]
    OversizedOrDeep,
    #[error("bridge request does not match the session binding")]
    SessionMismatch,
}

pub fn compile_custom_surface_isolation(
    mode: AppCustomSurfaceMode,
    requested_sandbox: &str,
    killable_boundary: bool,
    package_revision_ref: AppReference,
) -> Result<AppCustomSurfaceIsolation, AppSandboxError> {
    let requested = normalize_sandbox(requested_sandbox);
    if requested == normalize_sandbox(GENERAL_IFRAME_SANDBOX) {
        return Err(AppSandboxError::GeneralIframeTokensRefused);
    }
    if requested.iter().any(|token| token == "allow-same-origin")
        && requested.iter().any(|token| token == "allow-scripts")
    {
        return Err(AppSandboxError::SameOriginWithScripts);
    }
    if requested.iter().any(|token| {
        matches!(
            token.as_str(),
            "allow-popups"
                | "allow-popups-to-escape-sandbox"
                | "allow-top-navigation"
                | "allow-top-navigation-by-user-activation"
                | "allow-downloads"
                | "allow-forms"
                | "allow-modals"
                | "allow-orientation-lock"
                | "allow-pointer-lock"
                | "allow-presentation"
                | "allow-storage-access-by-user-activation"
        )
    }) {
        return Err(AppSandboxError::DirectNetworkDenied);
    }

    match mode {
        AppCustomSurfaceMode::DeclarativeNoScript => {
            if requested.iter().any(|token| token == "allow-scripts") {
                return Err(AppSandboxError::ScriptedWithoutKillableBoundary);
            }
        },
        AppCustomSurfaceMode::ScriptedRequiresKillableBoundary => {
            if !killable_boundary {
                return Err(AppSandboxError::ScriptedWithoutKillableBoundary);
            }
            if requested.iter().any(|token| token == "allow-same-origin") {
                return Err(AppSandboxError::SameOriginWithScripts);
            }
        },
    }

    let allows_scripts =
        matches!(mode, AppCustomSurfaceMode::ScriptedRequiresKillableBoundary) && killable_boundary;
    let sandbox = if allows_scripts {
        "allow-scripts".to_owned()
    } else {
        String::new()
    };
    let isolation = AppCustomSurfaceIsolation {
        mode,
        sandbox,
        csp: "default-src 'none'; connect-src 'none'; frame-ancestors 'none'; form-action 'none'; \
              base-uri 'none'; img-src 'none'; media-src 'none'; font-src 'none'; style-src \
              'unsafe-inline'; script-src 'none'"
            .to_owned(),
        allows_scripts,
        allows_same_origin: false,
        allows_network: false,
        package_revision_ref: package_revision_ref.clone(),
        isolation_digest: AppDigest::blake3(
            format!(
                "{}|{}|{}|{}",
                mode_label(mode),
                allows_scripts,
                false,
                package_revision_ref.as_str()
            )
            .as_bytes(),
        ),
    };
    Ok(isolation)
}

pub fn mint_bridge_session(
    session_ref: AppReference,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    surface_revision: AppRevision,
    grant_revision: AppRevision,
    host_session_ref: AppReference,
    nonce: AppReference,
    now: DateTime<Utc>,
) -> AppBridgeSession {
    AppBridgeSession {
        session_ref,
        installation_id,
        package_revision_ref,
        surface_revision,
        grant_revision,
        host_session_ref,
        nonce,
        expires_at: now + DEFAULT_SESSION_TTL,
        seen_request_ids: BTreeSet::new(),
        last_sequence: 0,
    }
}

pub fn teardown_bridge_session(
    _session: &AppBridgeSession,
    reason: AppCustomSurfaceTeardown,
) -> AppSandboxError {
    let _ = reason;
    AppSandboxError::SessionTornDown
}

pub fn admit_bridge_message(
    session: &mut AppBridgeSession,
    message: &AppBridgeMessage,
    now: DateTime<Utc>,
    live_package_revision_ref: &AppReference,
    live_surface_revision: AppRevision,
    live_grant_revision: AppRevision,
    expected_origin: &str,
    limits: &AppContractLimits,
) -> Result<(), AppSandboxError> {
    if now >= session.expires_at {
        return Err(AppSandboxError::SessionExpired);
    }
    if message.schema_version != BRIDGE_SCHEMA_VERSION {
        return Err(AppSandboxError::UnknownMethod);
    }
    if message.session_ref != session.session_ref
        || message.nonce != session.nonce
        || message.installation_id != session.installation_id
    {
        return Err(AppSandboxError::SessionMismatch);
    }
    if message.origin != expected_origin {
        return Err(AppSandboxError::ForgedOrigin);
    }
    if message.package_revision_ref != *live_package_revision_ref
        || message.surface_revision != live_surface_revision
        || message.grant_revision != live_grant_revision
        || session.package_revision_ref != *live_package_revision_ref
        || session.surface_revision != live_surface_revision
        || session.grant_revision != live_grant_revision
        || message.host_binding_mismatch(session)
    {
        return Err(AppSandboxError::StaleRevision);
    }
    if session.seen_request_ids.contains(&message.request_id) {
        return Err(AppSandboxError::Replay);
    }
    if message.sequence != session.last_sequence.saturating_add(1) {
        return Err(AppSandboxError::OutOfOrder);
    }
    if payload_exceeds_limits(&message.payload, limits) {
        return Err(AppSandboxError::OversizedOrDeep);
    }
    session.seen_request_ids.insert(message.request_id.clone());
    session.last_sequence = message.sequence;
    let _ = message.method;
    Ok(())
}

impl AppBridgeMessage {
    fn host_binding_mismatch(&self, session: &AppBridgeSession) -> bool {
        self.package_revision_ref != session.package_revision_ref
            || self.surface_revision != session.surface_revision
            || self.grant_revision != session.grant_revision
    }
}

impl ValidateAppContract for AppBridgeMessage {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.schema_version != BRIDGE_SCHEMA_VERSION {
            return Err(AppContractError::invalid(
                "schema_version",
                "must be the admitted custom-surface bridge version",
            ));
        }
        if self.origin.is_empty() || self.origin.len() > 255 {
            return Err(AppContractError::invalid(
                "origin",
                "must contain between 1 and 255 bytes",
            ));
        }
        validate_json_value(&self.payload, limits)?;
        Ok(())
    }
}

fn normalize_sandbox(value: &str) -> BTreeSet<String> {
    value
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn mode_label(mode: AppCustomSurfaceMode) -> &'static str {
    match mode {
        AppCustomSurfaceMode::DeclarativeNoScript => "no-script",
        AppCustomSurfaceMode::ScriptedRequiresKillableBoundary => "scripted",
    }
}

fn payload_exceeds_limits(payload: &Value, limits: &AppContractLimits) -> bool {
    let Ok(bytes) = serde_json::to_vec(payload) else {
        return true;
    };
    if bytes.len() > limits.max_value_bytes() {
        return true;
    }
    !magician::magician_v2::json_traversal::json_bytes_nesting_is_bounded(
        &bytes,
        limits.max_json_depth(),
    ) || !magician::magician_v2::json_traversal::json_bytes_nodes_are_bounded(
        &bytes,
        limits.max_json_nodes(),
    )
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 18, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn installation() -> AppInstallationId {
        AppInstallationId::parse("install_1").unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    fn session(now: DateTime<Utc>) -> AppBridgeSession {
        mint_bridge_session(
            reference("bridge:session-1"),
            installation(),
            reference("package-revision:reading-list"),
            revision(1),
            revision(1),
            reference("host:session-1"),
            reference("nonce:1"),
            now,
        )
    }

    fn message(method: AppBridgeMethod) -> AppBridgeMessage {
        AppBridgeMessage {
            schema_version: BRIDGE_SCHEMA_VERSION,
            request_id: reference("req:1"),
            sequence: 1,
            method,
            origin: "null".to_owned(),
            session_ref: reference("bridge:session-1"),
            nonce: reference("nonce:1"),
            installation_id: installation(),
            package_revision_ref: reference("package-revision:reading-list"),
            surface_revision: revision(1),
            grant_revision: revision(1),
            view_or_action: Some(AppName::parse("items").unwrap()),
            payload: serde_json::json!({"select": ["title"]}),
        }
    }

    #[test]
    fn general_iframe_tokens_cannot_host_an_app_document() {
        let error = compile_custom_surface_isolation(
            AppCustomSurfaceMode::DeclarativeNoScript,
            GENERAL_IFRAME_SANDBOX,
            false,
            reference("package-revision:reading-list"),
        )
        .expect_err("general iframe tokens");
        assert_eq!(error, AppSandboxError::GeneralIframeTokensRefused);
    }

    #[test]
    fn scripted_surface_without_a_killable_boundary_fails_closed() {
        let error = compile_custom_surface_isolation(
            AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
            "allow-scripts",
            false,
            reference("package-revision:reading-list"),
        )
        .expect_err("no killable boundary");
        assert_eq!(error, AppSandboxError::ScriptedWithoutKillableBoundary);
    }

    #[test]
    fn first_release_is_declarative_no_script() {
        let isolation = compile_custom_surface_isolation(
            AppCustomSurfaceMode::DeclarativeNoScript,
            "",
            false,
            reference("package-revision:reading-list"),
        )
        .expect("no-script isolation");
        assert!(!isolation.allows_scripts);
        assert!(!isolation.allows_same_origin);
        assert!(!isolation.allows_network);
        assert!(isolation.sandbox.is_empty());
        assert!(isolation.csp.contains("default-src 'none'"));
        assert!(isolation.csp.contains("connect-src 'none'"));
        assert!(isolation.csp.contains("style-src 'unsafe-inline'"));
        assert!(isolation.csp.contains("script-src 'none'"));
    }

    #[test]
    fn scripted_surface_still_refuses_same_origin() {
        let error = compile_custom_surface_isolation(
            AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
            "allow-scripts allow-same-origin",
            true,
            reference("package-revision:reading-list"),
        )
        .expect_err("same origin");
        assert_eq!(error, AppSandboxError::GeneralIframeTokensRefused);
    }

    #[test]
    fn bridge_admits_only_closed_data_change_and_run_control_methods() {
        let now = time(1);
        let mut live = session(now);
        for (offset, method) in [
            AppBridgeMethod::Query,
            AppBridgeMethod::Mutate,
            AppBridgeMethod::InvokeAction,
            AppBridgeMethod::Subscribe,
            AppBridgeMethod::GetActionRun,
            AppBridgeMethod::WaitActionRun,
            AppBridgeMethod::CancelActionRun,
        ]
        .into_iter()
        .enumerate()
        {
            let mut request = message(method);
            request.sequence = u64::try_from(offset).unwrap() + 1;
            request.request_id = reference(&format!(
                "req:{}",
                match method {
                    AppBridgeMethod::Query => "q",
                    AppBridgeMethod::Mutate => "m",
                    AppBridgeMethod::InvokeAction => "a",
                    AppBridgeMethod::Subscribe => "s",
                    AppBridgeMethod::GetActionRun => "r",
                    AppBridgeMethod::WaitActionRun => "w",
                    AppBridgeMethod::CancelActionRun => "c",
                }
            ));
            admit_bridge_message(
                &mut live,
                &request,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            )
            .expect("admitted method");
        }
    }

    #[test]
    fn bridge_sequence_gaps_fail_without_consuming_the_next_sequence() {
        let now = time(1);
        let mut live = session(now);
        let mut gap = message(AppBridgeMethod::Query);
        gap.sequence = 2;
        assert_eq!(
            admit_bridge_message(
                &mut live,
                &gap,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            ),
            Err(AppSandboxError::OutOfOrder)
        );
        gap.sequence = 1;
        admit_bridge_message(
            &mut live,
            &gap,
            now,
            &reference("package-revision:reading-list"),
            revision(1),
            revision(1),
            "null",
            &AppContractLimits::default(),
        )
        .expect("the exact next sequence remains admissible");
    }

    #[test]
    fn replay_stale_revision_forged_origin_and_oversized_payload_fail_closed() {
        let now = time(1);
        let mut live = session(now);
        let first = message(AppBridgeMethod::Query);
        admit_bridge_message(
            &mut live,
            &first,
            now,
            &reference("package-revision:reading-list"),
            revision(1),
            revision(1),
            "null",
            &AppContractLimits::default(),
        )
        .expect("first request");
        assert_eq!(
            admit_bridge_message(
                &mut live,
                &first,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            )
            .expect_err("replay"),
            AppSandboxError::Replay
        );

        let mut stale = message(AppBridgeMethod::Query);
        stale.request_id = reference("req:stale");
        stale.surface_revision = revision(2);
        assert_eq!(
            admit_bridge_message(
                &mut live,
                &stale,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            )
            .expect_err("stale"),
            AppSandboxError::StaleRevision
        );

        let mut forged = message(AppBridgeMethod::Query);
        forged.request_id = reference("req:forged");
        forged.origin = "https://evil.example".to_owned();
        assert_eq!(
            admit_bridge_message(
                &mut live,
                &forged,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            )
            .expect_err("forged"),
            AppSandboxError::ForgedOrigin
        );

        let mut deep = message(AppBridgeMethod::Query);
        deep.request_id = reference("req:deep");
        deep.sequence = 2;
        let mut nested = serde_json::json!("leaf");
        for _ in 0..40 {
            nested = serde_json::json!({ "n": nested });
        }
        deep.payload = nested;
        assert_eq!(
            admit_bridge_message(
                &mut live,
                &deep,
                now,
                &reference("package-revision:reading-list"),
                revision(1),
                revision(1),
                "null",
                &AppContractLimits::default(),
            )
            .expect_err("deep"),
            AppSandboxError::OversizedOrDeep
        );
    }

    #[test]
    fn disable_quarantine_update_and_revocation_tear_the_session_down() {
        let live = session(time(1));
        for reason in [
            AppCustomSurfaceTeardown::Disable,
            AppCustomSurfaceTeardown::Quarantine,
            AppCustomSurfaceTeardown::Update,
            AppCustomSurfaceTeardown::Revocation,
        ] {
            assert_eq!(
                teardown_bridge_session(&live, reason),
                AppSandboxError::SessionTornDown
            );
        }
    }

    #[test]
    fn red_team_tokens_cannot_open_network_navigation_storage_or_cookies() {
        let isolation = compile_custom_surface_isolation(
            AppCustomSurfaceMode::DeclarativeNoScript,
            "",
            false,
            reference("package-revision:reading-list"),
        )
        .expect("empty sandbox");
        assert!(isolation.sandbox.is_empty());
        assert!(!isolation.allows_network);
        assert!(isolation.csp.contains("connect-src 'none'"));
        assert!(isolation.csp.contains("default-src 'none'"));
        assert!(!isolation.csp.contains("cookie"));
        for token in [
            "allow-popups",
            "allow-popups-to-escape-sandbox",
            "allow-top-navigation",
            "allow-top-navigation-by-user-activation",
            "allow-downloads",
            "allow-forms",
            "allow-modals",
            "allow-pointer-lock",
            "allow-presentation",
            "allow-storage-access-by-user-activation",
            "allow-scripts allow-same-origin",
        ] {
            assert!(
                compile_custom_surface_isolation(
                    AppCustomSurfaceMode::DeclarativeNoScript,
                    token,
                    false,
                    reference("package-revision:reading-list"),
                )
                .is_err(),
                "{token}"
            );
        }
    }
}
