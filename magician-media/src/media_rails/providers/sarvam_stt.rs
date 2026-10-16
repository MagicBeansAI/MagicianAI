//! Sarvam Saaras file speech-to-text adapter for Dictation.
//!
//! `POST /speech-to-text` takes multipart form data and returns JSON
//! `{ "transcript", "language_code", "language_probability" }`. Measured
//! 2026-09-30 against `saaras:v4`:
//!
//! - Auto-detect is `language_code=unknown`; a hint Sarvam does not carry
//!   (`fr`) is sent as `unknown` rather than refused.
//! - The synchronous endpoint refuses audio over 30 seconds with a 400. A
//!   longer WAV clip is cut into pieces of at most 28 seconds, each cut at
//!   the quietest moment after 18 seconds so words are not split, and the
//!   transcripts joined. A compressed clip (web webm, iOS m4a) cannot be
//!   measured without decoding, so it is sent whole first; if Sarvam refuses
//!   it for length, ffmpeg converts it to 16 kHz mono WAV and it is cut the
//!   same way. Without ffmpeg the refusal stands and Dictation moves to its
//!   next provider.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use magician::magician_v2::execution::media_edit::ffmpeg::ffmpeg_bin;
use reqwest::{multipart, Client};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info, warn};

use super::sarvam_speech::{
    parse_wav, sarvam_language, split_wav_at_pauses, SARVAM_AUTH_HEADER,
    SARVAM_SPEECH_DEFAULT_BASE_URL, SARVAM_STT_LANGUAGES,
};
use super::stt::{SttError, SttProvider, SttRequest, SttResponse};

pub const SARVAM_STT_DEFAULT_BASE_URL: &str = SARVAM_SPEECH_DEFAULT_BASE_URL;
pub const SARVAM_STT_PROVIDER_ID: &str = "sarvam-saaras";
pub const SARVAM_STT_DEFAULT_MODEL: &str = "saaras:v4";
const SARVAM_STT_AUTO_LANGUAGE: &str = "unknown";
/// Longest piece sent, under the 30-second synchronous limit.
const SARVAM_STT_PIECE_MAX_SECS: u32 = 28;
/// Earliest point in a piece where a cut may land.
const SARVAM_STT_PIECE_MIN_SECS: u32 = 18;
const SARVAM_STT_TRANSCODE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct SarvamSttProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
    default_language: Option<String>,
}

impl SarvamSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, SARVAM_STT_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("failed to build Sarvam STT HTTP client"),
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: SARVAM_STT_PROVIDER_ID.to_string(),
            label: None,
            default_model: SARVAM_STT_DEFAULT_MODEL.to_string(),
            default_language: None,
        }
    }

    pub fn with_provider_id(mut self, id: impl Into<String>) -> Self {
        self.provider_id = id.into();
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Language used when a request carries no hint. `auto` (or anything
    /// Sarvam does not carry) keeps auto-detect.
    pub fn with_default_language(mut self, language: &str) -> Self {
        self.default_language = sarvam_language(Some(language), SARVAM_STT_LANGUAGES);
        self
    }

    async fn transcribe_piece(
        &self,
        audio: Bytes,
        mime: &str,
        filename: &str,
        model: &str,
        language: &str,
    ) -> Result<SarvamSttResponse, SttError> {
        let audio_part = multipart::Part::bytes(audio.to_vec())
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|error| SttError::BadRequest(format!("invalid content type: {error}")))?;
        let form = multipart::Form::new()
            .text("model", model.to_string())
            .text("language_code", language.to_string())
            .text("mode", "transcribe")
            .part("file", audio_part);
        let url = stt_endpoint(&self.base_url);
        let response = self
            .client
            .post(&url)
            .header(SARVAM_AUTH_HEADER, &self.api_key)
            .timeout(Duration::from_secs(120))
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                error!("[STT:sarvam] transport failure: {error}");
                SttError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[STT:sarvam] upstream rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        response.json().await.map_err(|error| {
            error!("[STT:sarvam] response decode failure: {error}");
            SttError::Transport(format!("decoding response: {error}"))
        })
    }

    async fn transcribe_pieces(
        &self,
        pieces: Vec<Bytes>,
        mime: &str,
        filename: &str,
        model: &str,
        language: &str,
    ) -> Result<JoinedTranscript, SttError> {
        let mut joined = JoinedTranscript::default();
        for piece in pieces {
            let parsed = self
                .transcribe_piece(piece, mime, filename, model, language)
                .await?;
            let text = parsed.transcript.trim();
            if !text.is_empty() {
                joined.transcripts.push(text.to_string());
                if joined.language.is_none() {
                    joined.language = parsed.language_code;
                    joined.probability = parsed.language_probability;
                }
            }
            joined.request_ids.extend(parsed.request_id);
        }
        Ok(joined)
    }
}

