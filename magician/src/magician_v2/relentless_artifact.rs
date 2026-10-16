//! Declared artifact representations for the existing review/controller loop.
//! Formats and sample coverage are contract data, never inferred from a goal,
//! filename, application or an actor-provided assessment.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{ensure, Context, Result};
use magician_relentless::store::RunStore;
use magicllm::ContentBlock;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReviewRecipe {
    pub artifact_kind: String,
    /// Aggregate byte ceiling for inline images, prepared frames and media facts.
    pub maximum_prepared_bytes: u64,
    pub files: BTreeMap<String, ArtifactPresentation>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, ArtifactPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_probe: Option<MediaProbeRecipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<super::relentless_code::VerificationRecipe>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaProbeRecipe {
    pub maximum_result_bytes: u64,
    pub timeout_secs: u64,
    #[serde(default)]
    pub executable_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactPresentation {
    Text,
    Image {
        media_type: String,
    },
    Video {
        media_type: String,
        sampler: VideoSampler,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<super::relentless_audio::AudioReviewRecipe>,
    },
    Audio {
        media_type: String,
        audio: super::relentless_audio::AudioReviewRecipe,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoSampler {
    pub timestamps_secs: Vec<f64>,
    pub maximum_width: u32,
    pub maximum_height: u32,
    pub maximum_sample_bytes: u64,
    pub timeout_secs: u64,
    /// Resolved executable bytes, rather than a mutable command or PATH hint.
    #[serde(default)]
    pub executable_hash: String,
}

pub fn sampler_executable() -> Result<(PathBuf, String)> {
    super::execution::media_edit::ffmpeg::executable_pin()
        .context("artifact sampler executable unavailable")
}

impl ArtifactReviewRecipe {
    pub fn resolve_environment(&mut self) -> Result<()> {
        if let Some(verification) = &mut self.verification {
            verification.resolve_environment()?;
        }
        if let Some(probe) = &mut self.media_probe {
            if probe.executable_hash.is_empty() {
                probe.executable_hash =
                    super::execution::media_edit::ffmpeg::probe_executable_pin()?.1;
            }
        }
        for presentation in self.files.values_mut().chain(self.sources.values_mut()) {
            match presentation {
                ArtifactPresentation::Video {
                    audio: Some(audio), ..
                }
                | ArtifactPresentation::Audio { audio, .. } => audio.resolve_environment()?,
                _ => {},
            }
            if let ArtifactPresentation::Video { sampler, .. } = presentation {
                if sampler.executable_hash.is_empty() {
                    sampler.executable_hash = sampler_executable()?.1;
                }
            }
        }
        self.validate()
    }

    pub fn validate(&self) -> Result<()> {
        let mut prepared_allowance = self
            .verification
            .as_ref()
            .map_or(0, |recipe| recipe.maximum_result_bytes);
        if let Some(verification) = &self.verification {
            verification.validate()?;
            ensure!(
                verification.maximum_result_bytes <= self.maximum_prepared_bytes,
                "verification evidence exceeds artifact preparation ceiling"
            );
            ensure!(
                self.files
                    .values()
                    .all(|view| matches!(view, ArtifactPresentation::Text))
                    && verification
                        .protected_files
                        .keys()
                        .all(|file| self.files.contains_key(file)),
                "verification needs the complete declared text source set"
            );
            ensure!(
                verification.declared_outputs.iter().all(|output| self
                    .files
                    .keys()
                    .all(|file| !std::path::Path::new(file).starts_with(output))),
                "verification outputs overlap declared source files"
            );
            ensure!(
                self.files
                    .keys()
                    .all(|file| !std::path::Path::new(file).starts_with(".verification-home")),
                "verification source overlaps its private runtime scratch"
            );
        }
        if let Some(probe) = &self.media_probe {
            ensure!(
                probe.maximum_result_bytes > 0
                    && probe.maximum_result_bytes <= self.maximum_prepared_bytes
                    && probe.timeout_secs > 0
                    && super::execution::media_edit::ffmpeg::probe_executable_pin()?.1
                        == probe.executable_hash,
                "media probe is unbounded or its executable changed"
            );
            let media_files = self
                .files
                .values()
                .filter(|view| {
                    matches!(
                        view,
                        ArtifactPresentation::Video { .. } | ArtifactPresentation::Audio { .. }
                    )
                })
                .count() as u64;
            prepared_allowance = prepared_allowance
                .checked_add(
                    probe
                        .maximum_result_bytes
                        .checked_mul(media_files)
                        .context("media probe allowance overflow")?,
                )
                .context("aggregate preparation allowance overflow")?;
        }
        ensure!(
            self.maximum_prepared_bytes > 0,
            "artifact review needs a prepared-evidence byte ceiling"
        );
        ensure!(
            !self.artifact_kind.trim().is_empty() && !self.files.is_empty(),
            "empty artifact declaration"
        );
        for (file, presentation) in self.files.iter().chain(self.sources.iter()) {
            ensure!(
                !file.is_empty()
                    && !file.contains('\\')
                    && std::path::Path::new(file)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_))),
                "invalid declared artifact path"
            );
            match presentation {
                ArtifactPresentation::Text => {},
                ArtifactPresentation::Image { media_type } => ensure!(
                    matches!(
                        media_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp"
                    ),
                    "unsupported inline image representation"
                ),
                ArtifactPresentation::Video {
                    media_type,
                    sampler,
                    ..
                } => {
                    ensure!(
                        matches!(media_type.as_str(), "video/mp4" | "video/webm"),
                        "unsupported sealed video representation"
                    );
                    ensure!(
                        sampler
                            .maximum_sample_bytes
                            .checked_mul(sampler.timestamps_secs.len() as u64)
                            .is_some_and(|bytes| bytes <= self.maximum_prepared_bytes),
                        "video sample recipe exceeds the prepared-evidence byte ceiling"
                    );
                    prepared_allowance = prepared_allowance
                        .checked_add(
                            sampler
                                .maximum_sample_bytes
                                .checked_mul(sampler.timestamps_secs.len() as u64)
                                .context("video sample allowance overflow")?,
                        )
                        .context("aggregate preparation allowance overflow")?;
                    ensure!(
                        !sampler.timestamps_secs.is_empty()
                            && sampler
                                .timestamps_secs
                                .iter()
                                .all(|time| time.is_finite() && *time >= 0.0)
                            && sampler
                                .timestamps_secs
                                .windows(2)
                                .all(|times| times[0] < times[1])
                            && sampler.maximum_width > 0
                            && sampler.maximum_height > 0
                            && sampler.maximum_sample_bytes > 0
                            && sampler.maximum_sample_bytes < u64::MAX
                            && sampler.timeout_secs > 0
                            && sampler.executable_hash.len() == 64
                            && sampler
                                .executable_hash
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit()),
                        "invalid frozen video sampler"
                    );
                    ensure!(
                        sampler_executable()?.1 == sampler.executable_hash,
                        "artifact sampler executable changed; readmission required"
                    );
                },
                ArtifactPresentation::Audio { media_type, audio } => {
                    ensure!(
                        matches!(media_type.as_str(), "audio/wav" | "audio/mpeg"),
                        "unsupported bounded audio representation"
                    );
                    audio.validate()?;
                },
            }
            match presentation {
                ArtifactPresentation::Video {
                    audio: Some(audio), ..
                }
                | ArtifactPresentation::Audio { audio, .. } => {
                    audio.validate()?;
                    ensure!(
                        audio.maximum_prepared_bytes()? <= self.maximum_prepared_bytes,
                        "audio recipe exceeds prepared evidence ceiling"
                    );
                    prepared_allowance = prepared_allowance
                        .checked_add(audio.maximum_prepared_bytes()?)
                        .context("aggregate preparation allowance overflow")?;
                },
                _ => {},
            }
        }
        ensure!(
            prepared_allowance <= self.maximum_prepared_bytes,
            "combined code, media and audio recipes exceed the prepared evidence ceiling"
        );
        Ok(())
    }

    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(
            blake3::hash(&serde_json::to_vec(&("artifact-review-v1", self))?)
                .to_hex()
                .to_string(),
        )
    }

    pub fn has_images(&self) -> bool {
        self.files
            .values()
            .chain(self.sources.values())
            .any(|view| {
                matches!(
                    view,
                    ArtifactPresentation::Image { .. } | ArtifactPresentation::Video { .. }
                )
            })
    }
    pub fn has_perceptual_evidence(&self) -> bool {
        self.files
            .values()
            .chain(self.sources.values())
            .any(|view| !matches!(view, ArtifactPresentation::Text))
    }
}

