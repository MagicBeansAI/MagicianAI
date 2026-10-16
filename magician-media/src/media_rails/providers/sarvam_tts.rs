//! Sarvam Bulbul text-to-speech adapter.
//!
//! `POST /text-to-speech` takes JSON and returns `{ "audios": [base64] }`.
//! Measured 2026-09-30 against `bulbul:v3`:
//!
//! - `language_code` is required and has no `auto`, so the adapter picks it
//!   from the text's dominant Indic script and otherwise uses the configured
//!   default (`en-IN`). Bulbul reads code-mixed text in that language.
//! - `text` is capped at 2500 characters (1500 on `bulbul:v2`). A longer
//!   reply is split at sentence boundaries, synthesized piece by piece, and
//!   joined, which is why only concatenable formats (`mp3`, `wav`, `pcm`)
//!   are offered.
//! - Speakers are per model: `bulbul:v3` refuses the `bulbul:v2` voices and
//!   the reverse. The advertised list and default follow the model, and a
//!   voice that belongs to the other model is swapped for the model's
//!   default rather than sent to fail. V4 Flash uses persona IDs instead of
//!   short names; its published catalog lives in `sarvam_bulbul_v4`.
//! - `pace` is 0.5–2.0 on v3 (0.3–3.0 on v2); out of range is a 400.
//! - There is no whisper or emotion control; those hints are ignored.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use super::sarvam_speech::{
    build_wav, parse_wav, sarvam_language, tts_language_for_text, SARVAM_AUTH_HEADER,
    SARVAM_SPEECH_DEFAULT_BASE_URL, SARVAM_TTS_LANGUAGES,
};
use super::tts::{TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse};

pub const SARVAM_TTS_DEFAULT_BASE_URL: &str = SARVAM_SPEECH_DEFAULT_BASE_URL;
pub const SARVAM_TTS_PROVIDER_ID: &str = "sarvam";
pub const SARVAM_TTS_DEFAULT_MODEL: &str = "bulbul:v3";
pub const SARVAM_TTS_DEFAULT_VOICE: &str = "shubh";
pub const SARVAM_TTS_V4_FLASH_DEFAULT_VOICE: &str = "shubh_enhi_ads";
const SARVAM_TTS_V2_DEFAULT_VOICE: &str = "anushka";
pub const SARVAM_TTS_DEFAULT_FORMAT: &str = "mp3";
pub const SARVAM_TTS_DEFAULT_LANGUAGE: &str = "en-IN";
const SARVAM_TTS_SAMPLE_RATE: u32 = 24_000;
const SARVAM_TTS_MAX_CHARS: usize = 2500;
const SARVAM_TTS_V2_MAX_CHARS: usize = 1500;
const SARVAM_TTS_FORMATS: &[&str] = &["mp3", "wav", "pcm"];
const SARVAM_TTS_V3_VOICES: &[&str] = &[
    "shubh", "aditya", "ritu", "priya", "neha", "rahul", "pooja", "rohan", "simran", "kavya",
    "amit", "dev", "ishita", "shreya", "ratan", "varun", "manan", "sumit", "roopa", "kabir",
    "aayan", "ashutosh", "advait", "anand", "tanya", "tarun", "sunny", "mani", "gokul", "vijay",
    "shruti", "suhani", "mohit", "kavitha", "rehan", "soham", "rupali",
];
const SARVAM_TTS_V2_VOICES: &[&str] = &[
    "anushka", "manisha", "vidya", "arya", "abhilash", "karun", "hitesh",
];

/// Default speaker for each supported Bulbul generation.
pub fn sarvam_tts_default_voice(model: &str) -> &'static str {
    if is_v2(model) {
        SARVAM_TTS_V2_DEFAULT_VOICE
    } else if is_v4_flash(model) {
        SARVAM_TTS_V4_FLASH_DEFAULT_VOICE
    } else {
        SARVAM_TTS_DEFAULT_VOICE
    }
}

#[derive(Debug, Clone)]
pub struct SarvamTtsProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    default_voice: String,
    default_format: String,
    default_language: String,
    /// Operator-configured voices. Empty advertises the default model's
    /// built-in speakers.
    configured_voices: Vec<String>,
    formats: Vec<String>,
}

