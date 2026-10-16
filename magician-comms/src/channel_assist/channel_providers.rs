//! Data-driven channel-provider registry — the ONE place a channel's
//! classification lives, so adding a provider (or an email domain) is a
//! config edit, not scattered `match provider { … }` arms across
//! `channel_label` / `evidence_tier` / `channel_to_provider`.
//!
//! Source of truth is the `channel_providers:` block in
//! `operator-config.yaml`; built-in defaults cover the shipped providers so
//! the system still works if the block is absent. Each entry declares the
//! provider's transport-agnostic classification:
//!
//! ```yaml
//! channel_providers:
//!   gmail:          { kind: email, channel: email, domains: [example.com, corp.example.com, gmail.com] }
//!   agentmail:      { kind: email, domains: [agentmail.to] }   # channel defaults to the key
//!   whatsapp:       { kind: chat }
//!   whatsapp_kapso: { kind: chat }
//!   telegram:       { kind: chat }
//! ```
//!
//! Note on the model: `provider` is the TRANSPORT (which adapter/CLI), not
//! the domain — every Google domain (example.com / corp.example.com / gmail.com)
//! is the one `gmail` transport, while `agentmail.to` is a different one.
//! Many domains map to one provider, so `domains` is a provider→domains
//! index (used by [`provider_for_domain`] and documentation), and the
//! email-vs-chat `kind` is declared per provider. A genuinely new transport
//! still needs its ingestor code; everything derivable — the prompt word,
//! the WEG tier, the channel name, the known-provider set — comes from here.

use std::sync::OnceLock;

use magician::magician_v2::artifact_v2::workspace::runtime_config_path;
// Plan 3.1 prerequisite (b): the tier names are the cross-crate
// tier-name contract (lib-side `evidence::tier_contracts`), not local
// string literals — the tier distiller and the chat tier registry
// reference the same constants.
use magician::magician_v2::evidence::tier_contracts::{CHAT_EVIDENCE_TIER, EMAIL_EVIDENCE_TIER};

/// Whether a channel is email-shaped (subjects, addresses, threads) or
/// chat-shaped. Drives the distill prompt word and the WEG evidence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Email,
    Chat,
}

impl ChannelKind {
    /// The single `{channel}` word handed to the distill prompt.
    pub fn label(self) -> &'static str {
        match self {
            ChannelKind::Email => "email",
            ChannelKind::Chat => "chat",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "email" | "mail" => Some(ChannelKind::Email),
            "chat" | "message" | "im" => Some(ChannelKind::Chat),
            _ => None,
        }
    }
}

/// The WEG memory tier a channel's evidence lands in (mirrors the digest's
/// `writer_vars`). Fixed per KIND — email evidence for email, chat for chat.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceTier {
    pub tier: &'static str,
    pub key_prefix: &'static str,
    pub source_type: &'static str,
}

const EMAIL_TIER: EvidenceTier = EvidenceTier {
    tier: EMAIL_EVIDENCE_TIER,
    key_prefix: "email",
    source_type: "email_capture",
};
const CHAT_TIER: EvidenceTier = EvidenceTier {
    tier: CHAT_EVIDENCE_TIER,
    key_prefix: "chat",
    source_type: "chat_capture",
};

impl ChannelKind {
    pub fn evidence_tier(self) -> EvidenceTier {
        match self {
            ChannelKind::Email => EMAIL_TIER,
            ChannelKind::Chat => CHAT_TIER,
        }
    }
}

/// One provider's declaration.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Transport key, matched against `ChannelAccount::provider`.
    pub provider: String,
    /// User-facing channel name in the unified `channel_observe` config
    /// (gmail's channel is `email`; others default to the provider key).
    pub channel: String,
    pub kind: ChannelKind,
    /// Email domains served by this transport — a provider→domains index
    /// (not used to pick the transport, which several domains share).
    pub domains: Vec<String>,
}

