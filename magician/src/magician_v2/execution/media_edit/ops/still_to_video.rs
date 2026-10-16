use serde_json::{json, Value};

use crate::magician_v2::execution::error::ExecutionError;

use super::{MediaOp, OpContext};

pub struct StillToVideoOp;

impl MediaOp for StillToVideoOp {
    fn name(&self) -> &'static str {
        "still_to_video"
    }
    fn output_ext(&self, _params: &Value) -> &'static str {
        "mp4"
    }
    fn param_schema(&self) -> Value {
        json!({"type":"object", "required":["duration_secs"], "properties":{
            "duration_secs":{"type":"number", "exclusiveMinimum":0, "description":"Finite positive clip duration."},
            "frame_rate":{"type":"number", "exclusiveMinimum":0, "default":25, "description":"Frames per second."},
            "width":{"type":"integer", "minimum":2, "multipleOf":2, "description":"Optional even output width; the other dimension auto-scales when omitted."},
            "height":{"type":"integer", "minimum":2, "multipleOf":2, "description":"Optional even output height."}
        }})
    }
    fn build_args(&self, ctx: &OpContext<'_>) -> Result<Vec<String>, ExecutionError> {
        if ctx.inputs.len() != 1 {
            return Err(ExecutionError::Step(
                "still_to_video requires exactly one still image input".into(),
            ));
        }
        let positive = |name: &str, default: Option<f64>| -> Result<f64, ExecutionError> {
            let value = match ctx.params.get(name) {
                None => default,
                Some(value) => value.as_f64(),
            }
            .filter(|value| value.is_finite() && *value > 0.0);
            value.ok_or_else(|| {
                ExecutionError::Step(format!(
                    "still_to_video: `{name}` must be a finite positive number"
                ))
            })
        };
        let duration = positive("duration_secs", None)?;
        let frame_rate = positive("frame_rate", Some(25.0))?;
        let dimension = |name: &str| -> Result<Option<u64>, ExecutionError> {
            match ctx.params.get(name) {
                None => Ok(None),
                Some(value) => value
                    .as_u64()
                    .filter(|n| *n > 0 && n % 2 == 0)
                    .map(Some)
                    .ok_or_else(|| {
                        ExecutionError::Step(format!(
                            "still_to_video: `{name}` must be a positive even integer"
                        ))
                    }),
            }
        };
        let width = dimension("width")?;
        let height = dimension("height")?;
        let filter = if width.is_some() || height.is_some() {
            format!(
                "scale={}:{}",
                width.map(|n| n.to_string()).unwrap_or_else(|| "-2".into()),
                height.map(|n| n.to_string()).unwrap_or_else(|| "-2".into())
            )
        } else {
            // H.264/yuv420p needs even dimensions. Keep the source intact and
            // pad an odd edge rather than rejecting or distorting the still.
            "pad=ceil(iw/2)*2:ceil(ih/2)*2".into()
        };
        Ok(vec![
            "-y".into(),
            "-loop".into(),
            "1".into(),
            "-framerate".into(),
            frame_rate.to_string(),
            "-i".into(),
            ctx.inputs[0].display().to_string(),
            "-t".into(),
            duration.to_string(),
            "-vf".into(),
            filter,
            "-an".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-movflags".into(),
            "+faststart".into(),
            ctx.output_path.display().to_string(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn rejects_invalid_duration_rate_dimensions_and_input_count() {
        let inputs = [PathBuf::from("still.png")];
        let build = |params: Value| {
            StillToVideoOp.build_args(&OpContext {
                inputs: &inputs,
                params: &params,
                output_path: Path::new("clip.mp4"),
                probed_duration_secs: None,
            })
        };
        for params in [
            json!({}),
            json!({"duration_secs":0}),
            json!({"duration_secs":-1}),
            json!({"duration_secs":"3"}),
            json!({"duration_secs":1,"frame_rate":null}),
            json!({"duration_secs":1,"frame_rate":0}),
            json!({"duration_secs":1,"width":3}),
            json!({"duration_secs":1,"height":"360"}),
        ] {
            assert!(build(params).is_err());
        }
        assert!(
            build(json!({"duration_secs":1.5,"frame_rate":24,"width":640,"height":360})).is_ok()
        );
        let params = json!({"duration_secs":1});
        for inputs in [vec![], vec![PathBuf::from("a.png"), PathBuf::from("b.png")]] {
            assert!(StillToVideoOp
                .build_args(&OpContext {
                    inputs: &inputs,
                    params: &params,
                    output_path: Path::new("clip.mp4"),
                    probed_duration_secs: None
                })
                .is_err());
        }
    }

    #[tokio::test]
    async fn scoped_stills_compose_real_silent_video_with_preserved_frames() {
        use crate::magician_v2::execution::media_edit::{ffmpeg, scoped};
        use base64::Engine;
        use chrono::Utc;
        use std::{sync::Arc, time::Duration};
        use tokio_util::sync::CancellationToken;

        let directory = Arc::new(tempfile::tempdir().unwrap());
        let root = directory.path().canonicalize().unwrap();
        let still = root.join("still.png");
        // A genuine PNG fixture, not a pre-rendered clip. Both operations must
        // produce real decodable frames through the pinned process owner.
        std::fs::write(&still, base64::engine::general_purpose::STANDARD.decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC"
        ).unwrap()).unwrap();
        let ppm = root.join("still.ppm");
        std::fs::write(&ppm, b"P3\n1 1\n255\n0 255 0\n").unwrap();
        let (executable, hash) = ffmpeg::executable_pin().unwrap();
        let owner = scoped::admit(
            &uuid::Uuid::new_v4().to_string(),
            &root,
            1_048_576,
            Utc::now() + chrono::Duration::seconds(30),
            CancellationToken::new(),
            &hash,
        )
        .unwrap();
        let clips = [root.join("first.mp4"), root.join("second.mp4")];
        for ((output, duration), (image, media_type)) in clips
            .iter()
            .zip([0.5, 1.0])
            .zip([(still, "image/png"), (ppm, "image/x-portable-pixmap")])
        {
            let inputs = [image];
            let params = json!({"duration_secs":duration,"frame_rate":20,"width":64,"height":32});
            let args = StillToVideoOp
                .build_bounded_args(&OpContext {
                    inputs: &inputs,
                    params: &params,
                    output_path: output,
                    probed_duration_secs: None,
                })
                .unwrap();
            let mut bounded = Vec::new();
            for arg in args {
                if arg == "-i" {
                    bounded.extend(scoped::input_options(media_type).unwrap());
                }
                if arg == output.display().to_string() {
                    bounded.extend([
                        "-threads".into(),
                        "1".into(),
                        "-fs".into(),
                        "1048576".into(),
                    ]);
                }
                bounded.push(arg);
            }
            owner
                .run_owned(
                    &executable,
                    bounded,
                    directory.clone(),
                    Duration::from_secs(10),
                )
                .await
                .unwrap();
        }
        // An audio-inclusive join cannot accept these genuinely silent clips.
        // Its complete actionable error must fit the task's bounded transcript;
        // the caller can then choose the documented video-only composition.
        let failed_output = root.join("audio-required.mp4");
        let args = super::super::concat::ConcatOp
            .build_bounded_args(&OpContext {
                inputs: &clips,
                params: &json!({}),
                output_path: &failed_output,
                probed_duration_secs: None,
            })
            .unwrap();
        let mut bounded = Vec::new();
        for arg in args {
            if arg == "-i" {
                bounded.extend(scoped::input_options("video/mp4").unwrap());
            }
            if arg == failed_output.display().to_string() {
                bounded.extend([
                    "-threads".into(),
                    "1".into(),
                    "-fs".into(),
                    "1048576".into(),
                ]);
            }
            bounded.push(arg);
        }
        let failure = owner
            .run_owned(
                &executable,
                bounded,
                directory.clone(),
                Duration::from_secs(10),
            )
            .await
            .expect_err("silent inputs have no audio stream to concatenate")
            .to_string();
        assert!(failure.contains("matches no streams"), "{failure}");
        assert!(
            failure.len() < 1024,
            "actionable error was oversized: {failure}"
        );
        assert!(
            !failure.contains("ffmpeg version"),
            "startup banner obscures the error"
        );
        let output = root.join("combined.mp4");
        let params = json!({"include_audio":false});
        let args = super::super::concat::ConcatOp
            .build_bounded_args(&OpContext {
                inputs: &clips,
                params: &params,
                output_path: &output,
                probed_duration_secs: None,
            })
            .unwrap();
        let mut bounded = Vec::new();
        for arg in args {
            if arg == "-i" {
                bounded.extend(scoped::input_options("video/mp4").unwrap());
            }
            if arg == output.display().to_string() {
                bounded.extend([
                    "-threads".into(),
                    "1".into(),
                    "-fs".into(),
                    "1048576".into(),
                ]);
            }
            bounded.push(arg);
        }
        let composed = owner
            .run_owned(
                &executable,
                bounded,
                directory.clone(),
                Duration::from_secs(10),
            )
            .await;
        owner.close_and_drain().await;
        composed.unwrap();
        let (probe, _) = ffmpeg::probe_executable_pin().unwrap();
        let result = tokio::process::Command::new(probe)
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "format=duration:stream=codec_type,width,height,nb_read_frames",
                "-of",
                "json",
            ])
            .arg(&output)
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let facts: Value = serde_json::from_slice(&result.stdout).unwrap();
        let streams = facts["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0]["codec_type"], "video");
        assert_eq!(streams[0]["width"], 64);
        assert_eq!(streams[0]["height"], 32);
        assert_eq!(streams[0]["nb_read_frames"], "30");
        assert!(
            (facts["format"]["duration"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
                - 1.5)
                .abs()
                < 0.001
        );
        for (timestamp, expected_channel) in [("0.25", 0), ("0.75", 1)] {
            let frame = tokio::process::Command::new(&executable)
                .args(["-v", "error", "-ss", timestamp, "-i"])
                .arg(&output)
                .args([
                    "-vf",
                    "scale=1:1",
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "pipe:1",
                ])
                .output()
                .await
                .unwrap();
            assert!(
                frame.status.success(),
                "{}",
                String::from_utf8_lossy(&frame.stderr)
            );
            assert_eq!(frame.stdout.len(), 3);
            for (channel, value) in frame.stdout.iter().enumerate() {
                assert!(
                    if channel == expected_channel {
                        *value > 220
                    } else {
                        *value < 30
                    },
                    "scene order/colour changed: {:?}",
                    frame.stdout
                );
            }
        }
    }
}