#[async_trait]
impl SttProvider for SarvamSttProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        let model = request
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.default_model.clone());
        let language = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|hint| sarvam_language(Some(hint), SARVAM_STT_LANGUAGES))
            .unwrap_or_else(|| self.default_language.clone())
            .unwrap_or_else(|| SARVAM_STT_AUTO_LANGUAGE.to_string());
        let filename = request
            .filename
            .clone()
            .unwrap_or_else(|| filename_for(&request.content_type));
        let mime = request.content_type.trim();
        let mime = if mime.is_empty() {
            "application/octet-stream"
        } else {
            mime
        };
        let is_wav = parse_wav(&request.audio).is_some();
        let pieces = wav_pieces(&request.audio).unwrap_or_else(|| vec![request.audio.clone()]);

        info!(
            "[STT:sarvam] transcribing model={} language={} bytes={} pieces={} content_type={}",
            model,
            language,
            request.audio.len(),
            pieces.len(),
            mime
        );
        let joined = match self
            .transcribe_pieces(pieces, mime, &filename, &model, &language)
            .await
        {
            Err(rejection) if !is_wav && is_duration_rejection(&rejection) => {
                let wav = match transcode_to_wav(&request.audio, &filename).await {
                    Ok(wav) => wav,
                    Err(reason) => {
                        warn!("[STT:sarvam] could not convert a long clip to WAV: {reason}");
                        return Err(rejection);
                    },
                };
                let wav = Bytes::from(wav);
                let pieces = wav_pieces(&wav).unwrap_or_else(|| vec![wav]);
                info!(
                    "[STT:sarvam] long {} clip converted to WAV pieces={}",
                    mime,
                    pieces.len()
                );
                self.transcribe_pieces(pieces, "audio/wav", "audio.wav", &model, &language)
                    .await?
            },
            other => other?,
        };

        let mut extras = serde_json::Map::new();
        if !joined.request_ids.is_empty() {
            extras.insert("request_ids".into(), json!(joined.request_ids));
        }
        if let Some(probability) = joined.probability {
            extras.insert("language_probability".into(), json!(probability));
        }
        Ok(SttResponse {
            transcript: joined.transcripts.join(" "),
            model,
            language: joined.language.or(request.language),
            message_id: request.message_id,
            extras: (!extras.is_empty()).then(|| serde_json::Value::Object(extras)),
        })
    }
}

#[derive(Debug, Default)]
struct JoinedTranscript {
    transcripts: Vec<String>,
    language: Option<String>,
    probability: Option<f64>,
    request_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SarvamSttResponse {
    #[serde(default)]
    transcript: String,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    language_probability: Option<f64>,
    #[serde(default)]
    request_id: Option<String>,
}

/// Cut a WAV clip over the piece limit at its pauses. `None` when the audio
/// is not a WAV or already fits.
fn wav_pieces(audio: &Bytes) -> Option<Vec<Bytes>> {
    let wav = parse_wav(audio)?;
    let pieces = split_wav_at_pauses(&wav, SARVAM_STT_PIECE_MIN_SECS, SARVAM_STT_PIECE_MAX_SECS)?;
    Some(pieces.into_iter().map(Bytes::from).collect())
}

/// Sarvam's 400 for a clip over the synchronous limit reads "Audio duration
/// exceeds the maximum limit of 30 seconds".
fn is_duration_rejection(error: &SttError) -> bool {
    matches!(
        error,
        SttError::Upstream { status: 400, body } if body.to_ascii_lowercase().contains("duration")
    )
}

/// Decode any container ffmpeg reads into 16 kHz mono 16-bit WAV, the rate
/// Sarvam recommends. Files, not pipes: m4a keeps its index at the end, which
/// ffmpeg cannot seek to on stdin.
async fn transcode_to_wav(audio: &[u8], filename: &str) -> Result<Vec<u8>, String> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or("audio");
    let stem = format!("magician-sarvam-stt-{}", uuid::Uuid::new_v4().simple());
    let input = std::env::temp_dir().join(format!("{stem}.{ext}"));
    let output = std::env::temp_dir().join(format!("{stem}.wav"));
    let result = run_transcode(audio, &input, &output).await;
    let _ = tokio::fs::remove_file(&input).await;
    let _ = tokio::fs::remove_file(&output).await;
    result
}