/// Built-in defaults for the shipped providers — used when the config omits
/// `channel_providers:` or a given provider. Config entries override/extend.
fn builtin() -> Vec<ProviderInfo> {
    vec![
        ProviderInfo {
            provider: "gmail".to_string(),
            channel: "email".to_string(),
            kind: ChannelKind::Email,
            domains: Vec::new(),
        },
        ProviderInfo {
            provider: "agentmail".to_string(),
            channel: "agentmail".to_string(),
            kind: ChannelKind::Email,
            domains: Vec::new(),
        },
        ProviderInfo {
            provider: "whatsapp".to_string(),
            channel: "whatsapp".to_string(),
            kind: ChannelKind::Chat,
            domains: Vec::new(),
        },
        ProviderInfo {
            provider: "whatsapp_kapso".to_string(),
            channel: "whatsapp_kapso".to_string(),
            kind: ChannelKind::Chat,
            domains: Vec::new(),
        },
        ProviderInfo {
            provider: "telegram".to_string(),
            channel: "telegram".to_string(),
            kind: ChannelKind::Chat,
            domains: Vec::new(),
        },
        ProviderInfo {
            provider: "imessage".to_string(),
            channel: "imessage".to_string(),
            kind: ChannelKind::Chat,
            domains: Vec::new(),
        },
    ]
}

/// Overlay the `channel_providers:` config block onto the built-in defaults
/// (pure — the config value is injected so this is unit-testable without a
/// file). A config entry overrides its provider's kind/channel/domains, or
/// adds a brand-new provider.
fn merge(mut table: Vec<ProviderInfo>, cfg: &serde_yaml::Value) -> Vec<ProviderInfo> {
    let Some(map) = cfg.get("channel_providers").and_then(|v| v.as_mapping()) else {
        return table;
    };
    for (key, val) in map {
        let Some(provider) = key.as_str() else {
            continue;
        };
        let kind = val
            .get("kind")
            .and_then(|k| k.as_str())
            .and_then(ChannelKind::from_str);
        let channel = val
            .get("channel")
            .and_then(|c| c.as_str())
            .map(str::to_string);
        let domains: Vec<String> = val
            .get("domains")
            .and_then(|d| d.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        match table.iter_mut().find(|p| p.provider == provider) {
            Some(existing) => {
                if let Some(k) = kind {
                    existing.kind = k;
                }
                if let Some(c) = channel {
                    existing.channel = c;
                }
                if !domains.is_empty() {
                    existing.domains = domains;
                }
            },
            None => {
                // A new provider needs a kind; without one it's uninterpretable.
                if let Some(k) = kind {
                    table.push(ProviderInfo {
                        provider: provider.to_string(),
                        channel: channel.unwrap_or_else(|| provider.to_string()),
                        kind: k,
                        domains,
                    });
                }
            },
        }
    }
    table
}

fn load_table() -> Vec<ProviderInfo> {
    let raw = std::fs::read_to_string(runtime_config_path(
        "operator-config.yaml",
        "skillshub/operator-config.yaml",
    ))
    .ok();
    let cfg: Option<serde_yaml::Value> = raw.as_deref().and_then(|r| serde_yaml::from_str(r).ok());
    match cfg {
        Some(cfg) => merge(builtin(), &cfg),
        None => builtin(),
    }
}

/// The process-wide registry, loaded once from operator-config (a config
/// change is picked up on restart, like every other operator-config field).
fn table() -> &'static [ProviderInfo] {
    static TABLE: OnceLock<Vec<ProviderInfo>> = OnceLock::new();
    TABLE.get_or_init(load_table)
}

pub fn info(provider: &str) -> Option<&'static ProviderInfo> {
    table().iter().find(|p| p.provider == provider)
}

/// The kind for a provider, or `None` if it isn't registered.
pub fn kind_of(provider: &str) -> Option<ChannelKind> {
    info(provider).map(|p| p.kind)
}

/// The distill prompt's `{channel}` word — `email` / `chat`, or `message`
/// for an unregistered provider (the prior fallback).
pub fn channel_label(provider: &str) -> &'static str {
    kind_of(provider)
        .map(ChannelKind::label)
        .unwrap_or("message")
}

/// The WEG evidence tier for a provider. Unregistered providers fall to the
/// chat tier (the prior catch-all).
pub fn evidence_tier(provider: &str) -> EvidenceTier {
    kind_of(provider)
        .unwrap_or(ChannelKind::Chat)
        .evidence_tier()
}

/// User-facing channel name → transport provider (`email` → `gmail`), or
/// `None` when the channel isn't a registered message provider (e.g.
/// `calendar`, which stays on the digest).
pub fn provider_for_channel(channel: &str) -> Option<&'static str> {
    table()
        .iter()
        .find(|p| p.channel == channel)
        .map(|p| p.provider.as_str())
}

/// Transport provider → its user-facing channel name (`gmail` → `email`);
/// unregistered providers map to themselves.
pub fn channel_for_provider(provider: &str) -> &str {
    info(provider)
        .map(|p| p.channel.as_str())
        .unwrap_or(provider)
}

