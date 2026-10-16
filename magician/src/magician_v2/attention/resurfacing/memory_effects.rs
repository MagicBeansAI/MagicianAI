//! Attributed memory effects (design steps 5–9).
//!
//! Compiled live mode is **shadow**: every effect is recorded on the
//! decision record and nothing is hidden. Salience deltas are computed
//! and capped but not written into stored scores until an owner Accept
//! moves [`effective_memory_effect_mode`] to Canary. Enforcement stays
//! off until that later Accept.
//!
//! Tiers of evidence:
//! - stated + normative + scoped → may propose suppress
//! - inferred + conditionable → down-rank only
//! - unknown / untrusted → show, no effect

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::{MemoryKind, MemoryTrust};

use crate::magician_v2::agents::memory_scope::CandidateAttributes;

use super::memory_context::{MemoryApplication, MemoryApplicationRecord, ScopedMemory};

/// Compiled default. Serving uses [`effective_memory_effect_mode`], which
/// starts as this const and can move after an owner HITL / `/memory` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEffectMode {
    Shadow,
    Canary,
    Enforced,
}

pub const MEMORY_EFFECT_MODE: MemoryEffectMode = MemoryEffectMode::Shadow;

impl MemoryEffectMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Canary => "canary",
            Self::Enforced => "enforced",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemoryEffectRuntimeFile {
    #[serde(default)]
    mode: Option<MemoryEffectMode>,
    #[serde(default)]
    last_prompted_advice: Option<String>,
    #[serde(default)]
    last_prompted_judgement_count: Option<u32>,
}

struct MemoryEffectRuntime {
    path: Option<PathBuf>,
    file: MemoryEffectRuntimeFile,
}

fn runtime() -> &'static Mutex<MemoryEffectRuntime> {
    static RUNTIME: OnceLock<Mutex<MemoryEffectRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Mutex::new(MemoryEffectRuntime {
            path: None,
            file: MemoryEffectRuntimeFile::default(),
        })
    })
}

fn persist_runtime(inner: &MemoryEffectRuntime) -> std::io::Result<()> {
    let Some(path) = inner.path.as_ref() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&inner.file)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, bytes)
}

fn load_runtime_file(path: &Path) -> MemoryEffectRuntimeFile {
    let Ok(bytes) = std::fs::read(path) else {
        return MemoryEffectRuntimeFile::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Load a persisted owner override (if any). Safe to call once at boot.
pub fn init_memory_effect_runtime(path: PathBuf) {
    let file = load_runtime_file(&path);
    let mut inner = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inner.path = Some(path);
    inner.file = file;
}

/// Compiled const unless an owner accepted Canary/Enforced.
pub fn effective_memory_effect_mode() -> MemoryEffectMode {
    runtime()
        .lock()
        .ok()
        .and_then(|inner| inner.file.mode)
        .unwrap_or(MEMORY_EFFECT_MODE)
}

pub fn set_memory_effect_mode(mode: MemoryEffectMode) -> std::io::Result<()> {
    let mut inner = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inner.file.mode = Some(mode);
    persist_runtime(&inner)
}

pub fn record_memory_effect_prompt(advice: &str, judgement_count: u32) -> std::io::Result<()> {
    let mut inner = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inner.file.last_prompted_advice = Some(advice.to_owned());
    inner.file.last_prompted_judgement_count = Some(judgement_count);
    persist_runtime(&inner)
}

pub fn last_memory_effect_prompt() -> (Option<String>, Option<u32>) {
    let inner = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        inner.file.last_prompted_advice.clone(),
        inner.file.last_prompted_judgement_count,
    )
}

// Gated like `magician_v2::test_support`: the comms crate's test modules
// reach these through the relocation shim, which cannot raise `pub(crate)`
// visibility or keep a dependency crate's `cfg(test)` active.
#[cfg(any(test, feature = "test-fixtures"))]
pub static MEMORY_EFFECT_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(any(test, feature = "test-fixtures"))]
pub fn reset_memory_effect_runtime_for_test() {
    let mut inner = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *inner = MemoryEffectRuntime {
        path: None,
        file: MemoryEffectRuntimeFile::default(),
    };
}

