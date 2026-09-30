use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use chrono::Utc;
use serde::{de::IgnoredAny, Deserialize, Deserializer};
use serde_json::Value;

use crate::magician_v2::agents::AgentStorage;
use crate::magician_v2::json_traversal::{
    clone_json_iteratively, discard_json_iteratively, inspect_json_bounded, MAX_RETAINED_JSON_DEPTH,
};

use super::{
    models::{PersistedExecutionArtifactRecord, PersistedExecutionArtifactsIndex},
    service::{ArtifactV2Error, ScopeRef},
    workspace::ArtifactV2Workspace,
};

const MAX_EXECUTION_ARTIFACT_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXECUTION_ARTIFACT_INDEX_NODES: usize = 1_000_000;
const MAX_EXECUTION_ARTIFACT_RECORDS: usize = 10_000;
const MAX_EXECUTION_ARTIFACT_METADATA_FIELD_BYTES: usize = 64 * 1024;
const MAX_EXECUTION_ARTIFACT_METADATA_RECORD_BYTES: usize = 512 * 1024;

/// Return the process-local transaction lock for one durable artifact index.
///
/// The advisory file lock remains the cross-process authority, but sending all
/// writers in this process directly to its retry loop makes every waiter start
/// the same timeout concurrently. Under CPU or filesystem pressure, a healthy
/// serialized queue can then time out before reaching its turn. This fair async
/// mutex queues local writers first, so only the head waiter consumes the
/// cross-process lock timeout. Weak entries keep the registry proportional to
/// active indexes rather than all indexes ever observed by the process.
fn process_artifact_index_lock(index_path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(index_path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(index_path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

#[derive(Debug, Deserialize)]
struct PersistedExecutionArtifactMetadataIndex {
    execution_id: String,
    #[serde(default)]
    artifacts: Vec<PersistedExecutionArtifactMetadataRecord>,
    #[allow(dead_code)]
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct PersistedExecutionArtifactMetadataRecord {
    artifact_id: String,
    artifact_type: String,
    content_type: String,
    #[serde(deserialize_with = "deserialize_artifact_payload_metadata")]
    payload: PersistedExecutionArtifactPayloadMetadata,
    produced_at: String,
    #[serde(default)]
    source_execution_id: Option<String>,
    #[serde(default)]
    source_artifact_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PersistedExecutionArtifactPayloadMetadata {
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    artifact_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    artifact_kind: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    content_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    mime_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_scalar")]
    size_bytes: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    produced_at: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    tool_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    display_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    file_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    task_relative_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    task_absolute_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    execution_relative_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    execution_absolute_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    relative_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    absolute_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    export_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    task_download_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    execution_download_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    execution_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    task_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_scalar")]
    agent_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_scalar")]
    iteration: Option<u64>,
}

struct ArtifactPayloadMetadataVisitor;

impl<'de> serde::de::Visitor<'de> for ArtifactPayloadMetadataVisitor {
    type Value = PersistedExecutionArtifactPayloadMetadata;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an artifact payload object or an ignored non-object payload")
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        PersistedExecutionArtifactPayloadMetadata::deserialize(
            serde::de::value::MapAccessDeserializer::new(map),
        )
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(PersistedExecutionArtifactPayloadMetadata::default())
    }
}

fn deserialize_artifact_payload_metadata<'de, D>(
    deserializer: D,
) -> Result<PersistedExecutionArtifactPayloadMetadata, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(ArtifactPayloadMetadataVisitor)
}

struct OptionalStringScalarVisitor;

impl<'de> serde::de::Visitor<'de> for OptionalStringScalarVisitor {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a string scalar or an ignored non-string value")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Some(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

fn deserialize_optional_string_scalar<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(OptionalStringScalarVisitor)
}

struct OptionalU64ScalarVisitor;

impl<'de> serde::de::Visitor<'de> for OptionalU64ScalarVisitor {
    type Value = Option<u64>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an unsigned integer scalar or an ignored non-integer value")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(u64::try_from(value).ok())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

fn deserialize_optional_u64_scalar<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(OptionalU64ScalarVisitor)
}

fn index_shape_is_admitted(index: &PersistedExecutionArtifactsIndex) -> bool {
    index_shape_is_admitted_with_limits(
        index,
        MAX_EXECUTION_ARTIFACT_RECORDS,
        MAX_EXECUTION_ARTIFACT_INDEX_NODES,
        MAX_RETAINED_JSON_DEPTH,
    )
}

fn index_shape_is_admitted_with_limits(
    index: &PersistedExecutionArtifactsIndex,
    max_records: usize,
    max_nodes: usize,
    max_depth: usize,
) -> bool {
    if index.artifacts.len() > max_records {
        return false;
    }
    // Exact node count for the typed wire shape: root object + its three
    // fields, then each record object, six scalar metadata fields, and its
    // payload tree.
    let mut nodes = 4usize;
    for record in &index.artifacts {
        let Some(payload) = inspect_json_bounded(&record.payload, max_nodes.saturating_sub(nodes))
        else {
            return false;
        };
        if payload.max_depth.saturating_add(3) > max_depth {
            return false;
        }
        nodes = nodes.saturating_add(7).saturating_add(payload.nodes);
        if nodes > max_nodes {
            return false;
        }
    }
    true
}

fn discard_execution_artifact_index_payloads(index: &mut PersistedExecutionArtifactsIndex) {
    for record in &mut index.artifacts {
        discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
    }
}

fn discard_execution_artifact_index(mut index: PersistedExecutionArtifactsIndex) {
    discard_execution_artifact_index_payloads(&mut index);
}

fn artifact_metadata_record_is_admitted(record: &PersistedExecutionArtifactMetadataRecord) -> bool {
    let payload = &record.payload;
    let fields = [
        Some(record.artifact_id.as_str()),
        Some(record.artifact_type.as_str()),
        Some(record.content_type.as_str()),
        Some(record.produced_at.as_str()),
        record.source_execution_id.as_deref(),
        record.source_artifact_id.as_deref(),
        payload.artifact_id.as_deref(),
        payload.artifact_kind.as_deref(),
        payload.content_type.as_deref(),
        payload.mime_type.as_deref(),
        payload.produced_at.as_deref(),
        payload.tool_name.as_deref(),
        payload.display_name.as_deref(),
        payload.file_name.as_deref(),
        payload.task_relative_path.as_deref(),
        payload.task_absolute_path.as_deref(),
        payload.execution_relative_path.as_deref(),
        payload.execution_absolute_path.as_deref(),
        payload.relative_path.as_deref(),
        payload.absolute_path.as_deref(),
        payload.export_path.as_deref(),
        payload.task_download_url.as_deref(),
        payload.execution_download_url.as_deref(),
        payload.execution_id.as_deref(),
        payload.task_id.as_deref(),
        payload.agent_id.as_deref(),
    ];
    let mut total = 0usize;
    for value in fields.into_iter().flatten() {
        if value.len() > MAX_EXECUTION_ARTIFACT_METADATA_FIELD_BYTES {
            return false;
        }
        total = total.saturating_add(value.len());
        if total > MAX_EXECUTION_ARTIFACT_METADATA_RECORD_BYTES {
            return false;
        }
    }
    true
}

fn insert_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &'static str,
    value: Option<String>,
) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value));
    }
}

