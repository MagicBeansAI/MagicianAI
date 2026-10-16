//! SKILL.md frontmatter types + structural kind inference.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

/// A loaded SKILL.md — frontmatter + lazy body.
#[derive(Debug, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,

    #[serde(default)]
    pub license: Option<String>,

    #[serde(default)]
    pub compatibility: Option<String>,

    /// Spec-experimental field; space-separated tool list.
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Option<String>,

    #[serde(default)]
    pub metadata: SkillMetadata,

    /// Resolved skill folder path (set after load; not part of the on-disk frontmatter).
    #[serde(skip)]
    pub source_dir: PathBuf,

    /// Lazy-loaded body content.
    /// `pub(super)` so sibling loader code can construct manifests and reset
    /// the body cache; external callers go through `body()` which lazy-loads.
    #[serde(skip)]
    pub(super) body: OnceLock<String>,
}

impl Clone for SkillManifest {
    fn clone(&self) -> Self {
        // OnceLock is not Clone; build a fresh one and let the body re-load if needed.
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            license: self.license.clone(),
            compatibility: self.compatibility.clone(),
            allowed_tools: self.allowed_tools.clone(),
            metadata: self.metadata.clone(),
            source_dir: self.source_dir.clone(),
            body: OnceLock::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Magician extensions live exclusively here.
    #[serde(default)]
    pub magician: Option<MagicianMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MagicianMetadata {
    /// Presence promotes the skill to a personality-mode.
    #[serde(default)]
    pub personality: Option<PersonalitySpec>,

    #[serde(default)]
    pub requires: SkillRequires,

    /// Hints surfaced in Forge when `requires` are unmet.
    #[serde(default)]
    pub install_hint: std::collections::HashMap<String, String>,

    /// Capability Evolution Phase 3: when this skill was emitted by
    /// the promotion bridge as the SKILL.md form of a mined
    /// `ApiCapability`, the frontmatter carries a back-reference
    /// here. Operators reading the skill folder can tell it's
    /// auto-mined (not hand-authored) and tools like Forge surface
    /// an "evolved" badge based on its presence.
    #[serde(default)]
    pub evolved_pack_ref: Option<EvolvedPackRef>,
}

/// Back-reference embedded in evolved skill manifests linking to the
/// originating capability pack catalog entry. Present iff this skill
/// was emitted by `CapabilityPromotionBridge::emit_evolved_skill`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolvedPackRef {
    /// Capability ID from the mining-side `ApiCapability` record.
    /// Stable across promotions; lets the runtime correlate skill
    /// invocations back to replay counters on the mined capability.
    pub capability_id: String,
    /// Origin URL the capability was mined from.
    pub mining_origin: String,
    /// Lifecycle status at emission time. Updated by demotion when
    /// the bridge re-emits.
    pub lifecycle_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalitySpec {
    pub active_mode: String,
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub expression_bias: String,
    #[serde(default)]
    pub suppression_rules: String,
    #[serde(default)]
    pub expression_triggers: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillRequires {
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub scripts: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub env: Vec<String>,
    /// This skill relays through the macOS host gateway (the Tauri desktop app).
    /// When the gateway is unreachable (headless server, no desktop app) the
    /// skill is suppressed from the agent's catalog instead of failing at
    /// dispatch. See `config_extras::host_gateway_available`.
    #[serde(default)]
    pub host_gateway: bool,
    /// Local or relayed CuaDriver, independently of the Mac automation gate.
    #[serde(default)]
    pub cua: bool,
}

impl SkillRequires {
    pub fn providers_available(&self, mac_gateway: bool, cua: bool) -> bool {
        (!self.host_gateway || mac_gateway) && (!self.cua || cua)
    }
}

/// Two runtime routes inferred from frontmatter structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferredKind {
    /// Default — LLM-pickable from the catalog. May be plain (no scripts/bin)
    /// or tool-backed (has scripts/ and/or bin/).
    Procedure,
    /// `metadata.magician.personality` block present. Activated via
    /// `switch_personality`; writes into the agent's `personality_profile`
    /// memory tier.
    PersonalityMode,
}

impl SkillManifest {
    /// Inferred kind from `metadata.magician.*` block presence.
    pub fn inferred_kind(&self) -> InferredKind {
        match self.metadata.magician.as_ref() {
            Some(m) if m.personality.is_some() => InferredKind::PersonalityMode,
            _ => InferredKind::Procedure,
        }
    }

    pub fn body_loaded(&self) -> bool {
        self.body.get().is_some()
    }

    /// Lazy body load. The body is everything after the closing `---\n` of
    /// frontmatter. Cached in OnceLock for the lifetime of the manifest.
    pub fn body(&self) -> std::io::Result<&str> {
        if let Some(b) = self.body.get() {
            return Ok(b);
        }
        // `source_dir/SKILL.md` is a host-absolute symlink into skillshub on a
        // materialized scope; rewrite the target for this environment (identity
        // on a native host) so the lazy body load resolves in a container too.
        let raw = std::fs::read_to_string(super::path_rewrite::resolve_skill_path(
            &self.source_dir.join("SKILL.md"),
        ))?;
        let body_start = body_offset(&raw).unwrap_or(0);
        let _ = self.body.set(raw[body_start..].to_string());
        // Unwrap is safe: we just set it (or another caller did, OnceLock guarantees one wins).
        Ok(self.body.get().expect("body just set"))
    }
}

/// Find the byte offset of the body (just after closing `\n---\n` of frontmatter).
/// Returns None if no frontmatter is present.
fn body_offset(raw: &str) -> Option<usize> {
    let trimmed = raw.trim_start_matches('\u{feff}'); // strip BOM if present
    if !trimmed.starts_with("---\n") {
        return None;
    }
    let after_open = raw.len() - trimmed.len() + 4;
    raw[after_open..]
        .find("\n---\n")
        .map(|i| after_open + i + 5)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn cua_is_independent_of_the_mac_gateway() {
        let cua: SkillRequires = serde_yaml::from_str("cua: true").unwrap();
        let mac: SkillRequires = serde_yaml::from_str("host_gateway: true").unwrap();
        assert!(cua.providers_available(false, true));
        assert!(!cua.providers_available(true, false));
        assert!(!mac.providers_available(false, true));
        assert!(mac.providers_available(true, false));
        let both: SkillRequires = serde_yaml::from_str("cua: true\nhost_gateway: true").unwrap();
        assert!(!both.providers_available(false, true));
        assert!(!both.providers_available(true, false));
    }

    fn manifest(metadata: SkillMetadata) -> SkillManifest {
        SkillManifest {
            name: "x".into(),
            description: "d".into(),
            license: None,
            compatibility: None,
            allowed_tools: None,
            metadata,
            source_dir: PathBuf::new(),
            body: OnceLock::new(),
        }
    }

    #[test]
    fn inferred_kind_defaults_to_procedure() {
        assert_eq!(
            manifest(SkillMetadata::default()).inferred_kind(),
            InferredKind::Procedure
        );
    }

    #[test]
    fn inferred_kind_picks_personality_when_personality_block_present() {
        let m = manifest(SkillMetadata {
            magician: Some(MagicianMetadata {
                personality: Some(PersonalitySpec {
                    active_mode: "witty".into(),
                    voice: "x".into(),
                    expression_bias: String::new(),
                    suppression_rules: String::new(),
                    expression_triggers: String::new(),
                }),
                requires: SkillRequires::default(),
                install_hint: Default::default(),
                evolved_pack_ref: None,
            }),
        });
        assert_eq!(m.inferred_kind(), InferredKind::PersonalityMode);
    }

    #[test]
    fn body_offset_finds_body_after_closing_marker() {
        let raw = "---\nname: x\ndescription: d\n---\n# body\nstuff\n";
        let off = body_offset(raw).unwrap();
        assert_eq!(&raw[off..], "# body\nstuff\n");
    }

    #[test]
    fn body_offset_handles_bom() {
        let raw = "\u{feff}---\nname: x\ndescription: d\n---\n# body\n";
        let off = body_offset(raw).unwrap();
        assert_eq!(&raw[off..], "# body\n");
    }

    #[test]
    fn body_offset_returns_none_for_missing_frontmatter() {
        assert!(body_offset("just markdown").is_none());
    }
}
