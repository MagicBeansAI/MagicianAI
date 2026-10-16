//! Deterministic, provider-free fixtures for the canonical app data plane.
//!
//! Rust constructs every fixture through the real wire types. Code generation
//! exports the resulting JSON unchanged to web and Swift consumers, so the
//! clients do not maintain independent examples that can quietly drift.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use super::models::{
    AppActionInvocation, AppActionResult, AppActionStatus, AppArtifactProjection,
    AppComparisonOperator, AppContractError, AppContractLimits, AppDataClassification,
    AppDataEnvelope, AppDataSource, AppDigest, AppErrorCode, AppErrorDisposition, AppErrorEnvelope,
    AppExpectedRecordRevision, AppFieldPath, AppHandlingLabels, AppInstallationId,
    AppModelProcessing, AppMutationAtomicity, AppMutationCommand, AppMutationOperation, AppName,
    AppOrderDirection, AppPredicate, AppPredicateNode, AppProtocolVersion, AppQueryOrder,
    AppQueryPage, AppQueryRequest, AppRecordId, AppRecordProjection, AppReference, AppRevision,
    AppScopeBindingRef, AppSourceRef, AppSourceRefKind, ValidateAppContract,
};
use magician::magician_v2::json_traversal::canonical_json_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppContractFixtureKind {
    QueryRequest,
    QueryPage,
    MutationCommand,
    ActionInvocation,
    ActionResult,
    ArtifactProjection,
    ErrorEnvelope,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppContractFixture {
    pub name: &'static str,
    pub kind: AppContractFixtureKind,
    pub value: Value,
}

/// Build the compatibility goldens exported to every client language.
pub fn canonical_app_contract_fixtures() -> Result<Vec<AppContractFixture>, AppContractError> {
    let limits = AppContractLimits::default();

    let query = AppQueryRequest {
        pagination: Default::default(),
        protocol_version: AppProtocolVersion::V1,
        source_installation_id: installation("install_reading_room")?,
        entity: name("saved_video")?,
        select: vec![
            field("title")?,
            field("duration_seconds")?,
            field("status")?,
        ],
        predicate: Some(AppPredicate {
            root: 2,
            nodes: vec![
                AppPredicateNode::Compare {
                    field: field("duration_seconds")?,
                    operator: AppComparisonOperator::GreaterThan,
                    value: json!(900),
                },
                AppPredicateNode::In {
                    field: field("status")?,
                    values: vec![json!("queued"), json!("ready")],
                },
                AppPredicateNode::All {
                    children: vec![0, 1],
                },
            ],
        }),
        order: vec![AppQueryOrder {
            field: field("created_at")?,
            direction: AppOrderDirection::Descending,
        }],
        cursor: Some(reference("cursor:v1:page_2")?),
        limit: 25,
        relation_expansions: Vec::new(),
        purpose: name("library_view")?,
    };
    query.validate_app_contract(&limits)?;

    let projection = AppRecordProjection {
        entity: name("saved_video")?,
        record_id: record("video_01")?,
        record_revision: revision(7)?,
        fields: BTreeMap::from([
            (field("duration_seconds")?, json!(1_842)),
            (field("status")?, json!("ready")),
            (field("title")?, json!("The Shape of Useful Time")),
        ]),
    };
    let page_schema = reference("schema:saved_video_projection:v1")?;
    let page = AppQueryPage {
        envelope: envelope(
            AppDataSource::AppStore,
            vec![projection],
            page_schema.clone(),
        )?,
        next_cursor: Some(reference("cursor:v1:page_3")?),
        result_schema_ref: page_schema,
    };
    page.validate_app_contract(&limits)?;

    let mutation = AppMutationCommand {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key: reference("mutation-key:fixture:create-video")?,
        atomicity: AppMutationAtomicity::AllOrNothing,
        expected_schema_revision: revision(3)?,
        operations: vec![
            AppMutationOperation::Create {
                entity: name("saved_video")?,
                temporary_id: name("new_video")?,
                record_id: None,
                payload: json!({
                    "source_url": "https://example.test/watch/42",
                    "status": "queued",
                    "title": "A smaller, clearer film"
                }),
            },
            AppMutationOperation::Update {
                entity: name("saved_video")?,
                record_id: record("video_01")?,
                patch: json!({"status": "archived"}),
            },
        ],
        expected_record_revisions: vec![AppExpectedRecordRevision {
            entity: name("saved_video")?,
            record_id: record("video_01")?,
            revision: revision(7)?,
        }],
    };
    mutation.validate_app_contract(&limits)?;

    let invocation = AppActionInvocation {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key: reference("action-key:fixture:shorten-video")?,
        action_id: name("shorten_video")?,
        action_revision: revision(2)?,
        input: envelope(
            AppDataSource::UserInput,
            json!({
                "source_url": "https://example.test/watch/42",
                "target_minutes": 8
            }),
            reference("schema:shorten_video_input:v2")?,
        )?,
        requested_result_schema_ref: reference("schema:shorten_video_result:v2")?,
        caller_surface_or_execution_ref: reference("surface:library:session_9")?,
    };
    invocation.validate_app_contract(&limits)?;

    let completed = AppActionResult {
        protocol_version: AppProtocolVersion::V1,
        action_id: name("shorten_video")?,
        run_ref: reference("run:shorten_video:01")?,
        status: AppActionStatus::Completed,
        output: Some(envelope(
            AppDataSource::AppAction,
            json!({
                "artifact_ref": "artifact:video:short_01",
                "duration_seconds": 481
            }),
            reference("schema:shorten_video_result:v2")?,
        )?),
        mutation_receipt_refs: vec![reference("mutation:receipt_01")?],
        external_effect_receipt_refs: Vec::new(),
        error: None,
    };
    completed.validate_app_contract(&limits)?;

    let artifact_projection = envelope(
        AppDataSource::ArtifactProjection,
        AppArtifactProjection {
            artifact_ref: reference("artifact:video:short_01")?,
            artifact_revision: revision(3)?,
            media_type: "video/mp4".to_owned(),
            byte_len: 24_000_000,
            content_digest: AppDigest::blake3(b"fixture-video-bytes"),
            value_schema_ref: reference("schema:video_clip:v1")?,
        },
        reference("schema:artifact_projection:v1")?,
    )?;
    artifact_projection.value.validate_app_contract(&limits)?;

    let uncertain = AppActionResult::<Value> {
        protocol_version: AppProtocolVersion::V1,
        action_id: name("publish_clip")?,
        run_ref: reference("run:publish_clip:02")?,
        status: AppActionStatus::Uncertain,
        output: None,
        mutation_receipt_refs: Vec::new(),
        external_effect_receipt_refs: vec![reference("external_receipt:publish_02")?],
        error: Some(AppErrorEnvelope {
            code: AppErrorCode::ExternalOutcomeUncertain,
            disposition: AppErrorDisposition::OutcomeUncertain,
            message: "The destination accepted the request but did not confirm its final state."
                .to_owned(),
            details: BTreeMap::from([(
                name("destination")?,
                json!("https://publisher.example.test"),
            )]),
            retry_after_ms: None,
        }),
    };
    uncertain.validate_app_contract(&limits)?;

    let conflict = AppErrorEnvelope {
        code: AppErrorCode::StaleRevision,
        disposition: AppErrorDisposition::RefreshAndRetry,
        message: "The record changed after this edit began.".to_owned(),
        details: BTreeMap::from([(name("current_revision")?, json!(8))]),
        retry_after_ms: None,
    };
    conflict.validate_app_contract(&limits)?;

    Ok(vec![
        fixture(
            "query_request",
            AppContractFixtureKind::QueryRequest,
            &query,
        )?,
        fixture("query_page", AppContractFixtureKind::QueryPage, &page)?,
        fixture(
            "mutation_command",
            AppContractFixtureKind::MutationCommand,
            &mutation,
        )?,
        fixture(
            "action_invocation",
            AppContractFixtureKind::ActionInvocation,
            &invocation,
        )?,
        fixture(
            "action_result_completed",
            AppContractFixtureKind::ActionResult,
            &completed,
        )?,
        fixture(
            "action_result_uncertain",
            AppContractFixtureKind::ActionResult,
            &uncertain,
        )?,
        fixture(
            "artifact_projection",
            AppContractFixtureKind::ArtifactProjection,
            &artifact_projection,
        )?,
        fixture(
            "error_stale_revision",
            AppContractFixtureKind::ErrorEnvelope,
            &conflict,
        )?,
    ])
}

fn envelope<T>(
    source: AppDataSource,
    value: T,
    value_schema_ref: AppReference,
) -> Result<AppDataEnvelope<T>, AppContractError>
where
    T: Serialize,
{
    let digest_value =
        serde_json::to_value(&value).map_err(|error| AppContractError::InvalidJson {
            message: error.to_string(),
        })?;
    let content_digest =
        AppDigest::blake3(&canonical_json_bytes(&digest_value).map_err(|error| {
            AppContractError::InvalidJson {
                message: error.to_string(),
            }
        })?);
    Ok(AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source,
        scope_binding_ref: AppScopeBindingRef::parse("scope_binding_owner_default")?,
        installation_id: installation("install_reading_room")?,
        package_revision_ref: reference("package:reading_room:1.0.0")?,
        schema_revision: revision(3)?,
        grant_revision: revision(2)?,
        value_schema_ref,
        value,
        source_refs: vec![AppSourceRef {
            kind: AppSourceRefKind::EntityRecord,
            reference: reference("record:saved_video:video_01")?,
            revision: Some(revision(7)?),
            fields: Vec::new(),
        }],
        handling_labels: AppHandlingLabels {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            policy_digest: AppDigest::blake3(b"fixture-policy"),
            provenance_digest: AppDigest::blake3(b"fixture-provenance"),
        },
        content_digest,
        produced_at: timestamp("2026-08-14T06:30:00Z")?,
        expires_at: Some(timestamp("2026-08-14T07:30:00Z")?),
    })
}

