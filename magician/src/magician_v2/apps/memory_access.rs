//! Owner-controlled memory access for apps (`app_memory_read_v1`).
//!
//! An app's manifest may *request* read access to named user-memory tiers
//! (facts about the owner) and to named agents' learned memory. A request is
//! never authority. The owner grants a subset at install review and can
//! narrow, widen within the request, or revoke it at any time. The grant is
//! split by run mode: what the app may read while the owner is using it
//! (`interactive`) and what its unattended behaviors may read
//! (`background`, empty unless the owner ticks it).
//!
//! Tiers are classified here, fail-closed:
//! - `Ordinary` tiers are an explicit list and are what a default grant
//!   includes.
//! - `NotReadable` tiers (the root `knowledge`/`user` tiers, which normalize to
//!   everything) can never be requested.
//! - Every other tier — `identity`, `accounts`, screen observations, the
//!   email/calendar/chat evidence lanes, and any tier added later — is
//!   `Sensitive`: grantable only by an explicit owner tick, never by default.
//!
//! Engagement binding is part of the grant. Today every app is `OwnerOnly`:
//! it reads the owner's own memory and never engagement- or meeting-labelled
//! memory. A future client-serving app would be bound to exactly one
//! engagement at install and could never see another's.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::models::AppDigest;
use crate::magician_v2::agents::storage::validate_agent_identifier;
use crate::magician_v2::chat::service::USER_MEMORY_TIERS;

pub const APP_MEMORY_READ_GRANT_SCHEMA: &str = "magician.app-memory-read-grant.v1";
pub const MAX_APP_MEMORY_READ_TIERS: usize = 16;
pub const MAX_APP_MEMORY_READ_AGENTS: usize = 16;
pub const MAX_APP_MEMORY_READ_PURPOSE_BYTES: usize = 512;

/// User tiers an app may be granted by default. Anything else in the tier
/// registry is sensitive (explicit tick) or not readable at all.
const ORDINARY_USER_TIERS: &[&str] = &[
    "preferences",
    "skills",
    "contacts",
    "channels",
    "routines",
    "research_findings",
    "workflows",
    "organization",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryTierReadability {
    Ordinary,
    Sensitive,
    NotReadable,
}

/// Readability of a user-memory tier for apps, or `None` when the name is not
/// a registered user tier.
pub fn app_memory_tier_readability(name: &str) -> Option<AppMemoryTierReadability> {
    let tier = USER_MEMORY_TIERS.iter().find(|tier| tier.name == name)?;
    Some(if tier.normalizes_to_root {
        AppMemoryTierReadability::NotReadable
    } else if ORDINARY_USER_TIERS.contains(&tier.name) {
        AppMemoryTierReadability::Ordinary
    } else {
        AppMemoryTierReadability::Sensitive
    })
}

/// Every user tier an app could be granted, with its readability, for review
/// and settings UIs.
pub fn app_readable_user_memory_tiers() -> Vec<(&'static str, AppMemoryTierReadability)> {
    let mut tiers = USER_MEMORY_TIERS
        .iter()
        .filter_map(|tier| {
            let readability = app_memory_tier_readability(tier.name)?;
            (readability != AppMemoryTierReadability::NotReadable)
                .then_some((tier.name, readability))
        })
        .collect::<Vec<_>>();
    tiers.sort_by(|left, right| left.0.cmp(right.0));
    tiers
}

/// The manifest's `app.memory.read` block: a request, never authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryReadRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_tiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Shown to the owner at review: why the app wants this memory.
    pub purpose: String,
}

/// The manifest's `app.memory` block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestMemory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<AppMemoryReadRequest>,
}

/// One run mode's granted memory. Always sorted and unique.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryReadSelection {
    #[serde(default)]
    pub user_tiers: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

impl AppMemoryReadSelection {
    pub fn is_empty(&self) -> bool {
        self.user_tiers.is_empty() && self.agents.is_empty()
    }

