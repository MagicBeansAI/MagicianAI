use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::magician_v2::execution::error::ExecutionError;

mod concat;
mod convert;
mod merge;
mod overlay_text;
mod separate;
mod speed;
mod still_to_video;
mod thumbnail;
mod trim;

/// Everything an op needs to build its ffmpeg invocation. Inputs are already
/// resolved to absolute, sandbox-checked paths by the time an op sees them.
pub struct OpContext<'a> {
    pub inputs: &'a [PathBuf],
    pub params: &'a Value,
    pub output_path: &'a Path,
    /// Populated by the job runner only when `needs_probe()` is true.
    pub probed_duration_secs: Option<f64>,
}

pub trait MediaOp: Send + Sync {
    /// Not read by dispatch (lookup is by registry key) — exists so the
    /// registry-consistency test can assert a registered key matches what
    /// the op itself claims, catching a copy-paste mismatch at test time.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn output_ext(&self, params: &Value) -> &'static str;
    /// True if this op needs the first input's duration probed before
    /// `build_args` runs (e.g. "auto" thumbnail timestamp = midpoint).
    fn needs_probe(&self) -> bool {
        false
    }
    fn requires_probe(&self, _params: &Value) -> bool {
        self.needs_probe()
    }
    /// The canonical parameter names are checked before dispatch. The full
    /// operation schema accompanies a shape error so callers can repair it.
    fn param_schema(&self) -> Value;
    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError>;
    fn build_bounded_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        self.build_args(ctx)
    }
}

pub fn validate_params(op: &dyn MediaOp, params: &Value) -> Result<(), ExecutionError> {
    let schema = op.param_schema();
    let properties = schema["properties"].as_object().ok_or_else(|| {
        ExecutionError::Step(format!(
            "media_edit {} has no parameter contract",
            op.name()
        ))
    })?;
    let object = params.as_object().ok_or_else(|| {
        ExecutionError::Step(format!(
            "media_edit {}: params must be an object; expected schema: {schema}",
            op.name()
        ))
    })?;
    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(ExecutionError::Step(format!(
                "media_edit {}: unsupported parameter `{key}`; expected schema: {schema}",
                op.name()
            )));
        }
    }
    Ok(())
}

pub fn registry() -> HashMap<&'static str, Box<dyn MediaOp>> {
    let mut ops: HashMap<&'static str, Box<dyn MediaOp>> = HashMap::new();
    ops.insert("trim", Box::new(trim::TrimOp));
    ops.insert("speed", Box::new(speed::SpeedOp));
    ops.insert("extract_audio", Box::new(separate::ExtractAudioOp));
    ops.insert("mute_video", Box::new(separate::MuteVideoOp));
    ops.insert("convert", Box::new(convert::ConvertOp));
    ops.insert("still_to_video", Box::new(still_to_video::StillToVideoOp));
    ops.insert("merge", Box::new(merge::MergeOp));
    ops.insert("concat", Box::new(concat::ConcatOp));
    ops.insert("overlay_text", Box::new(overlay_text::OverlayTextOp));
    ops.insert("thumbnail", Box::new(thumbnail::ThumbnailOp));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_op_has_a_schema_with_required_fields() {
        for (name, op) in registry() {
            assert_eq!(op.name(), name, "registry key must match op.name()");
            let schema = op.param_schema();
            assert_eq!(
                schema.get("type").and_then(Value::as_str),
                Some("object"),
                "op `{name}` schema must be a JSON object schema"
            );
            assert!(
                schema.get("properties").is_some(),
                "op `{name}` schema must declare `properties`"
            );
        }
        assert!(
            !registry().is_empty(),
            "op registry should not be empty once ops are registered"
        );
    }

    #[test]
    fn operation_contracts_reject_ignored_parameters_and_expose_repair_schema() {
        for (_, op) in registry() {
            let error =
                validate_params(op.as_ref(), &serde_json::json!({"not_a_parameter": false}))
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("not_a_parameter"));
            assert!(error.contains(&op.param_schema().to_string()));
            assert!(validate_params(op.as_ref(), &Value::Null).is_err());
        }
        let op = concat::ConcatOp;
        let error = validate_params(&op, &serde_json::json!({"audio": false}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("include_audio"));
        assert!(validate_params(&op, &serde_json::json!({"include_audio": false})).is_ok());
    }
}
