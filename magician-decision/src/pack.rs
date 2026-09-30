//! Versioned question packs.
//!
//! Packs are sibling artifacts to managed prompts: versioned JSON under
//! `data/magician_v2/decision_packs/<pack_id>/<version>.json`, loadable
//! beside the executable first and from the repo's data tree second. A
//! missing pack leaves its structured route unbound. The shared action driver
//! explicitly requests a generative planner when its configured route is absent.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::DecisionError;
use crate::primitives::Question;
use crate::request::{DecisionRequest, DecisionState};

/// A loaded, parsed question pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    /// Semver string. Thresholds and eval snapshots are pinned to
    /// (model id, pack id, pack version); bumping a pack re-derives them.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub questions: Vec<Question>,
}

impl Pack {
    /// Build the request for this pack against a state. The host may then
    /// instantiate dynamic Choice candidates before dispatching.
    pub fn to_request(&self, operation: &str, state: DecisionState) -> DecisionRequest {
        DecisionRequest {
            operation: operation.to_string(),
            pack_id: self.id.clone(),
            pack_version: self.version.clone(),
            state,
            questions: self.questions.clone(),
        }
    }
}

/// Resolves packs from the first root that has them.
///
/// Root order mirrors the prompt-store rule: packaged data beside the
/// running executable first, then the repo's `data/` tree (resolved via
/// `CARGO_MANIFEST_DIR` so test binaries do not depend on the working
/// directory). The host may prepend an explicit override root.
#[derive(Debug, Clone)]
pub struct PackStore {
    roots: Vec<PathBuf>,
}

impl PackStore {
    /// Default resolution: explicit override first, then the repo data tree.
    pub fn new(override_root: Option<&Path>) -> Self {
        let mut roots = Vec::new();
        if let Some(root) = override_root {
            roots.push(root.to_path_buf());
        }
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            // Workspace member: manifest dir is <repo>/<crate>, the data
            // tree lives one level up.
            roots.push(Path::new(&manifest_dir).join("../data/magician_v2/decision_packs"));
        }
        // Beside the executable (packaged layout).
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                roots.push(exe_dir.join("data/magician_v2/decision_packs"));
            }
        }
        // Working-directory fallback for dev runs from the repo root.
        roots.push(PathBuf::from("data/magician_v2/decision_packs"));
        Self { roots }
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn load(&self, pack_id: &str, version: &str) -> Result<Pack, DecisionError> {
        for root in &self.roots {
            let path = root.join(pack_id).join(format!("{version}.json"));
            if !path.is_file() {
                continue;
            }
            let raw = std::fs::read_to_string(&path).map_err(|err| {
                DecisionError::Transport(format!("pack read {path:?} failed: {err}"))
            })?;
            let pack: Pack = serde_json::from_str(&raw).map_err(|err| {
                DecisionError::InvalidResponse(format!("pack {path:?} parse failed: {err}"))
            })?;
            if pack.id != pack_id {
                return Err(DecisionError::InvalidResponse(format!(
                    "pack file {path:?} declares id '{}' but was loaded as '{pack_id}'",
                    pack.id
                )));
            }
            if pack.version != version {
                return Err(DecisionError::InvalidResponse(format!(
                    "pack file {path:?} declares version '{}' but was loaded as '{version}'",
                    pack.version
                )));
            }
            pack.validate()?;
            return Ok(pack);
        }
        if let Some(raw) = embedded(pack_id, version) {
            let pack: Pack = serde_json::from_str(raw)
                .map_err(|error| DecisionError::InvalidResponse(error.to_string()))?;
            pack.validate()?;
            return Ok(pack);
        }
        Err(DecisionError::PackMissing(format!("{pack_id}@{version}")))
    }
}

impl Pack {
    /// Structural invariants that would otherwise surface as confusing
    /// adapter errors mid-flight.
    pub fn validate(&self) -> Result<(), DecisionError> {
        if self.questions.is_empty() {
            return Err(DecisionError::InvalidResponse(format!(
                "pack {}@{} has no questions",
                self.id, self.version
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for question in &self.questions {
            if !seen.insert(question.id().clone()) {
                return Err(DecisionError::InvalidResponse(format!(
                    "pack {}@{} has duplicate question id '{}'",
                    self.id,
                    self.version,
                    question.id().as_str()
                )));
            }
            if let crate::primitives::Question::Score(score) = question {
                if score.levels.len() < 2 {
                    return Err(DecisionError::InvalidResponse(format!(
                        "score question '{}' needs at least two rubric levels",
                        score.id.as_str()
                    )));
                }
            }
        }
        Ok(())
    }
}

fn embedded(id: &str, version: &str) -> Option<&'static str> {
    match (id, version) {
        ("memory_lifecycle_relation", "1.1.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_lifecycle_relation/1.1.0.json"
        )),
        ("memory_connection_gate", "1.1.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_connection_gate/1.1.0.json"
        )),
        ("evidence_promote", "1.1.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/evidence_promote/1.1.0.json"
        )),
        ("memory_applicability", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_applicability/1.0.0.json"
        )),
        ("memory_utility_review", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_utility_review/1.0.0.json"
        )),
        ("memory_lifecycle_relation", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_lifecycle_relation/1.0.0.json"
        )),
        ("procedure_feedback", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/procedure_feedback/1.0.0.json"
        )),
        ("memory_connection_gate", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_connection_gate/1.0.0.json"
        )),
        ("evidence_promote", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/evidence_promote/1.0.0.json"
        )),
        ("memory_episode_quality", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_episode_quality/1.0.0.json"
        )),
        ("memory_conflict_review", "1.0.0") => Some(include_str!(
            "../../data/magician_v2/decision_packs/memory_conflict_review/1.0.0.json"
        )),
        _ => None,
    }
}