impl SarvamTtsProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, SARVAM_TTS_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("failed to build Sarvam TTS HTTP client"),
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_model: SARVAM_TTS_DEFAULT_MODEL.to_string(),
            default_voice: SARVAM_TTS_DEFAULT_VOICE.to_string(),
            default_format: SARVAM_TTS_DEFAULT_FORMAT.to_string(),
            default_language: SARVAM_TTS_DEFAULT_LANGUAGE.to_string(),
            configured_voices: Vec::new(),
            formats: SARVAM_TTS_FORMATS
                .iter()
                .map(|format| (*format).to_string())
                .collect(),
        }
    }

    pub fn with_defaults(
        mut self,
        model: impl Into<String>,
        voice: impl Into<String>,
        format: impl Into<String>,
    ) -> Self {
        self.default_model = model.into();
        self.default_voice = voice_for_model(voice.into().trim(), &self.default_model);
        self.default_format = format.into();
        self
    }

    /// Language for text with no Indic script. Ignored unless it is one
    /// Bulbul reads.
    pub fn with_default_language(mut self, language: &str) -> Self {
        if is_v4_flash(&self.default_model)
            && matches!(
                language.trim().to_ascii_lowercase().as_str(),
                "as" | "as-in"
            )
        {
            self.default_language = "as-IN".to_string();
        } else if let Some(language) = sarvam_language(Some(language), SARVAM_TTS_LANGUAGES) {
            self.default_language = language;
        }
        self
    }

    pub fn with_voices(mut self, voices: Vec<String>) -> Self {
        let voices = voices
            .into_iter()
            .map(|voice| voice.trim().to_string())
            .filter(|voice| !voice.is_empty())
            .filter(|voice| voice_for_model(voice, &self.default_model) == *voice)
            .collect::<Vec<_>>();
        if !voices.is_empty() {
            self.configured_voices = voices;
        }
        self
    }

    pub fn with_formats(mut self, formats: Vec<String>) -> Self {
        let formats = formats
            .into_iter()
            .map(|format| format.trim().to_string())
            .filter(|format| codec_for(format).is_some())
            .collect::<Vec<_>>();
        if !formats.is_empty() {
            self.formats = formats;
        }
        self
    }

    async fn synthesize_chunk(&self, body: &Value) -> Result<Vec<u8>, TtsError> {
        let url = tts_endpoint(&self.base_url);
        let response = self
            .client
            .post(&url)
            .header(SARVAM_AUTH_HEADER, &self.api_key)
            .timeout(Duration::from_secs(120))
            .json(body)
            .send()
            .await
            .map_err(|error| {
                error!("[TTS:sarvam] transport failure: {error}");
                TtsError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[TTS:sarvam] upstream rejected status={} body={}",
                status, body
            );
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: SarvamTtsResponse = response.json().await.map_err(|error| {
            error!("[TTS:sarvam] response decode failure: {error}");
            TtsError::Transport(format!("decoding response: {error}"))
        })?;
        let mut audio = Vec::new();
        for encoded in parsed.audios {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|error| TtsError::Upstream {
                    status: status.as_u16(),
                    body: format!("audio is not base64: {error}"),
                })?;
            audio.extend_from_slice(&decoded);
        }
        if audio.is_empty() {
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body: "empty audio".into(),
            });
        }
        Ok(audio)
    }
}

#[derive(Debug, Deserialize)]
struct SarvamTtsResponse {
    #[serde(default)]
    audios: Vec<String>,
}

#[async_trait]
impl TtsProvider for SarvamTtsProvider {
    fn id(&self) -> &str {
        SARVAM_TTS_PROVIDER_ID
    }

    fn default_voice(&self) -> Option<&str> {
        Some(&self.default_voice)
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn default_format(&self) -> Option<&str> {
        Some(&self.default_format)
    }

    fn supported_voices(&self) -> Vec<String> {
        if !self.configured_voices.is_empty() {
            return self.configured_voices.clone();
        }
        match builtin_voices(&self.default_model) {
            Some(voices) => voices
                .iter()
                .filter(|voice| {
                    !voice.contains("_as_")
                        || self.default_language == "as-IN"
                        || **voice == self.default_voice
                })
                .map(|voice| (*voice).to_string())
                .collect(),
            None => vec![self.default_voice.clone()],
        }
    }

    fn supported_formats(&self) -> Vec<String> {
        self.formats.clone()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let text = request.text.trim();
        if text.is_empty() {
            return Err(TtsError::BadRequest("empty text".into()));
        }

        let model = request
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.default_model.clone());
        let voice = voice_for_model(
            request
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|voice| !voice.is_empty())
                .unwrap_or(&self.default_voice),
            &model,
        );
        // An unsupported format falls back to the default, and the response
        // carries the real content type.
        let codec = request
            .format
            .as_deref()
            .and_then(codec_for)
            .or_else(|| codec_for(&self.default_format))
            .unwrap_or(Codec::Mp3);
        // Marathi and Hindi share Devanagari. A selected Marathi persona
        // provides the narrower hint instead of silently normalizing as Hindi.
        let language_hint = if is_v4_flash(&model) {
            v4_voice_language(&voice)
                .filter(|language| matches!(*language, "mr-IN" | "as-IN"))
                .unwrap_or(&self.default_language)
        } else {
            &self.default_language
        };
        let language = tts_language_for_text(text, language_hint);
        let chunks = split_text(text, max_chars_for(&model));

