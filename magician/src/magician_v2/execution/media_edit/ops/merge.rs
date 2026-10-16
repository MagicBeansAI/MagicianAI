use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

/// Mux a video file with an audio file into one output. Optional
/// `sync_offset_ms` shifts the audio's start relative to the video —
/// positive delays audio, negative advances it — for realigning drifted
/// A/V. Inputs are `[video_path, audio_path]`, in that order.
pub struct MergeOp;

impl MediaOp for MergeOp {
    fn name(&self) -> &'static str {
        "merge"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sync_offset_ms": {"type": "integer", "description": "Shift the audio track's start by this many milliseconds relative to the video. Positive delays audio, negative advances it. Defaults to 0."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let video = ctx.inputs.first().ok_or_else(|| {
            ExecutionError::Step("merge requires two inputs: [video_path, audio_path]".into())
        })?;
        let audio = ctx.inputs.get(1).ok_or_else(|| {
            ExecutionError::Step("merge requires two inputs: [video_path, audio_path]".into())
        })?;
        let offset_ms = ctx
            .params
            .get("sync_offset_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            video.display().to_string(),
        ];
        if offset_ms != 0 {
            // -itsoffset applies to the NEXT -i, so it must precede the audio input.
            args.extend([
                "-itsoffset".to_string(),
                format!("{:.3}", offset_ms as f64 / 1000.0),
            ]);
        }
        args.extend([
            "-i".to_string(),
            audio.display().to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-map".to_string(),
            "1:a:0".to_string(),
            "-c:v".to_string(),
            "copy".to_string(),
            "-c:a".to_string(),
            "aac".to_string(),
            "-shortest".to_string(),
        ]);
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
    fn merges_without_offset_by_default() {
        let inputs = vec![PathBuf::from("/tmp/v.mp4"), PathBuf::from("/tmp/a.mp3")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        let args = MergeOp.build_args(&ctx(&inputs, &params, &output)).unwrap();
        assert!(!args.contains(&"-itsoffset".to_string()));
    }

    #[test]
    fn applies_sync_offset_before_audio_input() {
        let inputs = vec![PathBuf::from("/tmp/v.mp4"), PathBuf::from("/tmp/a.mp3")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"sync_offset_ms": 250});
        let args = MergeOp.build_args(&ctx(&inputs, &params, &output)).unwrap();
        let offset_idx = args.iter().position(|a| a == "-itsoffset").unwrap();
        let audio_input_idx = args.iter().position(|a| a == "/tmp/a.mp3").unwrap();
        assert!(offset_idx < audio_input_idx);
    }

    #[test]
    fn rejects_single_input() {
        let inputs = vec![PathBuf::from("/tmp/v.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        assert!(MergeOp.build_args(&ctx(&inputs, &params, &output)).is_err());
    }
}