const STATED_SUPPRESS_STRENGTH: f64 = 0.08;
const STATED_SALIENCE_STRENGTH: f64 = 0.06;
const INFERRED_SALIENCE_STRENGTH: f64 = 0.04;
const MAX_PASS_SALIENCE: f64 = 0.20;
const SUPPRESS_STRENGTH_FLOOR: f64 = 0.03;
/// Each exact series dismiss raises the suppress floor this much.
const DISMISS_FLOOR_STEP: f64 = 0.01;
/// An old / often-dismissed exclusion must clear this much strength to keep firing.
const DISMISS_FLOOR_CAP: f64 = 0.15;
const NORMATIVE_HALF_LIFE_DAYS: f64 = 60.0;
const OTHER_HALF_LIFE_DAYS: f64 = 90.0;
const SECONDS_PER_DAY: f64 = 86_400.0;
/// Enforced-mode floor. Shadow shows the whole would-hide population (1.0).
pub const ENFORCED_EXPLORATION_RATE: f64 = 0.03;
/// Novelty never lowers the bar. WeakSignal already drops `<= 0`; serendipity
/// slots refuse near-zero junk that barely cleared that cut.
pub const SERENDIPITY_SALIENCE_FLOOR: f32 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEffectKind {
    Explain,
    Salience,
    Suppress,
    ProposeAction,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryConflict {
    pub memory_key: String,
    pub rationale: String,
    #[serde(default)]
    pub agree_count: u32,
    #[serde(default)]
    pub disagree_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryJudgement {
    pub applications: MemoryApplicationRecord,
    pub would_suppress: bool,
    pub suppress_reason: Option<String>,
    pub suppress_series_key: Option<String>,
    pub salience_delta: f64,
    pub conflicts: Vec<MemoryConflict>,
    pub proposed_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryExploration {
    pub explore: bool,
    pub series_key: Option<String>,
    pub exact_prior_dismiss: bool,
    pub propensity: f64,
}

impl MemoryJudgement {
    /// Shadow never removes a card. Enforced would flip this.
    pub fn hard_eligible(&self, mode: MemoryEffectMode) -> (bool, Option<String>) {
        match mode {
            MemoryEffectMode::Shadow | MemoryEffectMode::Canary => {
                (true, self.suppress_reason.clone())
            },
            MemoryEffectMode::Enforced => (!self.would_suppress, self.suppress_reason.clone()),
        }
    }
}

pub fn decay_factor(age_days: f64, kind: MemoryKind) -> f64 {
    let half_life = if matches!(kind, MemoryKind::Normative) {
        NORMATIVE_HALF_LIFE_DAYS
    } else {
        OTHER_HALF_LIFE_DAYS
    };
    if !age_days.is_finite() || age_days <= 0.0 {
        return 1.0;
    }
    0.5_f64.powf(age_days / half_life)
}

pub fn parse_updated_at_secs(raw: Option<&str>) -> Option<i64> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<i64>() {
        return Some(secs);
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.timestamp())
}

fn age_days(updated_at: Option<&str>, now_secs: i64) -> f64 {
    match parse_updated_at_secs(updated_at) {
        Some(then) if now_secs > then => (now_secs - then) as f64 / SECONDS_PER_DAY,
        _ => 0.0,
    }
}

fn trust_rank(trust: MemoryTrust) -> u8 {
    match trust {
        MemoryTrust::Stated => 2,
        MemoryTrust::Inferred => 1,
        MemoryTrust::Untrusted => 0,
    }
}

/// Stated beats inferred; newer beats older. Specific (more topics) beats general.
fn precedence_key(memory: &ScopedMemory) -> (u8, i64, usize) {
    (
        trust_rank(memory.trust),
        parse_updated_at_secs(memory.updated_at.as_deref()).unwrap_or(0),
        memory.scope.topics.len() + memory.scope.entities.len(),
    )
}

pub fn evaluate_memory_effects(
    candidate: &CandidateAttributes,
    memories: &[ScopedMemory],
    now_secs: i64,
    recent_positive_engagement: bool,
) -> MemoryJudgement {
    evaluate_memory_effects_with_dismisses(
        candidate,
        memories,
        now_secs,
        recent_positive_engagement,
        0,
    )
}

pub fn evaluate_memory_effects_with_dismisses(
    candidate: &CandidateAttributes,
    memories: &[ScopedMemory],
    now_secs: i64,
    recent_positive_engagement: bool,
    exact_series_dismisses: u32,
) -> MemoryJudgement {
    let mut hits: Vec<&ScopedMemory> = memories
        .iter()
        .filter(|memory| memory.scope.matches(candidate))
        .collect();
    hits.sort_by(|left, right| precedence_key(right).cmp(&precedence_key(left)));
    hits.truncate(5);

    let mut judgement = MemoryJudgement::default();
    let mut pass_salience = 0.0_f64;
    let mut inferred_overridden = false;

    for memory in hits {
        if matches!(memory.trust, MemoryTrust::Untrusted) {
            continue;
        }
        let decay = decay_factor(
            age_days(memory.updated_at.as_deref(), now_secs),
            memory.kind,
        );
        if memory.may_explain {
            judgement.applications.would_apply.push(MemoryApplication {
                memory_key: memory.key.clone(),
                memory_revision: memory.updated_at.clone(),
                direction: "explain".to_string(),
                rationale: explain_rationale(memory),
                strength: None,
            });
        }
        if memory.may_propose_action && matches!(memory.trust, MemoryTrust::Stated) {
            judgement.proposed_action = Some(format!("follow {}", memory.key));
            judgement.applications.would_apply.push(MemoryApplication {
                memory_key: memory.key.clone(),
                memory_revision: memory.updated_at.clone(),
                direction: "propose".to_string(),
                rationale: format!("a stored procedure at {} may apply", memory.key),
                strength: None,
            });
        }

        if memory.may_suppress {
            let strength = STATED_SUPPRESS_STRENGTH * decay;
            let suppress_floor =
                suppress_floor_after_dismisses(SUPPRESS_STRENGTH_FLOOR, exact_series_dismisses);
            if strength >= suppress_floor && !judgement.would_suppress {
                judgement.would_suppress = true;
                judgement.suppress_series_key = Some(series_key(memory));
                judgement.suppress_reason = Some(format!(
                    "stated preference {} (strength {strength:.2}, learned prior)",
                    memory.key
                ));
                judgement.applications.would_apply.push(MemoryApplication {
                    memory_key: memory.key.clone(),
                    memory_revision: memory.updated_at.clone(),
                    direction: "suppress".to_string(),
                    rationale: judgement.suppress_reason.clone().unwrap_or_default(),
                    strength: Some(strength),
                });
                if recent_positive_engagement {
                    judgement.conflicts.push(MemoryConflict {
                        memory_key: memory.key.clone(),
                        rationale: format!(
                            "still applying {} — recent engagement disagrees",
                            memory.key
                        ),
                        disagree_count: 1,
                        ..Default::default()
                    });
                }
            }
        } else if memory.may_condition && !matches!(memory.trust, MemoryTrust::Untrusted) {
            if judgement.would_suppress {
                inferred_overridden = matches!(memory.trust, MemoryTrust::Inferred);
                continue;
            }
            let prior = if matches!(memory.trust, MemoryTrust::Stated) {
                STATED_SALIENCE_STRENGTH
            } else {
                INFERRED_SALIENCE_STRENGTH
            };
            let delta = (-prior * decay).clamp(-STATED_SALIENCE_STRENGTH, 0.0);
            let next = (pass_salience + delta).clamp(-MAX_PASS_SALIENCE, MAX_PASS_SALIENCE);
            let applied = next - pass_salience;
            if applied.abs() > f64::EPSILON {
                pass_salience = next;
                judgement.applications.would_apply.push(MemoryApplication {
                    memory_key: memory.key.clone(),
                    memory_revision: memory.updated_at.clone(),
                    direction: "salience".to_string(),
                    rationale: format!(
                        "salience {applied:+.3} from {} (decay {:.2})",
                        memory.key, decay
                    ),
                    strength: Some(applied),
                });
            }
        }
    }

    if inferred_overridden {
        if let Some(reason) = judgement.suppress_reason.as_ref() {
            judgement.conflicts.push(MemoryConflict {
                memory_key: reason.clone(),
                rationale: "stated rule overrides inferred down-rank".to_string(),
                ..Default::default()
            });
        }
    }
    judgement.salience_delta = pass_salience;
    judgement
}

fn explain_rationale(memory: &ScopedMemory) -> String {
    if !memory.may_explain {
        return format!("a stored preference at {} may apply", memory.key);
    }
    if memory.text.trim().is_empty() {
        format!("you keep a preference at {}", memory.key)
    } else {
        format!("you said {}", memory.text.trim())
    }
}

/// Stage-2 may only drop a stage-1 attachment. It cannot invent a memory,
/// raise trust, or grant suppress rights the rule pass did not already propose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage2ApplyVerdict {
    pub memory_key: String,
    pub applies: bool,
}

pub fn parse_stage2_verdicts(raw: &str) -> Vec<Stage2ApplyVerdict> {
    let trimmed = raw.trim();
    let json = if let Some(start) = trimmed.find('{') {
        let end = trimmed
            .rfind('}')
            .unwrap_or(trimmed.len().saturating_sub(1));
        &trimmed[start..=end]
    } else {
        trimmed
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(rows) = value.get("memories").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let memory_key = row.get("key").and_then(|v| v.as_str())?.trim();
            if memory_key.is_empty() {
                return None;
            }
            Some(Stage2ApplyVerdict {
                memory_key: memory_key.to_owned(),
                applies: row.get("applies").and_then(|v| v.as_bool()).unwrap_or(true),
            })
        })
        .collect()
}

