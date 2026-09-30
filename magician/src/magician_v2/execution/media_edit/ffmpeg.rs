use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

fn resolve_bin(name: &str, fallbacks: &[&str]) -> String {
    for candidate in fallbacks {
        if Path::new(candidate).exists() {
            return (*candidate).to_string();
        }
    }
    name.to_string()
}

pub fn ffmpeg_bin() -> String {
    resolve_bin(
        "ffmpeg",
        &["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"],
    )
}

pub fn ffprobe_bin() -> String {
    resolve_bin(
        "ffprobe",
        &["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe"],
    )
}

pub async fn probe_duration_secs(input: &Path) -> Result<f64, String> {
    let output = tokio::process::Command::new(ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("ffprobe spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("could not parse ffprobe duration output: {e}"))
}

/// Cap retained stderr so a chatty ffmpeg failure never balloons memory —
/// mirrors the retention cap `execute_bash_action_streaming` uses.
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// Run ffmpeg to completion, invoking `on_progress` with a 0.0-100.0 percent
/// whenever `total_duration_secs` is known; `None` percent otherwise (still
/// invoked so callers can mark "still running").
pub async fn run_ffmpeg(
    bin: &str,
    args: &[String],
    total_duration_secs: Option<f64>,
    mut on_progress: impl FnMut(Option<f32>) + Send + 'static,
) -> Result<(), String> {
    let mut full_args = args.to_vec();
    full_args.extend([
        "-progress".to_string(),
        "pipe:1".to_string(),
        "-nostats".to_string(),
    ]);

    let mut child = tokio::process::Command::new(bin)
        .args(&full_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn {bin}: {e}"))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > STDERR_TAIL_BYTES {
                let drop_to = tail.len() - STDERR_TAIL_BYTES;
                tail.drain(0..drop_to);
            }
        }
        tail
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut out_time_ms: Option<u64> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(value) = line.strip_prefix("out_time_ms=") {
            out_time_ms = value.trim().parse::<u64>().ok();
        }
        if line.starts_with("progress=") {
            let pct = match (out_time_ms, total_duration_secs) {
                (Some(ms), Some(total)) if total > 0.0 => {
                    Some(((ms as f64 / 1000.0 / total) * 100.0).clamp(0.0, 100.0) as f32)
                },
                _ => None,
            };
            on_progress(pct);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("waiting on ffmpeg failed: {e}"))?;
    let stderr_tail = stderr_task.await.unwrap_or_default();

    if !status.success() {
        return Err(format!(
            "ffmpeg exited with {status}: {}",
            stderr_tail.trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_bare_name_when_no_candidate_exists() {
        // No real system has this exact path; exercises the "return bare name" arm.
        let resolved = super::resolve_bin("made-up-binary-xyz", &["/definitely/not/here"]);
        assert_eq!(resolved, "made-up-binary-xyz");
    }

    #[test]
    fn ffmpeg_bin_resolves_to_something_runnable() {
        // Whatever this resolves to must actually exist on PATH or as an absolute
        // path in dev/CI — both this repo's screen capture code and this test
        // assume ffmpeg is an installed dependency.
        let bin = ffmpeg_bin();
        let found_on_path = std::process::Command::new(&bin)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(
            found_on_path,
            "ffmpeg not runnable via resolved path `{bin}` — is ffmpeg installed?"
        );
    }

    #[tokio::test]
    async fn probes_duration_of_a_synthetic_clip() {
        let dir =
            std::env::temp_dir().join(format!("media_edit_probe_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let status = tokio::process::Command::new(ffmpeg_bin())
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=64x64:rate=5",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=2",
                "-shortest",
            ])
            .arg(&clip)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "failed to synthesize test clip");

        let duration = probe_duration_secs(&clip).await.unwrap();
        assert!(
            (duration - 2.0).abs() < 0.2,
            "expected ~2.0s, got {duration}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probe_errors_cleanly_on_missing_file() {
        let result = probe_duration_secs(std::path::Path::new("/no/such/file.mp4")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn runs_ffmpeg_and_reports_completion() {
        let dir = std::env::temp_dir().join(format!("media_edit_run_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        let args: Vec<String> = [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=64x64:rate=5",
        ]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(output.display().to_string()))
        .collect();

        let updates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let updates_clone = updates.clone();
        let result = run_ffmpeg(&ffmpeg_bin(), &args, Some(1.0), move |pct| {
            updates_clone.lock().unwrap().push(pct);
        })
        .await;

        assert!(result.is_ok(), "ffmpeg run failed: {result:?}");
        assert!(output.exists(), "output file was not produced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn surfaces_ffmpeg_failure_with_stderr() {
        let result = run_ffmpeg(
            &ffmpeg_bin(),
            &[
                "-y".to_string(),
                "-i".to_string(),
                "/no/such/input.mp4".to_string(),
                "/tmp/never_written.mp4".to_string(),
            ],
            None,
            |_pct| {},
        )
        .await;
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("exited"),
            "expected exit-status context in error, got: {error}"
        );
    }
}
