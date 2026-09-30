use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct ThumbnailOp;

impl MediaOp for ThumbnailOp {
    fn name(&self) -> &'static str {
        "thumbnail"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "png"
    }

    fn needs_probe(&self) -> bool {
        true
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timestamp_secs": {"type": "number", "description": "Timestamp to grab, in seconds. Omit for 'auto' (midpoint of the clip)."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = ctx
            .inputs
            .first()
            .ok_or_else(|| ExecutionError::Step("thumbnail requires exactly one input".into()))?;

        let timestamp = match ctx.params.get("timestamp_secs").and_then(Value::as_f64) {
            Some(t) => t,
            None => {
                let duration = ctx.probed_duration_secs.ok_or_else(|| {
                    ExecutionError::Step(
                        "thumbnail: could not determine clip duration for auto timestamp".into(),
                    )
                })?;
                duration / 2.0
            },
        };
        if timestamp < 0.0 {
            return Err(ExecutionError::Step(
                "thumbnail: `timestamp_secs` must be >= 0".into(),
            ));
        }

        Ok(vec![
            "-y".to_string(),
            "-ss".to_string(),
            timestamp.to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
            ctx.output_path.display().to_string(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn ctx<'a>(
        inputs: &'a [PathBuf],
        params: &'a Value,
        output: &'a std::path::Path,
        probed: Option<f64>,
    ) -> OpContext<'a> {
        OpContext {
            inputs,
            params,
            output_path: output,
            probed_duration_secs: probed,
        }
    }

    #[test]
    fn uses_explicit_timestamp_when_given() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.png");
        let params = json!({"timestamp_secs": 3.0});
        let args = ThumbnailOp
            .build_args(&ctx(&inputs, &params, &output, None))
            .unwrap();
        assert!(args.contains(&"3".to_string()));
    }

    #[test]
    fn falls_back_to_probed_midpoint() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.png");
        let params = json!({});
        let args = ThumbnailOp
            .build_args(&ctx(&inputs, &params, &output, Some(10.0)))
            .unwrap();
        assert!(args.contains(&"5".to_string()));
    }

    #[test]
    fn errors_when_auto_requested_but_not_probed() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.png");
        let params = json!({});
        assert!(ThumbnailOp
            .build_args(&ctx(&inputs, &params, &output, None))
            .is_err());
    }
}
