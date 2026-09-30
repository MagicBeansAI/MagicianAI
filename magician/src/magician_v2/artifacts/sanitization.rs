//! # Sanitization Gateway
//!
//! Generates consumer-specific projections of artifacts. Adopts projection-based
//! exposure instead of returning raw payloads.
//!
//! Sanitization pipeline:
//! 1. **Classification**: assign sensitivity class at write time or first catalog registration.
//! 2. **Projection**: build surface-specific views (gaui, internal) with type-aware redaction.
//! 3. **Validation**: enforce max depth/size, strip secrets/path-like sensitive tokens,
//!    preserve minimal provenance.
//! 4. **Audit**: record projection policy version used for each response.
//!
//! Default contract:
//! - GAUI endpoints are read-only and sanitized by default.
//! - Raw access requires explicit privileged scope and audit event.

use super::types::{ArtifactMetadata, ArtifactProjection, ExposureClass, ProjectionSurface};
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::json_traversal::inspect_json;
use crate::magician_v2::json_traversal::{
    canonicalize_json, discard_json_iteratively, inspect_json_bounded, json_encoded_len,
    json_string_encoded_len, MAX_RETAINED_JSON_DEPTH,
};
use serde_json::Value;
use std::fmt;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during artifact sanitization and projection.
#[derive(Debug, Clone)]
pub enum SanitizationError {
    /// The artifact's exposure class does not permit the requested projection surface.
    ExposureNotAllowed {
        surface: ProjectionSurface,
        exposure_class: ExposureClass,
    },
    /// The projected content exceeds the maximum allowed size.
    ContentTooLarge { size_bytes: usize, max_bytes: usize },
    /// The content is invalid or cannot be projected.
    InvalidContent { reason: String },
}

impl fmt::Display for SanitizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SanitizationError::ExposureNotAllowed {
                surface,
                exposure_class,
            } => write!(
                f,
                "exposure not allowed: surface {:?} is not permitted for exposure class {:?}",
                surface, exposure_class
            ),
            SanitizationError::ContentTooLarge {
                size_bytes,
                max_bytes,
            } => write!(
                f,
                "content too large: {} bytes exceeds maximum of {} bytes",
                size_bytes, max_bytes
            ),
            SanitizationError::InvalidContent { reason } => {
                write!(f, "invalid content: {}", reason)
            },
        }
    }
}

impl std::error::Error for SanitizationError {}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum JSON nesting depth for projections.
const DEFAULT_MAX_CONTENT_DEPTH: usize = 10;

/// Default maximum serialized content size in bytes (256 KB).
const DEFAULT_MAX_CONTENT_SIZE_BYTES: usize = 256 * 1024;

/// Stricter maximum serialized content size for GAUI projections (128 KB).
const GAUI_MAX_CONTENT_SIZE_BYTES: usize = 128 * 1024;

/// A projection is an API payload, not an unbounded document store. Enforcing
/// this while traversing prevents a very wide value from replacing stack risk
/// with unbounded heap growth.
const DEFAULT_MAX_CONTENT_NODES: usize = 100_000;

/// Default policy version tag.
const DEFAULT_POLICY_VERSION: &str = "v1.0";

/// Sensitive key substrings. A JSON object key whose lowercased form contains
/// any of these is treated as a secret and redacted during projection.
const SECRET_PATTERNS: &[&str] = &[
    "password",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "private_key",
];

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Sanitization Gateway — builds consumer-specific projections from raw artifact
/// content and metadata.
#[derive(Debug, Clone)]
pub struct SanitizationGateway {
    /// Maximum JSON nesting depth allowed in projections.
    pub max_content_depth: usize,
    /// Maximum serialized byte size allowed per projection.
    pub max_content_size_bytes: usize,
    /// Maximum JSON values retained in one projected tree.
    pub max_content_nodes: usize,
    /// Lowercased substrings treated as secret indicators in JSON keys.
    pub secret_patterns: Vec<String>,
    /// Policy version stamped on every projection for audit replay.
    pub policy_version: String,
}

