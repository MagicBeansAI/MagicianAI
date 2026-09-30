use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct ConcatOp;

impl MediaOp for ConcatOp {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        if ctx.inputs.len() < 2 {
            return Err(ExecutionError::Step(
                "concat requires at least two inputs, in join order".into(),
            ));
        }

        let mut args = vec!["-y".to_string()];
        for input in ctx.inputs {
            args.extend(["-i".to_string(), input.display().to_string()]);
        }

        let n = ctx.inputs.len();
        let stream_refs: String = (0..n).map(|i| format!("[{i}:v:0][{i}:a:0]")).collect();
        args.extend([
            "-filter_complex".to_string(),
            format!("{stream_refs}concat=n={n}:v=1:a=1[v][a]"),
            "-map".to_string(),
            "[v]".to_string(),
            "-map".to_string(),
            "[a]".to_string(),
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "veryfast".to_string(),
            "-c:a".to_string(),
            "aac".to_string(),
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
    fn builds_filter_complex_for_three_clips() {
        let inputs = vec![
            PathBuf::from("/tmp/a.mp4"),
            PathBuf::from("/tmp/b.mp4"),
            PathBuf::from("/tmp/c.mp4"),
        ];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        let args = ConcatOp
            .build_args(&ctx(&inputs, &params, &output))
            .unwrap();
        let filter = args.iter().find(|a| a.contains("concat=n=")).unwrap();
        assert!(filter.contains("concat=n=3:v=1:a=1"));
        assert!(filter.starts_with("[0:v:0][0:a:0][1:v:0][1:a:0][2:v:0][2:a:0]"));
    }

    #[test]
    fn rejects_fewer_than_two_inputs() {
        let inputs = vec![PathBuf::from("/tmp/a.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({});
        assert!(ConcatOp
            .build_args(&ctx(&inputs, &params, &output))
            .is_err());
    }
}