pub fn apply_stage2_verdicts(judgement: &mut MemoryJudgement, verdicts: &[Stage2ApplyVerdict]) {
    if verdicts.is_empty() {
        return;
    }
    let denied: HashSet<&str> = verdicts
        .iter()
        .filter(|verdict| !verdict.applies)
        .map(|verdict| verdict.memory_key.as_str())
        .collect();
    if denied.is_empty() {
        return;
    }
    judgement
        .applications
        .would_apply
        .retain(|application| !denied.contains(application.memory_key.as_str()));
    judgement
        .conflicts
        .retain(|conflict| !denied.contains(conflict.memory_key.as_str()));
    if !judgement
        .applications
        .would_apply
        .iter()
        .any(|application| application.direction == "suppress")
    {
        judgement.would_suppress = false;
        judgement.suppress_reason = None;
        judgement.suppress_series_key = None;
    }
    if !judgement
        .applications
        .would_apply
        .iter()
        .any(|application| application.direction == "propose")
    {
        judgement.proposed_action = None;
    }
    judgement.salience_delta = judgement
        .applications
        .would_apply
        .iter()
        .filter(|application| application.direction == "salience")
        .filter_map(|application| application.strength)
        .sum();
}

/// Raise the suppress floor with each exact series dismiss so an often-seen
/// exclusion must still clear a slightly higher bar. Caps at [`DISMISS_FLOOR_CAP`].
pub fn suppress_floor_after_dismisses(base: f64, exact_series_dismisses: u32) -> f64 {
    (base + DISMISS_FLOOR_STEP * f64::from(exact_series_dismisses)).min(DISMISS_FLOOR_CAP)
}