async fn run_transcode(audio: &[u8], input: &PathBuf, output: &PathBuf) -> Result<Vec<u8>, String> {
    tokio::fs::write(input, audio)
        .await
        .map_err(|error| format!("writing {}: {error}", input.display()))?;
    let child = tokio::process::Command::new(ffmpeg_bin())
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawning ffmpeg: {error}"))?;
    let finished = tokio::time::timeout(SARVAM_STT_TRANSCODE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "ffmpeg timed out".to_string())?
        .map_err(|error| format!("waiting on ffmpeg: {error}"))?;
    if !finished.status.success() {
        return Err(format!(
            "ffmpeg exited with {}: {}",
            finished.status,
            String::from_utf8_lossy(&finished.stderr).trim()
        ));
    }
    tokio::fs::read(output)
        .await
        .map_err(|error| format!("reading {}: {error}", output.display()))
}

fn stt_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/speech-to-text") {
        base.to_string()
    } else {
        format!("{base}/speech-to-text")
    }
}

fn filename_for(content_type: &str) -> String {
    let ext = match content_type.to_ascii_lowercase().as_str() {
        value if value.contains("wav") => "wav",
        value if value.contains("mpeg") || value.contains("mp3") => "mp3",
        value if value.contains("ogg") || value.contains("opus") => "ogg",
        value if value.contains("flac") => "flac",
        value if value.contains("mp4") || value.contains("m4a") => "m4a",
        value if value.contains("webm") => "webm",
        value if value.contains("aac") => "aac",
        _ => "wav",
    };
    format!("audio.{ext}")
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    use super::super::sarvam_speech::test_wav;
    use super::*;

    fn request(audio: Bytes, language: Option<&str>) -> SttRequest {
        SttRequest {
            audio,
            content_type: "audio/wav".into(),
            language: language.map(str::to_string),
            model: None,
            message_id: None,
            filename: None,
            prompt: None,
        }
    }

    fn form_field(request: &Request, name: &str) -> Option<String> {
        let body = String::from_utf8_lossy(&request.body);
        let marker = format!("name=\"{name}\"\r\n\r\n");
        let start = body.find(&marker)? + marker.len();
        let end = body[start..].find("\r\n")? + start;
        Some(body[start..end].to_string())
    }

    #[tokio::test]
    async fn transcribe_posts_multipart_and_reads_transcript() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .and(header("api-subscription-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "request_id": "req-1",
                "transcript": "नमस्ते Magician",
                "language_code": "hi-IN",
                "language_probability": 0.97,
            })))
            .mount(&server)
            .await;

        let provider = SarvamSttProvider::with_base_url("test-key", server.uri())
            .with_provider_id("sarvam-saaras")
            .with_label("Sarvam · Saaras v4");
        let response = provider
            .transcribe(request(Bytes::from(test_wav(16_000, 160)), None))
            .await
            .unwrap();
        assert_eq!(response.transcript, "नमस्ते Magician");
        assert_eq!(response.model, SARVAM_STT_DEFAULT_MODEL);
        assert_eq!(response.language.as_deref(), Some("hi-IN"));
        assert_eq!(
            response.extras,
            Some(json!({ "request_ids": ["req-1"], "language_probability": 0.97 }))
        );

        let sent = &server.received_requests().await.unwrap()[0];
        assert_eq!(form_field(sent, "model").as_deref(), Some("saaras:v4"));
        assert_eq!(
            form_field(sent, "language_code").as_deref(),
            Some("unknown")
        );
        assert_eq!(form_field(sent, "mode").as_deref(), Some("transcribe"));
    }

    #[tokio::test]
    async fn language_hints_map_to_sarvam_codes_or_auto_detect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "transcript": "ok" })))
            .mount(&server)
            .await;

        let provider =
            SarvamSttProvider::with_base_url("test-key", server.uri()).with_default_language("ta");
        let audio = Bytes::from(test_wav(16_000, 16));
        for hint in [Some("hi"), Some("fr"), None] {
            provider
                .transcribe(request(audio.clone(), hint))
                .await
                .unwrap();
        }
        let sent = server.received_requests().await.unwrap();
        let languages = sent
            .iter()
            .map(|request| form_field(request, "language_code").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(languages, vec!["hi-IN", "unknown", "ta-IN"]);
    }

    #[tokio::test]
    async fn long_wav_is_cut_into_pieces_and_transcripts_joined() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transcript": "piece",
                "language_code": "en-IN",
            })))
            .expect(3)
            .mount(&server)
            .await;

        let provider = SarvamSttProvider::with_base_url("test-key", server.uri());
        // 60 silent seconds: every moment is equally quiet, so each cut lands
        // at the first allowed point, 18 s into the piece: 18 + 18 + 24.
        let audio = Bytes::from(test_wav(8_000, 8_000 * 60));
        let response = provider.transcribe(request(audio, None)).await.unwrap();
        assert_eq!(response.transcript, "piece piece piece");
        assert_eq!(response.language.as_deref(), Some("en-IN"));
    }

    #[test]
    fn short_or_non_wav_audio_is_sent_whole() {
        assert!(wav_pieces(&Bytes::from(test_wav(8_000, 8_000 * 28))).is_none());
        assert!(wav_pieces(&Bytes::from_static(b"OggS-not-wav")).is_none());
        let pieces = wav_pieces(&Bytes::from(test_wav(8_000, 8_000 * 29))).unwrap();
        assert_eq!(pieces.len(), 2);
    }

    /// Matches a request whose raw body contains `needle`. Multipart bodies
    /// carry binary audio, so string matchers cannot read them.
    struct BodyHas(&'static [u8]);

    impl Match for BodyHas {
        fn matches(&self, request: &Request) -> bool {
            request
                .body
                .windows(self.0.len())
                .any(|window| window == self.0)
        }
    }

    /// A 40-second 440 Hz tone as AAC in m4a, the container iOS uploads.
    fn long_m4a() -> Vec<u8> {
        let dir = std::env::temp_dir();
        let out = dir.join(format!(
            "sarvam-stt-test-{}.m4a",
            uuid::Uuid::new_v4().simple()
        ));
        let status = std::process::Command::new(ffmpeg_bin())
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=40",
            ])
            .args(["-c:a", "aac"])
            .arg(&out)
            .status()
            .expect("ffmpeg must be installed (make setup-ffmpeg)");
        assert!(status.success(), "ffmpeg could not make the test m4a");
        let bytes = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        bytes
    }

    #[tokio::test]
    async fn long_compressed_clip_is_converted_to_wav_and_cut() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .and(BodyHas(b"filename=\"audio.wav\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transcript": "piece",
                "language_code": "en-IN",
            })))
            .expect(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .and(BodyHas(b"filename=\"voice.m4a\""))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"error":{"message":"Audio duration exceeds the maximum limit of 30 seconds."}}"#,
            ))
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let provider = SarvamSttProvider::with_base_url("test-key", server.uri());
        let mut request = request(Bytes::from(long_m4a()), None);
        request.content_type = "audio/m4a".into();
        request.filename = Some("voice.m4a".into());
        let response = provider.transcribe(request).await.unwrap();
        assert_eq!(response.transcript, "piece piece");
    }

    #[tokio::test]
    async fn long_clip_ffmpeg_cannot_read_keeps_the_rejection() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/speech-to-text"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("Audio duration exceeds the maximum limit of 30 seconds."),
            )
            .mount(&server)
            .await;

        let provider = SarvamSttProvider::with_base_url("test-key", server.uri());
        let mut request = request(Bytes::from_static(b"OggS"), None);
        request.content_type = "audio/ogg".into();
        let error = provider.transcribe(request).await.unwrap_err();
        assert!(matches!(error, SttError::Upstream { status: 400, .. }));
    }

    #[tokio::test]
    async fn empty_audio_is_rejected_before_the_request() {
        let provider = SarvamSttProvider::with_base_url("test-key", "https://example.invalid");
        let error = provider
            .transcribe(request(Bytes::new(), None))
            .await
            .unwrap_err();
        assert!(matches!(error, SttError::BadRequest(_)));
    }

    #[test]
    fn endpoint_accepts_a_host_or_a_full_url() {
        assert_eq!(
            stt_endpoint("https://api.sarvam.ai/"),
            "https://api.sarvam.ai/speech-to-text"
        );
        assert_eq!(
            stt_endpoint("https://example.invalid/speech-to-text"),
            "https://example.invalid/speech-to-text"
        );
    }

    #[tokio::test]
    #[ignore = "calls api.sarvam.ai; set SARVAM_API_KEY and SARVAM_STT_LIVE_WAV"]
    async fn live_transcribe_spoken_wav() {
        let key = std::env::var("SARVAM_API_KEY").expect("SARVAM_API_KEY");
        let path = std::env::var("SARVAM_STT_LIVE_WAV").expect("SARVAM_STT_LIVE_WAV");
        let audio = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
        let provider = SarvamSttProvider::new(key);
        let response = provider
            .transcribe(request(Bytes::from(audio), None))
            .await
            .expect("live transcribe");
        assert!(
            response
                .transcript
                .to_ascii_lowercase()
                .contains("magician"),
            "transcript was {:?}",
            response.transcript
        );
    }
}
