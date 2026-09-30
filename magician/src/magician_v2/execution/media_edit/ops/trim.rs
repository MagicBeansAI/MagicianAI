use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct TrimOp;

impl MediaOp for TrimOp {
    fn name(&self) -> &'static str {
        "trim"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["start_secs", "end_secs"],
            "properties": {
                "start_secs": {"type": "number", "description": "Trim start, in seconds from the start of the input."},
                "end_secs": {"type": "number", "description": "Trim end, in seconds. Must be greater than start_secs."},
                "fast": {"type": "boolean", "description": "Stream-copy instead of re-encoding — faster, but the cut snaps to the nearest keyframe rather than landing exactly on start_secs/end_secs. Defaults to false (frame-accurate re-encode)."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = ctx
            .inputs
            .first()
            .ok_or_else(|| ExecutionError::Step("trim requires exactly one input".into()))?;
        let start = ctx
            .params
            .get("start_secs")
            .and_then(Value::as_f64)
            .ok_or_else(|| ExecutionError::Step("trim requires `start_secs` (number)".into()))?;
        let end = ctx
            .params
            .get("end_secs")
            .and_then(Value::as_f64)
            .ok_or_else(|| ExecutionError::Step("trim requires `end_secs` (number)".into()))?;
        if start < 0.0 {
            return Err(ExecutionError::Step(
                "trim: `start_secs` must be >= 0".into(),
            ));
        }
        if end <= start {
            return Err(ExecutionError::Step(
                "trim: `end_secs` must be greater than `start_secs`".into(),
            ));
        }
        let fast = ctx
            .params
            .get("fast")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut args = vec![
            "-y".to_string(),
            "-ss".to_string(),
            start.to_string(),
            "-to".to_string(),
            end.to_string(),
            "-i".to_string(),
            input.display().to_string(),
        ];
        if fast {
            args.extend(["-c".to_string(), "copy".to_string()]);
        } else {
            args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                // Speed over compression ratio: this tool serves quick edits,
                // not final delivery-quality mastering. `veryfast` trades some
                // file size for a much shorter wait.
                "-preset".to_string(),
                "veryfast".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
            ]);
        }
        args.push(ctx.output_path.display().to_string());
        Ok(args)
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
    ) -> OpContext<'a> {
        OpContext {
            inputs,
            params,
            output_path: output,
            probed_duration_secs: None,
        }
    }

    #[test]
    fn builds_reencode_args_by_default() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"start_secs": 1.5, "end_secs": 4.0});
        let args = TrimOp.build_args(&ctx(&inputs, &params, &output)).unwrap();
        assert!(args.contains(&"1.5".to_string()));
        assert!(args.contains(&"4".to_string()) || args.contains(&"4.0".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.contains(&"veryfast".to_string()));
        assert!(!args.contains(&"copy".to_string()));
    }

    #[test]
    fn fast_mode_stream_copies() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"start_secs": 0.0, "end_secs": 2.0, "fast": true});
        let args = TrimOp.build_args(&ctx(&inputs, &params, &output)).unwrap();
        assert!(args.contains(&"copy".to_string()));
    }

    #[test]
    fn rejects_end_before_start() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"start_secs": 5.0, "end_secs": 2.0});
        assert!(TrimOp.build_args(&ctx(&inputs, &params, &output)).is_err());
    }

    #[test]
    fn rejects_negative_start() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"start_secs": -1.0, "end_secs": 2.0});
        assert!(TrimOp.build_args(&ctx(&inputs, &params, &output)).is_err());
    }

    #[test]
    fn rejects_missing_params() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        assert!(TrimOp.build_args(&ctx(&inputs, &params, &output)).is_err());
    }
}