pub struct ArtifactReviewInput {
    pub texts: BTreeMap<String, String>,
    pub source_texts: BTreeMap<String, String>,
    pub images: Vec<ContentBlock>,
    pub manifest: serde_json::Value,
}

fn parse_media_facts(
    value: &serde_json::Value,
) -> Result<BTreeMap<magician_relentless::specification::MediaFact, u64>> {
    use magician_relentless::specification::MediaFact;
    let mut facts = BTreeMap::new();
    let streams = value["streams"]
        .as_array()
        .context("media probe has no stream inventory")?;
    ensure!(
        streams
            .iter()
            .all(|stream| stream["codec_type"].as_str().is_some()),
        "media probe stream type is unavailable"
    );
    let video: Vec<_> = streams
        .iter()
        .filter(|stream| stream["codec_type"] == "video")
        .collect();
    facts.insert(MediaFact::VideoStreamCount, video.len() as u64);
    facts.insert(
        MediaFact::AudioStreamCount,
        streams
            .iter()
            .filter(|stream| stream["codec_type"] == "audio")
            .count() as u64,
    );
    if let [stream] = video.as_slice() {
        if let Some(width) = stream["width"].as_u64() {
            facts.insert(MediaFact::VideoWidth, width);
        }
        if let Some(height) = stream["height"].as_u64() {
            facts.insert(MediaFact::VideoHeight, height);
        }
    }
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|duration| duration.parse::<f64>().ok())
        .or_else(|| value["format"]["duration"].as_f64());
    if let Some(duration) = duration {
        let micros = duration * 1_000_000.0;
        ensure!(
            duration.is_finite()
                && duration >= 0.0
                && micros.is_finite()
                && micros < u64::MAX as f64,
            "media probe duration is invalid"
        );
        facts.insert(MediaFact::DurationMicros, micros.round() as u64);
    }
    Ok(facts)
}