        info!(
            "[TTS:sarvam] synthesizing model={} voice={} language={} codec={:?} chunks={}",
            model,
            voice,
            language,
            codec,
            chunks.len()
        );
        let mut pieces = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            let body = json!({
                "text": chunk,
                "language_code": language,
                "model": model,
                "speaker": voice,
                "pace": pace_for(&request, &model),
                "speech_sample_rate": SARVAM_TTS_SAMPLE_RATE,
                "output_audio_codec": codec.wire_name(),
            });
            pieces.push(self.synthesize_chunk(&body).await?);
        }
        let audio = join_audio(codec, pieces)?;
        Ok(TtsResponse {
            audio: audio.into(),
            content_type: codec.content_type().to_string(),
            model,
            voice: Some(voice),
            message_id: request.message_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Mp3,
    Wav,
    Pcm,
}

impl Codec {
    fn wire_name(self) -> &'static str {
        match self {
            Codec::Mp3 => "mp3",
            Codec::Wav => "wav",
            Codec::Pcm => "linear16",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Codec::Mp3 => "audio/mpeg",
            Codec::Wav => "audio/wav",
            Codec::Pcm => "audio/pcm",
        }
    }
}

fn codec_for(format: &str) -> Option<Codec> {
    match format.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mpeg" | "audio/mpeg" => Some(Codec::Mp3),
        "wav" | "audio/wav" => Some(Codec::Wav),
        "pcm" | "linear16" | "s16le" | "raw" | "audio/pcm" => Some(Codec::Pcm),
        _ => None,
    }
}

/// MP3 frames and raw PCM concatenate as-is; WAV pieces are re-wrapped under
/// one header.
fn join_audio(codec: Codec, mut pieces: Vec<Vec<u8>>) -> Result<Vec<u8>, TtsError> {
    if pieces.len() == 1 {
        return Ok(pieces.remove(0));
    }
    if codec != Codec::Wav {
        return Ok(pieces.concat());
    }
    let parsed = pieces
        .iter()
        .map(|piece| parse_wav(piece))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| TtsError::Upstream {
            status: 200,
            body: "Sarvam returned a WAV piece that does not parse".into(),
        })?;
    let data = parsed
        .iter()
        .flat_map(|wav| wav.data.iter().copied())
        .collect::<Vec<_>>();
    Ok(build_wav(parsed[0].fmt, &data))
}

fn is_v2(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("bulbul:v2")
}

fn is_v3(model: &str) -> bool {
    let model = model.trim();
    model.eq_ignore_ascii_case("bulbul:v3") || model.eq_ignore_ascii_case("bulbul:v3-beta")
}

fn is_v4_flash(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("bulbul:v4-flash")
}

fn v4_voice_language(voice: &str) -> Option<&'static str> {
    let mut parts = voice.splitn(3, '_');
    parts.next().filter(|name| !name.is_empty())?;
    let tag = parts.next()?;
    parts.next().filter(|style| !style.is_empty())?;
    match tag {
        "as" => Some("as-IN"),
        "en" => Some("en-IN"),
        "hi" => Some("hi-IN"),
        "bn" => Some("bn-IN"),
        "gu" => Some("gu-IN"),
        "kn" => Some("kn-IN"),
        "mr" => Some("mr-IN"),
        "pa" => Some("pa-IN"),
        "ta" => Some("ta-IN"),
        "te" => Some("te-IN"),
        // A bilingual persona uses the configured language as the tie-break.
        _ => None,
    }
}

fn is_v4_persona(voice: &str) -> bool {
    v4_voice_language(voice).is_some() || super::sarvam_bulbul_v4::VOICES.contains(&voice)
}

/// Built-in speakers of a model this adapter knows.
fn builtin_voices(model: &str) -> Option<&'static [&'static str]> {
    if is_v2(model) {
        Some(SARVAM_TTS_V2_VOICES)
    } else if is_v3(model) {
        Some(SARVAM_TTS_V3_VOICES)
    } else if is_v4_flash(model) {
        Some(super::sarvam_bulbul_v4::VOICES)
    } else {
        None
    }
}