/// True when a would-be-suppress series exists and this is not the exact
/// dismissed id. Exploration may only show a *new* member of that series.
pub fn is_new_series_member(
    candidate_id: &str,
    series_key: Option<&str>,
    exact_dismissed_ids: &HashSet<String>,
) -> bool {
    series_key.is_some() && !exact_dismissed_ids.contains(candidate_id)
}

pub fn series_key(memory: &ScopedMemory) -> String {
    let mut tokens = memory.scope.topics.clone();
    tokens.extend(memory.scope.entities.iter().cloned());
    tokens.sort();
    tokens.dedup();
    format!("{}|{}", memory.key, tokens.join(","))
}

fn exploration_bucket(candidate_id: &str, series: &str) -> f64 {
    let digest = blake3::hash(format!("memory-explore\0{series}\0{candidate_id}").as_bytes());
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(prefix) as f64 / u64::MAX as f64
}

/// Exploration tests whether an exclusion still holds. It shows a *new*
/// member of the suppressed series, never the exact dismissed item.
/// Shadow is 100% exploration (propensity 1). Enforced uses the 3% floor.
pub fn assign_exploration(
    judgement: &MemoryJudgement,
    candidate_id: &str,
    exact_dismissed_ids: &HashSet<String>,
    mode: MemoryEffectMode,
) -> MemoryExploration {
    let series_key = judgement.suppress_series_key.clone();
    if !judgement.would_suppress {
        return MemoryExploration {
            explore: false,
            series_key,
            exact_prior_dismiss: false,
            propensity: 0.0,
        };
    }
    let exact_prior_dismiss = exact_dismissed_ids.contains(candidate_id);
    if exact_prior_dismiss {
        return MemoryExploration {
            explore: false,
            series_key,
            exact_prior_dismiss: true,
            propensity: 0.0,
        };
    }
    match mode {
        MemoryEffectMode::Shadow | MemoryEffectMode::Canary => MemoryExploration {
            explore: true,
            series_key,
            exact_prior_dismiss: false,
            propensity: 1.0,
        },
        MemoryEffectMode::Enforced => {
            if !is_new_series_member(candidate_id, series_key.as_deref(), exact_dismissed_ids) {
                return MemoryExploration {
                    explore: false,
                    series_key,
                    exact_prior_dismiss: false,
                    propensity: 0.0,
                };
            }
            let series = judgement
                .suppress_series_key
                .as_deref()
                .unwrap_or("unknown");
            let drawn = exploration_bucket(candidate_id, series) < ENFORCED_EXPLORATION_RATE;
            MemoryExploration {
                explore: drawn,
                series_key,
                exact_prior_dismiss: false,
                propensity: ENFORCED_EXPLORATION_RATE,
            }
        },
    }
}