/// A typed read of sealed bytes, sharing the controller's private cache and
/// owned process cancellation. No duration is inferred from sampled frames.
pub async fn prepare_media_facts(
    owner_id: &str,
    recipe: &ArtifactReviewRecipe,
    files: &BTreeMap<String, Vec<u8>>,
    artifact_hash: &str,
    store: Arc<RunStore>,
    run_id: Uuid,
    deadline: chrono::DateTime<chrono::Utc>,
    cancel: CancellationToken,
) -> Result<magician_relentless::specification::MediaFacts> {
    recipe.validate()?;
    let Some(probe) = &recipe.media_probe else {
        return Ok(BTreeMap::new());
    };
    let (executable, executable_hash) =
        super::execution::media_edit::ffmpeg::probe_executable_pin()?;
    ensure!(
        executable_hash == probe.executable_hash,
        "media probe executable changed"
    );
    let mut facts = BTreeMap::new();
    for (file, presentation) in &recipe.files {
        let media_type = match presentation {
            ArtifactPresentation::Video { media_type, .. }
            | ArtifactPresentation::Audio { media_type, .. } => media_type,
            _ => continue,
        };
        let bytes = files.get(file).context("media probe artifact is missing")?;
        let input_hash = blake3::hash(bytes).to_hex().to_string();
        let key = blake3::hash(&serde_json::to_vec(&(
            "media-probe-v1",
            probe,
            &input_hash,
            media_type,
        ))?)
        .to_hex()
        .to_string();
        let reader = store.clone();
        let artifact = artifact_hash.to_owned();
        let identity = key.clone();
        let cache = tokio::task::spawn_blocking(move || {
            reader.prepared_evidence(run_id, &artifact, &identity)
        })
        .await??;
        let probed: BTreeMap<magician_relentless::specification::MediaFact, u64> =
            if let Some(cache) = cache {
                ensure!(cache.len() == 1, "media probe cache shape changed");
                serde_json::from_slice(
                    cache
                        .get("facts.json")
                        .context("media probe cache is missing")?,
                )?
            } else {
                ensure!(
                    !cancel.is_cancelled() && deadline > chrono::Utc::now(),
                    "media probe cancelled or expired"
                );
                let directory = Arc::new(tempfile::tempdir()?);
                let input = directory.path().join("sealed-input");
                let output = directory.path().join("probe.json");
                std::fs::write(&input, bytes)?;
                let owner = super::execution::media_edit::scoped::admit_probe(
                    owner_id,
                    directory.path(),
                    probe.maximum_result_bytes,
                    deadline,
                    cancel.clone(),
                    &probe.executable_hash,
                )?;
                let mut args = vec!["-v".into(), "error".into(), "-threads".into(), "1".into()];
                args.extend(super::execution::media_edit::scoped::input_options(
                    media_type,
                )?);
                args.extend([
                    "-show_entries".into(),
                    "format=duration:stream=codec_type,width,height".into(),
                    "-of".into(),
                    "json".into(),
                    "-o".into(),
                    output.display().to_string(),
                    input.display().to_string(),
                ]);
                let run = owner
                    .run_owned(
                        &executable,
                        args,
                        directory.clone(),
                        std::time::Duration::from_secs(probe.timeout_secs),
                    )
                    .await;
                owner.close_and_drain().await;
                run?;
                ensure!(
                    std::fs::metadata(&output)?.len() <= probe.maximum_result_bytes,
                    "media probe exceeds its result byte bound"
                );
                let raw = std::fs::read(&output)?;
                let probed = parse_media_facts(&serde_json::from_slice(&raw)?)?;
                let normalized = serde_json::to_vec(&probed)?;
                ensure!(
                    normalized.len() as u64 <= probe.maximum_result_bytes,
                    "normalized media facts exceed their result byte bound"
                );
                let sealed = BTreeMap::from([("facts.json".into(), normalized)]);
                let writer = store.clone();
                let artifact = artifact_hash.to_owned();
                tokio::task::spawn_blocking(move || {
                    writer.seal_prepared_evidence(run_id, &artifact, &key, &sealed)
                })
                .await??;
                probed
            };
        ensure!(
            serde_json::to_vec(&probed)?.len() as u64 <= probe.maximum_result_bytes,
            "normalized media facts exceed their result byte bound"
        );
        facts.insert(file.clone(), probed);
        ensure!(
            serde_json::to_vec(&facts)?.len() as u64 <= recipe.maximum_prepared_bytes,
            "media facts exceed the prepared-evidence byte ceiling"
        );
    }
    Ok(facts)
}