impl SanitizationGateway {
    /// Create a new gateway with sensible defaults.
    pub fn new() -> Self {
        Self {
            max_content_depth: DEFAULT_MAX_CONTENT_DEPTH,
            max_content_size_bytes: DEFAULT_MAX_CONTENT_SIZE_BYTES,
            max_content_nodes: DEFAULT_MAX_CONTENT_NODES,
            secret_patterns: SECRET_PATTERNS.iter().map(|s| s.to_string()).collect(),
            policy_version: DEFAULT_POLICY_VERSION.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Build a surface-specific projection of an artifact.
    ///
    /// Returns an `ArtifactProjection` containing the redacted/truncated content
    /// and a list of field paths that were redacted.
    pub fn project(
        &self,
        metadata: &ArtifactMetadata,
        content: &Value,
        surface: ProjectionSurface,
    ) -> Result<ArtifactProjection, SanitizationError> {
        // 1. Exposure gate — check if the artifact may be shown on this surface.
        if !self.check_exposure_allowed(metadata, surface) {
            return Err(SanitizationError::ExposureNotAllowed {
                surface,
                exposure_class: metadata.policy.exposure_class,
            });
        }

        // 2. Project content based on target surface. Sanitized surfaces use
        // one bounded iterative traversal which combines redaction and depth
        // enforcement. InternalRaw remains semantically raw, but is still
        // admitted through process-wide depth/node/byte safety contracts.
        let (projected_content, redacted_fields) =
            match surface {
                ProjectionSurface::InternalRaw => {
                    let metrics = inspect_json_bounded(content, self.max_content_nodes)
                        .ok_or_else(|| SanitizationError::InvalidContent {
                            reason: format!(
                                "content node count exceeds maximum {}",
                                self.max_content_nodes
                            ),
                        })?;
                    if metrics.max_depth > MAX_RETAINED_JSON_DEPTH {
                        return Err(SanitizationError::InvalidContent {
                            reason: format!(
                                "content depth {} exceeds retained depth {}",
                                metrics.max_depth, MAX_RETAINED_JSON_DEPTH
                            ),
                        });
                    }
                    let size = encoded_len(content)?;
                    if size > self.max_content_size_bytes {
                        return Err(SanitizationError::ContentTooLarge {
                            size_bytes: size,
                            max_bytes: self.max_content_size_bytes,
                        });
                    }
                    (canonicalize_json(content), Vec::new())
                },
                ProjectionSurface::InternalSanitized => {
                    let (sanitized, redacted) = self.sanitize_iteratively(
                        content,
                        SanitizationRules::internal_sanitized(),
                        self.max_content_depth,
                        self.max_content_size_bytes,
                    )?;
                    (
                        enforce_projected_size(sanitized, self.max_content_size_bytes)?,
                        redacted,
                    )
                },
                ProjectionSurface::Gaui => {
                    let (sanitized, redacted) = self.sanitize_iteratively(
                        content,
                        SanitizationRules::gaui(),
                        self.max_content_depth,
                        GAUI_MAX_CONTENT_SIZE_BYTES,
                    )?;
                    (
                        enforce_projected_size(sanitized, GAUI_MAX_CONTENT_SIZE_BYTES)?,
                        redacted,
                    )
                },
                ProjectionSurface::External => {
                    let (sanitized, redacted) = self.sanitize_iteratively(
                        content,
                        SanitizationRules::external(),
                        self.max_content_depth,
                        GAUI_MAX_CONTENT_SIZE_BYTES,
                    )?;
                    (
                        enforce_projected_size(sanitized, GAUI_MAX_CONTENT_SIZE_BYTES)?,
                        redacted,
                    )
                },
            };

        Ok(ArtifactProjection {
            artifact_uid: metadata.artifact_uid.clone(),
            domain: metadata.domain,
            surface,
            producer_agent_id: metadata.producer.producer_agent_id.clone(),
            produced_at: metadata.producer.produced_at,
            lifecycle_state: metadata.lifecycle_state,
            content: projected_content,
            redacted_fields,
        })
    }

    /// Check whether an artifact's exposure class allows the requested surface.
    ///
    /// The exposure class defines the *most permissive* surface the artifact may
    /// be projected to. Surfaces are ordered from most restrictive (InternalRaw)
    /// to most permissive (External). An artifact may appear on any surface that
    /// is *at least as restrictive* as its exposure class allows.
    pub fn check_exposure_allowed(
        &self,
        metadata: &ArtifactMetadata,
        surface: ProjectionSurface,
    ) -> bool {
        let exposure = metadata.policy.exposure_class;
        match exposure {
            ExposureClass::InternalRaw => matches!(surface, ProjectionSurface::InternalRaw),
            ExposureClass::InternalSanitized => matches!(
                surface,
                ProjectionSurface::InternalRaw | ProjectionSurface::InternalSanitized
            ),
            ExposureClass::GauiSanitized => matches!(
                surface,
                ProjectionSurface::InternalRaw
                    | ProjectionSurface::InternalSanitized
                    | ProjectionSurface::Gaui
            ),
            ExposureClass::ExternalRedacted => true,
        }
    }

    /// Return the current policy version (for audit stamping).
    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Stack-safe compatibility helper retained for focused policy tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    fn strip_secrets(&self, value: &Value) -> (Value, Vec<String>) {
        self.sanitize_iteratively(
            value,
            SanitizationRules::secrets_only(),
            MAX_RETAINED_JSON_DEPTH,
            usize::MAX,
        )
        .expect("bounded compatibility sanitization")
    }

    /// Check if a key name matches any secret pattern (case-insensitive contains).
    fn is_secret_key(&self, key: &str) -> bool {
        let lower = key.to_lowercase();
        self.secret_patterns.iter().any(|p| lower.contains(p))
    }

    /// Strip values that look like file system or URL paths.
    ///
    /// A string value is considered a path if it:
    /// - starts with `/` (Unix absolute path)
    /// - contains `://` (URL-like)
    /// - contains `\` (Windows-style path)
    ///
    /// Returns `(stripped_value, list_of_redacted_field_paths)`.
    #[cfg(any(test, feature = "test-fixtures"))]
    fn strip_file_paths(&self, value: &Value) -> (Value, Vec<String>) {
        self.sanitize_iteratively(
            value,
            SanitizationRules::paths_only(),
            MAX_RETAINED_JSON_DEPTH,
            usize::MAX,
        )
        .expect("bounded compatibility path sanitization")
    }

    fn sanitize_iteratively(
        &self,
        value: &Value,
        rules: SanitizationRules,
        max_depth: usize,
        max_projected_bytes: usize,
    ) -> Result<(Value, Vec<String>), SanitizationError> {
        sanitize_value_iteratively(
            value,
            self,
            rules,
            max_depth.min(MAX_RETAINED_JSON_DEPTH),
            self.max_content_nodes,
            max_projected_bytes,
        )
    }
}

impl Default for SanitizationGateway {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Iterative projection helpers
// ---------------------------------------------------------------------------

const INTERNAL_KEYS: &[&str] = &[
    "physical_locator",
    "ownership",
    "execution_id",
    "thread_id",
    "task_id",
    "workflow_instance_id",
    "run_id",
    "cycle_id",
    "transition_log",
    "references",
];

const OWNERSHIP_KEYS: &[&str] = &[
    "owner",
    "owner_id",
    "created_by",
    "producer_agent_id",
    "producer_stage",
    "agent_id",
    "producer",
];

#[derive(Clone, Copy)]
struct SanitizationRules {
    secrets: bool,
    paths: bool,
    internal_fields: bool,
    ownership_fields: bool,
}

impl SanitizationRules {
    const fn secrets_only() -> Self {
        Self {
            secrets: true,
            paths: false,
            internal_fields: false,
            ownership_fields: false,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    const fn paths_only() -> Self {
        Self {
            paths: true,
            secrets: false,
            internal_fields: false,
            ownership_fields: false,
        }
    }

    const fn internal_sanitized() -> Self {
        Self::secrets_only()
    }

    const fn gaui() -> Self {
        Self {
            secrets: true,
            paths: true,
            internal_fields: true,
            ownership_fields: false,
        }
    }

    const fn external() -> Self {
        Self {
            ownership_fields: true,
            ..Self::gaui()
        }
    }
}

enum SanitizeFrame<'a> {
    Array {
        remaining: std::iter::Enumerate<std::slice::Iter<'a, Value>>,
        output: Vec<Value>,
        path: String,
        child_depth: usize,
    },
    Object {
        remaining: serde_json::map::Iter<'a>,
        output: serde_json::Map<String, Value>,
        active_key: Option<String>,
        path: String,
        child_depth: usize,
    },
}

fn field_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn array_path(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        format!("[{index}]")
    } else {
        format!("{parent}[{index}]")
    }
}

fn depth_sentinel() -> Value {
    // This must remain a scalar. A container-shaped sentinel placed at the
    // depth boundary would itself add another retained JSON level and violate
    // `max_content_depth`, even though the source subtree was truncated.
    Value::String("[TRUNCATED: depth_limit_exceeded]".to_string())
}

fn admit_subtree_nodes(
    value: &Value,
    inspected_nodes: &mut usize,
    max_nodes: usize,
    root_already_counted: bool,
) -> Result<(), SanitizationError> {
    let already_counted = usize::from(root_already_counted);
    let remaining = max_nodes
        .saturating_sub(*inspected_nodes)
        .saturating_add(already_counted);
    let metrics = inspect_json_bounded(value, remaining).ok_or_else(|| {
        SanitizationError::InvalidContent {
            reason: format!("content node count exceeds maximum {max_nodes}"),
        }
    })?;
    *inspected_nodes =
        inspected_nodes.saturating_add(metrics.nodes.saturating_sub(already_counted));
    Ok(())
}

fn next_object_child<'a>(
    remaining: &mut serde_json::map::Iter<'a>,
    output: &mut serde_json::Map<String, Value>,
    parent_path: &str,
    gateway: &SanitizationGateway,
    rules: SanitizationRules,
    redacted: &mut Vec<String>,
    inspected_nodes: &mut usize,
    max_nodes: usize,
) -> Result<Option<(String, &'a Value, String)>, SanitizationError> {
    for (key, child) in remaining {
        let path = field_path(parent_path, key);
        let lower = key.to_lowercase();
        if rules.internal_fields && INTERNAL_KEYS.contains(&lower.as_str()) {
            admit_subtree_nodes(child, inspected_nodes, max_nodes, false)?;
            redacted.push(path);
            continue;
        }
        if rules.ownership_fields && OWNERSHIP_KEYS.contains(&lower.as_str()) {
            admit_subtree_nodes(child, inspected_nodes, max_nodes, false)?;
            redacted.push(path);
            continue;
        }
        if rules.secrets && gateway.is_secret_key(key) {
            admit_subtree_nodes(child, inspected_nodes, max_nodes, false)?;
            output.insert(key.clone(), Value::String("[REDACTED]".to_string()));
            redacted.push(path);
            continue;
        }
        if *inspected_nodes > max_nodes {
            return Err(SanitizationError::InvalidContent {
                reason: format!("content node count exceeds maximum {max_nodes}"),
            });
        }
        return Ok(Some((key.clone(), child, path)));
    }
    Ok(None)
}

fn sanitize_value_iteratively(
    root: &Value,
    gateway: &SanitizationGateway,
    rules: SanitizationRules,
    max_depth: usize,
    max_nodes: usize,
    max_projected_bytes: usize,
) -> Result<(Value, Vec<String>), SanitizationError> {
    let mut frames = Vec::<SanitizeFrame<'_>>::new();
    let mut current = root;
    let mut current_path = String::new();
    let mut current_depth = 0usize;
    let mut produced: Option<Value> = None;
    let mut redacted = Vec::new();
    let mut inspected_nodes = 0usize;
    let mut depth_truncated = false;

    if max_nodes == 0 {
        return Err(SanitizationError::InvalidContent {
            reason: "content node count exceeds maximum 0".to_string(),
        });
    }

    loop {
        if produced.is_none() {
            inspected_nodes = inspected_nodes.saturating_add(1);
            if inspected_nodes > max_nodes {
                return Err(SanitizationError::InvalidContent {
                    reason: format!("content node count exceeds maximum {max_nodes}"),
                });
            }
            if current_depth >= max_depth && matches!(current, Value::Array(_) | Value::Object(_)) {
                // The subtree is intentionally not materialized in the
                // projection, but it still belongs to the admitted input.
                // Charge all of its nodes iteratively so a huge value cannot
                // evade the node ceiling by hiding below a depth or redaction
                // boundary.
                admit_subtree_nodes(current, &mut inspected_nodes, max_nodes, true)?;
                produced = Some(depth_sentinel());
                depth_truncated = true;
            } else {
                match current {
                    Value::String(text) if rules.paths && looks_like_path(text) => {
                        redacted.push(current_path.clone());
                        produced = Some(Value::String("[PATH_REDACTED]".to_string()));
                    },
                    Value::String(text) => {
                        produced = Some(Value::String(materialize_projected_string(
                            text,
                            max_projected_bytes,
                        )?));
                    },
                    Value::Array(values) if !values.is_empty() => {
                        let mut remaining = values.iter().enumerate();
                        let (index, child) = remaining.next().expect("non-empty array");
                        let child_depth = current_depth.saturating_add(1);
                        let path = std::mem::take(&mut current_path);
                        current = child;
                        current_path = array_path(&path, index);
                        current_depth = child_depth;
                        frames.push(SanitizeFrame::Array {
                            remaining,
                            output: Vec::with_capacity(values.len().min(max_nodes)),
                            path,
                            child_depth,
                        });
                        continue;
                    },
                    Value::Object(values) if !values.is_empty() => {
                        let mut remaining = values.iter();
                        let mut output = serde_json::Map::new();
                        let path = std::mem::take(&mut current_path);
                        let child_depth = current_depth.saturating_add(1);
                        match next_object_child(
                            &mut remaining,
                            &mut output,
                            &path,
                            gateway,
                            rules,
                            &mut redacted,
                            &mut inspected_nodes,
                            max_nodes,
                        )? {
                            Some((key, child, child_path)) => {
                                current = child;
                                current_path = child_path;
                                current_depth = child_depth;
                                frames.push(SanitizeFrame::Object {
                                    remaining,
                                    output,
                                    active_key: Some(key),
                                    path,
                                    child_depth,
                                });
                                continue;
                            },
                            None => produced = Some(Value::Object(output)),
                        }
                    },
                    Value::Array(_) => produced = Some(Value::Array(Vec::new())),
                    Value::Object(_) => produced = Some(Value::Object(serde_json::Map::new())),
                    scalar => produced = Some(scalar.clone()),
                }
            }
        }

        let value = produced.take().expect("scalar or completed container");
        let Some(frame) = frames.last_mut() else {
            if depth_truncated {
                redacted.push("__depth_truncated".to_string());
            }
            return Ok((value, redacted));
        };
        match frame {
            SanitizeFrame::Array {
                remaining,
                output,
                path,
                child_depth,
            } => {
                output.push(value);
                if let Some((index, child)) = remaining.next() {
                    current = child;
                    current_path = array_path(path, index);
                    current_depth = *child_depth;
                } else {
                    let output = std::mem::take(output);
                    frames.pop();
                    produced = Some(Value::Array(output));
                }
            },
            SanitizeFrame::Object {
                remaining,
                output,
                active_key,
                path,
                child_depth,
            } => {
                output.insert(
                    active_key.take().expect("object child has an active key"),
                    value,
                );
                match next_object_child(
                    remaining,
                    output,
                    path,
                    gateway,
                    rules,
                    &mut redacted,
                    &mut inspected_nodes,
                    max_nodes,
                )? {
                    Some((key, child, child_path)) => {
                        *active_key = Some(key);
                        current = child;
                        current_path = child_path;
                        current_depth = *child_depth;
                    },
                    None => {
                        let output = std::mem::take(output);
                        frames.pop();
                        produced = Some(Value::Object(output));
                    },
                }
            },
        }
    }
}

fn looks_like_path(text: &str) -> bool {
    text.starts_with('/') || text.contains("://") || text.contains('\\')
}

#[cfg(any(test, feature = "test-fixtures"))]
thread_local! {
    static MAX_PROJECTED_STRING_MATERIALIZATION_BYTES: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

fn materialize_projected_string(
    text: &str,
    max_projected_bytes: usize,
) -> Result<String, SanitizationError> {
    let encoded_bytes =
        json_string_encoded_len(text).map_err(|error| SanitizationError::InvalidContent {
            reason: format!("failed to measure projected string: {error}"),
        })?;
    let projected = if text.len() > 1_024 && encoded_bytes > max_projected_bytes {
        truncate_borrowed_string_safely(text)
    } else {
        text.to_string()
    };
    #[cfg(any(test, feature = "test-fixtures"))]
    MAX_PROJECTED_STRING_MATERIALIZATION_BYTES.with(|maximum| {
        maximum.set(maximum.get().max(projected.len()));
    });
    Ok(projected)
}

fn encoded_len(value: &Value) -> Result<usize, SanitizationError> {
    json_encoded_len(value).map_err(|error| SanitizationError::InvalidContent {
        reason: format!("failed to measure serialized content: {error}"),
    })
}

enum ShrinkFrame {
    Array {
        remaining: std::vec::IntoIter<Value>,
        output: Vec<Value>,
        original_len: usize,
        retained_len: usize,
    },
    Object {
        remaining: serde_json::map::IntoIter,
        output: serde_json::Map<String, Value>,
        active_key: Option<String>,
    },
}

fn truncate_borrowed_string_safely(text: &str) -> String {
    if text.len() <= 1024 {
        return text.to_string();
    }
    let original_len = text.len();
    let mut boundary = 1024;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!(
        "{}... [truncated, {original_len} bytes total]",
        &text[..boundary],
    )
}

fn truncate_string_safely(text: String) -> String {
    if text.len() <= 1024 {
        text
    } else {
        truncate_borrowed_string_safely(&text)
    }
}

fn array_truncation_sentinel(original_len: usize) -> Value {
    let mut sentinel = serde_json::Map::new();
    sentinel.insert("__truncated".to_string(), Value::Bool(true));
    sentinel.insert(
        "__reason".to_string(),
        Value::String(format!(
            "array_truncated_from_{original_len}_to_10_elements"
        )),
    );
    Value::Object(sentinel)
}

fn discard_remaining(values: std::vec::IntoIter<Value>) {
    for value in values {
        discard_json_iteratively(value);
    }
}

fn shrink_value_owned(root: Value) -> Value {
    let mut frames = Vec::<ShrinkFrame>::new();
    let mut current = root;
    let mut produced: Option<Value> = None;

    loop {
        if produced.is_none() {
            let value = std::mem::replace(&mut current, Value::Null);
            match value {
                Value::String(text) => produced = Some(Value::String(truncate_string_safely(text))),
                Value::Array(values) if !values.is_empty() => {
                    let original_len = values.len();
                    let retained_len = original_len.min(10);
                    let mut remaining = values.into_iter();
                    current = remaining.next().expect("non-empty array");
                    frames.push(ShrinkFrame::Array {
                        remaining,
                        output: Vec::with_capacity(retained_len.saturating_add(1)),
                        original_len,
                        retained_len,
                    });
                    continue;
                },
                Value::Object(values) if !values.is_empty() => {
                    let mut remaining = values.into_iter();
                    let (key, child) = remaining.next().expect("non-empty object");
                    current = child;
                    frames.push(ShrinkFrame::Object {
                        remaining,
                        output: serde_json::Map::new(),
                        active_key: Some(key),
                    });
                    continue;
                },
                scalar => produced = Some(scalar),
            }
        }

        let value = produced.take().expect("scalar or completed container");
        let Some(frame) = frames.last_mut() else {
            return value;
        };
        match frame {
            ShrinkFrame::Array {
                remaining,
                output,
                original_len,
                retained_len,
            } => {
                output.push(value);
                if output.len() < *retained_len {
                    current = remaining.next().expect("retained array child");
                } else {
                    discard_remaining(std::mem::replace(remaining, Vec::new().into_iter()));
                    if *original_len > *retained_len {
                        output.push(array_truncation_sentinel(*original_len));
                    }
                    let output = std::mem::take(output);
                    frames.pop();
                    produced = Some(Value::Array(output));
                }
            },
            ShrinkFrame::Object {
                remaining,
                output,
                active_key,
            } => {
                output.insert(
                    active_key.take().expect("object child has an active key"),
                    value,
                );
                if let Some((key, child)) = remaining.next() {
                    *active_key = Some(key);
                    current = child;
                } else {
                    let output = std::mem::take(output);
                    frames.pop();
                    produced = Some(Value::Object(output));
                }
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn shrink_value(value: &Value, _max_bytes: usize) -> Value {
    shrink_value_owned(canonicalize_json(value))
}

fn enforce_projected_size(value: Value, max_bytes: usize) -> Result<Value, SanitizationError> {
    let size = encoded_len(&value)?;
    if size <= max_bytes {
        return Ok(value);
    }
    let shrunk = shrink_value_owned(value);
    let shrunk_size = encoded_len(&shrunk)?;
    if shrunk_size <= max_bytes {
        Ok(shrunk)
    } else {
        Err(SanitizationError::ContentTooLarge {
            size_bytes: shrunk_size,
            max_bytes,
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::types::{
        ArtifactDomain, FreshnessClass, LifecycleState, OwnershipScope, PhysicalLocator,
        PolicyBindings, ProducerInfo, ProtectionFlags, RetentionClass,
    };
    use chrono::Utc;
    use serde_json::json;
    use std::collections::VecDeque;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build a minimal `ArtifactMetadata` with the given exposure class.
    fn make_metadata(exposure: ExposureClass) -> ArtifactMetadata {
        ArtifactMetadata {
            artifact_uid: "test-uid-001".to_string(),
            domain: ArtifactDomain::Pipeline,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: "test-key".to_string(),
            },
            route_target: None,
            ownership: OwnershipScope {
                execution_id: Some("exec-42".to_string()),
                task_id: Some("task-7".to_string()),
                workflow_instance_id: Some("wf-99".to_string()),
                run_id: Some("run-1".to_string()),
                cycle_id: None,
            },
            producer: ProducerInfo {
                producer_agent_id: "agent-alpha".to_string(),
                producer_stage: Some("extract".to_string()),
                produced_at: Utc::now(),
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::GoalLifetime,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: exposure,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        }
    }

    // -----------------------------------------------------------------------
    // InternalRaw passes through content unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn internal_raw_passes_content_unchanged() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "password": "hunter2",
            "data": {"nested_secret": "foo"},
            "path": "/etc/passwd"
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalRaw)
            .unwrap();

        assert_eq!(projection.content, content);
        assert!(projection.redacted_fields.is_empty());
        assert_eq!(projection.surface, ProjectionSurface::InternalRaw);
        assert_eq!(projection.artifact_uid, "test-uid-001");
    }

    // -----------------------------------------------------------------------
    // InternalSanitized strips secrets
    // -----------------------------------------------------------------------

    #[test]
    fn internal_sanitized_strips_secrets() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "title": "Hello",
            "password": "hunter2",
            "config": {
                "api_key": "sk-12345",
                "timeout": 30
            }
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalSanitized)
            .unwrap();

        assert_eq!(projection.content["title"], "Hello");
        assert_eq!(projection.content["password"], "[REDACTED]");
        assert_eq!(projection.content["config"]["api_key"], "[REDACTED]");
        assert_eq!(projection.content["config"]["timeout"], 30);
        assert!(projection.redacted_fields.contains(&"password".to_string()));
        assert!(projection
            .redacted_fields
            .contains(&"config.api_key".to_string()));
    }

    // -----------------------------------------------------------------------
    // Gaui strips secrets + file paths + enforces smaller size limit
    // -----------------------------------------------------------------------

    #[test]
    fn gaui_strips_secrets_and_paths() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "title": "Report",
            "authorization": "Bearer xyz",
            "source": "/home/user/data.csv",
            "url": "https://example.com/api",
            "physical_locator": {"type": "file", "path": "/tmp/artifact.bin"},
            "ownership": {"execution_id": "t-1"},
            "plain_value": 42
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::Gaui)
            .unwrap();

        // Secrets redacted
        assert_eq!(projection.content["authorization"], "[REDACTED]");
        // File paths redacted
        assert_eq!(projection.content["source"], "[PATH_REDACTED]");
        assert_eq!(projection.content["url"], "[PATH_REDACTED]");
        // Internal fields removed entirely
        assert!(projection.content.get("physical_locator").is_none());
        assert!(projection.content.get("ownership").is_none());
        // Plain values preserved
        assert_eq!(projection.content["plain_value"], 42);
        assert_eq!(projection.content["title"], "Report");
    }

    // -----------------------------------------------------------------------
    // External strips everything aggressively
    // -----------------------------------------------------------------------

    #[test]
    fn external_strips_aggressively() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "title": "Public Data",
            "credential": "abc123",
            "producer_agent_id": "agent-alpha",
            "producer": {"stage": "extract"},
            "owner_id": "user-1",
            "physical_locator": "/tmp/data",
            "ownership": {"execution_id": "t-1"},
            "file": "/var/log/app.log",
            "count": 7
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::External)
            .unwrap();

        assert_eq!(projection.content["title"], "Public Data");
        assert_eq!(projection.content["credential"], "[REDACTED]");
        assert_eq!(projection.content["count"], 7);
        // Ownership/producer fields removed
        assert!(projection.content.get("producer_agent_id").is_none());
        assert!(projection.content.get("producer").is_none());
        assert!(projection.content.get("owner_id").is_none());
        assert!(projection.content.get("physical_locator").is_none());
        assert!(projection.content.get("ownership").is_none());
        // File path redacted
        assert_eq!(projection.content["file"], "[PATH_REDACTED]");
    }

    // -----------------------------------------------------------------------
    // Exposure class checks
    // -----------------------------------------------------------------------

    #[test]
    fn exposure_internal_raw_only_allows_internal_raw_surface() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::InternalRaw);

        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalRaw));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::InternalSanitized));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::Gaui));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::External));
    }

    #[test]
    fn exposure_internal_sanitized_allows_raw_and_sanitized() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::InternalSanitized);

        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalRaw));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalSanitized));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::Gaui));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::External));
    }

    #[test]
    fn exposure_gaui_sanitized_allows_raw_sanitized_gaui() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::GauiSanitized);

        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalRaw));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalSanitized));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::Gaui));
        assert!(!gw.check_exposure_allowed(&meta, ProjectionSurface::External));
    }

    #[test]
    fn exposure_external_redacted_allows_all_surfaces() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);

        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalRaw));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::InternalSanitized));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::Gaui));
        assert!(gw.check_exposure_allowed(&meta, ProjectionSurface::External));
    }

    #[test]
    fn exposure_not_allowed_returns_error() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::InternalRaw);
        let content = json!({"data": 1});

        let result = gw.project(&meta, &content, ProjectionSurface::Gaui);
        assert!(result.is_err());
        match result.unwrap_err() {
            SanitizationError::ExposureNotAllowed {
                surface,
                exposure_class,
            } => {
                assert_eq!(surface, ProjectionSurface::Gaui);
                assert_eq!(exposure_class, ExposureClass::InternalRaw);
            },
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Depth limiting
    // -----------------------------------------------------------------------

    #[test]
    fn depth_limiting_truncates_deep_nesting() {
        let gw = SanitizationGateway {
            max_content_depth: 2,
            ..SanitizationGateway::new()
        };
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        // Depth counting: root object is depth 0, its immediate children
        // values are at depth 1, and their children at depth 2.
        // With max_content_depth=2, objects/arrays at depth 2 get truncated.
        //
        // root (depth 0) -> "a" value (depth 1) -> "b" value (depth 2, truncated)
        let content = json!({
            "a": {
                "b": {
                    "c": {
                        "d": "deep"
                    }
                }
            }
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalSanitized)
            .unwrap();

        // "a" should exist (its value is at depth 1, fine)
        let a = projection.content.get("a").unwrap();
        // "a.b" should be the truncation sentinel because the value of key "b"
        // is an object at depth 2 which equals max_depth.
        let b = a.get("b").unwrap();
        assert_eq!(b, "[TRUNCATED: depth_limit_exceeded]");
        assert!(inspect_json(&projection.content).max_depth <= gw.max_content_depth);
    }

    #[test]
    fn depth_limiting_preserves_scalars_at_boundary() {
        let gw = SanitizationGateway {
            max_content_depth: 1,
            ..SanitizationGateway::new()
        };
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "name": "test",
            "count": 42,
            "nested": {"key": "val"}
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalSanitized)
            .unwrap();

        // Scalars at depth 1 are fine
        assert_eq!(projection.content["name"], "test");
        assert_eq!(projection.content["count"], 42);
        // Object at depth 1 gets truncated
        let nested = projection.content.get("nested").unwrap();
        assert_eq!(nested, "[TRUNCATED: depth_limit_exceeded]");
        assert!(inspect_json(&projection.content).max_depth <= gw.max_content_depth);
    }

    // -----------------------------------------------------------------------
    // Size limiting
    // -----------------------------------------------------------------------

    #[test]
    fn size_limiting_rejects_oversized_content() {
        let gw = SanitizationGateway {
            max_content_size_bytes: 50,
            ..SanitizationGateway::new()
        };
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        // Build content that is definitely > 50 bytes even after shrinking.
        let content = json!({
            "a": "x".repeat(100),
            "b": "y".repeat(100),
            "c": "z".repeat(100),
        });

        let result = gw.project(&meta, &content, ProjectionSurface::InternalRaw);
        assert!(result.is_err());
        match result.unwrap_err() {
            SanitizationError::ContentTooLarge {
                size_bytes: _,
                max_bytes,
            } => {
                assert_eq!(max_bytes, 50);
            },
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn size_limiting_allows_content_under_limit() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({"small": "data"});

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalRaw)
            .unwrap();
        assert_eq!(projection.content["small"], "data");
    }

    #[test]
    fn oversized_safe_string_is_truncated_before_projection_materialization() {
        let gateway = SanitizationGateway {
            max_content_size_bytes: 2_048,
            ..SanitizationGateway::new()
        };
        let metadata = make_metadata(ExposureClass::ExternalRedacted);
        let original = "x".repeat(4 * 1024 * 1024);
        let content = json!({"safe": original});
        MAX_PROJECTED_STRING_MATERIALIZATION_BYTES.with(|maximum| maximum.set(0));

        let projection = gateway
            .project(&metadata, &content, ProjectionSurface::InternalSanitized)
            .expect("the established shrink representation fits the projection budget");
        let projected = projection.content["safe"]
            .as_str()
            .expect("projected safe string");
        assert_eq!(
            projected,
            truncate_borrowed_string_safely(content["safe"].as_str().expect("source string")),
        );
        MAX_PROJECTED_STRING_MATERIALIZATION_BYTES.with(|maximum| {
            assert_eq!(maximum.get(), projected.len());
            assert!(maximum.get() < 2_048);
        });
    }

    #[test]
    fn gaui_enforces_128kb_size_limit() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        // Build content with many 1000-char string fields (under shrink
        // threshold of 1024), each about 1KB. 200 fields ~= 200KB which
        // exceeds the 128KB GAUI limit and cannot be shrunk since each
        // individual string is under the 1024-char truncation threshold.
        let mut map = serde_json::Map::new();
        for i in 0..200 {
            map.insert(format!("field_{}", i), json!("x".repeat(1000)));
        }
        let content = Value::Object(map);

        // InternalRaw should succeed (under 256KB global limit).
        let raw_result = gw.project(&meta, &content, ProjectionSurface::InternalRaw);
        assert!(raw_result.is_ok());

        // GAUI should fail because the aggregate size exceeds 128KB and
        // individual strings are under the shrink threshold.
        let gaui_result = gw.project(&meta, &content, ProjectionSurface::Gaui);
        assert!(gaui_result.is_err());
        match gaui_result.unwrap_err() {
            SanitizationError::ContentTooLarge { max_bytes, .. } => {
                assert_eq!(max_bytes, 128 * 1024);
            },
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Secret stripping with nested objects
    // -----------------------------------------------------------------------

    #[test]
    fn secret_stripping_handles_nested_objects() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "level1": {
                "level2": {
                    "API_KEY": "sk-secret",
                    "safe": "visible"
                },
                "Password": "abc"
            }
        });

        let (sanitized, redacted) = gw.strip_secrets(&content);

        assert_eq!(sanitized["level1"]["level2"]["API_KEY"], "[REDACTED]");
        assert_eq!(sanitized["level1"]["level2"]["safe"], "visible");
        assert_eq!(sanitized["level1"]["Password"], "[REDACTED]");
        assert!(redacted.contains(&"level1.level2.API_KEY".to_string()));
        assert!(redacted.contains(&"level1.Password".to_string()));
        assert_eq!(redacted.len(), 2);
    }

    #[test]
    fn secret_stripping_handles_arrays_with_objects() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "items": [
                {"name": "a", "token": "t1"},
                {"name": "b", "authorization": "Bearer abc"}
            ]
        });

        let (sanitized, redacted) = gw.strip_secrets(&content);

        assert_eq!(sanitized["items"][0]["name"], "a");
        assert_eq!(sanitized["items"][0]["token"], "[REDACTED]");
        assert_eq!(sanitized["items"][1]["authorization"], "[REDACTED]");
        assert!(redacted.contains(&"items[0].token".to_string()));
        assert!(redacted.contains(&"items[1].authorization".to_string()));
    }

    #[test]
    fn secret_stripping_is_case_insensitive() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "PASSWORD": "a",
            "Secret": "b",
            "API_KEY": "c",
            "Private_Key": "d"
        });

        let (sanitized, redacted) = gw.strip_secrets(&content);

        assert_eq!(sanitized["PASSWORD"], "[REDACTED]");
        assert_eq!(sanitized["Secret"], "[REDACTED]");
        assert_eq!(sanitized["API_KEY"], "[REDACTED]");
        assert_eq!(sanitized["Private_Key"], "[REDACTED]");
        assert_eq!(redacted.len(), 4);
    }

    // -----------------------------------------------------------------------
    // File path stripping
    // -----------------------------------------------------------------------

    #[test]
    fn file_path_stripping_detects_unix_paths() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "config_file": "/etc/app/config.yaml",
            "name": "safe_value"
        });

        let (stripped, redacted) = gw.strip_file_paths(&content);

        assert_eq!(stripped["config_file"], "[PATH_REDACTED]");
        assert_eq!(stripped["name"], "safe_value");
        assert!(redacted.contains(&"config_file".to_string()));
    }

    #[test]
    fn file_path_stripping_detects_urls() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "endpoint": "https://internal.example.com/api",
            "count": 5
        });

        let (stripped, redacted) = gw.strip_file_paths(&content);

        assert_eq!(stripped["endpoint"], "[PATH_REDACTED]");
        assert_eq!(stripped["count"], 5);
        assert!(redacted.contains(&"endpoint".to_string()));
    }

    #[test]
    fn file_path_stripping_detects_windows_paths() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "log_dir": "C:\\Users\\admin\\logs",
            "label": "ok"
        });

        let (stripped, redacted) = gw.strip_file_paths(&content);

        assert_eq!(stripped["log_dir"], "[PATH_REDACTED]");
        assert_eq!(stripped["label"], "ok");
        assert!(redacted.contains(&"log_dir".to_string()));
    }

    #[test]
    fn file_path_stripping_handles_nested_paths() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "config": {
                "input": "/data/input.csv",
                "output": "/data/output.csv"
            },
            "items": ["/tmp/a", "safe"]
        });

        let (stripped, redacted) = gw.strip_file_paths(&content);

        assert_eq!(stripped["config"]["input"], "[PATH_REDACTED]");
        assert_eq!(stripped["config"]["output"], "[PATH_REDACTED]");
        assert_eq!(stripped["items"][0], "[PATH_REDACTED]");
        assert_eq!(stripped["items"][1], "safe");
        assert_eq!(redacted.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Redacted fields tracking
    // -----------------------------------------------------------------------

    #[test]
    fn redacted_fields_are_tracked_across_operations() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "password": "secret",
            "path_field": "/usr/local/bin",
            "physical_locator": {"type": "file"},
            "safe_data": "visible"
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::Gaui)
            .unwrap();

        // Should have redacted entries for password, path, and internal field
        assert!(projection.redacted_fields.contains(&"password".to_string()));
        assert!(!projection.redacted_fields.is_empty());
        // safe_data should survive
        assert_eq!(projection.content["safe_data"], "visible");
    }

    // -----------------------------------------------------------------------
    // Projection metadata correctness
    // -----------------------------------------------------------------------

    #[test]
    fn projection_carries_correct_metadata() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({"x": 1});

        let projection = gw
            .project(&meta, &content, ProjectionSurface::Gaui)
            .unwrap();

        assert_eq!(projection.artifact_uid, "test-uid-001");
        assert_eq!(projection.domain, ArtifactDomain::Pipeline);
        assert_eq!(projection.surface, ProjectionSurface::Gaui);
        assert_eq!(projection.producer_agent_id, "agent-alpha");
        assert_eq!(projection.lifecycle_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Default constructor
    // -----------------------------------------------------------------------

    #[test]
    fn default_constructor_has_expected_values() {
        let gw = SanitizationGateway::new();
        assert_eq!(gw.max_content_depth, 10);
        assert_eq!(gw.max_content_size_bytes, 256 * 1024);
        assert_eq!(gw.policy_version, "v1.0");
        assert_eq!(gw.secret_patterns.len(), 8);
        assert!(gw.secret_patterns.contains(&"password".to_string()));
        assert!(gw.secret_patterns.contains(&"secret".to_string()));
        assert!(gw.secret_patterns.contains(&"token".to_string()));
        assert!(gw.secret_patterns.contains(&"api_key".to_string()));
        assert!(gw.secret_patterns.contains(&"apikey".to_string()));
        assert!(gw.secret_patterns.contains(&"authorization".to_string()));
        assert!(gw.secret_patterns.contains(&"credential".to_string()));
        assert!(gw.secret_patterns.contains(&"private_key".to_string()));
    }

    #[test]
    fn default_trait_matches_new() {
        let from_new = SanitizationGateway::new();
        let from_default = SanitizationGateway::default();
        assert_eq!(from_new.max_content_depth, from_default.max_content_depth);
        assert_eq!(
            from_new.max_content_size_bytes,
            from_default.max_content_size_bytes
        );
        assert_eq!(from_new.policy_version, from_default.policy_version);
        assert_eq!(from_new.secret_patterns, from_default.secret_patterns);
    }

    // -----------------------------------------------------------------------
    // Error Display implementations
    // -----------------------------------------------------------------------

    #[test]
    fn error_display_exposure_not_allowed() {
        let err = SanitizationError::ExposureNotAllowed {
            surface: ProjectionSurface::Gaui,
            exposure_class: ExposureClass::InternalRaw,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("exposure not allowed"));
        assert!(msg.contains("Gaui"));
        assert!(msg.contains("InternalRaw"));
    }

    #[test]
    fn error_display_content_too_large() {
        let err = SanitizationError::ContentTooLarge {
            size_bytes: 500_000,
            max_bytes: 256_000,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("content too large"));
        assert!(msg.contains("500000"));
        assert!(msg.contains("256000"));
    }

    #[test]
    fn error_display_invalid_content() {
        let err = SanitizationError::InvalidContent {
            reason: "bad json".to_string(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("invalid content"));
        assert!(msg.contains("bad json"));
    }

    #[test]
    fn error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(SanitizationError::InvalidContent {
            reason: "test".to_string(),
        });
        assert!(err.to_string().contains("invalid content"));
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn null_and_bool_values_pass_through() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "flag": true,
            "nothing": null,
            "count": 0
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalSanitized)
            .unwrap();

        assert_eq!(projection.content["flag"], true);
        assert!(projection.content["nothing"].is_null());
        assert_eq!(projection.content["count"], 0);
    }

    #[test]
    fn empty_object_projects_cleanly() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({});

        for surface in &[
            ProjectionSurface::InternalRaw,
            ProjectionSurface::InternalSanitized,
            ProjectionSurface::Gaui,
            ProjectionSurface::External,
        ] {
            let projection = gw.project(&meta, &content, *surface).unwrap();
            assert_eq!(projection.content, json!({}));
        }
    }

    #[test]
    fn array_at_root_level_projects() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!([1, 2, 3]);

        let projection = gw
            .project(&meta, &content, ProjectionSurface::InternalSanitized)
            .unwrap();
        assert_eq!(projection.content, json!([1, 2, 3]));
    }

    #[test]
    fn deeply_nested_secret_is_redacted() {
        let gw = SanitizationGateway::new();
        let content = json!({
            "a": {
                "b": {
                    "c": {
                        "d": {
                            "secret_value": "hidden"
                        }
                    }
                }
            }
        });

        let (sanitized, redacted) = gw.strip_secrets(&content);
        assert_eq!(sanitized["a"]["b"]["c"]["d"]["secret_value"], "[REDACTED]");
        assert!(redacted.contains(&"a.b.c.d.secret_value".to_string()));
    }

    #[test]
    fn shrink_truncates_large_arrays() {
        let large_array: Vec<Value> = (0..100).map(|i| json!(i)).collect();
        let content = json!({"data": large_array});

        let shrunk = shrink_value(&content, 1024);
        let arr = shrunk["data"].as_array().unwrap();
        // 10 elements + 1 truncation sentinel
        assert_eq!(arr.len(), 11);
        let sentinel = arr.last().unwrap();
        assert_eq!(sentinel["__truncated"], true);
    }

    #[test]
    fn internal_fields_are_stripped_for_gaui() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "result": "data",
            "execution_id": "exec-42",
            "task_id": "task-7",
            "run_id": "run-1",
            "cycle_id": "cycle-0",
            "references": [{"id": "ref-1"}],
            "transition_log": [{"from": "active"}]
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::Gaui)
            .unwrap();

        assert_eq!(projection.content["result"], "data");
        assert!(projection.content.get("execution_id").is_none());
        assert!(projection.content.get("task_id").is_none());
        assert!(projection.content.get("run_id").is_none());
        assert!(projection.content.get("cycle_id").is_none());
        assert!(projection.content.get("references").is_none());
        assert!(projection.content.get("transition_log").is_none());
    }

    #[test]
    fn external_strips_ownership_fields() {
        let gw = SanitizationGateway::new();
        let meta = make_metadata(ExposureClass::ExternalRedacted);
        let content = json!({
            "result": "ok",
            "owner": "user-1",
            "owner_id": "uid-1",
            "created_by": "admin",
            "producer_agent_id": "agent-1",
            "producer_stage": "extract",
            "agent_id": "a-1",
            "producer": {"name": "p"}
        });

        let projection = gw
            .project(&meta, &content, ProjectionSurface::External)
            .unwrap();

        assert_eq!(projection.content["result"], "ok");
        assert!(projection.content.get("owner").is_none());
        assert!(projection.content.get("owner_id").is_none());
        assert!(projection.content.get("created_by").is_none());
        assert!(projection.content.get("producer_agent_id").is_none());
        assert!(projection.content.get("producer_stage").is_none());
        assert!(projection.content.get("agent_id").is_none());
        assert!(projection.content.get("producer").is_none());
    }

    #[test]
    fn policy_version_accessor_returns_configured_version() {
        let gw = SanitizationGateway::new();
        assert_eq!(gw.policy_version(), "v1.0");

        let custom = SanitizationGateway {
            policy_version: "v2.3-beta".to_string(),
            ..SanitizationGateway::new()
        };
        assert_eq!(custom.policy_version(), "v2.3-beta");
    }

    #[test]
    fn iterative_projection_bounds_adversarial_depth_on_a_small_stack() {
        std::thread::Builder::new()
            .name("sanitization-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut content = Value::String("safe".to_string());
                for _ in 0..10_000 {
                    content = Value::Array(vec![content]);
                }
                let gateway = SanitizationGateway::new();
                let metadata = make_metadata(ExposureClass::ExternalRedacted);
                let projection = gateway
                    .project(&metadata, &content, ProjectionSurface::External)
                    .expect("deep input is bounded iteratively");
                let projected_depth = inspect_json(&projection.content).max_depth;
                discard_json_iteratively(content);
                assert!(projected_depth <= gateway.max_content_depth);
            })
            .expect("small-stack sanitization thread")
            .join()
            .expect("iterative sanitization must not overflow");
    }

    #[test]
    fn node_admission_limit_fails_before_building_an_unbounded_projection() {
        let gateway = SanitizationGateway {
            max_content_nodes: 4,
            ..SanitizationGateway::new()
        };
        let metadata = make_metadata(ExposureClass::ExternalRedacted);
        let error = gateway
            .project(
                &metadata,
                &json!({"a": 1, "b": 2, "c": 3, "d": 4}),
                ProjectionSurface::External,
            )
            .expect_err("root plus four values exceeds a four-node limit");
        assert!(matches!(error, SanitizationError::InvalidContent { .. }));
    }

    #[test]
    fn redacted_and_depth_truncated_subtrees_cannot_evade_node_admission() {
        let gateway = SanitizationGateway {
            max_content_depth: 2,
            max_content_nodes: 8,
            ..SanitizationGateway::new()
        };
        let metadata = make_metadata(ExposureClass::ExternalRedacted);

        for content in [
            json!({"password": [0, 1, 2, 3, 4, 5, 6, 7]}),
            json!({"visible": {"nested": [0, 1, 2, 3, 4, 5, 6, 7]}}),
        ] {
            let error = gateway
                .project(&metadata, &content, ProjectionSurface::External)
                .expect_err("discarded input nodes still consume the admission budget");
            assert!(matches!(error, SanitizationError::InvalidContent { .. }));
        }
    }

    #[test]
    fn size_shrinking_never_slices_through_utf8() {
        let content = json!({"text": "🧙🏽‍♀️".repeat(300)});
        let shrunk = shrink_value(&content, 1024);
        let text = shrunk["text"].as_str().expect("shrunk string");
        assert!(text.contains("[truncated,"));
        assert!(serde_json::to_string(&shrunk).is_ok());
    }

    #[test]
    fn internal_raw_rejects_retained_depth_beyond_the_process_contract() {
        let mut content = Value::Null;
        for _ in 0..(MAX_RETAINED_JSON_DEPTH + 1) {
            content = Value::Array(vec![content]);
        }
        let gateway = SanitizationGateway::new();
        let metadata = make_metadata(ExposureClass::ExternalRedacted);
        let result = gateway.project(&metadata, &content, ProjectionSurface::InternalRaw);
        assert!(matches!(
            result,
            Err(SanitizationError::InvalidContent { .. })
        ));
        discard_json_iteratively(content);
    }
}
