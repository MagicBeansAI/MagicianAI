//! Resolved, content-addressed criterion specifications. Contracts name these
//! hashes; runtime admission must resolve every one before any paid work.
use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    contract::{CriterionKind, FrozenContract},
    review::Outcome,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Specification {
    Check {
        assertions: Vec<Assertion>,
    },
    Verification {
        #[serde(default)]
        policy_hash: String,
    },
    Rubric {
        instructions: String,
        anchors: Vec<ScoreAnchor>,
    },
    Grounding {
        instructions: String,
    },
    Perceptual {
        instructions: String,
        #[serde(default)]
        sampler_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreAnchor {
    pub score: f64,
    pub description: String,
}

/// Declared assertions over sealed bytes, rather than use-case heuristics or
/// executable owner/actor-supplied commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    FileExists {
        file: String,
    },
    Utf8 {
        file: String,
    },
    ByteLength {
        file: String,
        minimum: u64,
        maximum: u64,
    },
    Json {
        file: String,
    },
    MediaNumber {
        file: String,
        fact: MediaFact,
        minimum: u64,
        maximum: u64,
    },
}

/// Runtime-probed metadata only. Stream presence is not evidence that speech
/// or music was heard, and duration does not certify unseen video intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFact {
    DurationMicros,
    VideoWidth,
    VideoHeight,
    VideoStreamCount,
    AudioStreamCount,
}

pub type MediaFacts = BTreeMap<String, BTreeMap<MediaFact, u64>>;

#[derive(Debug, Clone, Serialize)]
pub struct CheckObservation {
    pub file: String,
    pub outcome: Outcome,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub outcome: Outcome,
    pub observations: Vec<CheckObservation>,
}

fn file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && std::path::Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

impl Specification {
    pub fn kind(&self) -> CriterionKind {
        match self {
            Self::Check { .. } | Self::Verification { .. } => CriterionKind::Check,
            Self::Rubric { .. } => CriterionKind::Rubric,
            Self::Grounding { .. } => CriterionKind::Grounding,
            Self::Perceptual { .. } => CriterionKind::Perceptual,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Verification { policy_hash } => ensure!(
                crate::contract::valid_hash(policy_hash),
                "unresolved verification policy"
            ),
            Self::Check { assertions } => {
                ensure!(!assertions.is_empty(), "empty check specification");
                for a in assertions {
                    let file = match a {
                        Assertion::FileExists { file }
                        | Assertion::Utf8 { file }
                        | Assertion::Json { file } => file,
                        Assertion::ByteLength {
                            file,
                            minimum,
                            maximum,
                        }
                        | Assertion::MediaNumber {
                            file,
                            minimum,
                            maximum,
                            ..
                        } => {
                            ensure!(minimum <= maximum, "invalid assertion bounds");
                            file
                        },
                    };
                    ensure!(file_name(file), "invalid assertion artifact path");
                }
            },
            Self::Rubric {
                instructions,
                anchors,
            } => {
                ensure!(
                    !instructions.trim().is_empty() && anchors.len() >= 2,
                    "rubric needs instructions and score anchors"
                );
                let mut previous = None;
                for anchor in anchors {
                    ensure!(
                        crate::contract::unit(anchor.score)
                            && !anchor.description.trim().is_empty(),
                        "invalid rubric anchor"
                    );
                    ensure!(
                        previous.is_none_or(|p| anchor.score > p),
                        "anchors must have distinct increasing scores"
                    );
                    previous = Some(anchor.score);
                }
                ensure!(
                    anchors.first().is_some_and(|a| a.score == 0.0)
                        && anchors.last().is_some_and(|a| a.score == 1.0),
                    "anchors must cover the full scale"
                );
            },
            Self::Grounding { instructions } => ensure!(
                !instructions.trim().is_empty(),
                "empty grounding specification"
            ),
            Self::Perceptual {
                instructions,
                sampler_hash,
            } => ensure!(
                !instructions.trim().is_empty() && crate::contract::valid_hash(sampler_hash),
                "unresolved perceptual specification"
            ),
        }
        Ok(())
    }

    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    pub fn check(&self, files: &BTreeMap<String, Vec<u8>>) -> Result<Outcome> {
        self.check_with_media_facts(files, &MediaFacts::new())
    }

    pub fn check_with_media_facts(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        media: &MediaFacts,
    ) -> Result<Outcome> {
        Ok(self.check_report_with_media_facts(files, media)?.outcome)
    }