    pub fn grants_tier(&self, tier: &str) -> bool {
        self.user_tiers.iter().any(|granted| granted == tier)
    }

    pub fn grants_agent(&self, agent_id: &str) -> bool {
        self.agents.iter().any(|granted| granted == agent_id)
    }

    fn normalized(mut self) -> Self {
        self.user_tiers.sort();
        self.user_tiers.dedup();
        self.agents.sort();
        self.agents.dedup();
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryEngagementBinding {
    /// The owner's own memory only; engagement- and meeting-labelled memory
    /// is never visible.
    OwnerOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryRunMode {
    /// The owner opened or is using the app.
    Interactive,
    /// A schedule, event, recurrence or other unattended behavior.
    Background,
}

impl AppMemoryRunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "interactive" => Some(Self::Interactive),
            "background" => Some(Self::Background),
            _ => None,
        }
    }
}

/// The owner's grant. `request_digest` pins which request it narrows, so a
/// package update that changes the request cannot silently inherit it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryReadGrant {
    pub schema: String,
    pub request_digest: AppDigest,
    pub engagement: AppMemoryEngagementBinding,
    pub interactive: AppMemoryReadSelection,
    pub background: AppMemoryReadSelection,
}

impl AppMemoryReadGrant {
    pub fn selection(&self, mode: AppMemoryRunMode) -> &AppMemoryReadSelection {
        match mode {
            AppMemoryRunMode::Interactive => &self.interactive,
            AppMemoryRunMode::Background => &self.background,
        }
    }
}

/// Validate a manifest request. Refuses unknown and root tiers, invalid agent
/// ids, duplicates, empty requests and oversized lists.
pub fn validate_memory_read_request(request: &AppMemoryReadRequest) -> Result<(), String> {
    if request.user_tiers.is_empty() && request.agents.is_empty() {
        return Err("app.memory.read must request at least one user tier or agent".to_owned());
    }
    let purpose = request.purpose.trim();
    if purpose.is_empty() || request.purpose.len() > MAX_APP_MEMORY_READ_PURPOSE_BYTES {
        return Err(format!(
            "app.memory.read.purpose must contain between 1 and {MAX_APP_MEMORY_READ_PURPOSE_BYTES} bytes"
        ));
    }
    if request.user_tiers.len() > MAX_APP_MEMORY_READ_TIERS {
        return Err(format!(
            "app.memory.read.user_tiers exceeds {MAX_APP_MEMORY_READ_TIERS} entries"
        ));
    }
    if request.agents.len() > MAX_APP_MEMORY_READ_AGENTS {
        return Err(format!(
            "app.memory.read.agents exceeds {MAX_APP_MEMORY_READ_AGENTS} entries"
        ));
    }
    let mut seen = BTreeSet::new();
    for tier in &request.user_tiers {
        match app_memory_tier_readability(tier) {
            None => {
                return Err(format!(
                    "app.memory.read.user_tiers: `{tier}` is not a user memory tier"
                ))
            },
            Some(AppMemoryTierReadability::NotReadable) => {
                return Err(format!(
                "app.memory.read.user_tiers: `{tier}` is a root tier and cannot be granted to apps"
            ))
            },
            Some(_) => {},
        }
        if !seen.insert(tier.as_str()) {
            return Err(format!("app.memory.read.user_tiers lists `{tier}` twice"));
        }
    }
    let mut seen = BTreeSet::new();
    for agent in &request.agents {
        validate_agent_identifier(agent).map_err(|error| {
            format!("app.memory.read.agents: `{agent}` is not a valid agent id: {error}")
        })?;
        if !seen.insert(agent.as_str()) {
            return Err(format!("app.memory.read.agents lists `{agent}` twice"));
        }
    }
    Ok(())
}

