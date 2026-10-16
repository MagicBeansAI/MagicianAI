//! Deterministic, provider-free Phase-0 qualification fixtures.
//!
//! Large datasets are generated as an iterator so the harness can exercise the
//! intended scale without first allocating a second complete in-memory copy.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;

use magician::magician_v2::apps::models::{
    AppContractLimits, AppFieldPath, AppMutationAtomicity, AppMutationCommand,
    AppMutationOperation, AppName, AppProtocolVersion, AppRecordId, AppRecordProjection,
    AppReference, AppRevision,
};

pub const APP_BENCHMARK_ACTIVE_INSTALLATION_RECORDS: u32 = 10_000;
pub const APP_BENCHMARK_SCOPE_RECORDS: u32 = 100_000;
pub const APP_BENCHMARK_QUERY_ROWS: u32 = 100;
pub const APP_BENCHMARK_MUTATION_OPERATIONS: u32 = 100;
pub const APP_BENCHMARK_OUTBOX_ROWS: u32 = 1_000;
pub const APP_PHASE2E_QUALIFICATION_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppQualificationLatencyGate {
    pub operation: &'static str,
    pub percentile: &'static str,
    pub maximum_milliseconds: u32,
}

/// Checked-in provider-free qualification metadata for the Phase-2E data
/// plane. The runner consumes these targets; it must not invent looser values
/// from environment configuration after observing a result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPhase2EQualificationMetadata {
    pub schema_version: u16,
    pub suite_id: &'static str,
    pub provider_free: bool,
    pub fixture: AppPhase0BenchmarkProfile,
    pub enabled_installations: u16,
    pub entity_schemas_per_installation: u16,
    pub contention_seconds: u16,
    pub foreground_writer_streams: u16,
    pub background_writer_streams: u16,
    pub concurrent_indexed_readers: u16,
    pub saturated_admission_timeout_ms: u32,
    pub warm_latency_gates: Vec<AppQualificationLatencyGate>,
    pub required_crash_boundaries: Vec<&'static str>,
    pub forbidden_foreground_dependencies: Vec<&'static str>,
    pub required_privacy_assertions: Vec<&'static str>,
}