fn artifact_metadata_record_into_value(record: PersistedExecutionArtifactMetadataRecord) -> Value {
    let mut payload = serde_json::Map::new();
    insert_optional_string(&mut payload, "artifact_id", record.payload.artifact_id);
    insert_optional_string(&mut payload, "artifact_kind", record.payload.artifact_kind);
    insert_optional_string(&mut payload, "content_type", record.payload.content_type);
    insert_optional_string(&mut payload, "mime_type", record.payload.mime_type);
    if let Some(size_bytes) = record.payload.size_bytes {
        payload.insert("size_bytes".to_string(), Value::from(size_bytes));
    }
    insert_optional_string(&mut payload, "produced_at", record.payload.produced_at);
    insert_optional_string(&mut payload, "tool_name", record.payload.tool_name);
    insert_optional_string(&mut payload, "display_name", record.payload.display_name);
    insert_optional_string(&mut payload, "file_name", record.payload.file_name);
    insert_optional_string(
        &mut payload,
        "task_relative_path",
        record.payload.task_relative_path,
    );
    insert_optional_string(
        &mut payload,
        "task_absolute_path",
        record.payload.task_absolute_path,
    );
    insert_optional_string(
        &mut payload,
        "execution_relative_path",
        record.payload.execution_relative_path,
    );
    insert_optional_string(
        &mut payload,
        "execution_absolute_path",
        record.payload.execution_absolute_path,
    );
    insert_optional_string(&mut payload, "relative_path", record.payload.relative_path);
    insert_optional_string(&mut payload, "absolute_path", record.payload.absolute_path);
    insert_optional_string(&mut payload, "export_path", record.payload.export_path);
    insert_optional_string(
        &mut payload,
        "task_download_url",
        record.payload.task_download_url,
    );
    insert_optional_string(
        &mut payload,
        "execution_download_url",
        record.payload.execution_download_url,
    );
    insert_optional_string(&mut payload, "execution_id", record.payload.execution_id);
    insert_optional_string(&mut payload, "task_id", record.payload.task_id);
    insert_optional_string(&mut payload, "agent_id", record.payload.agent_id);
    if let Some(iteration) = record.payload.iteration {
        payload.insert("iteration".to_string(), Value::from(iteration));
    }

    let mut object = serde_json::Map::new();
    object.insert("artifact_id".to_string(), Value::String(record.artifact_id));
    object.insert(
        "artifact_type".to_string(),
        Value::String(record.artifact_type),
    );
    object.insert(
        "content_type".to_string(),
        Value::String(record.content_type),
    );
    object.insert("payload".to_string(), Value::Object(payload));
    object.insert("produced_at".to_string(), Value::String(record.produced_at));
    object.insert(
        "source_execution_id".to_string(),
        record
            .source_execution_id
            .map_or(Value::Null, Value::String),
    );
    object.insert(
        "source_artifact_id".to_string(),
        record.source_artifact_id.map_or(Value::Null, Value::String),
    );
    Value::Object(object)
}

fn clone_execution_artifact_record_stack_safe(
    record: &PersistedExecutionArtifactRecord,
) -> PersistedExecutionArtifactRecord {
    PersistedExecutionArtifactRecord {
        artifact_id: record.artifact_id.clone(),
        artifact_type: record.artifact_type.clone(),
        content_type: record.content_type.clone(),
        payload: clone_json_iteratively(&record.payload),
        produced_at: record.produced_at.clone(),
        source_execution_id: record.source_execution_id.clone(),
        source_artifact_id: record.source_artifact_id.clone(),
    }
}