fn fixture<T>(
    name: &'static str,
    kind: AppContractFixtureKind,
    value: &T,
) -> Result<AppContractFixture, AppContractError>
where
    T: Serialize,
{
    let value = serde_json::to_value(value).map_err(|error| AppContractError::InvalidJson {
        message: error.to_string(),
    })?;
    Ok(AppContractFixture { name, kind, value })
}

fn installation(value: &str) -> Result<AppInstallationId, AppContractError> {
    AppInstallationId::parse(value)
}

fn record(value: &str) -> Result<AppRecordId, AppContractError> {
    AppRecordId::parse(value)
}

fn reference(value: &str) -> Result<AppReference, AppContractError> {
    AppReference::parse(value)
}

fn name(value: &str) -> Result<AppName, AppContractError> {
    AppName::parse(value)
}

fn field(value: &str) -> Result<AppFieldPath, AppContractError> {
    AppFieldPath::parse(value)
}

fn revision(value: u64) -> Result<AppRevision, AppContractError> {
    AppRevision::new(value)
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, AppContractError> {
    value
        .parse()
        .map_err(|error| AppContractError::invalid("fixture.timestamp", format!("{error}")))
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;

    #[test]
    fn canonical_fixtures_round_trip_through_the_server_contracts() {
        let limits = AppContractLimits::default();
        for fixture in canonical_app_contract_fixtures().unwrap() {
            match fixture.kind {
                AppContractFixtureKind::QueryRequest => {
                    round_trip::<AppQueryRequest>(&fixture, &limits)
                },
                AppContractFixtureKind::QueryPage => round_trip::<AppQueryPage>(&fixture, &limits),
                AppContractFixtureKind::MutationCommand => {
                    round_trip::<AppMutationCommand>(&fixture, &limits)
                },
                AppContractFixtureKind::ActionInvocation => {
                    round_trip::<AppActionInvocation<Value>>(&fixture, &limits)
                },
                AppContractFixtureKind::ActionResult => {
                    round_trip::<AppActionResult<Value>>(&fixture, &limits)
                },
                AppContractFixtureKind::ArtifactProjection => {
                    round_trip::<AppDataEnvelope<AppArtifactProjection>>(&fixture, &limits)
                },
                AppContractFixtureKind::ErrorEnvelope => {
                    round_trip::<AppErrorEnvelope>(&fixture, &limits)
                },
            }
        }
    }

    #[test]
    fn divergent_action_and_error_interpretations_remain_invalid() {
        let limits = AppContractLimits::default();
        let empty_completed: AppActionResult<Value> = serde_json::from_value(json!({
            "protocol_version": "1",
            "action_id": "shorten_video",
            "run_ref": "run:shorten_video:bad",
            "status": "completed"
        }))
        .unwrap();
        assert!(empty_completed.validate_app_contract(&limits).is_err());

        let effectful_waiting: AppActionResult<Value> = serde_json::from_value(json!({
            "protocol_version": "1",
            "action_id": "shorten_video",
            "run_ref": "run:shorten_video:bad_wait",
            "status": "waiting",
            "mutation_receipt_refs": ["mutation:premature"]
        }))
        .unwrap();
        assert!(effectful_waiting.validate_app_contract(&limits).is_err());

        let effectful_failure: AppActionResult<Value> = serde_json::from_value(json!({
            "protocol_version": "1",
            "action_id": "shorten_video",
            "run_ref": "run:shorten_video:bad_failure",
            "status": "failed",
            "external_effect_receipt_refs": ["external_receipt:ambiguous"],
            "error": {
                "code": "invalid_request",
                "disposition": "terminal",
                "message": "An ordinary failure cannot claim an external effect."
            }
        }))
        .unwrap();
        assert!(effectful_failure.validate_app_contract(&limits).is_err());

        let mismatched_uncertainty: AppErrorEnvelope = serde_json::from_value(json!({
            "code": "external_outcome_uncertain",
            "disposition": "retry_same_input",
            "message": "Do not retry an uncertain external effect blindly."
        }))
        .unwrap();
        assert!(mismatched_uncertainty
            .validate_app_contract(&limits)
            .is_err());
    }

    fn round_trip<T>(fixture: &AppContractFixture, limits: &AppContractLimits)
    where
        T: DeserializeOwned + Serialize + ValidateAppContract,
    {
        let decoded: T = serde_json::from_value(fixture.value.clone()).unwrap();
        decoded.validate_app_contract(limits).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), fixture.value);
    }
}