/// Stable identity of a request, pinned by every grant made from it.
pub fn memory_read_request_digest(request: &AppMemoryReadRequest) -> Option<AppDigest> {
    let mut tiers = request.user_tiers.clone();
    tiers.sort();
    let mut agents = request.agents.clone();
    agents.sort();
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": "magician.app-memory-read-request.v1",
        "user_tiers": tiers,
        "agents": agents,
        "purpose": request.purpose,
    }))
    .ok()
}

/// The grant an owner gets without choosing: every requested non-sensitive
/// tier and every requested agent while the owner uses the app, nothing in
/// the background.
pub fn default_memory_read_grant(request: &AppMemoryReadRequest) -> Option<AppMemoryReadGrant> {
    let interactive = AppMemoryReadSelection {
        user_tiers: request
            .user_tiers
            .iter()
            .filter(|tier| {
                app_memory_tier_readability(tier) == Some(AppMemoryTierReadability::Ordinary)
            })
            .cloned()
            .collect(),
        agents: request.agents.clone(),
    }
    .normalized();
    Some(AppMemoryReadGrant {
        schema: APP_MEMORY_READ_GRANT_SCHEMA.to_owned(),
        request_digest: memory_read_request_digest(request)?,
        engagement: AppMemoryEngagementBinding::OwnerOnly,
        interactive,
        background: AppMemoryReadSelection::default(),
    })
}

/// Build an owner-chosen grant, refusing anything outside the request or a
/// grant made against a different request.
pub fn owner_memory_read_grant(
    request: &AppMemoryReadRequest,
    reviewed_request_digest: &AppDigest,
    interactive: AppMemoryReadSelection,
    background: AppMemoryReadSelection,
) -> Result<AppMemoryReadGrant, String> {
    let request_digest = memory_read_request_digest(request)
        .ok_or_else(|| "memory read request could not be digested".to_owned())?;
    if &request_digest != reviewed_request_digest {
        return Err("memory grant was made against a different request; review again".to_owned());
    }
    let grant = AppMemoryReadGrant {
        schema: APP_MEMORY_READ_GRANT_SCHEMA.to_owned(),
        request_digest,
        engagement: AppMemoryEngagementBinding::OwnerOnly,
        interactive: interactive.normalized(),
        background: background.normalized(),
    };
    validate_memory_read_grant(&grant, request)?;
    Ok(grant)
}