fn prepare_copied_artifact_record(
    mut record: PersistedExecutionArtifactRecord,
    source_execution_id: &str,
) -> PersistedExecutionArtifactRecord {
    if record.source_execution_id.is_none() {
        record.source_execution_id = Some(source_execution_id.to_string());
    }
    if record.source_artifact_id.is_none() {
        record.source_artifact_id = Some(record.artifact_id.clone());
    }
    record
}

fn discard_selected_artifact_records(
    records: &mut BTreeMap<String, (Option<PersistedExecutionArtifactRecord>, usize)>,
) {
    for (record, _) in records.values_mut() {
        if let Some(mut record) = record.take() {
            discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(payload: Value) -> PersistedExecutionArtifactRecord {
        PersistedExecutionArtifactRecord {
            artifact_id: "artifact-1".to_string(),
            artifact_type: "tool_result".to_string(),
            content_type: "application/json".to_string(),
            payload,
            produced_at: "2026-08-05T00:00:00Z".to_string(),
            source_execution_id: None,
            source_artifact_id: None,
        }
    }

    #[test]
    fn typed_index_admission_has_exact_node_record_and_depth_boundaries() {
        let index = PersistedExecutionArtifactsIndex {
            execution_id: "exec-source".to_string(),
            artifacts: vec![record(json!([null, null]))],
            updated_at: "2026-08-05T00:00:00Z".to_string(),
        };
        // Root/index fields = 4, record/metadata = 7, payload array + two
        // scalars = 3.
        assert!(index_shape_is_admitted_with_limits(&index, 1, 14, 64));
        assert!(!index_shape_is_admitted_with_limits(&index, 1, 13, 64));
        assert!(!index_shape_is_admitted_with_limits(&index, 0, 14, 64));

        let mut nested = Value::Null;
        for _ in 0..62 {
            nested = Value::Array(vec![nested]);
        }
        let deep = PersistedExecutionArtifactsIndex {
            execution_id: "exec-deep".to_string(),
            artifacts: vec![record(nested)],
            updated_at: "2026-08-05T00:00:00Z".to_string(),
        };
        assert!(!index_shape_is_admitted_with_limits(
            &deep,
            1,
            1_000,
            MAX_RETAINED_JSON_DEPTH,
        ));
    }

    #[test]
    fn metadata_projection_preserves_legacy_non_object_payload_tolerance() {
        for payload in [json!(null), json!("legacy text"), json!([1, 2, 3])] {
            let wire = json!({
                "execution_id": "exec-legacy-payload",
                "artifacts": [{
                    "artifact_id": "artifact-1",
                    "artifact_type": "tool_result",
                    "content_type": "application/json",
                    "payload": payload,
                    "produced_at": "2026-08-05T00:00:00Z",
                    "source_execution_id": null,
                    "source_artifact_id": null
                }],
                "updated_at": "2026-08-05T00:00:00Z"
            });
            let projected: PersistedExecutionArtifactMetadataIndex =
                serde_json::from_value(wire).expect("legacy scalar payload remains readable");
            let value = artifact_metadata_record_into_value(
                projected
                    .artifacts
                    .into_iter()
                    .next()
                    .expect("one metadata record"),
            );
            assert_eq!(value["payload"], json!({}));
        }
    }

    #[test]
    fn metadata_projection_does_not_make_the_required_payload_field_optional() {
        let wire = json!({
            "execution_id": "exec-missing-payload",
            "artifacts": [{
                "artifact_id": "artifact-1",
                "artifact_type": "tool_result",
                "content_type": "application/json",
                "produced_at": "2026-08-05T00:00:00Z"
            }],
            "updated_at": "2026-08-05T00:00:00Z"
        });
        assert!(serde_json::from_value::<PersistedExecutionArtifactMetadataIndex>(wire).is_err());
    }

    #[tokio::test]
    async fn streamed_index_roundtrip_and_selected_copy_preserve_record_contract() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = ScopeRef::system_internal_unauthenticated(
            &"anonymous".to_string(),
            &"default".to_string(),
        );
        for task_id in ["task-source", "task-target"] {
            workspace
                .create_dir_all_path(workspace.task_dir(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                ))
                .await
                .expect("task root");
        }
        let store = FilesystemExecutionArtifactIndexStore::new(workspace);
        let source = record(json!({"rows": [{"id": 1}, {"id": 2}]}));
        store
            .upsert_artifact(&scope, "task-source", "exec-source", source.clone())
            .await
            .expect("source upsert");
        store
            .copy_selected_artifacts(
                &scope,
                "task-source",
                "exec-source",
                &scope,
                "task-target",
                "exec-target",
                &[source.artifact_id.clone()],
            )
            .await
            .expect("selected copy");

        let copied = store
            .list_artifacts(&scope, "task-target", "exec-target")
            .await
            .expect("target list");
        assert_eq!(copied.len(), 1);
        assert_eq!(copied[0].payload, source.payload);
        assert_eq!(
            copied[0].source_execution_id.as_deref(),
            Some("exec-source")
        );
        assert_eq!(copied[0].source_artifact_id.as_deref(), Some("artifact-1"));
    }

    #[tokio::test]
    async fn terminal_file_links_recover_legacy_paths_without_crossing_execution_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = ScopeRef::system_internal_unauthenticated("owner", "default");
        workspace
            .create_dir_all_path(workspace.task_dir("owner", "default", "task-1"))
            .await
            .unwrap();
        workspace
            .ensure_execution_workspace("owner", "default", "task-1", "exec-1")
            .await
            .unwrap();
        let outputs = workspace.execution_outputs_dir("owner", "default", "task-1", "exec-1");
        let article = outputs.join("article.md");
        tokio::fs::write(&article, b"# The actual article")
            .await
            .unwrap();
        let outside = temp.path().join("another-execution.md");
        tokio::fs::write(&outside, b"another execution's content")
            .await
            .unwrap();
        let mut paths = vec![article.clone(), outside.clone(), outputs.join("missing.md")];
        #[cfg(unix)]
        {
            let escaped = outputs.join("escaped.md");
            std::os::unix::fs::symlink(&outside, &escaped).unwrap();
            paths.push(escaped);
        }
        let records = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let mut row =
                    record(json!({"execution_absolute_path": path, "display_name": "article.md"}));
                row.artifact_id = format!("terminal-{i}");
                row.artifact_type = "article".into();
                row.source_execution_id = Some("exec-1".into());
                row
            })
            .collect();
        let legacy = PersistedExecutionArtifactsIndex {
            execution_id: "exec-1".into(),
            artifacts: records,
            updated_at: Utc::now().to_rfc3339(),
        };
        let index_path =
            workspace.execution_artifacts_index_path("owner", "default", "task-1", "exec-1");
        tokio::fs::write(&index_path, serde_json::to_vec(&legacy).unwrap())
            .await
            .unwrap();
        let store = FilesystemExecutionArtifactIndexStore::new(workspace);
        let listed = store
            .list_artifacts(&scope, "task-1", "exec-1")
            .await
            .unwrap();
        let metadata = store
            .list_artifact_metadata_values(&scope, "task-1", "exec-1")
            .await
            .unwrap();
        for payload in [&listed[0].payload, &metadata[0]["payload"]] {
            assert_eq!(payload["execution_relative_path"], "outputs/article.md");
            assert_eq!(
                payload["task_relative_path"],
                "executions/exec-1/outputs/article.md"
            );
        }
        for row in &listed[1..] {
            assert!(row.payload.get("task_relative_path").is_none());
        }
        for row in &metadata[1..] {
            assert!(row["payload"].get("task_relative_path").is_none());
        }
        // Reads repair the projection, without rewriting historical evidence.
        assert!(store
            .load_index(&scope, "task-1", "exec-1")
            .await
            .unwrap()
            .artifacts[0]
            .payload
            .get("task_relative_path")
            .is_none());
        let mut new_record = legacy.artifacts[0].clone();
        new_record.artifact_id = "new-terminal".into();
        store
            .upsert_artifact(&scope, "task-1", "exec-1", new_record)
            .await
            .unwrap();
        let saved = store.load_index(&scope, "task-1", "exec-1").await.unwrap();
        assert_eq!(
            saved.artifacts.last().unwrap().payload["task_relative_path"],
            "executions/exec-1/outputs/article.md"
        );
    }

    #[test]
    fn metadata_projection_skips_large_and_deep_raw_payload_bodies_on_a_small_stack() {
        std::thread::Builder::new()
            .name("artifact-metadata-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("small-stack runtime");
                runtime.block_on(async {
                    let temp = tempfile::tempdir().expect("temp workspace");
                    let workspace = ArtifactV2Workspace::new(temp.path());
                    let scope = ScopeRef::system_internal_unauthenticated(
                        &"anonymous".to_string(),
                        &"default".to_string(),
                    );
                    workspace
                        .create_dir_all_path(workspace.task_dir(
                            &scope.principal(),
                            &scope.workspace(),
                            "task-metadata",
                        ))
                        .await
                        .expect("task root");
                    let store = FilesystemExecutionArtifactIndexStore::new(workspace);
                    let mut deep_unknown = Value::String("leaf".to_string());
                    for _ in 0..MAX_RETAINED_JSON_DEPTH.saturating_sub(5) {
                        deep_unknown = Value::Array(vec![deep_unknown]);
                    }
                    let payload = json!({
                        "tool_name": "content_read",
                        "task_relative_path": "outputs/report.json",
                        "size_bytes": 2_097_152,
                        "raw": "x".repeat(2 * 1024 * 1024),
                        "deep_unknown": deep_unknown,
                        "display_name": {"wrong": "shape is ignored like as_str()"}
                    });
                    store
                        .upsert_artifact(&scope, "task-metadata", "exec-metadata", record(payload))
                        .await
                        .expect("artifact upsert");

                    let metadata = store
                        .list_artifact_metadata_values(&scope, "task-metadata", "exec-metadata")
                        .await
                        .expect("bounded metadata projection");
                    assert_eq!(metadata.len(), 1);
                    assert_eq!(metadata[0]["payload"]["tool_name"], "content_read");
                    assert_eq!(
                        metadata[0]["payload"]["task_relative_path"],
                        "outputs/report.json"
                    );
                    assert!(metadata[0]["payload"].get("raw").is_none());
                    assert!(metadata[0]["payload"].get("deep_unknown").is_none());
                    assert!(metadata[0]["payload"].get("display_name").is_none());
                });
            })
            .expect("spawn artifact metadata regression")
            .join()
            .expect("artifact metadata regression completes");
    }

    #[tokio::test]
    async fn parallel_artifact_upserts_preserve_every_record_and_index_identity() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = ScopeRef::system_internal_unauthenticated(
            &"anonymous".to_string(),
            &"default".to_string(),
        );
        workspace
            .create_dir_all_path(workspace.task_dir(
                &scope.principal(),
                &scope.workspace(),
                "task-parallel",
            ))
            .await
            .expect("task root");
        let store = FilesystemExecutionArtifactIndexStore::new(workspace.clone());
        let mut writers = Vec::new();
        for index in 0..32 {
            let store = store.clone();
            let scope = scope.clone();
            writers.push(tokio::spawn(async move {
                let mut record = record(json!({"index": index}));
                record.artifact_id = format!("artifact-{index:02}");
                store
                    .upsert_artifact(&scope, "task-parallel", "exec-parallel", record)
                    .await
            }));
        }
        for writer in writers {
            writer.await.expect("writer task").expect("artifact upsert");
        }
        let index = store
            .load_index(&scope, "task-parallel", "exec-parallel")
            .await
            .expect("complete index");
        assert_eq!(index.execution_id, "exec-parallel");
        assert_eq!(index.artifacts.len(), 32);

        let wrong_path = workspace.execution_artifacts_index_path(
            &scope.principal(),
            &scope.workspace(),
            "task-parallel",
            "exec-wrong-path",
        );
        let wrong_index = PersistedExecutionArtifactsIndex {
            execution_id: "exec-other".to_string(),
            artifacts: Vec::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        workspace
            .write_json_value_atomic_stream_path(
                wrong_path,
                wrong_index,
                usize::try_from(MAX_EXECUTION_ARTIFACT_INDEX_BYTES).unwrap_or(usize::MAX),
            )
            .await
            .expect("mismatched index fixture");
        store
            .load_index(&scope, "task-parallel", "exec-wrong-path")
            .await
            .expect_err("path-bound execution identity must fail closed");
    }

    #[tokio::test]
    async fn exact_artifact_identity_admission_is_execution_scoped_and_duplicate_free() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = ScopeRef::system_internal_unauthenticated(
            &"anonymous".to_string(),
            &"default".to_string(),
        );
        workspace
            .create_dir_all_path(workspace.task_dir(
                &scope.principal(),
                &scope.workspace(),
                "task-provenance",
            ))
            .await
            .expect("task root");
        let store = FilesystemExecutionArtifactIndexStore::new(workspace);
        let mut admitted = record(json!({"secret": "payload body is not needed for identity"}));
        admitted.artifact_id = "report-1".to_owned();
        store
            .upsert_artifact(&scope, "task-provenance", "exec-source", admitted)
            .await
            .expect("artifact fixture");

        assert!(store
            .contains_exact_artifact_ids(
                &scope,
                "task-provenance",
                "exec-source",
                &["report-1".to_owned()],
            )
            .await
            .unwrap());
        assert!(!store
            .contains_exact_artifact_ids(
                &scope,
                "task-provenance",
                "exec-other",
                &["report-1".to_owned()],
            )
            .await
            .unwrap());
        assert!(store
            .contains_exact_artifact_ids(
                &scope,
                "task-provenance",
                "exec-source",
                &["report-1".to_owned(), "report-1".to_owned()],
            )
            .await
            .is_err());
    }

    #[test]
    fn process_artifact_index_locks_share_only_an_exact_index_identity() {
        let first_path = Path::new("/tmp/execution-a/persisted_artifacts.json");
        let second_path = Path::new("/tmp/execution-b/persisted_artifacts.json");

        let first = process_artifact_index_lock(first_path);
        let same = process_artifact_index_lock(first_path);
        let second = process_artifact_index_lock(second_path);

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn selected_copy_moves_deep_payload_and_only_clones_true_duplicate_requests() {
        std::thread::Builder::new()
            .name("execution-artifact-copy-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("small-stack runtime");
                runtime.block_on(async {
                    let temp = tempfile::tempdir().expect("temp workspace");
                    let workspace = ArtifactV2Workspace::new(temp.path());
                    let scope = ScopeRef::system_internal_unauthenticated(
                        &"anonymous".to_string(),
                        &"default".to_string(),
                    );
                    for task_id in ["task-source", "task-target"] {
                        workspace
                            .create_dir_all_path(workspace.task_dir(
                                &scope.principal(),
                                &scope.workspace(),
                                task_id,
                            ))
                            .await
                            .expect("task root");
                    }
                    let store = FilesystemExecutionArtifactIndexStore::new(workspace);
                    let mut deep_payload = Value::String("leaf".to_string());
                    for _ in 0..MAX_RETAINED_JSON_DEPTH.saturating_sub(5) {
                        deep_payload = Value::Array(vec![deep_payload]);
                    }
                    store
                        .upsert_artifact(&scope, "task-source", "exec-source", record(deep_payload))
                        .await
                        .expect("deep source upsert");
                    let mut second = record(json!({"kind": "second"}));
                    second.artifact_id = "artifact-2".to_string();
                    store
                        .upsert_artifact(&scope, "task-source", "exec-source", second)
                        .await
                        .expect("second source upsert");

                    store
                        .copy_selected_artifacts(
                            &scope,
                            "task-source",
                            "exec-source",
                            &scope,
                            "task-target",
                            "exec-target",
                            &[
                                "artifact-2".to_string(),
                                "artifact-1".to_string(),
                                "artifact-1".to_string(),
                            ],
                        )
                        .await
                        .expect("ordered duplicate selected copy");

                    let mut target = store
                        .load_index(&scope, "task-target", "exec-target")
                        .await
                        .expect("target index");
                    assert_eq!(target.artifacts.len(), 2);
                    assert_eq!(target.artifacts[0].artifact_id, "artifact-2");
                    assert_eq!(target.artifacts[1].artifact_id, "artifact-1");
                    assert_eq!(
                        target.artifacts[1].source_execution_id.as_deref(),
                        Some("exec-source")
                    );
                    assert_eq!(
                        target.artifacts[1].source_artifact_id.as_deref(),
                        Some("artifact-1")
                    );
                    assert_eq!(
                        crate::magician_v2::json_traversal::inspect_json(
                            &target.artifacts[1].payload
                        )
                        .max_depth,
                        MAX_RETAINED_JSON_DEPTH.saturating_sub(5)
                    );
                    discard_execution_artifact_index(std::mem::take(&mut target));
                });
            })
            .expect("spawn small-stack artifact-copy thread")
            .join()
            .expect("small-stack artifact-copy thread");
    }
}