pub fn app_phase2e_qualification_metadata() -> AppPhase2EQualificationMetadata {
    AppPhase2EQualificationMetadata {
        schema_version: APP_PHASE2E_QUALIFICATION_SCHEMA_VERSION,
        suite_id: "app-entity-data-plane-phase2e-v1",
        provider_free: true,
        fixture: AppPhase0BenchmarkProfile::default(),
        enabled_installations: 10,
        entity_schemas_per_installation: 10,
        contention_seconds: 60,
        foreground_writer_streams: 4,
        background_writer_streams: 4,
        concurrent_indexed_readers: 8,
        saturated_admission_timeout_ms: 250,
        warm_latency_gates: vec![
            AppQualificationLatencyGate {
                operation: "installation_route_binding_lookup",
                percentile: "p95",
                maximum_milliseconds: 25,
            },
            AppQualificationLatencyGate {
                operation: "point_entity_read",
                percentile: "p95",
                maximum_milliseconds: 25,
            },
            AppQualificationLatencyGate {
                operation: "indexed_query_100_rows",
                percentile: "p95",
                maximum_milliseconds: 75,
            },
            AppQualificationLatencyGate {
                operation: "mutation_transaction_100_operations",
                percentile: "p95",
                maximum_milliseconds: 100,
            },
            AppQualificationLatencyGate {
                operation: "default_view_backend_hydration",
                percentile: "p95",
                maximum_milliseconds: 150,
            },
            AppQualificationLatencyGate {
                operation: "entity_commit_to_outbox_availability",
                percentile: "p95",
                maximum_milliseconds: 25,
            },
            AppQualificationLatencyGate {
                operation: "disclosure_label_resolution_100_rows",
                percentile: "p95",
                maximum_milliseconds: 25,
            },
            AppQualificationLatencyGate {
                operation: "brokered_cross_app_projection_100_rows",
                percentile: "p95",
                maximum_milliseconds: 100,
            },
            AppQualificationLatencyGate {
                operation: "continuation_eligibility_check",
                percentile: "p95",
                maximum_milliseconds: 10,
            },
            AppQualificationLatencyGate {
                operation: "contended_foreground_one_record_mutation",
                percentile: "p95",
                maximum_milliseconds: 150,
            },
            AppQualificationLatencyGate {
                operation: "contended_foreground_indexed_query_100_rows",
                percentile: "p95",
                maximum_milliseconds: 100,
            },
            AppQualificationLatencyGate {
                operation: "contended_foreground_writer_admission_wait",
                percentile: "p99",
                maximum_milliseconds: 250,
            },
        ],
        required_crash_boundaries: vec![
            "before_entity_transaction",
            "after_entity_transaction_before_response",
            "before_cursor_publication",
            "after_cursor_publication_before_response",
            "before_outbox_claim",
            "after_outbox_claim_before_ack",
        ],
        forbidden_foreground_dependencies: vec!["llm", "ollama", "lancedb", "embedding"],
        required_privacy_assertions: vec![
            "mixed_policy_fields_do_not_escape_approved_projection",
            "record_narrowing_is_checked_before_cursor_publication",
            "cross_scope_installations_are_concealed",
            "package_and_data_exports_remain_disjoint",
            "imported_record_identity_is_destination_local",
            "no_payload_or_raw_record_id_enters_retention_or_forget_audit",
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPhase2EQualificationObservations {
    pub suite_id: String,
    pub fixture: AppPhase0BenchmarkProfile,
    pub enabled_installations: u16,
    pub entity_schemas_per_installation: u16,
    pub contention_seconds: u16,
    pub foreground_writer_streams: u16,
    pub background_writer_streams: u16,
    pub concurrent_indexed_readers: u16,
    pub latency_samples_ms: BTreeMap<String, Vec<u32>>,
    pub passed_crash_boundaries: BTreeSet<String>,
    pub observed_foreground_dependencies: BTreeSet<String>,
    pub passed_privacy_assertions: BTreeSet<String>,
    pub escaped_sqlite_busy: u64,
    pub lost_receipts: u64,
    pub background_starvation_events: u64,
    pub foreground_reserve_effective: bool,
    pub saturated_admission_max_ms: u32,
    pub saturated_admission_typed_overload: bool,
}

pub trait AppPhase2EQualificationDriver {
    type Error;

    fn execute(
        &mut self,
        metadata: &AppPhase2EQualificationMetadata,
    ) -> Result<AppPhase2EQualificationObservations, Self::Error>;
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPhase2EQualificationReport {
    pub suite_id: String,
    pub passed: bool,
    pub evaluated_latency_gates: usize,
    pub failures: Vec<String>,
}

/// Execute the checked-in provider-free driver and evaluate its observations
/// against the immutable metadata. The driver owns workload mechanics; it
/// cannot relax gates after observing results because the runner compares the
/// returned fixture, topology and every required gate to this module's values.
pub fn run_app_phase2e_qualification<D: AppPhase2EQualificationDriver>(
    driver: &mut D,
) -> Result<AppPhase2EQualificationReport, D::Error> {
    let metadata = app_phase2e_qualification_metadata();
    let observations = driver.execute(&metadata)?;
    let mut failures = Vec::new();
    if observations.suite_id != metadata.suite_id
        || observations.fixture != metadata.fixture
        || observations.enabled_installations != metadata.enabled_installations
        || observations.entity_schemas_per_installation != metadata.entity_schemas_per_installation
        || observations.contention_seconds != metadata.contention_seconds
        || observations.foreground_writer_streams != metadata.foreground_writer_streams
        || observations.background_writer_streams != metadata.background_writer_streams
        || observations.concurrent_indexed_readers != metadata.concurrent_indexed_readers
    {
        failures.push("qualification fixture or contention topology drifted".to_owned());
    }
    for gate in &metadata.warm_latency_gates {
        let measured = observations
            .latency_samples_ms
            .get(gate.operation)
            .and_then(|samples| percentile_ms(samples, gate.percentile));
        match measured {
            Some(measured) if measured <= gate.maximum_milliseconds => {},
            Some(measured) => failures.push(format!(
                "{} {} was {}ms, above {}ms",
                gate.operation, gate.percentile, measured, gate.maximum_milliseconds
            )),
            None => failures.push(format!(
                "{} has no valid {} samples",
                gate.operation, gate.percentile
            )),
        }
    }
    for boundary in &metadata.required_crash_boundaries {
        if !observations.passed_crash_boundaries.contains(*boundary) {
            failures.push(format!(
                "crash boundary `{boundary}` was not exercised successfully"
            ));
        }
    }
    for assertion in &metadata.required_privacy_assertions {
        if !observations.passed_privacy_assertions.contains(*assertion) {
            failures.push(format!("privacy assertion `{assertion}` did not pass"));
        }
    }
    for forbidden in &metadata.forbidden_foreground_dependencies {
        if observations
            .observed_foreground_dependencies
            .contains(*forbidden)
        {
            failures.push(format!(
                "foreground path used forbidden dependency `{forbidden}`"
            ));
        }
    }
    if observations.escaped_sqlite_busy != 0
        || observations.lost_receipts != 0
        || observations.background_starvation_events != 0
        || !observations.foreground_reserve_effective
    {
        failures.push("contention correctness or foreground fairness failed".to_owned());
    }
    if !observations.saturated_admission_typed_overload
        || observations.saturated_admission_max_ms > metadata.saturated_admission_timeout_ms
    {
        failures.push("saturated admission was not a typed bounded overload".to_owned());
    }
    failures.sort();
    failures.dedup();
    Ok(AppPhase2EQualificationReport {
        suite_id: metadata.suite_id.to_owned(),
        passed: failures.is_empty(),
        evaluated_latency_gates: metadata.warm_latency_gates.len(),
        failures,
    })
}

fn percentile_ms(samples: &[u32], percentile: &str) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    let numerator = match percentile {
        "p95" => 95usize,
        "p99" => 99usize,
        _ => return None,
    };
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = sorted.len().checked_mul(numerator)?.saturating_add(99) / 100;
    sorted.get(rank.saturating_sub(1)).copied()
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPhase0BenchmarkProfile {
    pub active_installation_records: u32,
    pub scope_records: u32,
    pub query_rows: u32,
    pub mutation_operations: u32,
    pub retained_outbox_rows: u32,
}

impl Default for AppPhase0BenchmarkProfile {
    fn default() -> Self {
        Self {
            active_installation_records: APP_BENCHMARK_ACTIVE_INSTALLATION_RECORDS,
            scope_records: APP_BENCHMARK_SCOPE_RECORDS,
            query_rows: APP_BENCHMARK_QUERY_ROWS,
            mutation_operations: APP_BENCHMARK_MUTATION_OPERATIONS,
            retained_outbox_rows: APP_BENCHMARK_OUTBOX_ROWS,
        }
    }
}

impl AppPhase0BenchmarkProfile {
    pub fn validate(&self) -> Result<(), AppBenchmarkFixtureError> {
        let limits = AppContractLimits::default();
        if self.active_installation_records == 0
            || self.scope_records < self.active_installation_records
            || self.active_installation_records > APP_BENCHMARK_ACTIVE_INSTALLATION_RECORDS
            || self.scope_records > APP_BENCHMARK_SCOPE_RECORDS
        {
            return Err(AppBenchmarkFixtureError::InvalidDatasetScale);
        }
        if self.query_rows == 0
            || usize::try_from(self.query_rows).unwrap_or(usize::MAX) > limits.max_page_rows()
        {
            return Err(AppBenchmarkFixtureError::QueryRowLimit {
                limit: limits.max_page_rows(),
            });
        }
        if self.mutation_operations == 0
            || usize::try_from(self.mutation_operations).unwrap_or(usize::MAX)
                > limits.max_collection_items()
        {
            return Err(AppBenchmarkFixtureError::MutationLimit {
                limit: limits.max_collection_items(),
            });
        }
        if self.retained_outbox_rows == 0 || self.retained_outbox_rows > APP_BENCHMARK_OUTBOX_ROWS {
            return Err(AppBenchmarkFixtureError::InvalidOutboxScale);
        }
        Ok(())
    }

    pub fn scope_records(&self) -> Result<AppRecordFixtureIter, AppBenchmarkFixtureError> {
        self.validate()?;
        AppRecordFixtureIter::new(self.scope_records)
    }

    pub fn active_installation_records(
        &self,
    ) -> Result<AppRecordFixtureIter, AppBenchmarkFixtureError> {
        self.validate()?;
        AppRecordFixtureIter::new(self.active_installation_records)
    }

    pub fn query_page(&self) -> Result<Vec<AppRecordProjection>, AppBenchmarkFixtureError> {
        self.validate()?;
        AppRecordFixtureIter::new(self.query_rows)?.collect()
    }

    pub fn mutation_batch(&self) -> Result<AppMutationCommand, AppBenchmarkFixtureError> {
        self.validate()?;
        let count = usize::try_from(self.mutation_operations)
            .map_err(|_| AppBenchmarkFixtureError::InvalidDatasetScale)?;
        let entity = AppName::parse("benchmark_record")?;
        let operations = (0..count)
            .map(|index| {
                Ok(AppMutationOperation::Create {
                    entity: entity.clone(),
                    temporary_id: AppName::parse(format!("new_{index}"))?,
                    record_id: None,
                    payload: record_payload(index),
                })
            })
            .collect::<Result<Vec<_>, AppBenchmarkFixtureError>>()?;
        Ok(AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation-key:benchmark-batch")?,
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1)?,
            operations,
            expected_record_revisions: Vec::new(),
        })
    }

    pub fn retained_outbox(&self) -> Result<AppOutboxFixtureIter, AppBenchmarkFixtureError> {
        self.validate()?;
        Ok(AppOutboxFixtureIter {
            next: 0,
            end: self.retained_outbox_rows,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppOutboxFixture {
    pub sequence: u32,
    pub idempotency_key: AppReference,
    pub payload_digest: magician::magician_v2::apps::models::AppDigest,
}

pub struct AppOutboxFixtureIter {
    next: u32,
    end: u32,
}

impl Iterator for AppOutboxFixtureIter {
    type Item = Result<AppOutboxFixture, AppBenchmarkFixtureError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let sequence = self.next;
        self.next = self.next.saturating_add(1);
        Some(
            AppReference::parse(format!("outbox-key:benchmark:{sequence}"))
                .map(|idempotency_key| AppOutboxFixture {
                    sequence,
                    idempotency_key,
                    payload_digest: magician::magician_v2::apps::models::AppDigest::blake3(
                        sequence.to_string().as_bytes(),
                    ),
                })
                .map_err(AppBenchmarkFixtureError::Contract),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.end.saturating_sub(self.next)).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AppOutboxFixtureIter {}

pub struct AppRecordFixtureIter {
    next: u32,
    end: u32,
    entity: AppName,
    title: AppFieldPath,
    ordinal: AppFieldPath,
    completed: AppFieldPath,
}

impl AppRecordFixtureIter {
    fn new(end: u32) -> Result<Self, AppBenchmarkFixtureError> {
        if end == 0 || end > APP_BENCHMARK_SCOPE_RECORDS {
            return Err(AppBenchmarkFixtureError::RecordLimit {
                limit: APP_BENCHMARK_SCOPE_RECORDS,
            });
        }
        Ok(Self {
            next: 0,
            end,
            entity: AppName::parse("benchmark_record")?,
            title: AppFieldPath::parse("title")?,
            ordinal: AppFieldPath::parse("ordinal")?,
            completed: AppFieldPath::parse("completed")?,
        })
    }
}

impl Iterator for AppRecordFixtureIter {
    type Item = Result<AppRecordProjection, AppBenchmarkFixtureError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next = self.next.saturating_add(1);
        let mut fields = BTreeMap::new();
        fields.insert(self.title.clone(), json!(format!("Record {index}")));
        fields.insert(self.ordinal.clone(), json!(index));
        fields.insert(self.completed.clone(), json!(index % 3 == 0));
        Some(
            AppRecordId::parse(format!("record_{index}"))
                .and_then(|record_id| {
                    AppRevision::new(u64::from(index).saturating_add(1)).map(|record_revision| {
                        AppRecordProjection {
                            entity: self.entity.clone(),
                            record_id,
                            record_revision,
                            fields,
                        }
                    })
                })
                .map_err(AppBenchmarkFixtureError::Contract),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.end.saturating_sub(self.next)).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AppRecordFixtureIter {}

fn record_payload(index: usize) -> Value {
    json!({
        "title": format!("Record {index}"),
        "ordinal": index,
        "completed": index % 3 == 0,
    })
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppBenchmarkFixtureError {
    #[error("app benchmark dataset scale is invalid")]
    InvalidDatasetScale,
    #[error("app benchmark outbox scale must be positive")]
    InvalidOutboxScale,
    #[error("app benchmark query exceeds the {limit} row contract ceiling")]
    QueryRowLimit { limit: usize },
    #[error("app benchmark mutation exceeds the {limit} operation contract ceiling")]
    MutationLimit { limit: usize },
    #[error("app benchmark record generator exceeds the {limit} record fixture ceiling")]
    RecordLimit { limit: u32 },
    #[error(transparent)]
    Contract(#[from] magician::magician_v2::apps::models::AppContractError),
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;
    use magician::magician_v2::apps::models::ValidateAppContract;

    #[test]
    fn fixed_profile_matches_the_design_qualification_scale() {
        let profile = AppPhase0BenchmarkProfile::default();
        profile.validate().unwrap();
        assert_eq!(profile.active_installation_records, 10_000);
        assert_eq!(profile.scope_records, 100_000);
        assert_eq!(profile.query_rows, 100);
        assert_eq!(profile.mutation_operations, 100);
        assert_eq!(profile.retained_outbox_rows, 1_000);
    }

    #[test]
    fn phase2e_metadata_pins_provider_free_contention_crash_and_privacy_gates() {
        let metadata = app_phase2e_qualification_metadata();
        assert_eq!(metadata.schema_version, 1);
        assert!(metadata.provider_free);
        assert_eq!(metadata.enabled_installations, 10);
        assert_eq!(metadata.foreground_writer_streams, 4);
        assert_eq!(metadata.background_writer_streams, 4);
        assert_eq!(metadata.concurrent_indexed_readers, 8);
        assert_eq!(metadata.saturated_admission_timeout_ms, 250);
        assert!(metadata
            .warm_latency_gates
            .iter()
            .any(|gate| gate.operation == "indexed_query_100_rows"
                && gate.maximum_milliseconds == 75));
        assert!(metadata.forbidden_foreground_dependencies.contains(&"llm"));
        assert!(metadata
            .required_crash_boundaries
            .contains(&"after_entity_transaction_before_response"));
    }

    struct RecordedQualificationDriver {
        exceed_indexed_query_gate: bool,
    }

    impl AppPhase2EQualificationDriver for RecordedQualificationDriver {
        type Error = Infallible;

        fn execute(
            &mut self,
            metadata: &AppPhase2EQualificationMetadata,
        ) -> Result<AppPhase2EQualificationObservations, Self::Error> {
            let latency_samples_ms = metadata
                .warm_latency_gates
                .iter()
                .map(|gate| {
                    let value = if self.exceed_indexed_query_gate
                        && gate.operation == "indexed_query_100_rows"
                    {
                        gate.maximum_milliseconds.saturating_add(1)
                    } else {
                        gate.maximum_milliseconds
                    };
                    (gate.operation.to_owned(), vec![value; 20])
                })
                .collect();
            Ok(AppPhase2EQualificationObservations {
                suite_id: metadata.suite_id.to_owned(),
                fixture: metadata.fixture,
                enabled_installations: metadata.enabled_installations,
                entity_schemas_per_installation: metadata.entity_schemas_per_installation,
                contention_seconds: metadata.contention_seconds,
                foreground_writer_streams: metadata.foreground_writer_streams,
                background_writer_streams: metadata.background_writer_streams,
                concurrent_indexed_readers: metadata.concurrent_indexed_readers,
                latency_samples_ms,
                passed_crash_boundaries: metadata
                    .required_crash_boundaries
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                observed_foreground_dependencies: BTreeSet::new(),
                passed_privacy_assertions: metadata
                    .required_privacy_assertions
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                escaped_sqlite_busy: 0,
                lost_receipts: 0,
                background_starvation_events: 0,
                foreground_reserve_effective: true,
                saturated_admission_max_ms: metadata.saturated_admission_timeout_ms,
                saturated_admission_typed_overload: true,
            })
        }
    }

    #[test]
    fn qualification_runner_consumes_fixed_gates_and_fails_closed_on_regression() {
        let passing = run_app_phase2e_qualification(&mut RecordedQualificationDriver {
            exceed_indexed_query_gate: false,
        })
        .unwrap();
        assert!(passing.passed);
        let failing = run_app_phase2e_qualification(&mut RecordedQualificationDriver {
            exceed_indexed_query_gate: true,
        })
        .unwrap();
        assert!(!failing.passed);
        assert!(failing
            .failures
            .iter()
            .any(|failure| failure.contains("indexed_query_100_rows")));
    }

    #[test]
    fn large_record_fixture_is_streamed_and_exact_sized() {
        let mut records = AppPhase0BenchmarkProfile::default()
            .scope_records()
            .unwrap();
        assert_eq!(records.len(), 100_000);
        let first = records.next().unwrap().unwrap();
        assert_eq!(first.record_id.as_str(), "record_0");
        assert_eq!(records.len(), 99_999);
        let last = records.last().unwrap().unwrap();
        assert_eq!(last.record_id.as_str(), "record_99999");
    }

    #[test]
    fn outbox_fixture_is_streamed_with_unique_replay_identity() {
        let mut outbox = AppPhase0BenchmarkProfile::default()
            .retained_outbox()
            .unwrap();
        assert_eq!(outbox.len(), 1_000);
        let first = outbox.next().unwrap().unwrap();
        let second = outbox.next().unwrap().unwrap();
        assert_ne!(first.idempotency_key, second.idempotency_key);
        assert_eq!(outbox.len(), 998);
    }

    #[test]
    fn query_and_mutation_fixtures_are_canonical_contract_values() {
        let profile = AppPhase0BenchmarkProfile::default();
        let page = profile.query_page().unwrap();
        assert_eq!(page.len(), 100);
        let mutation = profile.mutation_batch().unwrap();
        assert_eq!(mutation.operations.len(), 100);
        mutation
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }

    #[test]
    fn fixture_generator_rejects_scale_that_exceeds_contract_limits() {
        let profile = AppPhase0BenchmarkProfile {
            query_rows: u32::try_from(AppContractLimits::default().max_page_rows())
                .unwrap()
                .saturating_add(1),
            ..AppPhase0BenchmarkProfile::default()
        };
        assert!(matches!(
            profile.validate(),
            Err(AppBenchmarkFixtureError::QueryRowLimit { .. })
        ));
        assert!(AppRecordFixtureIter::new(APP_BENCHMARK_SCOPE_RECORDS + 1).is_err());
    }
}