/// `voice`, unless it is a speaker of the other Bulbul generation, which
/// `model` would refuse; then the model's default speaker. A name in neither
/// list (a speaker Sarvam added later) is sent as asked.
fn voice_for_model(voice: &str, model: &str) -> String {
    if voice.is_empty() {
        return sarvam_tts_default_voice(model).to_string();
    }
    let in_list = |voices: &[&str]| voices.iter().any(|name| name.eq_ignore_ascii_case(voice));
    let incompatible = if is_v2(model) {
        in_list(SARVAM_TTS_V3_VOICES) || is_v4_persona(voice)
    } else if is_v3(model) {
        in_list(SARVAM_TTS_V2_VOICES) || is_v4_persona(voice)
    } else if is_v4_flash(model) {
        in_list(SARVAM_TTS_V2_VOICES) || in_list(SARVAM_TTS_V3_VOICES)
    } else {
        false
    };
    if incompatible {
        let fallback = sarvam_tts_default_voice(model);
        warn!("[TTS:sarvam] voice {voice} is not a {model} speaker; using {fallback}");
        fallback.to_string()
    } else {
        voice.to_string()
    }
}

fn max_chars_for(model: &str) -> usize {
    if is_v2(model) {
        SARVAM_TTS_V2_MAX_CHARS
    } else {
        SARVAM_TTS_MAX_CHARS
    }
}

fn pace_for(request: &TtsRequest, model: &str) -> f32 {
    let pace = request
        .rate
        .unwrap_or(match request.pace.unwrap_or_default() {
            TtsPace::Slow => 0.85,
            TtsPace::Normal => 1.0,
            TtsPace::Fast => 1.25,
        });
    if is_v2(model) {
        pace.clamp(0.3, 3.0)
    } else {
        pace.clamp(0.5, 2.0)
    }
}

/// Split `text` into pieces of at most `max` characters, preferring sentence
/// ends, then whitespace, and cutting mid-word only when a single word is
/// longer than `max`.
fn split_text(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > max {
        let window_end = rest
            .char_indices()
            .nth(max)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let window = &rest[..window_end];
        let cut = last_break(window, |ch| {
            matches!(ch, '.' | '!' | '?' | '।' | '॥' | '\n')
        })
        .or_else(|| last_break(window, char::is_whitespace))
        .unwrap_or(window_end);
        let (head, tail) = rest.split_at(cut);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_string());
        }
        rest = tail.trim_start();
    }
    if !rest.is_empty() {
        chunks.push(rest.to_string());
    }
    chunks
}