pub fn serendipity_floor_holds(intrinsic_salience: f32) -> bool {
    intrinsic_salience.is_finite() && intrinsic_salience >= SERENDIPITY_SALIENCE_FLOOR
}

/// Novelty selects among things already worth showing. It never lowers the bar.
pub fn admit_serendipity_slot(intrinsic_salience: f32, unlike_recent: bool) -> bool {
    unlike_recent && serendipity_floor_holds(intrinsic_salience)
}

/// Shadow serving gate: always eligible, reason recorded when a stated
/// normative memory would hide the card.
pub fn shadow_routing_eligibility(
    candidate: &CandidateAttributes,
    memories: &[ScopedMemory],
    now_secs: i64,
    recent_positive_engagement: bool,
) -> (bool, Option<String>) {
    evaluate_memory_effects(candidate, memories, now_secs, recent_positive_engagement)
        .hard_eligible(effective_memory_effect_mode())
}

/// Apply math for Canary/Enforced. Tests call this with a mode so they can
/// assert Canary apply without flipping the live [`MEMORY_EFFECT_MODE`] const.
fn apply_capped_salience_for_mode(score: f32, delta: f64, mode: MemoryEffectMode) -> f32 {
    if mode == MemoryEffectMode::Shadow || !delta.is_finite() {
        return score;
    }
    if delta > 0.0 && !serendipity_floor_holds(score) {
        return score;
    }
    let next = f64::from(score) + delta;
    if next.is_finite() {
        next.clamp(0.0, 1.0) as f32
    } else {
        score
    }
}

