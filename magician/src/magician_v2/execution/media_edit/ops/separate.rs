use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct ExtractAudioOp;

impl MediaOp for ExtractAudioOp {
    fn name(&self) -> &'static str {
        "extract_audio"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp3"
    }

    fn param_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = single_input(ctx, "extract_audio")?;
        Ok(vec![
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-vn".to_string(),
            "-acodec".to_string(),
            "libmp3lame".to_string(),
            ctx.output_path.display().to_string(),
        ])
    }
}

pub struct MuteVideoOp;

impl MediaOp for MuteVideoOp {
    fn name(&self) -> &'static str {
        "mute_video"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = single_input(ctx, "mute_video")?;
        Ok(vec![
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-an".to_string(),
            "-c:v".to_string(),
            "copy".to_string(),
            ctx.output_path.display().to_string(),
        ])
    }
}

fn single_input<'a>(
    ctx: &'a OpContext<'_>,
    op_name: &str,
) -> Result<&'a std::path::PathBuf, ExecutionError> {
    ctx.inputs
        .first()
        .ok_or_else(|| ExecutionError::Step(format!("{op_name} requires exactly one input")))
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
    fn extract_audio_strips_video_stream() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp3");
        let params = json!({});
        let args = ExtractAudioOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(args.contains(&"-vn".to_string()));
    }

    #[test]
    fn mute_video_strips_audio_stream() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        let args = MuteVideoOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        assert!(args.contains(&"-an".to_string()));
    }

    #[test]
    fn rejects_missing_input() {
        let inputs: Vec<PathBuf> = vec![];
        let output = PathBuf::from("/tmp/out.mp3");
        let params = json!({});
        assert!(ExtractAudioOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }
}