/// Byte index just past the last `is_break` char in `window`, when that
/// leaves a non-empty head.
fn last_break(window: &str, is_break: impl Fn(char) -> bool) -> Option<usize> {
    window
        .char_indices()
        .filter(|(_, ch)| is_break(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .filter(|end| window[..*end].trim().chars().count() > 1)
        .last()
}

fn tts_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/text-to-speech") {
        base.to_string()
    } else {
        format!("{base}/text-to-speech")
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::sarvam_speech::test_wav;
    use super::*;

    fn request(text: &str) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
            voice: None,
            rate: None,
            model: None,
            format: None,
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }

    fn audio_response(bytes: &[u8]) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-1",
            "audios": [base64::engine::general_purpose::STANDARD.encode(bytes)],
        }))
    }

    #[test]
    fn provider_metadata_advertises_bulbul_v3_voices() {
        let provider = SarvamTtsProvider::new("test-key");
        assert_eq!(provider.id(), "sarvam");
        assert_eq!(provider.default_model(), "bulbul:v3");
        assert_eq!(provider.default_voice(), Some("shubh"));
        assert_eq!(provider.default_format(), Some("mp3"));
        assert!(provider.supported_voices().contains(&"priya".to_string()));
        assert_eq!(provider.supported_formats(), vec!["mp3", "wav", "pcm"]);
    }

    #[test]
    fn configured_formats_drop_ones_that_cannot_be_joined() {
        let provider =
            SarvamTtsProvider::new("test-key").with_formats(vec!["wav".into(), "opus".into()]);
        assert_eq!(provider.supported_formats(), vec!["wav"]);
    }

    #[test]
    fn v4_metadata_advertises_the_published_personas_and_normalizes_legacy_defaults() {
        let provider =
            SarvamTtsProvider::new("test-key").with_defaults("bulbul:v4-flash", "shubh", "mp3");
        assert_eq!(provider.default_voice(), Some("shubh_enhi_ads"));
        let voices = provider.supported_voices();
        assert_eq!(voices.len(), 222);
        assert!(voices.contains(&"shubh_enhi_ads".into()));
        assert!(voices.contains(&"simran_en_customer".into()));
        assert!(voices.contains(&"arnab_bn_conversation".into()));
        assert!(!voices.contains(&"shubh".into()));
        assert!(voices.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!provider.supports_streaming());
        let configured = provider.with_voices(vec!["shubh".into(), "aparna_hi_customer".into()]);
        assert_eq!(configured.supported_voices(), vec!["aparna_hi_customer"]);
    }

    #[test]
    fn v4_personas_do_not_leak_into_older_models() {
        for voice in ["shubh_enhi_ads", "aparna_hi_customer", "future_en_customer"] {
            assert_eq!(voice_for_model(voice, "bulbul:v3"), "shubh");
            assert_eq!(voice_for_model(voice, "bulbul:v2"), "anushka");
        }
        assert_eq!(
            voice_for_model("new_en_persona", "bulbul:v4-flash"),
            "new_en_persona"
        );
        let unknown =
            SarvamTtsProvider::new("unused").with_defaults("future-model", "future-voice", "mp3");
        assert_eq!(unknown.supported_voices(), vec!["future-voice"]);
    }

    #[tokio::test]
    async fn v4_posts_persona_model_and_retains_base64_audio_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(header("api-subscription-key", "test-key"))
            .and(body_json(json!({
                "text":"hello", "model":"bulbul:v4-flash", "speaker":"shubh_enhi_ads",
                "language_code":"en-IN", "pace":1.0, "speech_sample_rate":24000,
                "output_audio_codec":"mp3"
            })))
            .respond_with(audio_response(b"v4-mp3"))
            .expect(1)
            .mount(&server)
            .await;
        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "bulbul:v4-flash",
            "shubh",
            "mp3",
        );
        let mut request = request("hello");
        request.voice = Some("priya".into()); // Saved v3 selection after switching models.
        let response = provider.synthesize(request).await.unwrap();
        assert_eq!(response.audio.as_ref(), b"v4-mp3");
        assert_eq!(response.voice.as_deref(), Some("shubh_enhi_ads"));
        assert_eq!(response.model, "bulbul:v4-flash");
    }

    #[tokio::test]
    async fn v4_marathi_persona_preserves_marathi_for_shared_devanagari_script() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(body_partial_json(
                json!({"language_code":"mr-IN", "speaker":"ishita_mr_conversational"}),
            ))
            .respond_with(audio_response(b"mp3"))
            .expect(1)
            .mount(&server)
            .await;
        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "bulbul:v4-flash",
            "shubh_enhi_ads",
            "mp3",
        );
        let mut request = request("नमस्कार, तुम्ही कसे आहात?");
        request.voice = Some("ishita_mr_conversational".into());
        provider.synthesize(request).await.unwrap();
    }

    #[tokio::test]
    async fn v4_cross_language_voice_does_not_erase_configured_marathi_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(body_partial_json(
                json!({"language_code":"mr-IN", "speaker":"simran_en_customer"}),
            ))
            .respond_with(audio_response(b"mp3"))
            .expect(1)
            .mount(&server)
            .await;
        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri())
            .with_defaults("bulbul:v4-flash", "simran_en_customer", "mp3")
            .with_default_language("mr-IN");
        provider
            .synthesize(request("नमस्कार, तुम्ही कसे आहात?"))
            .await
            .unwrap();
    }

    #[test]
    fn shipped_config_exposes_v4_on_each_existing_sarvam_tts_surface() {
        let config: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../../magician-config.yaml")).unwrap();
        let providers = config["media"]["tts"]["providers"].as_sequence().unwrap();
        let v4 = providers
            .iter()
            .find(|p| p["id"].as_str() == Some("sarvam-bulbul-v4-flash"))
            .unwrap();
        assert_eq!(v4["model"].as_str(), Some("bulbul:v4-flash"));
        assert_eq!(
            v4["voice"].as_str(),
            Some(SARVAM_TTS_V4_FLASH_DEFAULT_VOICE)
        );
        assert!(v4["voices"].is_null(), "use the adapter's full catalog");
        let languages = v4["language_codes"].as_sequence().unwrap();
        assert_eq!(languages.len(), 9);
        assert!(!languages
            .iter()
            .any(|v| matches!(v.as_str(), Some("ml-IN" | "od-IN"))));
        assert!(providers
            .iter()
            .any(|p| p["id"].as_str() == Some("sarvam-bulbul")
                && p["model"].as_str() == Some("bulbul:v3")));
        let mut checked = 0;
        for (_, profile) in config["media"]["surface_profiles"]["profiles"]
            .as_mapping()
            .unwrap()
        {
            if let Some(options) = profile["tts"]["providers"].as_sequence() {
                if options.iter().any(|p| p.as_str() == Some("sarvam-bulbul")) {
                    assert!(options
                        .iter()
                        .any(|p| p.as_str() == Some("sarvam-bulbul-v4-flash")));
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 3);
    }

    #[tokio::test]
    #[ignore = "four billable Sarvam TTS calls; requires SARVAM_API_KEY"]
    async fn live_v4_personas_generate_decodable_audio_and_v3_remains_available() {
        let key = std::env::var("SARVAM_API_KEY").expect("SARVAM_API_KEY required");
        for (model, voice, text, language) in [
            (
                "bulbul:v4-flash",
                "shubh_enhi_ads",
                "Hello. Your meeting starts in ten minutes.",
                "en-IN",
            ),
            (
                "bulbul:v4-flash",
                "aparna_hi_customer",
                "नमस्ते। आपकी बैठक दस मिनट में शुरू होगी।",
                "hi-IN",
            ),
            (
                "bulbul:v4-flash",
                "arnab_bn_conversation",
                "নমস্কার। আপনার মিটিং দশ মিনিট পরে শুরু হবে।",
                "bn-IN",
            ),
            (
                "bulbul:v3",
                "shubh",
                "Hello. Bulbul version three is still available.",
                "en-IN",
            ),
        ] {
            let start = std::time::Instant::now();
            let provider = SarvamTtsProvider::new(key.clone()).with_defaults(model, voice, "wav");
            let response = provider
                .synthesize(request(text))
                .await
                .expect("live synthesis succeeds");
            let wav = parse_wav(&response.audio).expect("provider returns a valid WAV");
            assert!(wav.is_pcm16());
            assert!(
                wav.data.len() > 2400,
                "audio must contain more than a header"
            );
            assert_eq!(u32::from_le_bytes(wav.fmt[4..8].try_into().unwrap()), 24000);
            assert_eq!(response.voice.as_deref(), Some(voice));
            if let Ok(directory) = std::env::var("SARVAM_TTS_V4_LIVE_DIR") {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(
                    std::path::Path::new(&directory).join(format!(
                        "{}-{voice}-{language}.wav",
                        model.replace(':', "-")
                    )),
                    &response.audio,
                )
                .unwrap();
            }
            println!(
                "{model} / {voice}: {} bytes, {} ms",
                response.audio.len(),
                start.elapsed().as_millis()
            );
        }
    }

    #[tokio::test]
    async fn synthesize_posts_bulbul_json_and_decodes_base64_audio() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(header("api-subscription-key", "test-key"))
            .and(body_json(json!({
                "text": "hello",
                "language_code": "en-IN",
                "model": "bulbul:v3",
                "speaker": "shubh",
                "pace": 1.0,
                "speech_sample_rate": 24000,
                "output_audio_codec": "mp3",
            })))
            .respond_with(audio_response(b"mp3-bytes"))
            .mount(&server)
            .await;

        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri());
        let response = provider.synthesize(request("hello")).await.unwrap();
        assert_eq!(response.audio, Bytes::from_static(b"mp3-bytes"));
        assert_eq!(response.content_type, "audio/mpeg");
        assert_eq!(response.model, "bulbul:v3");
        assert_eq!(response.voice.as_deref(), Some("shubh"));
    }

    #[tokio::test]
    async fn devanagari_text_is_spoken_as_hindi_and_pace_is_clamped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(body_partial_json(json!({
                "language_code": "hi-IN",
                "speaker": "priya",
                "pace": 2.0,
                "output_audio_codec": "linear16",
            })))
            .respond_with(audio_response(b"pcm"))
            .mount(&server)
            .await;

        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri());
        let mut request = request("आज की मीटिंग 5 बजे है");
        request.voice = Some("priya".into());
        request.rate = Some(3.0);
        request.format = Some("pcm".into());
        let response = provider.synthesize(request).await.unwrap();
        assert_eq!(response.content_type, "audio/pcm");
    }

    #[tokio::test]
    async fn long_wav_reply_is_split_and_joined_under_one_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .respond_with(audio_response(&test_wav(24_000, 10)))
            .expect(2)
            .mount(&server)
            .await;

        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri());
        let sentence = "This sentence is repeated to pass the limit. ";
        let mut request = request(&sentence.repeat(60));
        request.format = Some("wav".into());
        let response = provider.synthesize(request).await.unwrap();
        assert_eq!(response.content_type, "audio/wav");
        let joined = parse_wav(&response.audio).expect("joined wav parses");
        assert_eq!(joined.data.len(), 40);
    }

    #[tokio::test]
    async fn upstream_rejection_is_an_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri());
        let error = provider.synthesize(request("hello")).await.unwrap_err();
        assert!(matches!(error, TtsError::Upstream { status: 403, .. }));
    }

    #[tokio::test]
    async fn empty_text_is_rejected_before_the_request() {
        let provider = SarvamTtsProvider::with_base_url("test-key", "https://example.invalid");
        let error = provider.synthesize(request("  ")).await.unwrap_err();
        assert!(matches!(error, TtsError::BadRequest(_)));
    }

    #[test]
    fn split_prefers_sentence_ends_then_whitespace() {
        assert_eq!(split_text("short", 10), vec!["short"]);
        assert_eq!(
            split_text("One two. Three four five.", 12),
            vec!["One two.", "Three four", "five."]
        );
        assert_eq!(split_text("नमस्ते। आप कैसे हैं", 10), vec!["नमस्ते।", "आप कैसे", "हैं"]);
        assert_eq!(split_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        for chunk in split_text(&"word ".repeat(700), 2500) {
            assert!(chunk.chars().count() <= 2500);
        }
    }

    #[test]
    fn voices_follow_the_model() {
        let v2 = SarvamTtsProvider::new("test-key").with_defaults(
            "bulbul:v2",
            sarvam_tts_default_voice("bulbul:v2"),
            "mp3",
        );
        assert_eq!(v2.default_voice(), Some("anushka"));
        assert_eq!(v2.supported_voices().len(), SARVAM_TTS_V2_VOICES.len());
        assert!(v2.supported_voices().contains(&"hitesh".to_string()));

        assert_eq!(voice_for_model("shubh", "bulbul:v2"), "anushka");
        assert_eq!(voice_for_model("Anushka", "bulbul:v3"), "shubh");
        assert_eq!(voice_for_model("priya", "bulbul:v3"), "priya");
        assert_eq!(voice_for_model("newvoice", "bulbul:v3"), "newvoice");
        assert_eq!(
            voice_for_model("anushka", "bulbul:v4-flash"),
            SARVAM_TTS_V4_FLASH_DEFAULT_VOICE
        );
    }

    #[tokio::test]
    async fn a_v3_voice_on_a_v2_model_is_swapped_for_the_v2_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/text-to-speech"))
            .and(body_partial_json(json!({
                "model": "bulbul:v2",
                "speaker": "anushka",
            })))
            .respond_with(audio_response(b"mp3"))
            .expect(1)
            .mount(&server)
            .await;

        // Config switched the model to v2 but kept the v3 default voice.
        let provider = SarvamTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "bulbul:v2",
            "shubh",
            "mp3",
        );
        let response = provider.synthesize(request("hello")).await.unwrap();
        assert_eq!(response.voice.as_deref(), Some("anushka"));
    }

    #[test]
    fn v2_keeps_its_own_limits() {
        assert_eq!(max_chars_for("bulbul:v2"), 1500);
        assert_eq!(max_chars_for("bulbul:v3"), 2500);
        assert_eq!(max_chars_for("bulbul:v4-flash"), 2500);
        let mut slow = request("x");
        slow.rate = Some(0.4);
        assert_eq!(pace_for(&slow, "bulbul:v2"), 0.4);
        assert_eq!(pace_for(&slow, "bulbul:v3"), 0.5);
        assert_eq!(pace_for(&slow, "bulbul:v4-flash"), 0.5);
    }

    #[test]
    fn endpoint_accepts_a_host_or_a_full_url() {
        assert_eq!(
            tts_endpoint("https://api.sarvam.ai/"),
            "https://api.sarvam.ai/text-to-speech"
        );
        assert_eq!(
            tts_endpoint("https://example.invalid/text-to-speech"),
            "https://example.invalid/text-to-speech"
        );
    }

    /// A reply over the 2500-character limit is synthesized in two pieces;
    /// the joined MP3 must decode cleanly and, transcribed back through
    /// Saaras (over 30 s, so via the ffmpeg path), still carry the words on
    /// both sides of the join.
    #[tokio::test]
    #[ignore = "calls api.sarvam.ai; set SARVAM_API_KEY; needs ffmpeg"]
    async fn live_long_reply_joins_cleanly_and_round_trips() {
        use magician::magician_v2::execution::media_edit::ffmpeg::{ffmpeg_bin, ffprobe_bin};

        use super::super::stt::SttProvider as _;

        const NOUNS: &[&str] = &[
            "apple", "bridge", "candle", "desert", "engine", "forest", "garden", "harbor",
            "island", "jacket", "kettle", "ladder", "market", "needle", "orange", "pencil",
            "rabbit", "saddle", "tomato", "violin", "window", "yellow", "anchor", "basket",
            "castle", "dragon", "feather", "guitar", "hammer", "jungle", "kitten", "lemon",
            "mirror", "napkin", "parrot", "pillow", "rocket", "silver", "tunnel", "valley",
            "walnut", "zipper", "bottle", "camera", "dollar", "falcon", "ginger", "helmet",
        ];
        let text = NOUNS
            .iter()
            .map(|noun| format!("The next word on the list is {noun}, and it is spoken clearly."))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = split_text(&text, SARVAM_TTS_MAX_CHARS);
        assert_eq!(chunks.len(), 2, "text should need exactly one join");
        let noun_in = |sentence: &str| {
            NOUNS
                .iter()
                .find(|noun| sentence.contains(&format!(" {noun},")))
                .copied()
                .expect("sentence has a noun")
        };
        let before_join = noun_in(chunks[0].rsplit_terminator('.').next().unwrap());
        let after_join = noun_in(chunks[1].split_terminator('.').next().unwrap());

        let key = std::env::var("SARVAM_API_KEY").expect("SARVAM_API_KEY");
        let response = SarvamTtsProvider::new(key.clone())
            .synthesize(request(&text))
            .await
            .expect("live synthesize");
        assert_eq!(response.content_type, "audio/mpeg");

        let path = std::env::temp_dir().join(format!(
            "sarvam-long-reply-{}.mp3",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &response.audio).unwrap();
        let decode = std::process::Command::new(ffmpeg_bin())
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg");
        let probe = std::process::Command::new(ffprobe_bin())
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe");
        let _ = std::fs::remove_file(&path);
        let decode_errors = String::from_utf8_lossy(&decode.stderr);
        assert!(
            decode.status.success() && decode_errors.trim().is_empty(),
            "joined mp3 has decode errors: {decode_errors}"
        );
        let secs: f64 = String::from_utf8_lossy(&probe.stdout)
            .trim()
            .parse()
            .unwrap();
        assert!(
            secs > 30.0,
            "reply is only {secs}s; the STT leg would not cut it"
        );

        let transcript = super::super::sarvam_stt::SarvamSttProvider::new(key)
            .transcribe(crate::media_rails::SttRequest {
                audio: response.audio.clone(),
                content_type: "audio/mpeg".into(),
                language: Some("en".into()),
                model: None,
                message_id: None,
                filename: Some("reply.mp3".into()),
                prompt: None,
            })
            .await
            .expect("live transcribe of the joined reply")
            .transcript
            .to_ascii_lowercase();
        let heard = NOUNS
            .iter()
            .filter(|noun| transcript.contains(**noun))
            .count();
        eprintln!(
            "reply {secs:.1}s, {} bytes; heard {heard}/{} words; join between {before_join} and {after_join}",
            response.audio.len(),
            NOUNS.len()
        );
        for noun in [NOUNS[0], before_join, after_join, NOUNS[NOUNS.len() - 1]] {
            assert!(
                transcript.contains(noun),
                "{noun:?} missing from {transcript:?}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "calls api.sarvam.ai; set SARVAM_API_KEY"]
    async fn live_synthesize_hindi() {
        let key = std::env::var("SARVAM_API_KEY").expect("SARVAM_API_KEY");
        let provider = SarvamTtsProvider::new(key);
        let response = provider
            .synthesize(request("नमस्ते, मैं Magician हूँ।"))
            .await
            .expect("live synthesize");
        assert_eq!(response.content_type, "audio/mpeg");
        assert!(response.audio.len() > 1000);
    }
    #[test]
    fn gated_assamese_voices_require_explicit_configuration() {
        let provider = SarvamTtsProvider::new("test-key").with_defaults(
            "bulbul:v4-flash",
            "shubh_enhi_ads",
            "wav",
        );
        assert_eq!(super::super::sarvam_bulbul_v4::VOICES.len(), 224);
        assert_eq!(provider.supported_voices().len(), 222);
        assert!(!provider
            .supported_voices()
            .iter()
            .any(|voice| voice.contains("_as_")));
        let beta = provider.with_default_language("as-IN");
        assert_eq!(beta.supported_voices().len(), 224);
        assert_eq!(
            v4_voice_language("kangkana_as_conversational"),
            Some("as-IN")
        );
        assert_eq!(
            voice_for_model("kangkana_as_conversational", "bulbul:v3"),
            "shubh"
        );
    }
}
