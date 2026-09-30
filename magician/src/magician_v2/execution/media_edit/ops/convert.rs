use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

const ALLOWED_FORMATS: &[&str] = &["mp4", "mov", "webm", "gif", "mp3", "wav"];

pub struct ConvertOp;

impl MediaOp for ConvertOp {
    fn name(&self) -> &'static str {
        "convert"
    }

    fn output_ext(&self, params: &Value) -> &'static str {
        // Falls back to "mp4" if `format` is missing/invalid; build_args
        // rejects that case before ffmpeg ever runs, so this default is
        // only ever observed transiently while validating.
        params
            .get("format")
            .and_then(Value::as_str)
            .and_then(|f| ALLOWED_FORMATS.iter().find(|allowed| **allowed == f))
            .copied()
            .unwrap_or("mp4")
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["format"],
            "properties": {
                "format": {"type": "string", "enum": ALLOWED_FORMATS, "description": "Target container/codec."},
                "width": {"type": "integer", "description": "Optional target width in pixels. Height auto-scales unless `height` is also given."},
                "height": {"type": "integer", "description": "Optional target height in pixels."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = ctx
            .inputs
            .first()
            .ok_or_else(|| ExecutionError::Step("convert requires exactly one input".into()))?;
        let format = ctx
            .params
            .get("format")
            .and_then(Value::as_str)
            .ok_or_else(|| ExecutionError::Step("convert requires `format` (string)".into()))?;
        if !ALLOWED_FORMATS.contains(&format) {
            return Err(ExecutionError::Step(format!(
                "convert: unsupported `format` `{format}`; allowed: {}",
                ALLOWED_FORMATS.join(", ")
            )));
        }

        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
        ];

        let width = ctx.params.get("width").and_then(Value::as_i64);
        let height = ctx.params.get("height").and_then(Value::as_i64);
        if width.is_some_and(|w| w <= 0) || height.is_some_and(|h| h <= 0) {
            return Err(ExecutionError::Step(
                "convert: `width`/`height` must be positive".into(),
            ));
        }
        if width.is_some() || height.is_some() {
            let w = width
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-2".to_string());
            let h = height
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-2".to_string());
            args.extend(["-vf".to_string(), format!("scale={w}:{h}")]);
        }

        // A fast preset only makes sense for the codec it's a flag on. mp4/mov
        // land on libx264 (ffmpeg's own default for both anyway, made explicit
        // so `-preset` has something to apply to); webm/gif/mp3/wav are left to
        // ffmpeg's per-format defaults rather than guessing a preset syntax
        // that doesn't apply to their codecs.
        if matches!(format, "mp4" | "mov") {
            args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-preset".to_string(),
                "veryfast".to_string(),
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
    fn builds_plain_convert_args() {
        let inputs = vec![PathBuf::from("/tmp/in.mov")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"format": "mp4"});
        let args = ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(!args.iter().any(|a| a == "-vf"));
    }

    #[test]
    fn adds_scale_filter_when_width_given() {
        let inputs = vec![PathBuf::from("/tmp/in.mov")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"format": "mp4", "width": 640});
        let args = ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(args.iter().any(|a| a == "scale=640:-2"));
    }

    #[test]
    fn rejects_non_positive_width_or_height() {
        let inputs = vec![PathBuf::from("/tmp/in.mov")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"format": "mp4", "width": -640});
        assert!(ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());

        let params = json!({"format": "mp4", "height": 0});
        assert!(ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }

    #[test]
    fn rejects_unsupported_format() {
        let inputs = vec![PathBuf::from("/tmp/in.mov")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"format": "avi"});
        assert!(ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }

    #[test]
    fn mp4_and_mov_targets_get_a_fast_preset() {
        let inputs = vec![PathBuf::from("/tmp/in.webm")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"format": "mp4"});
        let args = ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(args.contains(&"veryfast".to_string()));
    }

    #[test]
    fn non_libx264_targets_get_no_preset_flag() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp3");
        let params = json!({"format": "mp3"});
        let args = ConvertOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(!args.iter().any(|a| a == "-preset"));
    }
}
