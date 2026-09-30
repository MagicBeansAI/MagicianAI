use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct OverlayTextOp;

impl MediaOp for OverlayTextOp {
    fn name(&self) -> &'static str {
        "overlay_text"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "Caption/watermark text to burn in."},
                "position": {"type": "string", "enum": ["top_left", "top_right", "bottom_left", "bottom_right", "center"], "description": "Defaults to bottom_left."},
                "font_size": {"type": "integer", "description": "Defaults to 24."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = ctx.inputs.first().ok_or_else(|| {
            ExecutionError::Step("overlay_text requires exactly one input".into())
        })?;
        let text = ctx
            .params
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ExecutionError::Step("overlay_text requires `text` (string)".into()))?;
        if text.is_empty() {
            return Err(ExecutionError::Step(
                "overlay_text: `text` must not be empty".into(),
            ));
        }
        let font_size = ctx
            .params
            .get("font_size")
            .and_then(Value::as_i64)
            .unwrap_or(24);
        if font_size <= 0 {
            return Err(ExecutionError::Step(
                "overlay_text: `font_size` must be positive".into(),
            ));
        }
        let position = ctx
            .params
            .get("position")
            .and_then(Value::as_str)
            .unwrap_or("bottom_left");
        let (x, y) = match position {
            "top_left" => ("10", "10"),
            "top_right" => ("w-tw-10", "10"),
            "bottom_right" => ("w-tw-10", "h-th-10"),
            "center" => ("(w-tw)/2", "(h-th)/2"),
            _ => ("10", "h-th-10"), // bottom_left, and any unrecognized value
        };
        let escaped = escape_drawtext(text);

        Ok(vec![
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-vf".to_string(),
            format!(
                "drawtext=text='{escaped}':fontsize={font_size}:fontcolor=white:x={x}:y={y}:box=1:boxcolor=black@0.4:boxborderw=6"
            ),
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "veryfast".to_string(),
            "-c:a".to_string(),
            "copy".to_string(),
            ctx.output_path.display().to_string(),
        ])
    }
}

/// ffmpeg drawtext treats `:`, `'`, and `\` specially inside the filter
/// string; escape them so arbitrary caption text can't break the filtergraph.
fn escape_drawtext(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
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
    fn builds_bottom_left_by_default() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"text": "hello"});
        let args = OverlayTextOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        let filter = args.iter().find(|a| a.contains("drawtext")).unwrap();
        assert!(filter.contains("x=10:y=h-th-10"));
    }

    #[test]
    fn escapes_colons_and_quotes_in_text() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"text": "it's 5:00"});
        let args = OverlayTextOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        let filter = args.iter().find(|a| a.contains("drawtext")).unwrap();
        assert!(filter.contains("it\\'s 5\\:00"));
    }

    #[test]
    fn rejects_empty_text() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"text": ""});
        assert!(OverlayTextOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }

    #[test]
    fn rejects_non_positive_font_size() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"text": "hello", "font_size": 0});
        assert!(OverlayTextOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());

        let params = json!({"text": "hello", "font_size": -12});
        assert!(OverlayTextOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }
}
