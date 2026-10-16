//! Explicit audio samples and transcripts through the existing media seam.
use super::media_seam::{
    providers_registry::MediaProviderRegistry,
    providers_stt::{SttProvider, SttRequest},
};
use anyhow::{ensure, Context, Result};
use magician_relentless::{budget::OperationIntent, store::RunStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};
use tokio_util::sync::CancellationToken;

static PROVIDERS: OnceLock<Arc<MediaProviderRegistry>> = OnceLock::new();
pub fn install_providers(providers: Arc<MediaProviderRegistry>) -> Result<()> {
    PROVIDERS
        .set(providers)
        .map_err(|_| anyhow::anyhow!("audio review providers already installed"))
}

pub fn provider_catalog() -> Vec<Value> {
    PROVIDERS.get().map(|registry| registry.stt_chain().into_iter().filter_map(|provider|
        provider.review_configuration_hash().map(|hash| json!({"provider_id":provider.id(),"model":provider.default_model(),"configuration_hash":hash,"maximum_cost_microusd":0}))).collect()).unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSegment {
    pub start_secs: f64,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioReviewRecipe {
    pub segments: Vec<AudioSegment>,
    pub maximum_sample_bytes: u64,
    pub maximum_transcript_bytes: u64,
    pub timeout_secs: u64,
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub provider_configuration_hash: String,
    #[serde(default)]
    pub executable_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl AudioReviewRecipe {
    fn provider(&self) -> Result<Arc<dyn SttProvider>> {
        let provider = PROVIDERS
            .get()
            .context("artifact audio providers unavailable")?
            .stt_chain()
            .into_iter()
            .find(|provider| provider.id() == self.provider_id)
            .context("declared audio provider unavailable")?;
        ensure!(
            provider.default_model() == self.model
                && provider.review_configuration_hash().as_deref()
                    == Some(self.provider_configuration_hash.as_str()),
            "audio review provider/model changed or lacks bounded zero-cost preparation"
        );
        Ok(provider)
    }
    pub fn resolve_environment(&mut self) -> Result<()> {
        if self.executable_hash.is_empty() {
            self.executable_hash = super::relentless_artifact::sampler_executable()?.1;
        }
        if self.provider_configuration_hash.is_empty() {
            self.provider_configuration_hash = PROVIDERS
                .get()
                .context("audio review providers unavailable")?
                .stt_chain()
                .into_iter()
                .find(|provider| {
                    provider.id() == self.provider_id && provider.default_model() == self.model
                })
                .and_then(|provider| provider.review_configuration_hash())
                .context("audio provider has no bounded zero-cost review boundary")?;
        }
        self.validate()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.segments.is_empty()
                && self.maximum_sample_bytes > 0
                && self.maximum_transcript_bytes > 0
                && self.timeout_secs > 0
                && self.timeout_secs < u64::MAX
                && self.maximum_transcript_bytes < u64::MAX,
            "audio review requires finite samples and bounds"
        );
        ensure!(
            self.executable_hash == super::relentless_artifact::sampler_executable()?.1,
            "audio sampling executable changed"
        );
        self.provider()?;
        for segment in &self.segments {
            ensure!(
                segment.start_secs.is_finite()
                    && segment.start_secs >= 0.0
                    && segment.duration_secs.is_finite()
                    && segment.duration_secs > 0.0
                    && segment.duration_secs * 32_000.0 + 4096.0
                        <= self.maximum_sample_bytes as f64,
                "audio segment exceeds the PCM byte ceiling"
            );
        }
        ensure!(
            self.segments
                .windows(2)
                .all(
                    |segments| segments[0].start_secs + segments[0].duration_secs
                        <= segments[1].start_secs
                ),
            "audio segments overlap or are unordered"
        );
        Ok(())
    }
    pub fn frozen_hash(&self) -> Result<String> {
        Ok(
            blake3::hash(&serde_json::to_vec(&("audio-review-v1", self))?)
                .to_hex()
                .to_string(),
        )
    }
    pub fn maximum_prepared_bytes(&self) -> Result<u64> {
        (self.segments.len() as u64)
            .checked_mul(
                self.maximum_sample_bytes
                    .checked_add(self.maximum_transcript_bytes)
                    .context("audio byte limit overflow")?,
            )
            .context("audio sample limit overflow")
    }
}

pub async fn prepare(
    owner: &str,
    recipe: &AudioReviewRecipe,
    media_type: &str,
    input: &[u8],
    artifact_hash: &str,
    input_id: &str,
    store: Arc<RunStore>,
    run_id: uuid::Uuid,
    holder: &str,
    operation: &OperationIntent,
    deadline: chrono::DateTime<chrono::Utc>,
    cancel: CancellationToken,
) -> Result<Value> {
    recipe.validate()?;
    let provider = recipe.provider()?;
    let recipe_hash = recipe.frozen_hash()?;
    let input_hash = blake3::hash(input).to_hex().to_string();
    let mut results = Vec::new();
    for (index, segment) in recipe.segments.iter().enumerate() {
        let identity = blake3::hash(&serde_json::to_vec(&(
            "audio-sample-v1",
            &recipe_hash,
            &input_hash,
            input_id,
            index,
        ))?)
        .to_hex()
        .to_string();
        let reader = store.clone();
        let artifact = artifact_hash.to_owned();
        let key = identity.clone();
        if let Some(cache) =
            tokio::task::spawn_blocking(move || reader.prepared_evidence(run_id, &artifact, &key))
                .await??
        {
            let bytes = cache
                .get("transcript.json")
                .context("audio sample receipt missing")?;
            ensure!(
                bytes.len() as u64 <= recipe.maximum_transcript_bytes,
                "cached transcript exceeds ceiling"
            );
            let value: Value = serde_json::from_slice(bytes)?;
            if let Some(hash) = value.get("sample_hash").and_then(Value::as_str) {
                let audio = cache
                    .get("audio.wav")
                    .context("cached audio sample missing")?;
                ensure!(
                    audio.len() as u64 <= recipe.maximum_sample_bytes
                        && blake3::hash(audio).to_hex().as_str() == hash,
                    "cached audio sample receipt changed"
                );
            }
            results.push(value);
            continue;
        }
        let reader = store.clone();
        let artifact = artifact_hash.to_owned();
        let key = identity.clone();
        let holder = holder.to_owned();
        let operation = operation.clone();
        let started = tokio::task::spawn_blocking(move || {
            reader.begin_evidence_preparation(
                run_id,
                &holder,
                &operation,
                &artifact,
                &key,
                chrono::Utc::now(),
            )
        })
        .await??;
        if !started {
            results.push(json!({"status":"unknown","input_id":input_id,"index":index,"segment":segment,"reason":"Audio preparation was dispatched without a sealed result; no resend"}));
            continue;
        }
        let directory = Arc::new(tempfile::tempdir()?);
        let input_path = directory.path().join("sealed-input");
        let output_path = directory.path().join("sample.wav");
        std::fs::write(&input_path, input)?;
        let lease = super::execution::media_edit::scoped::admit(
            owner,
            directory.path(),
            recipe.maximum_sample_bytes,
            deadline,
            cancel.child_token(),
            &recipe.executable_hash,
        )?;
        let mut args = vec!["-y".into()];
        args.extend(super::execution::media_edit::scoped::input_options(
            media_type,
        )?);
        args.extend([
            "-ss".into(),
            segment.start_secs.to_string(),
            "-i".into(),
            input_path.display().to_string(),
            "-t".into(),
            segment.duration_secs.to_string(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-ac".into(),
            "1".into(),
            "-ar".into(),
            "16000".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            "-threads".into(),
            "1".into(),
            "-fs".into(),
            recipe.maximum_sample_bytes.to_string(),
            output_path.display().to_string(),
        ]);
        let recipe = recipe.clone();
        let segment = segment.clone();
        let provider = provider.clone();
        let input_id = input_id.to_owned();
        let artifact = artifact_hash.to_owned();
        let writer = store.clone();
        let result = lease.run_preparation(move |lease, cancel| async move {
        let sampled = lease
            .run_owned(
                &super::relentless_artifact::sampler_executable()?.0,
                args,
                directory.clone(),
                (deadline - chrono::Utc::now())
                    .to_std()?
                    .min(std::time::Duration::from_secs(recipe.timeout_secs)),
            )
            .await;
        let mut sealed_audio = None;
        let result = if let Err(error) = sampled {
            json!({"status":"unknown","input_id":input_id,"index":index,"segment":segment,"reason":error.to_string()})
        } else {
            ensure!(
                std::fs::metadata(&output_path)?.len() <= recipe.maximum_sample_bytes,
                "audio sample exceeds ceiling"
            );
            let audio = std::fs::read(&output_path)?;
            let reader = hound::WavReader::new(std::io::Cursor::new(&audio))?;
            let spec = reader.spec();
            let sample_count = reader.duration();
            ensure!(
                spec.channels == 1 && spec.sample_rate == 16000 && spec.bits_per_sample == 16,
                "audio sampler returned another format"
            );
            let mut energy = 0.0;
            let mut peak = 0.0f64;
            for sample in reader.into_samples::<i16>() {
                let value = f64::from(sample?) / 32768.0;
                energy += value * value;
                peak = peak.max(value.abs());
            }
            let rms = if sample_count == 0 {
                0.0
            } else {
                (energy / f64::from(sample_count)).sqrt()
            };
            let full_segment = f64::from(sample_count) + 1.0 >= segment.duration_secs * 16000.0;
            let audio_hash = blake3::hash(&audio).to_hex().to_string();
            sealed_audio = Some(audio.clone());
            let remaining = (deadline - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
                .min(std::time::Duration::from_secs(recipe.timeout_secs));
            let response = tokio::time::timeout(
                remaining,
                provider.transcribe_bounded_review(
                    SttRequest {
                        audio: bytes::Bytes::from(audio),
                        content_type: "audio/wav".into(),
                        language: recipe.language.clone(),
                        model: Some(recipe.model.clone()),
                        message_id: Some(identity.clone()),
                        filename: Some("sample.wav".into()),
                        prompt: None,
                    },
                    recipe.maximum_transcript_bytes,
                    cancel.child_token(),
                ),
            )
            .await;
            let mut receipt = json!({
                "input_id":input_id,"index":index,"segment":segment,
                "actual_samples":sample_count,"sample_rate":16000,"sample_hash":audio_hash,
                "provider_configuration_hash":recipe.provider_configuration_hash,
                "peak_dbfs":if peak>0.0 {Some(20.0*peak.log10())} else {None},
                "rms_dbfs":if rms>0.0 {Some(20.0*rms.log10())} else {None},
                "coverage":"numeric_level_in_declared_first_audio_track_segment_only",
            });
            match response {
                Ok(Ok(transcript)) if transcript.model == recipe.model => {
                    receipt["status"] = json!(if full_segment {"complete"} else {"partial"});
                    receipt["model"] = json!(transcript.model);
                    receipt["transcript"] = json!(transcript.transcript);
                    receipt["coverage"] = json!("recognized_speech_and_numeric_level_in_declared_first_audio_track_segment_only");
                },
                other => {
                    receipt["status"] = json!("unknown");
                    receipt["reason"] = json!(format!("Audio transcription did not produce a pinned receipt: {other:?}"));
                },
            }
            receipt
        };
        let bytes = serde_json::to_vec(&result)?;
        ensure!(
            bytes.len() as u64 <= recipe.maximum_transcript_bytes,
            "audio receipt exceeds preparation ceiling"
        );
        tokio::task::spawn_blocking(move || {
            let mut evidence = BTreeMap::from([("transcript.json".into(), bytes)]);
            if let Some(audio) = sealed_audio {
                evidence.insert("audio.wav".into(), audio);
            }
            writer.seal_prepared_evidence(run_id, &artifact, &identity, &evidence)
        })
        .await??;
        Ok(result)
        }).await?;
        results.push(result);
    }
    Ok(json!({"recipe_hash":recipe_hash,"input_hash":input_hash,"segments":results}))
}

pub fn sealed_evidence(
    recipe: &AudioReviewRecipe,
    input: &[u8],
    artifact_hash: &str,
    input_id: &str,
    store: &RunStore,
    run_id: uuid::Uuid,
) -> Result<Value> {
    let recipe_hash = recipe.frozen_hash()?;
    let input_hash = blake3::hash(input).to_hex().to_string();
    let mut segments = Vec::new();
    for (index, segment) in recipe.segments.iter().enumerate() {
        let identity = blake3::hash(&serde_json::to_vec(&(
            "audio-sample-v1",
            &recipe_hash,
            &input_hash,
            input_id,
            index,
        ))?)
        .to_hex()
        .to_string();
        let value = match store.prepared_evidence(run_id, artifact_hash, &identity)? {
            Some(cache) => {
                let bytes = cache
                    .get("transcript.json")
                    .context("audio receipt missing")?;
                ensure!(
                    bytes.len() as u64 <= recipe.maximum_transcript_bytes,
                    "audio receipt exceeds ceiling"
                );
                let value: Value = serde_json::from_slice(bytes)?;
                if let Some(hash) = value.get("sample_hash").and_then(Value::as_str) {
                    let audio = cache
                        .get("audio.wav")
                        .context("audio sample bytes missing")?;
                    ensure!(
                        audio.len() as u64 <= recipe.maximum_sample_bytes
                            && blake3::hash(audio).to_hex().as_str() == hash,
                        "audio sample receipt changed"
                    );
                }
                value
            },
            None => {
                json!({"status":"unavailable","input_id":input_id,"index":index,"segment":segment})
            },
        };
        segments.push(value);
    }
    Ok(json!({"recipe_hash":recipe_hash,"input_hash":input_hash,"segments":segments}))
}