pub fn bind_actor_pin(
    mut pin: magician_relentless::contract::ModelPin,
    recipe: Option<&ArtifactReviewRecipe>,
) -> Result<magician_relentless::contract::ModelPin> {
    if let Some(recipe) = recipe {
        pin.configuration_hash = blake3::hash(&serde_json::to_vec(&(
            "artifact-actor-v1",
            &pin,
            recipe.hash()?,
        ))?)
        .to_hex()
        .to_string();
    }
    Ok(pin)
}

/// Sample the controller-sealed bytes, publish their immutable cache before
/// provider I/O, then use those same samples for the whole critic panel.
pub async fn prepare_review(
    owner_id: &str,
    recipe: &ArtifactReviewRecipe,
    files: &BTreeMap<String, Vec<u8>>,
    sources: &BTreeMap<String, Vec<u8>>,
    artifact_hash: &str,
    store: Arc<RunStore>,
    run_id: Uuid,
    deadline: chrono::DateTime<chrono::Utc>,
    cancel: CancellationToken,
    holder: &str,
    operation: &magician_relentless::budget::OperationIntent,
) -> Result<ArtifactReviewInput> {
    recipe.validate()?;
    ensure!(
        recipe.files.keys().eq(files.keys()),
        "sealed artifact differs from its declared file set"
    );
    let media_facts = prepare_media_facts(
        owner_id,
        recipe,
        files,
        artifact_hash,
        store.clone(),
        run_id,
        deadline,
        cancel.clone(),
    )
    .await?;
    let recipe_hash = recipe.hash()?;
    let source_hashes = recipe
        .sources
        .keys()
        .map(|id| {
            Ok((
                id.clone(),
                blake3::hash(sources.get(id).context("declared source file missing")?)
                    .to_hex()
                    .to_string(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let cache_hash = blake3::hash(&serde_json::to_vec(&(&recipe_hash, &source_hashes))?)
        .to_hex()
        .to_string();
    let reader = store.clone();
    let artifact = artifact_hash.to_owned();
    let identity = cache_hash.clone();
    let cached =
        tokio::task::spawn_blocking(move || reader.prepared_evidence(run_id, &artifact, &identity))
            .await??;
    let mut samples = BTreeMap::new();
    let mut prepared_bytes = if media_facts.is_empty() {
        0
    } else {
        serde_json::to_vec(&media_facts)?.len() as u64
    };
    prepared_bytes = prepared_bytes
        .checked_add(
            recipe
                .verification
                .as_ref()
                .map_or(0, |verification| verification.maximum_result_bytes),
        )
        .context("prepared verification allowance overflow")?;
    ensure!(
        prepared_bytes <= recipe.maximum_prepared_bytes,
        "verification and metadata exceed aggregate prepared evidence ceiling"
    );
    let mut texts = BTreeMap::new();
    let mut source_texts = sources
        .iter()
        .filter(|(id, _)| !recipe.sources.contains_key(*id))
        .map(|(id, bytes)| Ok((id.clone(), std::str::from_utf8(bytes)?.to_owned())))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut views = Vec::new();
    let inputs = recipe
        .files
        .iter()
        .map(|(file, presentation)| Ok(("artifact", file, presentation, &files[file])))
        .chain(recipe.sources.iter().map(|(id, presentation)| {
            Ok((
                "source",
                id,
                presentation,
                sources.get(id).context("declared source file missing")?,
            ))
        }))
        .collect::<Result<Vec<_>>>()?;
    for (input_kind, file, presentation, bytes) in inputs.iter().copied() {
        ensure!(
            !cancel.is_cancelled() && deadline > chrono::Utc::now(),
            "artifact sampling cancelled or expired"
        );
        let label = serde_json::json!({"kind":input_kind,"id":file});
        let sample_key = format!("{input_kind}/{}", blake3::hash(file.as_bytes()).to_hex());
        match presentation {
            ArtifactPresentation::Text => {
                let target = if input_kind == "source" {
                    &mut source_texts
                } else {
                    &mut texts
                };
                target.insert(file.clone(), std::str::from_utf8(bytes)?.to_owned());
            },
            ArtifactPresentation::Image { media_type } => {
                prepared_bytes = prepared_bytes
                    .checked_add(bytes.len() as u64)
                    .context("prepared evidence byte count overflow")?;
                ensure!(
                    prepared_bytes <= recipe.maximum_prepared_bytes,
                    "inline images exceed the prepared-evidence byte ceiling"
                );
                samples.insert(sample_key.clone(), bytes.clone());
                views.push((
                    sample_key,
                    media_type.clone(),
                    serde_json::json!({"input":label,"coverage":"whole_image"}),
                ));
            },
            ArtifactPresentation::Video {
                media_type,
                sampler,
                ..
            } => {
                let executable = sampler_executable()?.0;
                let directory = Arc::new(tempfile::tempdir()?);
                let input = directory.path().join("sealed-input");
                std::fs::write(&input, bytes)?;
                let media_owner = if cached.is_none() {
                    Some(super::execution::media_edit::scoped::admit(
                        owner_id,
                        directory.path(),
                        sampler.maximum_sample_bytes,
                        deadline,
                        cancel.clone(),
                        &sampler.executable_hash,
                    )?)
                } else {
                    None
                };
                for (index, timestamp) in sampler.timestamps_secs.iter().enumerate() {
                    let name = format!("{sample_key}/{index}.png");
                    let sample = if let Some(cache) = &cached {
                        cache
                            .get(&name)
                            .context("sealed video sample missing")?
                            .clone()
                    } else {
                        let output = directory.path().join(format!("sample-{index}.png"));
                        // Force the admitted container; playlists and network
                        // demuxers are outside this sampler's input capability.
                        let mut args = vec![
                            "-y".into(),
                            "-protocol_whitelist".into(),
                            "file,pipe".into(),
                            "-f".into(),
                            if media_type == "video/mp4" {
                                "mov".into()
                            } else {
                                "matroska".into()
                            },
                        ];
                        if media_type == "video/mp4" {
                            args.extend(["-enable_drefs".into(), "0".into()]);
                        }
                        args.extend([
                            "-ss".into(),
                            timestamp.to_string(),
                            "-i".into(),
                            input.display().to_string(),
                            "-frames:v".into(),
                            "1".into(),
                            "-threads".into(),
                            "1".into(),
                            "-vf".into(),
                            format!(
                                "scale=w={}:h={}:force_original_aspect_ratio=decrease",
                                sampler.maximum_width, sampler.maximum_height
                            ),
                            "-fs".into(),
                            sampler.maximum_sample_bytes.to_string(),
                            output.display().to_string(),
                        ]);
                        let remaining = (deadline - chrono::Utc::now())
                            .to_std()
                            .context("artifact sampling deadline expired")?;
                        media_owner
                            .as_ref()
                            .context("video sampler has no process owner")?
                            .run_owned(
                                &executable,
                                args,
                                directory.clone(),
                                remaining.min(std::time::Duration::from_secs(sampler.timeout_secs)),
                            )
                            .await?;
                        let sample = std::fs::read(&output)?;
                        ensure!(
                            !sample.is_empty()
                                && sample.len() as u64 <= sampler.maximum_sample_bytes,
                            "video sample missing or exceeds its ceiling"
                        );
                        sample
                    };
                    prepared_bytes = prepared_bytes
                        .checked_add(sample.len() as u64)
                        .context("prepared evidence byte count overflow")?;
                    ensure!(
                        prepared_bytes <= recipe.maximum_prepared_bytes,
                        "prepared frames exceed the evidence byte ceiling"
                    );
                    samples.insert(name.clone(), sample);
                    views.push((name, "image/png".into(), serde_json::json!({"input":label,"timestamp_secs":timestamp,"coverage":"declared_frame_only"})));
                }
                if let Some(owner) = media_owner {
                    owner.close_and_drain().await;
                }
            },
            ArtifactPresentation::Audio { .. } => {},
        }
    }
    if let Some(cache) = cached {
        ensure!(cache == samples, "prepared artifact evidence changed");
    } else {
        let writer = store.clone();
        let artifact = artifact_hash.to_owned();
        let identity = cache_hash.clone();
        let sealed = samples.clone();
        tokio::task::spawn_blocking(move || {
            writer.seal_prepared_evidence(run_id, &artifact, &identity, &sealed)
        })
        .await??;
    }
    let mut manifest = views.iter().map(|(name, media_type, location)| serde_json::json!({
        "sample":name, "media_type":media_type, "content_hash":blake3::hash(&samples[name]).to_hex().to_string(), "location":location,
    })).collect::<Vec<_>>();
    for (file, facts) in media_facts {
        manifest.push(
            serde_json::json!({"input":{"kind":"artifact","id":file},"media_facts":facts,
            "coverage":"container_metadata_only","probe":recipe.media_probe}),
        );
    }
    let images = views
        .into_iter()
        .map(|(name, media_type, location)| ContentBlock::Image {
            data: samples[&name].clone(),
            media_type,
            caption: Some(location.to_string()),
        })
        .collect();
    let mut audio_manifest = Vec::new();
    for (input_kind, file, presentation, bytes) in &inputs {
        let (media_type, audio) = match presentation {
            ArtifactPresentation::Video {
                media_type,
                audio: Some(audio),
                ..
            }
            | ArtifactPresentation::Audio { media_type, audio } => (media_type, audio),
            _ => continue,
        };
        prepared_bytes = prepared_bytes
            .checked_add(audio.maximum_prepared_bytes()?)
            .context("audio prepared byte count overflow")?;
        ensure!(
            prepared_bytes <= recipe.maximum_prepared_bytes,
            "audio evidence exceeds aggregate preparation ceiling"
        );
        let label = format!("{input_kind}:{file}");
        audio_manifest.push(
            super::relentless_audio::prepare(
                owner_id,
                audio,
                media_type,
                bytes,
                artifact_hash,
                &label,
                store.clone(),
                run_id,
                holder,
                operation,
                deadline,
                cancel.clone(),
            )
            .await?,
        );
    }
    Ok(ArtifactReviewInput {
        texts,
        source_texts,
        images,
        manifest: serde_json::json!({"artifact_hash":artifact_hash,"recipe_hash":recipe_hash,"source_hashes":source_hashes,"samples":manifest,"audio":audio_manifest}),
    })
}