/// A grant may only narrow its request: every tier and agent in either run
/// mode must have been requested, and the grant must pin that request.
pub fn validate_memory_read_grant(
    grant: &AppMemoryReadGrant,
    request: &AppMemoryReadRequest,
) -> Result<(), String> {
    if grant.schema != APP_MEMORY_READ_GRANT_SCHEMA {
        return Err("unknown memory grant schema".to_owned());
    }
    if memory_read_request_digest(request).as_ref() != Some(&grant.request_digest) {
        return Err("memory grant does not pin this request".to_owned());
    }
    for (mode, selection) in [
        (AppMemoryRunMode::Interactive, &grant.interactive),
        (AppMemoryRunMode::Background, &grant.background),
    ] {
        for tier in &selection.user_tiers {
            if !request.user_tiers.contains(tier) {
                return Err(format!(
                    "{} memory grant includes tier `{tier}` the app did not request",
                    mode.as_str()
                ));
            }
        }
        for agent in &selection.agents {
            if !request.agents.contains(agent) {
                return Err(format!(
                    "{} memory grant includes agent `{agent}` the app did not request",
                    mode.as_str()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tiers: &[&str], agents: &[&str]) -> AppMemoryReadRequest {
        AppMemoryReadRequest {
            user_tiers: tiers.iter().map(|tier| (*tier).to_owned()).collect(),
            agents: agents.iter().map(|agent| (*agent).to_owned()).collect(),
            purpose: "Personalise suggestions".to_owned(),
        }
    }

    #[test]
    fn tier_readability_fails_closed() {
        assert_eq!(
            app_memory_tier_readability("preferences"),
            Some(AppMemoryTierReadability::Ordinary)
        );
        for sensitive in ["identity", "accounts", "screen_observations"] {
            assert_eq!(
                app_memory_tier_readability(sensitive),
                Some(AppMemoryTierReadability::Sensitive),
                "{sensitive}"
            );
        }
        for root in ["knowledge", "user"] {
            assert_eq!(
                app_memory_tier_readability(root),
                Some(AppMemoryTierReadability::NotReadable),
                "{root}"
            );
        }
        assert_eq!(app_memory_tier_readability("made_up"), None);
        // Every registered tier that is neither ordinary nor root is sensitive,
        // so a newly added tier is never granted by default.
        for tier in USER_MEMORY_TIERS {
            let readability = app_memory_tier_readability(tier.name).unwrap();
            if !ORDINARY_USER_TIERS.contains(&tier.name) && !tier.normalizes_to_root {
                assert_eq!(
                    readability,
                    AppMemoryTierReadability::Sensitive,
                    "{}",
                    tier.name
                );
            }
        }
    }

    #[test]
    fn requests_refuse_root_unknown_duplicate_and_empty() {
        assert!(validate_memory_read_request(&request(&["preferences"], &["scribe"])).is_ok());
        assert!(validate_memory_read_request(&request(&[], &[])).is_err());
        assert!(validate_memory_read_request(&request(&["knowledge"], &[])).is_err());
        assert!(validate_memory_read_request(&request(&["made_up"], &[])).is_err());
        assert!(validate_memory_read_request(&request(&["contacts", "contacts"], &[])).is_err());
        assert!(validate_memory_read_request(&request(&[], &["../escape"])).is_err());
        let mut blank = request(&["preferences"], &[]);
        blank.purpose = "  ".to_owned();
        assert!(validate_memory_read_request(&blank).is_err());
    }

    #[test]
    fn the_default_grant_leaves_sensitive_tiers_and_background_empty() {
        let request = request(&["preferences", "identity", "contacts"], &["scribe"]);
        let grant = default_memory_read_grant(&request).unwrap();
        assert_eq!(grant.interactive.user_tiers, ["contacts", "preferences"]);
        assert_eq!(grant.interactive.agents, ["scribe"]);
        assert!(grant.background.is_empty());
        assert_eq!(grant.engagement, AppMemoryEngagementBinding::OwnerOnly);
        assert!(validate_memory_read_grant(&grant, &request).is_ok());
    }

    #[test]
    fn an_owner_grant_can_only_narrow_the_request_it_reviewed() {
        let request = request(&["preferences", "identity"], &["scribe"]);
        let digest = memory_read_request_digest(&request).unwrap();
        let sensitive_ticked = owner_memory_read_grant(
            &request,
            &digest,
            AppMemoryReadSelection {
                user_tiers: vec!["identity".into()],
                agents: vec![],
            },
            AppMemoryReadSelection {
                user_tiers: vec!["preferences".into()],
                agents: vec!["scribe".into()],
            },
        )
        .expect("an explicit tick may include a sensitive tier");
        assert!(sensitive_ticked.interactive.grants_tier("identity"));
        assert!(sensitive_ticked.background.grants_agent("scribe"));

        let widened = owner_memory_read_grant(
            &request,
            &digest,
            AppMemoryReadSelection {
                user_tiers: vec!["contacts".into()],
                agents: vec![],
            },
            AppMemoryReadSelection::default(),
        );
        assert!(
            widened.is_err(),
            "a tier the app never requested cannot be granted"
        );

        let other = request_digest_of(&["preferences"]);
        assert!(owner_memory_read_grant(
            &request,
            &other,
            AppMemoryReadSelection::default(),
            AppMemoryReadSelection::default()
        )
        .is_err());
    }

    fn request_digest_of(tiers: &[&str]) -> AppDigest {
        memory_read_request_digest(&request(tiers, &[])).unwrap()
    }
}
