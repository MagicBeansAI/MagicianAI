//! Exact scheduled-input proof after the store owner reopens the reviewed selector.

use serde_json::Value;

use super::{
    manifest::AppManifestBehaviorInputSelector,
    models::{
        AppDataEnvelope, AppDataSource, AppDigest, AppFieldPath, AppReference, AppSourceRefKind,
    },
    records::{AppApprovedRecordProjection, AppDataHandlingPolicy},
    workflows::AppWorkflowError,
};

/// Both inputs are owner-resolved: `expected` is the sealed task projection;
/// `current` comes from the live store snapshot, never a caller envelope.
pub(super) fn approve_current_scheduled_projection(
    selector: &AppManifestBehaviorInputSelector,
    expected: &AppDataEnvelope<Value>,
    source_policy: &AppDataHandlingPolicy,
    current: &AppDataEnvelope<Value>,
    current_policy: &AppDataHandlingPolicy,
) -> Result<AppApprovedRecordProjection, AppWorkflowError> {
    let selector_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "entity": &selector.entity, "record_id": &selector.record_id,
    }))?;
    let reference = AppReference::parse(format!("record:{selector_digest}"))?;
    let fields = selector
        .fields
        .iter()
        .map(|field| AppFieldPath::parse(field.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    if expected.source != AppDataSource::AppStore
        || current.source != AppDataSource::AppStore
        || expected.scope_binding_ref != current.scope_binding_ref
        || expected.installation_id != current.installation_id
        || expected.package_revision_ref != current.package_revision_ref
        || expected.schema_revision != current.schema_revision
        || expected.grant_revision != current.grant_revision
        || expected.value_schema_ref != current.value_schema_ref
        || expected.value != current.value
        || expected.content_digest != current.content_digest
        || expected.source_refs != current.source_refs
        || source_policy != current_policy
        || !matches!(current.source_refs.as_slice(), [source]
            if source.kind == AppSourceRefKind::EntityField
                && source.reference == reference
                && source.fields == fields
                && source.revision.is_some())
    {
        return Err(AppWorkflowError::StaleRuntimeAuthority);
    }
    let source = &current.source_refs[0];
    let revision = source
        .revision
        .ok_or(AppWorkflowError::StaleRuntimeAuthority)?;
    Ok(AppApprovedRecordProjection {
        entity: selector.entity.clone(),
        fields,
        record_revisions: vec![AppReference::parse(format!(
            "{}@{}",
            source.reference,
            revision.get()
        ))?],
    })
}
