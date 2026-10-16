//! Decision Gate 2 — accepted recovery objectives.
//!
//! These numbers are the deployment SLO. A drill fails if measured RPO/RTO
//! exceeds the matching class. Recording a measurement without a target does
//! not close the gate.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryClass {
    CanonicalState,
    RequiredObjects,
    ObservabilityDatasets,
    SecretsConfiguration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoverySlo {
    pub rpo_secs: u64,
    pub rto_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate2Targets {
    pub canonical_state: RecoverySlo,
    pub required_objects: RecoverySlo,
    pub observability_datasets: RecoverySlo,
    pub secrets_configuration: RecoverySlo,
}

/// Storage owner that accepted these Track A local-embedded targets.
pub const GATE2_OWNER: &str = "Magician runtime and storage owners";

/// Accepted 2026-09-01 for the local_embedded profile.
pub const ACCEPTED_AT: &str = "2026-09-01";

pub fn accepted_targets() -> Gate2Targets {
    Gate2Targets {
        canonical_state: RecoverySlo {
            rpo_secs: 0,
            rto_secs: 900,
        },
        required_objects: RecoverySlo {
            rpo_secs: 0,
            rto_secs: 900,
        },
        observability_datasets: RecoverySlo {
            rpo_secs: 86_400,
            rto_secs: 14_400,
        },
        secrets_configuration: RecoverySlo {
            rpo_secs: 0,
            rto_secs: 1_800,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrillVerdict {
    pub class: RecoveryClass,
    pub measured_rpo_secs: u64,
    pub measured_rto_secs: u64,
    pub pass: bool,
}

impl DrillVerdict {
    pub fn compare(class: RecoveryClass, measured_rpo_secs: u64, measured_rto_secs: u64) -> Self {
        let slo = match class {
            RecoveryClass::CanonicalState => accepted_targets().canonical_state,
            RecoveryClass::RequiredObjects => accepted_targets().required_objects,
            RecoveryClass::ObservabilityDatasets => accepted_targets().observability_datasets,
            RecoveryClass::SecretsConfiguration => accepted_targets().secrets_configuration,
        };
        Self {
            class,
            measured_rpo_secs,
            measured_rto_secs,
            pass: measured_rpo_secs <= slo.rpo_secs && measured_rto_secs <= slo.rto_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate2_fails_when_measured_rto_exceeds_accepted_target() {
        let fail = DrillVerdict::compare(RecoveryClass::CanonicalState, 0, 901);
        assert!(!fail.pass);
        let pass = DrillVerdict::compare(RecoveryClass::CanonicalState, 0, 900);
        assert!(pass.pass);
        let rpo_fail = DrillVerdict::compare(RecoveryClass::ObservabilityDatasets, 86_401, 0);
        assert!(!rpo_fail.pass);
    }
}