fn corrupt_index(message: &str) -> ArtifactV2Error {
    ArtifactV2Error::Runtime(format!("corrupt execution artifact index: {message}"))
}

#[derive(Debug, Clone)]
pub struct FilesystemExecutionArtifactIndexStore {
    workspace: ArtifactV2Workspace,
}

impl FilesystemExecutionArtifactIndexStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    pub async fn load_index(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<PersistedExecutionArtifactsIndex, ArtifactV2Error> {
        let path = self.workspace.execution_artifacts_index_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        match self
            .workspace
            .read_json_bounded_stream_path_with_cleanup_on_error::<
                PersistedExecutionArtifactsIndex,
                _,
                _,
            >(
                &path,
                MAX_EXECUTION_ARTIFACT_INDEX_BYTES,
                MAX_RETAINED_JSON_DEPTH,
                MAX_EXECUTION_ARTIFACT_INDEX_NODES,
                discard_execution_artifact_index_payloads,
            )
            .await
        {
            Ok(index) => {
                if !index_shape_is_admitted(&index) {
                    discard_execution_artifact_index(index);
                    return Err(corrupt_index("typed shape limit exceeded"));
                }
                if index.execution_id != execution_id {
                    discard_execution_artifact_index(index);
                    return Err(corrupt_index("execution identity mismatch"));
                }
                Ok(index)
            },
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(PersistedExecutionArtifactsIndex {
                    execution_id: execution_id.to_string(),
                    artifacts: Vec::new(),
                    updated_at: Utc::now().to_rfc3339(),
                })
            },
            Err(ArtifactV2Error::InvalidRequest(message)) => Err(corrupt_index(&message)),
            Err(err) => Err(err),
        }
    }

    pub async fn list_artifacts(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<Vec<PersistedExecutionArtifactRecord>, ArtifactV2Error> {
        let mut index = self.load_index(scope, task_id, execution_id).await?;
        index
            .artifacts
            .sort_by(|left, right| left.produced_at.cmp(&right.produced_at));
        for record in &mut index.artifacts {
            if record.source_execution_id.as_deref() == Some(execution_id) {
                self.add_execution_output_paths(scope, task_id, execution_id, &mut record.payload)
                    .await;
            }
        }
        Ok(index.artifacts)
    }

    /// Validate a bounded, duplicate-free set of artifact identities against
    /// one exact execution without retaining artifact payloads. This is the
    /// provenance admission seam used by app workflow terminal commits.
    pub async fn contains_exact_artifact_ids(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        artifact_ids: &[String],
    ) -> Result<bool, ArtifactV2Error> {
        if artifact_ids.len() > 256
            || artifact_ids
                .iter()
                .any(|artifact_id| artifact_id.is_empty() || artifact_id.len() > 512)
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "artifact provenance references exceed their identity bound".to_owned(),
            ));
        }
        let requested = artifact_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if requested.len() != artifact_ids.len() {
            return Err(ArtifactV2Error::InvalidRequest(
                "artifact provenance references must be unique".to_owned(),
            ));
        }
        if requested.is_empty() {
            return Ok(true);
        }
        let metadata = self
            .list_artifact_metadata_values(scope, task_id, execution_id)
            .await?;
        let admitted = metadata
            .iter()
            .filter_map(|record| record.get("artifact_id").and_then(Value::as_str))
            .collect::<std::collections::BTreeSet<_>>();
        Ok(requested.is_subset(&admitted))
    }

    /// Read only the scalar artifact metadata used by continuation and chat
    /// projections. Unknown payload bodies are consumed by Serde's ignored
    /// visitor after the workspace's encoded byte/depth/node preflight, so a
    /// multi-megabyte raw tool result is never retained merely to render its
    /// path, MIME type, lineage, or download affordance.
    pub async fn list_artifact_metadata_values(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<Vec<Value>, ArtifactV2Error> {
        let path = self.workspace.execution_artifacts_index_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let mut index = match self
            .workspace
            .read_json_bounded_stream_path::<PersistedExecutionArtifactMetadataIndex, _>(
                &path,
                MAX_EXECUTION_ARTIFACT_INDEX_BYTES,
                MAX_RETAINED_JSON_DEPTH,
                MAX_EXECUTION_ARTIFACT_INDEX_NODES,
            )
            .await
        {
            Ok(index) => index,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(ArtifactV2Error::InvalidRequest(message)) => {
                return Err(corrupt_index(&message));
            },
            Err(error) => return Err(error),
        };
        if index.execution_id != execution_id {
            return Err(corrupt_index("execution identity mismatch"));
        }
        if index.artifacts.len() > MAX_EXECUTION_ARTIFACT_RECORDS {
            return Err(corrupt_index("record limit exceeded"));
        }
        if index
            .artifacts
            .iter()
            .any(|record| !artifact_metadata_record_is_admitted(record))
        {
            return Err(corrupt_index("metadata scalar limit exceeded"));
        }
        index
            .artifacts
            .sort_by(|left, right| left.produced_at.cmp(&right.produced_at));
        let mut records: Vec<Value> = index
            .artifacts
            .into_iter()
            .map(artifact_metadata_record_into_value)
            .collect();
        for record in &mut records {
            if record.get("source_execution_id").and_then(Value::as_str) == Some(execution_id) {
                if let Some(payload) = record.get_mut("payload") {
                    self.add_execution_output_paths(scope, task_id, execution_id, payload)
                        .await;
                }
            }
        }
        Ok(records)
    }

    /// Older terminal artifacts recorded an absolute file path only. Clients
    /// require a task-relative address to offer Open/Download. Recover that
    /// address only after resolving the file inside this exact execution's
    /// output directory; never infer ownership from a filename or path suffix.
    async fn add_execution_output_paths(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        payload: &mut Value,
    ) {
        let Some(fields) = payload.as_object_mut() else {
            return;
        };
        if fields
            .get("task_relative_path")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.is_empty())
            || fields
                .get("execution_relative_path")
                .and_then(Value::as_str)
                .is_some_and(|p| !p.is_empty())
        {
            return;
        }
        let Some(absolute) = fields
            .get("execution_absolute_path")
            .and_then(Value::as_str)
        else {
            return;
        };
        let outputs = self.workspace.execution_outputs_dir(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let Ok(root) = self.workspace.canonicalize_path(&outputs).await else {
            return;
        };
        let Ok(path) = self.workspace.canonicalize_path(absolute).await else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            return;
        };
        let Some(relative) = relative
            .to_str()
            .filter(|p| !p.is_empty() && !p.contains('\\'))
        else {
            return;
        };
        let execution_path = format!("outputs/{relative}");
        fields.insert(
            "task_relative_path".into(),
            Value::String(format!("executions/{execution_id}/{execution_path}")),
        );
        fields.insert(
            "execution_relative_path".into(),
            Value::String(execution_path),
        );
    }

    pub async fn upsert_artifact(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        mut record: PersistedExecutionArtifactRecord,
    ) -> Result<(), ArtifactV2Error> {
        let record_admitted = inspect_json_bounded(
            &record.payload,
            MAX_EXECUTION_ARTIFACT_INDEX_NODES.saturating_sub(11),
        )
        .is_some_and(|shape| shape.max_depth.saturating_add(3) <= MAX_RETAINED_JSON_DEPTH);
        if !record_admitted {
            discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
            return Err(ArtifactV2Error::InvalidRequest(
                "execution artifact payload exceeds the retained JSON shape limit".to_string(),
            ));
        }
        if let Err(error) = self
            .workspace
            .ensure_execution_workspace(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            )
            .await
        {
            discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
            return Err(error);
        }

        if record.source_execution_id.as_deref() == Some(execution_id) {
            self.add_execution_output_paths(scope, task_id, execution_id, &mut record.payload)
                .await;
        }

        let index_path = self.workspace.execution_artifacts_index_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        // Queue same-process writers before entering the cross-process retry
        // loop. Waiting for this guard must not spend the advisory-lock timeout:
        // a local predecessor is live progress, not an unresponsive peer.
        let _process_guard = process_artifact_index_lock(&index_path).lock_owned().await;
        // Atomic rename prevents torn readers but cannot protect a
        // read-modify-write index from lost updates. Hold the bounded advisory
        // transaction lock through the fresh read and publish; the guard uses
        // non-blocking retries and a timeout, so a dead peer cannot park the
        // runtime indefinitely.
        let _index_guard = match AgentStorage::acquire_file_lock_exclusive(&index_path).await {
            Ok(guard) => guard,
            Err(error) => {
                discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
                return Err(ArtifactV2Error::Runtime(format!(
                    "execution artifact index lock failed: {error}"
                )));
            },
        };
        let mut index = match self.load_index(scope, task_id, execution_id).await {
            Ok(index) => index,
            Err(error) => {
                discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
                return Err(error);
            },
        };
        index.execution_id = execution_id.to_string();
        index.updated_at = Utc::now().to_rfc3339();

        if let Some(existing) = index
            .artifacts
            .iter_mut()
            .find(|existing| existing.artifact_id == record.artifact_id)
        {
            let mut replaced = std::mem::replace(existing, record);
            discard_json_iteratively(std::mem::replace(&mut replaced.payload, Value::Null));
        } else {
            index.artifacts.push(record);
        }

        if !index_shape_is_admitted(&index) {
            discard_execution_artifact_index(index);
            return Err(ArtifactV2Error::InvalidRequest(
                "execution artifact index exceeds its durable admission limit".to_string(),
            ));
        }
        self.workspace
            .write_json_value_atomic_stream_path_with_cleanup(
                index_path,
                index,
                usize::try_from(MAX_EXECUTION_ARTIFACT_INDEX_BYTES).unwrap_or(usize::MAX),
                discard_execution_artifact_index_payloads,
            )
            .await
    }

    pub async fn copy_selected_artifacts(
        &self,
        source_scope: &ScopeRef,
        source_task_id: &str,
        source_execution_id: &str,
        target_scope: &ScopeRef,
        target_task_id: &str,
        target_execution_id: &str,
        artifact_ids: &[String],
    ) -> Result<(), ArtifactV2Error> {
        if artifact_ids.is_empty() {
            return Ok(());
        }
        if artifact_ids.len() > MAX_EXECUTION_ARTIFACT_RECORDS {
            return Err(ArtifactV2Error::InvalidRequest(
                "selected execution artifact count exceeds its admission limit".to_string(),
            ));
        }

        let mut requested_counts = BTreeMap::<String, usize>::new();
        for artifact_id in artifact_ids {
            let count = requested_counts.entry(artifact_id.clone()).or_default();
            *count = count.saturating_add(1);
        }
        let source = self
            .load_index(source_scope, source_task_id, source_execution_id)
            .await?;
        let mut selected =
            BTreeMap::<String, (Option<PersistedExecutionArtifactRecord>, usize)>::new();
        for mut record in source.artifacts {
            let Some(count) = requested_counts.get(&record.artifact_id).copied() else {
                discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
                continue;
            };
            if selected.contains_key(&record.artifact_id) {
                // Preserve the historical first-match behavior for a corrupt
                // legacy index containing duplicate artifact ids.
                discard_json_iteratively(std::mem::replace(&mut record.payload, Value::Null));
                continue;
            }
            selected.insert(record.artifact_id.clone(), (Some(record), count));
        }
        if let Some(missing) = artifact_ids
            .iter()
            .find(|artifact_id| !selected.contains_key(artifact_id.as_str()))
        {
            discard_selected_artifact_records(&mut selected);
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "persisted execution artifact '{}' was not found in source execution '{}'",
                missing, source_execution_id
            )));
        }

        for artifact_id in artifact_ids {
            let copied = match selected.get_mut(artifact_id) {
                Some((record, remaining)) => {
                    let copied = if *remaining > 1 {
                        record
                            .as_ref()
                            .map(clone_execution_artifact_record_stack_safe)
                    } else {
                        record.take()
                    };
                    if copied.is_some() {
                        *remaining = remaining.saturating_sub(1);
                    }
                    copied.ok_or_else(|| {
                        ArtifactV2Error::Runtime(format!(
                            "selected execution artifact '{}' was consumed before its final copy",
                            artifact_id
                        ))
                    })
                },
                None => Err(ArtifactV2Error::InvalidRequest(format!(
                    "persisted execution artifact '{}' was not found in source execution '{}'",
                    artifact_id, source_execution_id
                ))),
            };
            let copied = match copied {
                Ok(copied) => copied,
                Err(error) => {
                    discard_selected_artifact_records(&mut selected);
                    return Err(error);
                },
            };
            let copied = prepare_copied_artifact_record(copied, source_execution_id);
            if let Err(error) = self
                .upsert_artifact(target_scope, target_task_id, target_execution_id, copied)
                .await
            {
                discard_selected_artifact_records(&mut selected);
                return Err(error);
            }
        }

        discard_selected_artifact_records(&mut selected);
        Ok(())
    }

    /// Read-only admission check used before a delegated batch creates any
    /// child execution rows. Copying performs the same check again, but this
    /// preflight prevents a missing artifact on a later target from leaving
    /// earlier children behind.
    pub async fn validate_selected_artifacts(
        &self,
        source_scope: &ScopeRef,
        source_task_id: &str,
        source_execution_id: &str,
        artifact_ids: &[String],
    ) -> Result<(), ArtifactV2Error> {
        if artifact_ids.is_empty() {
            return Ok(());
        }
        if artifact_ids.len() > MAX_EXECUTION_ARTIFACT_RECORDS {
            return Err(ArtifactV2Error::InvalidRequest(
                "selected execution artifact count exceeds its admission limit".to_string(),
            ));
        }
        let source = self
            .load_index(source_scope, source_task_id, source_execution_id)
            .await?;
        for artifact_id in artifact_ids {
            if !source
                .artifacts
                .iter()
                .any(|record| record.artifact_id == *artifact_id)
            {
                discard_execution_artifact_index(source);
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "persisted execution artifact '{}' was not found in source execution '{}'",
                    artifact_id, source_execution_id
                )));
            }
        }
        discard_execution_artifact_index(source);
        Ok(())
    }
}