pub fn apply_capped_salience(score: f32, delta: f64) -> f32 {
    apply_capped_salience_for_mode(score, delta, effective_memory_effect_mode())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::memory_scope::{candidate_attributes, MemoryScope};

    fn memory(
        key: &str,
        source_type: &str,
        topics: &[&str],
        updated_at: Option<&str>,
    ) -> ScopedMemory {
        let trust = MemoryTrust::from_source_type(source_type);
        let kind = MemoryKind::for_tier("preferences");
        ScopedMemory {
            key: key.to_string(),
            tier: "preferences".to_string(),
            source_type: source_type.to_string(),
            trust,
            kind,
            text: key.to_string(),
            updated_at: updated_at.map(str::to_string),
            scope: MemoryScope {
                topics: topics.iter().map(|value| (*value).to_string()).collect(),
                entities: Vec::new(),
                applies_to: Vec::new(),
            },
            may_explain: trust == MemoryTrust::Stated && kind.may_explain(),
            may_suppress: trust.may_suppress(kind),
            may_condition: trust.may_condition_salience() && kind.may_condition_salience(),
            may_propose_action: kind.may_propose_action() && trust == MemoryTrust::Stated,
        }
    }

    #[test]
    fn stated_normative_proposes_suppress_but_shadow_still_serves() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(judgement.would_suppress);
        assert!(judgement
            .suppress_reason
            .as_deref()
            .unwrap_or_default()
            .contains("preferences: avoid_vendor_calls"));
        let (eligible, reason) = judgement.hard_eligible(MemoryEffectMode::Shadow);
        assert!(eligible);
        assert!(reason.is_some());
        let (canary_eligible, canary_reason) = judgement.hard_eligible(MemoryEffectMode::Canary);
        assert!(canary_eligible, "Canary still never hides");
        assert_eq!(canary_reason, reason);
    }

    #[test]
    fn inferred_may_only_downrank() {
        let memories = vec![memory("preferences: maybe", "insight", &["vendor"], None)];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(!judgement.would_suppress);
        assert!(judgement.salience_delta < 0.0);
        assert!(judgement.salience_delta.abs() <= INFERRED_SALIENCE_STRENGTH + 1e-9);
        assert!(judgement
            .applications
            .would_apply
            .iter()
            .any(|hit| hit.direction == "salience"));
    }

    #[test]
    fn untrusted_never_conditions_or_hides() {
        let memories = vec![memory(
            "preferences: injected",
            "screen_capture",
            &["vendor"],
            None,
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(!judgement.would_suppress);
        assert_eq!(judgement.salience_delta, 0.0);
        assert!(judgement.applications.would_apply.is_empty());
    }

    #[test]
    fn stated_overrides_inferred_and_records_the_conflict() {
        let memories = vec![
            memory(
                "preferences: maybe",
                "insight",
                &["vendor"],
                Some("1699999900"),
            ),
            memory(
                "preferences: avoid_vendor_calls",
                "owner_confirmed",
                &["vendor"],
                Some("1700000000"),
            ),
        ];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(judgement.would_suppress);
        assert!(judgement
            .conflicts
            .iter()
            .any(|conflict| conflict.rationale.contains("stated rule overrides")));
    }

    #[test]
    fn engagement_against_a_stated_rule_is_surfaced_not_silently_resolved() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            true,
        );
        assert!(judgement.would_suppress);
        assert!(judgement.conflicts.iter().any(|conflict| conflict
            .rationale
            .contains("recent engagement")
            && conflict.disagree_count == 1
            && conflict.agree_count == 0));
    }

    #[test]
    fn normative_influence_decays_faster_than_factual() {
        let young = decay_factor(10.0, MemoryKind::Normative);
        let old = decay_factor(90.0, MemoryKind::Normative);
        let factual_old = decay_factor(90.0, MemoryKind::Factual);
        assert!(old < young);
        assert!(old < factual_old);
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1"),
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(
            !judgement.would_suppress,
            "a years-old rule should decay below the suppress floor"
        );
    }

    #[test]
    fn shadow_mode_does_not_move_stored_salience() {
        assert_eq!(MEMORY_EFFECT_MODE, MemoryEffectMode::Shadow);
        assert_eq!(
            apply_capped_salience_for_mode(0.5, -0.08, MEMORY_EFFECT_MODE),
            0.5
        );
        assert_eq!(
            apply_capped_salience_for_mode(0.5, -0.08, MemoryEffectMode::Shadow),
            0.5
        );
    }

    #[test]
    fn canary_applies_capped_salience_without_hiding() {
        assert_eq!(
            apply_capped_salience_for_mode(0.5, -0.08, MemoryEffectMode::Canary),
            0.42
        );
        assert_eq!(
            apply_capped_salience_for_mode(0.02, -0.08, MemoryEffectMode::Canary),
            0.0
        );
        assert_eq!(
            apply_capped_salience_for_mode(0.5, -0.08, MemoryEffectMode::Enforced),
            0.42
        );
        assert_eq!(
            apply_capped_salience_for_mode(0.5, -0.08, MEMORY_EFFECT_MODE),
            0.5
        );
    }

    #[test]
    fn owner_override_moves_effective_mode_without_changing_the_const() {
        let _guard = MEMORY_EFFECT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_effect_runtime_for_test();
        assert_eq!(effective_memory_effect_mode(), MemoryEffectMode::Shadow);
        set_memory_effect_mode(MemoryEffectMode::Canary).unwrap();
        assert_eq!(effective_memory_effect_mode(), MemoryEffectMode::Canary);
        assert_eq!(MEMORY_EFFECT_MODE, MemoryEffectMode::Shadow);
        assert_eq!(apply_capped_salience(0.5, -0.08), 0.42);
        reset_memory_effect_runtime_for_test();
        assert_eq!(apply_capped_salience(0.5, -0.08), 0.5);
    }

    #[test]
    fn pass_salience_is_capped() {
        let many: Vec<ScopedMemory> = (0..8)
            .map(|index| {
                memory(
                    &format!("preferences: inferred_{index}"),
                    "insight",
                    &["vendor"],
                    None,
                )
            })
            .collect();
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &many,
            1_700_000_000,
            false,
        );
        assert!(judgement.salience_delta.abs() <= MAX_PASS_SALIENCE + 1e-9);
    }

    #[test]
    fn shadow_explores_a_new_series_member_and_never_the_exact_dismiss() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        let series = judgement.suppress_series_key.clone().expect("series");
        assert!(series.contains("avoid_vendor_calls"));
        assert!(series.contains("vendor"));

        let new_member = assign_exploration(
            &judgement,
            "agenda-aug-14",
            &HashSet::new(),
            MemoryEffectMode::Shadow,
        );
        assert!(new_member.explore);
        assert_eq!(new_member.propensity, 1.0);

        let mut dismissed = HashSet::new();
        dismissed.insert("agenda-jul-27".to_string());
        let exact = assign_exploration(
            &judgement,
            "agenda-jul-27",
            &dismissed,
            MemoryEffectMode::Shadow,
        );
        assert!(exact.exact_prior_dismiss);
        assert!(
            !exact.explore,
            "re-showing the rejected item is not exploration"
        );
    }

    #[test]
    fn enforced_exploration_is_a_bounded_draw_with_logged_propensity() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        let empty = HashSet::new();
        let mut drawn = 0;
        for index in 0..200 {
            let assignment = assign_exploration(
                &judgement,
                &format!("cand-{index}"),
                &empty,
                MemoryEffectMode::Enforced,
            );
            assert_eq!(assignment.propensity, ENFORCED_EXPLORATION_RATE);
            if assignment.explore {
                drawn += 1;
            }
        }
        assert!(
            drawn > 0 && drawn < 40,
            "3% of 200 should land in a small band, got {drawn}"
        );
    }

    #[test]
    fn is_new_series_member_requires_a_series_and_rejects_the_exact_id() {
        let mut dismissed = HashSet::new();
        dismissed.insert("agenda-jul-27".to_string());
        assert!(is_new_series_member(
            "agenda-aug-14",
            Some("preferences: avoid_vendor_calls|vendor"),
            &dismissed,
        ));
        assert!(!is_new_series_member(
            "agenda-jul-27",
            Some("preferences: avoid_vendor_calls|vendor"),
            &dismissed,
        ));
        assert!(!is_new_series_member("agenda-aug-14", None, &dismissed));
        assert!(!is_new_series_member(
            "agenda-aug-14",
            None,
            &HashSet::new(),
        ));
    }

    #[test]
    fn enforced_exploration_only_draws_on_new_series_members() {
        let with_series = MemoryJudgement {
            would_suppress: true,
            suppress_series_key: Some("preferences: avoid_vendor_calls|vendor".to_string()),
            ..Default::default()
        };
        let without_series = MemoryJudgement {
            would_suppress: true,
            suppress_series_key: None,
            ..Default::default()
        };
        let empty = HashSet::new();
        let mut dismissed = HashSet::new();
        dismissed.insert("agenda-jul-27".to_string());

        let no_series = assign_exploration(
            &without_series,
            "agenda-aug-14",
            &empty,
            MemoryEffectMode::Enforced,
        );
        assert!(!no_series.explore);
        assert_eq!(no_series.propensity, 0.0);

        let exact = assign_exploration(
            &with_series,
            "agenda-jul-27",
            &dismissed,
            MemoryEffectMode::Enforced,
        );
        assert!(exact.exact_prior_dismiss);
        assert!(!exact.explore);

        let shadow_without_series = assign_exploration(
            &without_series,
            "agenda-aug-14",
            &empty,
            MemoryEffectMode::Shadow,
        );
        assert!(
            shadow_without_series.explore,
            "Shadow stays 100% of would-hide minus exact dismiss"
        );
        assert_eq!(shadow_without_series.propensity, 1.0);
    }

    #[test]
    fn dismisses_raise_the_suppress_floor_then_cap() {
        assert_eq!(suppress_floor_after_dismisses(0.03, 0), 0.03);
        assert!((suppress_floor_after_dismisses(0.03, 1) - 0.04).abs() < 1e-12);
        assert!((suppress_floor_after_dismisses(0.03, 5) - 0.08).abs() < 1e-12);
        assert!((suppress_floor_after_dismisses(0.03, 12) - 0.15).abs() < 1e-12);
        assert_eq!(suppress_floor_after_dismisses(0.03, 20), 0.15);
    }

    #[test]
    fn many_exact_dismisses_can_hold_a_weak_rule_below_the_raised_floor() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let attrs = candidate_attributes(&["vendor"], "comm");
        let baseline = evaluate_memory_effects(&attrs, &memories, 1_700_000_000, false);
        assert!(baseline.would_suppress);
        let drifted =
            evaluate_memory_effects_with_dismisses(&attrs, &memories, 1_700_000_000, false, 20);
        assert!(
            !drifted.would_suppress,
            "0.08 prior should not clear a 0.15 floor"
        );
    }

    #[test]
    fn serendipity_never_lowers_the_intrinsic_bar() {
        assert!(admit_serendipity_slot(0.20, true));
        assert!(!admit_serendipity_slot(0.20, false));
        assert!(!admit_serendipity_slot(0.01, true));
        assert!(!admit_serendipity_slot(f32::NAN, true));
        assert!(serendipity_floor_holds(SERENDIPITY_SALIENCE_FLOOR));
    }

    #[test]
    fn stage2_may_only_drop_a_rule_attachment() {
        let memories = vec![memory(
            "preferences: avoid_vendor_calls",
            "owner_confirmed",
            &["vendor"],
            Some("1700000000"),
        )];
        let mut judgement = evaluate_memory_effects(
            &candidate_attributes(&["vendor"], "comm"),
            &memories,
            1_700_000_000,
            false,
        );
        assert!(judgement.would_suppress);
        apply_stage2_verdicts(
            &mut judgement,
            &[Stage2ApplyVerdict {
                memory_key: "preferences: avoid_vendor_calls".to_owned(),
                applies: false,
            }],
        );
        assert!(!judgement.would_suppress);
        assert!(judgement.applications.would_apply.is_empty());
    }

    #[test]
    fn stage2_parse_ignores_unknown_keys_and_wrapped_json() {
        let verdicts = parse_stage2_verdicts(
            "sure\n{\"memories\":[{\"key\":\"preferences: vendor\",\"applies\":false},{\"key\":\"\",\"applies\":true}]}\n",
        );
        assert_eq!(
            verdicts,
            vec![Stage2ApplyVerdict {
                memory_key: "preferences: vendor".to_owned(),
                applies: false,
            }]
        );
    }
}