    pub fn check_report_with_media_facts(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        media: &MediaFacts,
    ) -> Result<CheckReport> {
        self.validate()?;
        let Self::Check { assertions } = self else {
            anyhow::bail!("not a deterministic check");
        };
        let mut unknown = false;
        let mut failed = false;
        let mut observations = Vec::new();
        for assertion in assertions {
            let file = match assertion {
                Assertion::FileExists { file }
                | Assertion::Utf8 { file }
                | Assertion::Json { file }
                | Assertion::ByteLength { file, .. }
                | Assertion::MediaNumber { file, .. } => file,
            };
            let (outcome, detail) = if let Some(bytes) = files.get(file) {
                match assertion {
                    Assertion::FileExists { .. } => {
                        (Outcome::Pass, "The declared file exists".into())
                    },
                    Assertion::Utf8 { .. } => match std::str::from_utf8(bytes) {
                        Ok(_) => (Outcome::Pass, "The file is valid UTF-8".into()),
                        Err(error) => (Outcome::Fail, format!("Invalid UTF-8: {error}")),
                    },
                    Assertion::ByteLength {
                        minimum, maximum, ..
                    } => (
                        if (*minimum..=*maximum).contains(&(bytes.len() as u64)) {
                            Outcome::Pass
                        } else {
                            Outcome::Fail
                        },
                        format!(
                            "Observed {} bytes; required inclusive bounds {minimum}..{maximum}",
                            bytes.len()
                        ),
                    ),
                    Assertion::Json { .. } => {
                        match serde_json::from_slice::<serde_json::Value>(bytes) {
                            Ok(_) => (Outcome::Pass, "The file is valid JSON".into()),
                            Err(error) => (Outcome::Fail, format!("Invalid JSON: {error}")),
                        }
                    },
                    Assertion::MediaNumber {
                        file,
                        fact,
                        minimum,
                        maximum,
                    } => {
                        let fact_name = serde_json::to_value(fact)?
                            .as_str()
                            .context("media fact name missing")?
                            .to_owned();
                        if let Some(value) = media.get(file).and_then(|values| values.get(fact)) {
                            (if (*minimum..=*maximum).contains(value) { Outcome::Pass } else { Outcome::Fail },
                            format!("Observed {fact_name}={value}; required inclusive bounds {minimum}..{maximum}"))
                        } else {
                            (Outcome::Unknown, format!("Trusted {fact_name} metadata is unavailable; required inclusive bounds {minimum}..{maximum}"))
                        }
                    },
                }
            } else {
                (
                    Outcome::Fail,
                    "The declared artifact file is missing".into(),
                )
            };
            unknown |= outcome == Outcome::Unknown;
            failed |= outcome == Outcome::Fail;
            observations.push(CheckObservation {
                file: file.clone(),
                outcome,
                detail,
            });
        }
        Ok(CheckReport {
            outcome: if failed {
                Outcome::Fail
            } else if unknown {
                Outcome::Unknown
            } else {
                Outcome::Pass
            },
            observations,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Specifications(pub BTreeMap<String, Specification>);

impl Specifications {
    pub fn validate_contract(&self, contract: &FrozenContract) -> Result<()> {
        for criterion in &contract.definition().criteria {
            let spec = self
                .0
                .get(&criterion.specification_hash)
                .context("criterion specification unavailable")?;
            ensure!(
                spec.hash()? == criterion.specification_hash && spec.kind() == criterion.kind,
                "criterion specification identity mismatch"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_use_exact_sealed_files_and_specification_bytes() {
        let spec = Specification::Check {
            assertions: vec![
                Assertion::Utf8 {
                    file: "draft.md".into(),
                },
                Assertion::ByteLength {
                    file: "draft.md".into(),
                    minimum: 3,
                    maximum: 8,
                },
            ],
        };
        let mut files = BTreeMap::from([("draft.md".into(), b"draft".to_vec())]);
        assert_eq!(spec.check(&files).unwrap(), Outcome::Pass);
        files.insert("draft.md".into(), vec![0xff; 5]);
        assert_eq!(spec.check(&files).unwrap(), Outcome::Fail);
        files.clear();
        assert_eq!(spec.check(&files).unwrap(), Outcome::Fail);
        assert!(Specification::Check {
            assertions: vec![Assertion::FileExists {
                file: "../ledger".into()
            }]
        }
        .hash()
        .is_err());
        let c = crate::tests::contract();
        assert!(Specifications::default().validate_contract(&c).is_err());
        let spoofed = Specifications(BTreeMap::from([(
            c.definition().criteria[0].specification_hash.clone(),
            spec,
        )]));
        assert!(spoofed.validate_contract(&c).is_err());
    }
}