/// Every registered transport provider — the validation set for the
/// channels API.
pub fn known_providers() -> Vec<&'static str> {
    table().iter().map(|p| p.provider.as_str()).collect()
}

pub fn is_known_provider(provider: &str) -> bool {
    info(provider).is_some()
}

/// The transport that serves an email domain, if any is indexed. Case-
/// insensitive; matches the domain suffix (so `mail.example.com` matches an
/// `example.com` entry).
pub fn provider_for_domain(domain: &str) -> Option<&'static str> {
    let needle = domain.trim().to_ascii_lowercase();
    table().iter().find_map(|p| {
        p.domains
            .iter()
            .any(|d| needle == *d || needle.ends_with(&format!(".{d}")))
            .then_some(p.provider.as_str())
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn builtin_classification_matches_the_old_match_arms() {
        // gmail + agentmail -> email; whatsapp(_kapso) + telegram -> chat; unknown -> message/chat.
        let t = builtin();
        let get = |p: &str| t.iter().find(|x| x.provider == p).map(|x| x.kind);
        assert_eq!(get("gmail"), Some(ChannelKind::Email));
        assert_eq!(get("agentmail"), Some(ChannelKind::Email));
        assert_eq!(get("whatsapp"), Some(ChannelKind::Chat));
        assert_eq!(get("whatsapp_kapso"), Some(ChannelKind::Chat));
        assert_eq!(get("telegram"), Some(ChannelKind::Chat));
        assert_eq!(get("imessage"), Some(ChannelKind::Chat));
    }

    #[test]
    fn label_and_tier_derive_from_kind() {
        assert_eq!(ChannelKind::Email.label(), "email");
        assert_eq!(ChannelKind::Chat.label(), "chat");
        assert_eq!(
            ChannelKind::Email.evidence_tier().tier,
            "user.email_evidence"
        );
        assert_eq!(ChannelKind::Chat.evidence_tier().tier, "user.chat_evidence");
    }

    #[test]
    fn config_can_add_a_new_provider_and_override_domains() {
        let cfg: serde_yaml::Value = serde_yaml::from_str(
            r#"
channel_providers:
  gmail:
    kind: email
    domains: [example.com, corp.example.com, gmail.com]
  slack:
    kind: chat
    channel: slack
  agentmail:
    kind: email
    domains: [agentmail.to]
"#,
        )
        .unwrap();
        let t = merge(builtin(), &cfg);
        // New provider added from config alone.
        let slack = t.iter().find(|p| p.provider == "slack").unwrap();
        assert_eq!(slack.kind, ChannelKind::Chat);
        assert_eq!(slack.channel, "slack");
        // Domains overlaid onto the built-in gmail entry.
        let gmail = t.iter().find(|p| p.provider == "gmail").unwrap();
        assert!(gmail.domains.contains(&"example.com".to_string()));
        assert_eq!(gmail.channel, "email"); // built-in channel preserved
    }

    #[test]
    fn provider_for_domain_matches_suffix_case_insensitively() {
        let cfg: serde_yaml::Value = serde_yaml::from_str(
            r#"
channel_providers:
  gmail:      { kind: email, domains: [example.com, corp.example.com] }
  agentmail:  { kind: email, domains: [agentmail.to] }
"#,
        )
        .unwrap();
        let t = merge(builtin(), &cfg);
        let lookup = |domain: &str| {
            t.iter().find_map(|p| {
                p.domains
                    .iter()
                    .any(|d| {
                        let n = domain.to_ascii_lowercase();
                        n == *d || n.ends_with(&format!(".{d}"))
                    })
                    .then_some(p.provider.as_str())
            })
        };
        assert_eq!(lookup("Example.com"), Some("gmail"));
        assert_eq!(lookup("mail.corp.example.com"), Some("gmail"));
        assert_eq!(lookup("agentmail.to"), Some("agentmail"));
        assert_eq!(lookup("example.org"), None);
    }

    #[test]
    fn channel_provider_mapping_roundtrips() {
        let t = builtin();
        let p_for_c = |c: &str| {
            t.iter()
                .find(|p| p.channel == c)
                .map(|p| p.provider.as_str())
        };
        assert_eq!(p_for_c("email"), Some("gmail"));
        assert_eq!(p_for_c("agentmail"), Some("agentmail"));
        assert_eq!(p_for_c("calendar"), None); // not a message provider
    }
}
