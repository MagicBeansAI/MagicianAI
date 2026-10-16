use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct SpeedOp;

impl MediaOp for SpeedOp {
    fn name(&self) -> &'static str {
        "speed"
    }

    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }

    fn param_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["factor"],
            "properties": {
                "factor": {"type": "number", "description": "Playback speed multiplier. 2.0 = twice as fast, 0.5 = half speed. Must be > 0."}
            }
        })
    }

    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        let input = ctx
            .inputs
            .first()
            .ok_or_else(|| ExecutionError::Step("speed requires exactly one input".into()))?;
        let factor = ctx
            .params
            .get("factor")
            .and_then(Value::as_f64)
            .ok_or_else(|| ExecutionError::Step("speed requires `factor` (number)".into()))?;
        if factor <= 0.0 {
            return Err(ExecutionError::Step(
                "speed: `factor` must be greater than 0".into(),
            ));
        }

        // atempo only accepts [0.5, 100.0] per-filter; chain two atempo stages
        // for factors outside that range so e.g. 0.1x or 8x still work.
        let atempo_chain = atempo_filter_chain(factor);

        Ok(vec![
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-filter_complex".to_string(),
            format!(
                "[0:v]setpts={:.6}*PTS[v];[0:a]{}[a]",
                1.0 / factor,
                atempo_chain
            ),
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
            ctx.output_path.display().to_string(),
        ])
    }
}

fn atempo_filter_chain(mut factor: f64) -> String {
    let mut stages = Vec::new();
    while factor > 2.0 {
        stages.push(2.0);
        factor /= 2.0;
    }
    while factor < 0.5 {
        stages.push(0.5);
        factor /= 0.5;
    }
    stages.push(factor);
    stages
        .into_iter()
        .map(|s| format!("atempo={s:.6}"))
        .collect::<Vec<_>>()
        .join(",")
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
    fn builds_args_for_simple_speedup() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"factor": 2.0});
        let args = SpeedOp.build_args(&ctx(&inputs, &params, &output)).unwrap();
        let filter = args.iter().find(|a| a.contains("setpts")).unwrap();
        assert!(filter.contains("atempo=2"));
    }

    #[test]
    fn chains_atempo_for_extreme_factors() {
        let chain = super::atempo_filter_chain(8.0);
        assert_eq!(chain, "atempo=2.000000,atempo=2.000000,atempo=2.000000");
    }

    #[test]
    fn rejects_non_positive_factor() {
        let inputs = vec![PathBuf::from("/tmp/in.mp4")];
        let output = PathBuf::from("/tmp/out.mp4");
        let params = json!({"factor": 0.0});
        assert!(SpeedOp.build_args(&ctx(&inputs, &params, &output)).is_err());
    }
}
