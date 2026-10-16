//! Provider-free release-qualification manifest for the App Platform.
//!
//! Feature tests stay beside their canonical owners. This module prevents the
//! final P7.7-P7.9 gate from becoming a prose-only claim: every permanent
//! C01-C16 canary must be assigned to the applicable state, adversarial, and
//! reliability campaigns, and the repeated gate must fail if any trial fails.

use std::collections::BTreeSet;

pub const APP_PLATFORM_RELEASE_PASS_K: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppQualificationCampaign {
    StateInvariant,
    Adversarial,
    Reliability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppReleaseCanary {
    C01,
    C02,
    C03,
    C04,
    C05,
    C06,
    C07,
    C08,
    C09,
    C10,
    C11,
    C12,
    C13,
    C14,
    C15,
    C16,
}

impl AppReleaseCanary {
    const ALL: [Self; 16] = [
        Self::C01,
        Self::C02,
        Self::C03,
        Self::C04,
        Self::C05,
        Self::C06,
        Self::C07,
        Self::C08,
        Self::C09,
        Self::C10,
        Self::C11,
        Self::C12,
        Self::C13,
        Self::C14,
        Self::C15,
        Self::C16,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCanaryQualification {
    pub canary: AppReleaseCanary,
    pub campaigns: BTreeSet<AppQualificationCampaign>,
    pub provider_free_owner: &'static str,
    pub live_owner_required: bool,
}

fn campaigns(values: &[AppQualificationCampaign]) -> BTreeSet<AppQualificationCampaign> {
    values.iter().copied().collect()
}

pub fn app_release_qualification_manifest() -> Vec<AppCanaryQualification> {
    use AppQualificationCampaign::{Adversarial, Reliability, StateInvariant};
    use AppReleaseCanary::*;

    vec![
        AppCanaryQualification {
            canary: C01,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "registry/lifecycle/entity/update/portability/purge",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C02,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "compiled_dispatch/bound_http/bound_path/resource_authority",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C03,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "os_jail/tool_catalog/tool_dispatch",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C04,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "interactive/browser_capability/workflows",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C05,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "interactive/macos_pairing/macos_host/workflows",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C06,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "interactive/android_owner/android_device/workflows",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C07,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "procedure_publication/skill_dependencies/os_jail",
            live_owner_required: false,
        },
        AppCanaryQualification {
            canary: C08,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "agent_capability/artifact_v2/app_agent_tool",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C09,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "entity_store/entity_mutation/entity_changes",
            live_owner_required: false,
        },
        AppCanaryQualification {
            canary: C10,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "composition/composition_service/workflows",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C11,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "recipe_ir/recipe_lifecycle/authoring/public SDK/UI",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C12,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "contribution/memory/retrieval projection/outboxes",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C13,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "threat_model and hostile owner regressions",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C14,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "app contract codegen/SDK/independent consumer",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C15,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "models/workflows/apps_api/UI run control",
            live_owner_required: true,
        },
        AppCanaryQualification {
            canary: C16,
            campaigns: campaigns(&[StateInvariant, Adversarial, Reliability]),
            provider_free_owner: "registry migrations/observability/retention/purge",
            live_owner_required: true,
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppQualificationTrial {
    pub trial: usize,
    pub state_invariant_passed: bool,
    pub adversarial_passed: bool,
    pub reliability_passed: bool,
}

pub fn assess_repeated_qualification(trials: &[AppQualificationTrial]) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    if trials.len() != APP_PLATFORM_RELEASE_PASS_K {
        failures.push(format!(
            "expected exactly {APP_PLATFORM_RELEASE_PASS_K} qualification trials, found {}",
            trials.len()
        ));
    }
    let mut seen = BTreeSet::new();
    for trial in trials {
        if trial.trial == 0
            || trial.trial > APP_PLATFORM_RELEASE_PASS_K
            || !seen.insert(trial.trial)
        {
            failures.push(format!(
                "invalid or duplicate qualification trial {}",
                trial.trial
            ));
        }
        if !trial.state_invariant_passed {
            failures.push(format!("trial {} failed P7.7", trial.trial));
        }
        if !trial.adversarial_passed {
            failures.push(format!("trial {} failed P7.8", trial.trial));
        }
        if !trial.reliability_passed {
            failures.push(format!("trial {} failed P7.9", trial.trial));
        }
    }
    failures.sort();
    failures.dedup();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn p7_manifest_covers_every_permanent_canary_and_campaign() {
        let manifest = app_release_qualification_manifest();
        assert_eq!(manifest.len(), AppReleaseCanary::ALL.len());
        let by_canary: BTreeMap<_, _> =
            manifest.iter().map(|entry| (entry.canary, entry)).collect();
        assert_eq!(by_canary.len(), AppReleaseCanary::ALL.len());
        for canary in AppReleaseCanary::ALL {
            let entry = by_canary.get(&canary).expect("missing permanent canary");
            assert!(!entry.provider_free_owner.is_empty());
            assert!(entry
                .campaigns
                .contains(&AppQualificationCampaign::Adversarial));
            assert!(entry
                .campaigns
                .contains(&AppQualificationCampaign::Reliability));
            assert!(entry
                .campaigns
                .contains(&AppQualificationCampaign::StateInvariant));
        }
    }

    #[test]
    fn physical_and_assembled_canaries_cannot_be_misreported_as_provider_free() {
        let manifest = app_release_qualification_manifest();
        for canary in [
            AppReleaseCanary::C01,
            AppReleaseCanary::C02,
            AppReleaseCanary::C03,
            AppReleaseCanary::C04,
            AppReleaseCanary::C05,
            AppReleaseCanary::C06,
            AppReleaseCanary::C08,
            AppReleaseCanary::C10,
            AppReleaseCanary::C11,
            AppReleaseCanary::C12,
            AppReleaseCanary::C13,
            AppReleaseCanary::C14,
            AppReleaseCanary::C15,
            AppReleaseCanary::C16,
        ] {
            assert!(manifest
                .iter()
                .any(|entry| entry.canary == canary && entry.live_owner_required));
        }
    }

    #[test]
    fn repeated_gate_requires_three_complete_unique_passes() {
        let passing = (1..=APP_PLATFORM_RELEASE_PASS_K)
            .map(|trial| AppQualificationTrial {
                trial,
                state_invariant_passed: true,
                adversarial_passed: true,
                reliability_passed: true,
            })
            .collect::<Vec<_>>();
        assert_eq!(assess_repeated_qualification(&passing), Ok(()));

        let mut failing = passing;
        failing[1].adversarial_passed = false;
        let failures = assess_repeated_qualification(&failing).unwrap_err();
        assert_eq!(failures, vec!["trial 2 failed P7.8"]);
    }
}
