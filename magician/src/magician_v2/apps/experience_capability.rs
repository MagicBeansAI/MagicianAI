//! App-safe admission surface for the host experience owners (plan 1.3).
//!
//! The Tutor/App Copilot lanes draw on the host overlay and narrate through
//! the TTS media rail as first-party core paths. Apps must not inherit those
//! raw surfaces: `/host/overlay/draw` is reached from trusted core dispatch
//! only, and the media TTS endpoints accept provider/model/voice selectors
//! an untrusted package has no business naming. Mirroring the browser owner
//! adapter discipline, this module admits two closed experience classes —
//! overlay-draw and narration — as descriptor-level admission contracts
//! only:
//!
//! - overlay-draw bounds its payload with the **unchanged** tutor recipe
//!   validators (`tutor::validate_tutor_draw_payload_shape` and
//!   `tutor::validate_tutor_draw_storyboard_payload`) plus explicit app-side
//!   byte and storyboard-step ceilings, and returns only a receipt — no screen
//!   observation ever flows back to the app;
//! - narration bounds text, characters, rate and per-run spend, exposes only
//!   the provider-agnostic delivery vocabulary (`tts_types` enums), and carries
//!   **no** voice/model/provider field — voice selection stays host-owned.
//!
//! No dispatch consumer is wired in this admission. Every descriptor stays
//! non-dispatchable until a reviewed physical-owner adapter lands, so the
//! capability is admitted vocabulary, not granted execution.
//!
//! Voice-invocation admission was reviewed under the same gate and stopped
//! per plan risk R2: an app-declared wake/invocation phrase cannot be proven
//! collision-free against the core lane grammars (typed marker phrases share
//! the `@` sigil class with `@vibedev`, and spoken phrases share the
//! leading-verb grammar of the core lanes after ASR normalization). The
//! fail-closed default — no app voice invocation — is pinned by a red case
//! in `magician-apps/src/apps/threat_model.rs` and by the tests at the
//! bottom of this module. The full analysis lives in
//! `docs/components/magician/app-interactive-capabilities.md`.

use std::io::Write;

use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;

use super::models::AppDigest;
use crate::magician_v2::media_seam::tts_types::{TtsEmotion, TtsPace, TtsStyle, TtsVoiceMode};

pub const APP_EXPERIENCE_PROFILE_V1: &str = "magician.app-experience.v1";
pub(crate) const APP_OVERLAY_DRAW_IMPLEMENTATION_REVISION: &str =
    "magician.app-overlay-draw-admission.2026-08-26.1";
pub(crate) const APP_NARRATION_IMPLEMENTATION_REVISION: &str =
    "magician.app-narration-admission.2026-08-26.1";
/// Canonical host surface the overlay-draw class rides. The endpoint URL is
/// unchanged from the first-party path; only its admission generalizes.
pub const APP_OVERLAY_DRAW_HOST_SURFACE: &str = "/host/overlay/draw";
/// Canonical host surface the narration class rides: the reviewed TTS
/// provider chain behind `/media/tts/synthesize`, never a raw provider call.
pub const APP_NARRATION_HOST_SURFACE: &str = "media_rail_tts";

/// Hard byte ceiling for one admitted overlay-draw payload. The tutor
/// validators bound structure (shape vocabulary, nesting depth, per-field
/// lengths); this bound caps the whole serialized payload so a hostile
/// package cannot pad a structurally valid storyboard past review size.
pub const APP_OVERLAY_DRAW_PAYLOAD_CEILING: usize = 256 * 1024;
/// Maximum storyboard steps in one admitted overlay-draw payload. The
/// first-party tutor engine has no per-payload step cap; the app admission
/// adds one so a single draw action stays reviewable.
pub const APP_OVERLAY_DRAW_MAX_STORYBOARD_STEPS: usize = 64;
/// Overlay-draw returns a bounded receipt. No pixels, screen content, or
/// observation refs ever flow back to the app.
pub const APP_OVERLAY_DRAW_RESULT_CEILING: u64 = 4 * 1024;

/// Maximum serialized bytes of one narration utterance.
pub const APP_NARRATION_MAX_TEXT_BYTES: usize = 8 * 1024;
/// Maximum characters of one narration utterance.
pub const APP_NARRATION_MAX_TEXT_CHARS: usize = 4_000;
/// Reviewed speech-rate floor in milli-units (0.5x). The rate travels as a
/// JSON number in the schema; admission stores the exact integer millis.
pub const APP_NARRATION_MIN_RATE_MILLIS: i32 = 500;
/// Reviewed speech-rate ceiling in milli-units (2.0x).
pub const APP_NARRATION_MAX_RATE_MILLIS: i32 = 2_000;
/// Maximum emphasized-phrase characters. The emphasis hint is injected into
/// provider instructions by some adapters, so it stays tightly bounded.
pub const APP_NARRATION_MAX_EMPHASIS_CHARS: usize = 160;
/// Default per-run narration utterance ceiling.
pub const APP_NARRATION_MAX_RUN_UTTERANCES: u32 = 32;
/// Default per-run narration character budget — deliberately far below the
/// media endpoint's 32,000-byte per-request cap so one app run cannot
/// monopolize the provider chain.
pub const APP_NARRATION_MAX_RUN_CHARACTERS: usize = 24_000;
/// Narration returns a bounded receipt. Synthesized audio goes to the host
/// speaker; no audio bytes flow back to the app.
pub const APP_NARRATION_RESULT_CEILING: u64 = 4 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppExperienceError {
    #[error("experience admission input is invalid: {0}")]
    InvalidInput(String),
    #[error("experience admission payload exceeds its reviewed bound")]
    BoundExceeded,
    #[error("narration run budget is exhausted")]
    RunBudgetExhausted,
    #[error("experience admission encoding failed")]
    Encoding,
}

/// Bounded admission receipt for one overlay-draw payload. The digest is the
/// review identity of the exact payload; the step count and clear flag are
/// the only content the result projection repeats.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppOverlayDrawAdmission {
    payload_digest: AppDigest,
    storyboard_steps: usize,
    cleared: bool,
}

impl AppOverlayDrawAdmission {
    pub fn payload_digest(&self) -> &AppDigest {
        &self.payload_digest
    }

    pub fn storyboard_steps(&self) -> usize {
        self.storyboard_steps
    }

    pub fn cleared(&self) -> bool {
        self.cleared
    }
}

/// Validate one app-facing overlay-draw payload against the unchanged tutor
/// recipe vocabulary plus the app-side ceilings. The tutor validators stay
/// authoritative and unmodified: every shape type, geometry rule, nesting
/// bound and storyboard contract that gates the first-party screen-draw path
/// gates the app payload identically. Run-scoped entity grounding
/// (`tutor::validate_tutor_draw_payload_for_run`) is deliberately not
/// applied here — it belongs to the consumer's run context and must be added
/// by the reviewed dispatch adapter, not by this admission.
pub fn validate_app_overlay_draw_payload(
    payload: &Value,
) -> Result<AppOverlayDrawAdmission, AppExperienceError> {
    if !serialized_size_within(payload, APP_OVERLAY_DRAW_PAYLOAD_CEILING) {
        return Err(AppExperienceError::BoundExceeded);
    }
    crate::magician_v2::tutor::validate_tutor_draw_payload_shape(payload)
        .map_err(AppExperienceError::InvalidInput)?;
    let steps = crate::magician_v2::tutor::validate_tutor_draw_storyboard_payload(payload)
        .map_err(AppExperienceError::InvalidInput)?;
    if steps.len() > APP_OVERLAY_DRAW_MAX_STORYBOARD_STEPS {
        return Err(AppExperienceError::BoundExceeded);
    }
    // The storyboard validator returns an empty step list only for `clear`
    // payloads; every non-clear payload must carry at least one step.
    let cleared = steps.is_empty();
    let payload_digest =
        AppDigest::blake3_canonical_json(payload).map_err(|_| AppExperienceError::Encoding)?;
    Ok(AppOverlayDrawAdmission {
        payload_digest,
        storyboard_steps: steps.len(),
        cleared,
    })
}

/// Bounded admission receipt for one narration utterance.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppNarrationAdmission {
    text_digest: AppDigest,
    text_characters: usize,
    text_bytes: usize,
    rate_millis: Option<i32>,
}

impl AppNarrationAdmission {
    pub fn text_digest(&self) -> &AppDigest {
        &self.text_digest
    }

    pub fn text_characters(&self) -> usize {
        self.text_characters
    }

    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }

    pub fn rate_millis(&self) -> Option<i32> {
        self.rate_millis
    }
}

/// Closed narration input keys. Anything else — voice, model, provider,
/// format, message routing — is rejected: voice selection stays host-owned
/// and an app can never steer which provider account speaks.
const APP_NARRATION_INPUT_KEYS: [&str; 7] = [
    "text",
    "rate",
    "emotion",
    "style",
    "pace",
    "voice_mode",
    "emphasis",
];

/// Whether a narration string carries a control or invisible
/// format/separator character that would ride into provider instruction
/// strings. ASCII controls alone are too narrow a net: the rest of
/// Unicode's format characters render as nothing or silently steer text
/// direction and flow — the soft hyphen (U+00AD), the Arabic letter mark
/// (U+061C), the zero-width family (U+200B–U+200D), the bidi marks
/// (U+200E/U+200F), the line/paragraph separators (U+2028/U+2029), the
/// bidi embeddings and overrides (U+202A–U+202E), the word joiner and
/// invisible operators (U+2060–U+2064), the bidi isolates (U+2066–U+2069),
/// the BOM (U+FEFF), and the interlinear annotation anchors
/// (U+FFF9–U+FFFB) — so a hostile or careless package could use any of
/// them to smuggle instruction-shaping content past review.
fn narration_text_has_control_or_format_character(text: &str) -> bool {
    text.chars().any(|ch| {
        ch.is_control()
            || matches!(
                ch,
                '\u{00AD}' // soft hyphen
                    | '\u{061C}' // Arabic letter mark
                    | '\u{200B}'..='\u{200F}' // zero-width chars + LRM/RLM bidi marks
                    | '\u{2028}' | '\u{2029}' // line/paragraph separators
                    | '\u{202A}'..='\u{202E}' // bidi embeddings/overrides
                    | '\u{2060}'..='\u{2064}' // word joiner + invisible operators
                    | '\u{2066}'..='\u{2069}' // bidi isolates
                    | '\u{FEFF}' // BOM / zero-width no-break space
                    | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation anchors
            )
    })
}

/// Validate one app-facing narration input. Delivery hints are limited to
/// the provider-agnostic `tts_types` vocabulary; the text is capped in
/// both bytes and characters and may not carry control or invisible
/// format/separator characters (they would otherwise ride into provider
/// instruction strings — see
/// `narration_text_has_control_or_format_character`).
pub fn validate_app_narration_input(
    input: &Value,
) -> Result<AppNarrationAdmission, AppExperienceError> {
    let object = input.as_object().ok_or_else(|| {
        AppExperienceError::InvalidInput("narration input must be an object".to_owned())
    })?;
    for key in object.keys() {
        if !APP_NARRATION_INPUT_KEYS.contains(&key.as_str()) {
            return Err(AppExperienceError::InvalidInput(format!(
                "narration input field `{key}` is not part of the reviewed vocabulary"
            )));
        }
    }
    let text = object.get("text").and_then(Value::as_str).ok_or_else(|| {
        AppExperienceError::InvalidInput("narration input requires `text`".to_owned())
    })?;
    if text.trim().is_empty() {
        return Err(AppExperienceError::InvalidInput(
            "narration text must be non-empty after trimming".to_owned(),
        ));
    }
    if text.len() > APP_NARRATION_MAX_TEXT_BYTES {
        return Err(AppExperienceError::BoundExceeded);
    }
    if text.chars().count() > APP_NARRATION_MAX_TEXT_CHARS {
        return Err(AppExperienceError::BoundExceeded);
    }
    if narration_text_has_control_or_format_character(text) {
        return Err(AppExperienceError::InvalidInput(
            "narration text may not contain control or invisible \
             format/separator characters"
                .to_owned(),
        ));
    }
    let rate_millis = match object.get("rate") {
        None => None,
        Some(Value::Number(rate)) => {
            let rate = rate.as_f64().ok_or_else(|| {
                AppExperienceError::InvalidInput("narration `rate` must be a number".to_owned())
            })?;
            if !rate.is_finite() {
                return Err(AppExperienceError::InvalidInput(
                    "narration `rate` must be finite".to_owned(),
                ));
            }
            let millis = (rate * 1000.0).round() as i64;
            if !(i64::from(APP_NARRATION_MIN_RATE_MILLIS)
                ..=i64::from(APP_NARRATION_MAX_RATE_MILLIS))
                .contains(&millis)
            {
                return Err(AppExperienceError::InvalidInput(format!(
                    "narration `rate` must stay between {} and {}",
                    f64::from(APP_NARRATION_MIN_RATE_MILLIS) / 1000.0,
                    f64::from(APP_NARRATION_MAX_RATE_MILLIS) / 1000.0
                )));
            }
            Some(i32::try_from(millis).map_err(|_| AppExperienceError::Encoding)?)
        },
        Some(_) => {
            return Err(AppExperienceError::InvalidInput(
                "narration `rate` must be a number".to_owned(),
            ))
        },
    };
    validate_narration_delivery_hint::<TtsEmotion>(object, "emotion")?;
    validate_narration_delivery_hint::<TtsStyle>(object, "style")?;
    validate_narration_delivery_hint::<TtsPace>(object, "pace")?;
    validate_narration_delivery_hint::<TtsVoiceMode>(object, "voice_mode")?;
    if let Some(emphasis) = object.get("emphasis") {
        let emphasis = emphasis.as_str().ok_or_else(|| {
            AppExperienceError::InvalidInput("narration `emphasis` must be a string".to_owned())
        })?;
        if emphasis.trim().is_empty() || emphasis.chars().count() > APP_NARRATION_MAX_EMPHASIS_CHARS
        {
            return Err(AppExperienceError::BoundExceeded);
        }
        if narration_text_has_control_or_format_character(emphasis) {
            return Err(AppExperienceError::InvalidInput(
                "narration `emphasis` may not contain control or invisible \
                 format/separator characters"
                    .to_owned(),
            ));
        }
    }
    let text_digest = AppDigest::blake3_canonical_json(&json!({
        "text": text,
        "rate_millis": rate_millis,
        "emotion": object.get("emotion").cloned().unwrap_or(Value::Null),
        "style": object.get("style").cloned().unwrap_or(Value::Null),
        "pace": object.get("pace").cloned().unwrap_or(Value::Null),
        "voice_mode": object.get("voice_mode").cloned().unwrap_or(Value::Null),
        "emphasis": object.get("emphasis").cloned().unwrap_or(Value::Null),
    }))
    .map_err(|_| AppExperienceError::Encoding)?;
    Ok(AppNarrationAdmission {
        text_digest,
        text_characters: text.chars().count(),
        text_bytes: text.len(),
        rate_millis,
    })
}

fn validate_narration_delivery_hint<T>(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<(), AppExperienceError>
where
    T: serde::de::DeserializeOwned,
{
    match object.get(key) {
        None => Ok(()),
        Some(value) => serde_json::from_value::<T>(value.clone())
            .map(|_| ())
            .map_err(|_| {
                AppExperienceError::InvalidInput(format!(
                    "narration `{key}` is outside the provider-agnostic vocabulary"
                ))
            }),
    }
}

/// Fail-closed per-run narration budget. The dispatch adapter owns one
/// budget per app run and must consume before any provider call; exceeding
/// any ceiling rejects the utterance instead of deferring to the provider's
/// own accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppNarrationRunBudget {
    max_utterances: u32,
    max_characters: usize,
    used_utterances: u32,
    used_characters: usize,
}

impl AppNarrationRunBudget {
    pub fn reviewed(
        max_utterances: u32,
        max_characters: usize,
    ) -> Result<Self, AppExperienceError> {
        if max_utterances == 0
            || max_utterances > APP_NARRATION_MAX_RUN_UTTERANCES
            || max_characters == 0
            || max_characters > APP_NARRATION_MAX_RUN_CHARACTERS
        {
            return Err(AppExperienceError::InvalidInput(format!(
                "narration run budget exceeds the reviewed ceiling of {} utterances / {} \
                 characters",
                APP_NARRATION_MAX_RUN_UTTERANCES, APP_NARRATION_MAX_RUN_CHARACTERS,
            )));
        }
        Ok(Self {
            max_utterances,
            max_characters,
            used_utterances: 0,
            used_characters: 0,
        })
    }

    pub fn remaining_utterances(&self) -> u32 {
        self.max_utterances.saturating_sub(self.used_utterances)
    }

    pub fn remaining_characters(&self) -> usize {
        self.max_characters.saturating_sub(self.used_characters)
    }

    /// Consume budget for one admitted utterance. Fails closed on the first
    /// utterance or character that would cross either ceiling.
    pub fn consume(&mut self, characters: usize) -> Result<(), AppExperienceError> {
        let next_utterances = self.used_utterances.checked_add(1);
        let next_characters = self.used_characters.checked_add(characters);
        match (next_utterances, next_characters) {
            (Some(used_utterances), Some(used_characters))
                if used_utterances <= self.max_utterances
                    && used_characters <= self.max_characters =>
            {
                self.used_utterances = used_utterances;
                self.used_characters = used_characters;
                Ok(())
            },
            _ => Err(AppExperienceError::RunBudgetExhausted),
        }
    }
}

pub fn overlay_draw_action_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["shape_json"],
        "properties": {
            "shape_json": {
                "type": "string",
                "minLength": 2,
                "maxLength": APP_OVERLAY_DRAW_PAYLOAD_CEILING,
                "description": "JSON-encoded storyboard shape payload validated by the \
                                unchanged tutor recipe vocabulary and storyboard contract"
            }
        }
    })
}

pub fn overlay_draw_action_result_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "success", "payload_digest", "storyboard_steps", "cleared"],
        "properties": {
            "kind": {"const": "app_overlay_draw"},
            "success": {"type": "boolean"},
            "payload_digest": {"type": "string", "maxLength": 80},
            "storyboard_steps": {
                "type": "integer",
                "minimum": 0,
                "maximum": APP_OVERLAY_DRAW_MAX_STORYBOARD_STEPS
            },
            "cleared": {"type": "boolean"}
        }
    })
}

/// Closed narration input schema. There is intentionally no `voice`, `model`,
/// `provider` or `format` property: voice selection is host-owned and the
/// app can never steer which provider account speaks.
pub fn narration_action_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["text"],
        "properties": {
            "text": {
                "type": "string",
                "minLength": 1,
                "maxLength": APP_NARRATION_MAX_TEXT_CHARS,
                "description": "One narration utterance; capped in characters and bytes, \
                                no control or invisible format/separator characters"
            },
            "rate": {
                "type": "number",
                "minimum": 0.5,
                "maximum": 2.0,
                "description": "Speech rate multiplier clamped to the reviewed band"
            },
            "emotion": {
                "enum": ["neutral", "happy", "excited", "concerned", "apologetic",
                         "confident", "playful", "urgent", "sad", "confused"]
            },
            "style": {"enum": ["casual", "formal", "dramatic", "deadpan", "warm", "clinical"]},
            "pace": {"enum": ["slow", "normal", "fast"]},
            "voice_mode": {"enum": ["default", "whisper", "announcement"]},
            "emphasis": {
                "type": "string",
                "minLength": 1,
                "maxLength": APP_NARRATION_MAX_EMPHASIS_CHARS
            }
        }
    })
}

/// Narration returns a bounded receipt only; synthesized audio goes to the
/// host speaker and never enters the app's result bytes.
pub fn narration_action_result_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "success", "characters"],
        "properties": {
            "kind": {"const": "app_narration"},
            "success": {"type": "boolean"},
            "characters": {
                "type": "integer",
                "minimum": 1,
                "maximum": APP_NARRATION_MAX_TEXT_CHARS
            },
            "rate_millis": {
                "type": "integer",
                "minimum": APP_NARRATION_MIN_RATE_MILLIS,
                "maximum": APP_NARRATION_MAX_RATE_MILLIS
            }
        }
    })
}

/// Stable admission identity of the overlay-draw class: the exact bytes of
/// this admission module and the tutor validators it delegates to. When a
/// dispatch consumer lands, its transport files join this digest so review
/// sees the whole reviewed lowering, exactly like the browser owner.
pub fn app_overlay_draw_admission_digest() -> AppDigest {
    static DIGEST: std::sync::OnceLock<AppDigest> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hash_experience_component(
                &mut hasher,
                "revision",
                APP_OVERLAY_DRAW_IMPLEMENTATION_REVISION.as_bytes(),
            );
            hash_experience_component(
                &mut hasher,
                "apps/experience_capability.rs",
                include_bytes!("experience_capability.rs"),
            );
            hash_experience_component(&mut hasher, "tutor.rs", include_bytes!("../tutor.rs"));
            AppDigest::blake3(hasher.finalize().as_bytes())
        })
        .clone()
}

/// Stable admission identity of the narration class: this admission module
/// plus the provider-agnostic delivery vocabulary it reuses.
pub fn app_narration_admission_digest() -> AppDigest {
    static DIGEST: std::sync::OnceLock<AppDigest> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hash_experience_component(
                &mut hasher,
                "revision",
                APP_NARRATION_IMPLEMENTATION_REVISION.as_bytes(),
            );
            hash_experience_component(
                &mut hasher,
                "apps/experience_capability.rs",
                include_bytes!("experience_capability.rs"),
            );
            hash_experience_component(
                &mut hasher,
                "media_seam/tts_types.rs",
                include_bytes!("../media_seam/tts_types.rs"),
            );
            AppDigest::blake3(hasher.finalize().as_bytes())
        })
        .clone()
}

fn hash_experience_component(hasher: &mut blake3::Hasher, name: &str, bytes: &[u8]) {
    hasher.update(b"magician.app-experience.admission-component.v1\0");
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Reviewed implementation-plan digest for the overlay-draw `draw` action.
/// Binds the exact admission identity, schemas, ceilings and the canonical
/// host surface into one digest the descriptor and package lock carry.
pub fn overlay_draw_action_implementation_plan_digest(
    source_digest: &AppDigest,
) -> Result<AppDigest, AppExperienceError> {
    let input_schema_digest = schema_digest(&overlay_draw_action_input_schema())?;
    let result_schema_digest = schema_digest(&overlay_draw_action_result_schema())?;
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_EXPERIENCE_PROFILE_V1,
        "class": "overlay_draw",
        "action": "draw",
        "source_digest": source_digest,
        "admission_digest": app_overlay_draw_admission_digest(),
        "input_schema_digest": input_schema_digest,
        "result_schema_digest": result_schema_digest,
        "payload_byte_ceiling": APP_OVERLAY_DRAW_PAYLOAD_CEILING,
        "max_storyboard_steps": APP_OVERLAY_DRAW_MAX_STORYBOARD_STEPS,
        "result_byte_ceiling": APP_OVERLAY_DRAW_RESULT_CEILING,
        "host_surface": APP_OVERLAY_DRAW_HOST_SURFACE,
        "returns": "receipt",
        "consumer_wired": false,
    }))
    .map_err(|_| AppExperienceError::Encoding)
}

/// Reviewed implementation-plan digest for the narration `speak` action.
pub fn narration_action_implementation_plan_digest(
    source_digest: &AppDigest,
) -> Result<AppDigest, AppExperienceError> {
    let input_schema_digest = schema_digest(&narration_action_input_schema())?;
    let result_schema_digest = schema_digest(&narration_action_result_schema())?;
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_EXPERIENCE_PROFILE_V1,
        "class": "narration",
        "action": "speak",
        "source_digest": source_digest,
        "admission_digest": app_narration_admission_digest(),
        "input_schema_digest": input_schema_digest,
        "result_schema_digest": result_schema_digest,
        "max_text_bytes": APP_NARRATION_MAX_TEXT_BYTES,
        "max_text_characters": APP_NARRATION_MAX_TEXT_CHARS,
        "rate_millis_band": [
            APP_NARRATION_MIN_RATE_MILLIS,
            APP_NARRATION_MAX_RATE_MILLIS
        ],
        "run_budget_ceiling": [
            APP_NARRATION_MAX_RUN_UTTERANCES,
            APP_NARRATION_MAX_RUN_CHARACTERS
        ],
        "voice_selection": "host_owned",
        "result_byte_ceiling": APP_NARRATION_RESULT_CEILING,
        "host_surface": APP_NARRATION_HOST_SURFACE,
        "returns": "receipt",
        "consumer_wired": false,
    }))
    .map_err(|_| AppExperienceError::Encoding)
}

fn schema_digest(schema: &Value) -> Result<AppDigest, AppExperienceError> {
    AppDigest::blake3_canonical_json(schema).map_err(|_| AppExperienceError::Encoding)
}

struct BoundedSizeWriter {
    written: usize,
    limit: usize,
}

impl Write for BoundedSizeWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.written.checked_add(bytes.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "payload size overflow")
        })?;
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "payload exceeds admission ceiling",
            ));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_size_within(value: &Value, limit: usize) -> bool {
    let mut writer = BoundedSizeWriter { written: 0, limit };
    serde_json::to_writer(&mut writer, value).is_ok()
}

/// The fail-closed voice-invocation answer (plan 1.3, risk R2). This
/// function exists so the stop is a named, reviewable surface instead of an
/// absence: it admits nothing, accepts no phrase input, and exists solely to
/// carry the reviewed verdict that app-declared invocation phrases stay
/// unadmitted because collision with the core wake grammars cannot be
/// excluded at admission time.
pub fn app_voice_invocation_admission_verdict() -> AppVoiceInvocationAdmissionVerdict {
    AppVoiceInvocationAdmissionVerdict {
        admitted: false,
        reason: "app invocation phrases cannot be proven collision-free against the core lane \
                 wake grammars; fail-closed per plan 1.3 risk R2",
    }
}

/// Opaque verdict record. It intentionally has no phrase, surface or
/// namespace field: there is nothing to configure on a stopped class.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct AppVoiceInvocationAdmissionVerdict {
    admitted: bool,
    reason: &'static str,
}

impl AppVoiceInvocationAdmissionVerdict {
    pub fn admitted(&self) -> bool {
        self.admitted
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

/// Resolve the reviewed action ceilings for one experience action name.
/// Returns `None` for any name outside the two admitted leaves so callers
/// can never look up an unreviewed vocabulary entry.
pub fn experience_action_result_ceiling(action_name: &str) -> Option<u64> {
    match action_name {
        "draw" => Some(APP_OVERLAY_DRAW_RESULT_CEILING),
        "speak" => Some(APP_NARRATION_RESULT_CEILING),
        _ => None,
    }
}

/// Resolve the reviewed input schema for one experience action name.
pub fn experience_action_input_schema(action_name: &str) -> Option<Value> {
    match action_name {
        "draw" => Some(overlay_draw_action_input_schema()),
        "speak" => Some(narration_action_input_schema()),
        _ => None,
    }
}

/// Resolve the reviewed result schema for one experience action name.
pub fn experience_action_result_schema(action_name: &str) -> Option<Value> {
    match action_name {
        "draw" => Some(overlay_draw_action_result_schema()),
        "speak" => Some(narration_action_result_schema()),
        _ => None,
    }
}

/// Resolve the reviewed implementation-plan digest for one experience action
/// name against the exact primitive source digest.
pub fn experience_action_implementation_plan_digest(
    action_name: &str,
    source_digest: &AppDigest,
) -> Result<Option<AppDigest>, AppExperienceError> {
    match action_name {
        "draw" => overlay_draw_action_implementation_plan_digest(source_digest).map(Some),
        "speak" => narration_action_implementation_plan_digest(source_digest).map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_storyboard_payload() -> Value {
        json!({
            "type": "group",
            "tutor_step_label": "point at the button",
            "narration": "first press the blue button",
            "shapes": [
                {
                    "type": "arrow",
                    "from_x": 10,
                    "from_y": 10,
                    "to_x": 40,
                    "to_y": 40,
                    "color": "red"
                }
            ]
        })
    }

    #[test]
    fn overlay_draw_admits_a_valid_tutor_storyboard() {
        let admission =
            validate_app_overlay_draw_payload(&valid_storyboard_payload()).expect("admitted");
        assert_eq!(admission.storyboard_steps(), 1);
        assert!(!admission.cleared());
        assert!(admission.payload_digest().as_str().starts_with("blake3:"));
    }

    #[test]
    fn overlay_draw_admits_a_clear_payload_as_a_receipt_only() {
        let admission = validate_app_overlay_draw_payload(&json!({"type": "clear"}))
            .expect("clear is a valid storyboard");
        assert!(admission.cleared());
        assert_eq!(admission.storyboard_steps(), 0);
    }

    #[test]
    fn overlay_draw_rejects_shapes_outside_the_tutor_vocabulary() {
        let error = validate_app_overlay_draw_payload(&json!({"type": "not_a_shape"}))
            .expect_err("unsupported shape type");
        assert!(matches!(error, AppExperienceError::InvalidInput(_)));
    }

    #[test]
    fn overlay_draw_rejects_storyboards_without_step_metadata() {
        // The tutor storyboard contract requires a label and narration on
        // every leaf; the app admission inherits it unchanged.
        let error = validate_app_overlay_draw_payload(&json!({
            "type": "arrow",
            "from_x": 1,
            "from_y": 1,
            "to_x": 2,
            "to_y": 2
        }))
        .expect_err("missing storyboard metadata");
        assert!(matches!(error, AppExperienceError::InvalidInput(_)));
    }

    #[test]
    fn overlay_draw_rejects_payloads_over_the_app_byte_ceiling() {
        // Structurally valid to the tutor validators (the unknown filler key
        // is inert there) but padded past the admission ceiling.
        let mut payload = valid_storyboard_payload();
        payload["filler"] = Value::String("x".repeat(APP_OVERLAY_DRAW_PAYLOAD_CEILING));
        let error = validate_app_overlay_draw_payload(&payload).expect_err("payload byte ceiling");
        assert_eq!(error, AppExperienceError::BoundExceeded);
    }

    #[test]
    fn overlay_draw_rejects_storyboards_over_the_step_ceiling() {
        // Distinct step labels defeat the storyboard validator's semantic
        // dedupe so every leaf counts toward the step ceiling.
        let shapes = (0..=APP_OVERLAY_DRAW_MAX_STORYBOARD_STEPS)
            .map(|index| {
                json!({
                    "type": "label",
                    "text": "step",
                    "tutor_step_label": format!("step {index}"),
                    "narration": "one",
                    "x": index,
                    "y": index
                })
            })
            .collect::<Vec<_>>();
        let payload = json!({
            "type": "group",
            "shapes": shapes
        });
        let error =
            validate_app_overlay_draw_payload(&payload).expect_err("storyboard step ceiling");
        assert_eq!(error, AppExperienceError::BoundExceeded);
    }

    #[test]
    fn overlay_draw_rejects_over_deep_nesting_through_the_tutor_validator() {
        let mut payload = valid_storyboard_payload();
        for _ in 0..16 {
            payload = json!({
                "type": "group",
                "shapes": [payload]
            });
        }
        let error = validate_app_overlay_draw_payload(&payload).expect_err("depth bound");
        assert!(matches!(error, AppExperienceError::InvalidInput(_)));
    }

    #[test]
    fn narration_admits_bounded_input_with_delivery_hints() {
        let admission = validate_app_narration_input(&json!({
            "text": "hello",
            "rate": 1.25,
            "emotion": "happy",
            "style": "casual",
            "pace": "normal",
            "voice_mode": "default",
            "emphasis": "hello"
        }))
        .expect("admitted");
        assert_eq!(admission.text_characters(), 5);
        assert_eq!(admission.rate_millis(), Some(1_250));
    }

    #[test]
    fn narration_rejects_voice_model_and_provider_fields() {
        for field in ["voice", "model", "provider", "format", "message_id"] {
            let mut input = json!({"text": "hello"});
            input[field] = json!("alloy");
            let error =
                validate_app_narration_input(&input).expect_err("voice selection stays host-owned");
            assert!(
                matches!(error, AppExperienceError::InvalidInput(reason) if reason.contains(field)),
                "field {field} must be rejected by name"
            );
        }
    }

    #[test]
    fn narration_rejects_empty_oversized_and_control_character_text() {
        assert!(validate_app_narration_input(&json!({"text": "   "})).is_err());
        let long = "a".repeat(APP_NARRATION_MAX_TEXT_CHARS + 1);
        assert_eq!(
            validate_app_narration_input(&json!({"text": long})),
            Err(AppExperienceError::BoundExceeded)
        );
        // 4,001 two-byte characters stay under the byte cap but cross the
        // character cap, proving both bounds are enforced independently.
        let wide = "é".repeat(APP_NARRATION_MAX_TEXT_CHARS + 1);
        assert_eq!(
            validate_app_narration_input(&json!({"text": wide})),
            Err(AppExperienceError::BoundExceeded)
        );
        assert!(validate_app_narration_input(&json!({"text": "hello\u{7}world"})).is_err());
    }

    /// The control filter is Unicode-aware, not ASCII-only: the full
    /// format-character set — bidi marks, embeddings, and isolates, the
    /// BOM, the line/paragraph separators, the zero-width family, the soft
    /// hyphen, the Arabic letter mark, the word joiner and invisible
    /// operators, and the interlinear annotation anchors — is rejected for
    /// both `text` and `emphasis`; they render as nothing or silently
    /// steer text direction and flow, so none of them may ride into
    /// provider instruction strings. Ordinary CJK, emoji, and accented
    /// text still passes untouched.
    #[test]
    fn narration_rejects_non_ascii_control_and_format_characters() {
        for (name, ch) in [
            ("soft hyphen", '\u{00AD}'),
            ("Arabic letter mark", '\u{061C}'),
            ("bidi mark LRM", '\u{200E}'),
            ("bidi override RLO", '\u{202E}'),
            ("BOM", '\u{FEFF}'),
            ("line separator", '\u{2028}'),
            ("zero-width space", '\u{200B}'),
            ("word joiner", '\u{2060}'),
            ("bidi isolate LRI", '\u{2066}'),
            ("interlinear annotation anchor", '\u{FFF9}'),
        ] {
            let text = format!("hello{ch}world");
            let error = validate_app_narration_input(&json!({ "text": text }))
                .expect_err(&format!("{name} must be rejected in `text`"));
            assert!(
                matches!(error, AppExperienceError::InvalidInput(_)),
                "{name} rejection is an input error"
            );
            let error = validate_app_narration_input(
                &json!({"text": "hello", "emphasis": format!("very{ch}good")}),
            )
            .expect_err(&format!("{name} must be rejected in `emphasis`"));
            assert!(
                matches!(error, AppExperienceError::InvalidInput(_)),
                "{name} rejection is an input error"
            );
        }
        // Ordinary non-ASCII narration is unaffected.
        assert!(validate_app_narration_input(&json!({
            "text": "Expliquons le problème — 你好 🌍 ça va très bien!"
        }))
        .is_ok());
    }

    #[test]
    fn narration_rejects_rates_outside_the_reviewed_band() {
        for rate in [0.25, 2.5, -1.0, 0.0] {
            let error = validate_app_narration_input(&json!({"text": "hi", "rate": rate}))
                .expect_err("rate outside band");
            assert!(matches!(error, AppExperienceError::InvalidInput(_)));
        }
        assert!(validate_app_narration_input(&json!({"text": "hi", "rate": 0.5})).is_ok());
        assert!(validate_app_narration_input(&json!({"text": "hi", "rate": 2.0})).is_ok());
    }

    #[test]
    fn narration_rejects_delivery_hints_outside_the_provider_agnostic_vocabulary() {
        for (field, value) in [
            ("emotion", "furious"),
            ("style", "sarcastic"),
            ("pace", "glacial"),
            ("voice_mode", "shout"),
        ] {
            let mut input = json!({"text": "hi"});
            input[field] = json!(value);
            let error = validate_app_narration_input(&input).expect_err("unknown delivery hint");
            assert!(
                matches!(error, AppExperienceError::InvalidInput(reason) if reason.contains(field)),
                "hint {field} must be rejected by name"
            );
        }
    }

    #[test]
    fn narration_run_budget_fails_closed() {
        let mut budget = AppNarrationRunBudget::reviewed(2, 10).expect("reviewed budget");
        budget.consume(6).expect("first utterance");
        budget.consume(4).expect("second utterance fits exactly");
        assert_eq!(
            budget.consume(1),
            Err(AppExperienceError::RunBudgetExhausted)
        );
        assert_eq!(budget.remaining_utterances(), 0);
        assert_eq!(budget.remaining_characters(), 0);
        // A single utterance larger than the whole budget is rejected rather
        // than partially spent.
        let mut strict = AppNarrationRunBudget::reviewed(4, 8).expect("reviewed budget");
        assert_eq!(
            strict.consume(9),
            Err(AppExperienceError::RunBudgetExhausted)
        );
        assert_eq!(strict.remaining_characters(), 8);
    }

    #[test]
    fn narration_run_budget_review_rejects_ceiling_escalation() {
        assert!(AppNarrationRunBudget::reviewed(0, 100).is_err());
        assert!(AppNarrationRunBudget::reviewed(4, 0).is_err());
        assert!(
            AppNarrationRunBudget::reviewed(APP_NARRATION_MAX_RUN_UTTERANCES + 1, 100).is_err()
        );
        assert!(AppNarrationRunBudget::reviewed(4, APP_NARRATION_MAX_RUN_CHARACTERS + 1).is_err());
    }

    #[test]
    fn experience_schemas_are_closed_and_bounded() {
        for schema in [
            overlay_draw_action_input_schema(),
            overlay_draw_action_result_schema(),
            narration_action_input_schema(),
            narration_action_result_schema(),
        ] {
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            assert!(schema["required"].as_array().is_some_and(|v| !v.is_empty()));
        }
        let narration_schema = narration_action_input_schema();
        let narration_properties = narration_schema["properties"]
            .as_object()
            .expect("closed property set");
        for forbidden in ["voice", "model", "provider", "format"] {
            assert!(
                !narration_properties.contains_key(forbidden),
                "narration schema must not expose `{forbidden}`"
            );
        }
        assert_eq!(
            overlay_draw_action_input_schema()["properties"]["shape_json"]["maxLength"],
            json!(APP_OVERLAY_DRAW_PAYLOAD_CEILING)
        );
    }

    #[test]
    fn admission_digests_are_stable_and_class_scoped() {
        assert_eq!(
            app_overlay_draw_admission_digest(),
            app_overlay_draw_admission_digest()
        );
        assert_ne!(
            app_overlay_draw_admission_digest(),
            app_narration_admission_digest()
        );
    }

    #[test]
    fn implementation_plan_digests_bind_the_admission_identity() {
        let source_digest = AppDigest::blake3(b"experience-primitive-source");
        let draw = overlay_draw_action_implementation_plan_digest(&source_digest)
            .expect("draw plan digest");
        let speak =
            narration_action_implementation_plan_digest(&source_digest).expect("speak plan digest");
        assert_ne!(draw, speak);
        assert_eq!(
            experience_action_implementation_plan_digest("draw", &source_digest),
            Ok(Some(draw))
        );
        assert_eq!(
            experience_action_implementation_plan_digest("speak", &source_digest),
            Ok(Some(speak))
        );
        assert_eq!(
            experience_action_implementation_plan_digest("unreviewed", &source_digest),
            Ok(None)
        );
    }

    #[test]
    fn action_lookups_fail_closed_outside_the_admitted_roster() {
        assert!(experience_action_input_schema("snapshot").is_none());
        assert!(experience_action_result_schema("navigate").is_none());
        assert!(experience_action_result_ceiling("click").is_none());
    }

    #[test]
    fn voice_invocation_stays_fail_closed_and_phrase_free() {
        // The R2 stop is a named verdict, not an absence: nothing about the
        // stopped class is configurable, and every app-shaped utterance and
        // marker falls through the core lane grammars unchanged.
        let verdict = app_voice_invocation_admission_verdict();
        assert!(!verdict.admitted());
        assert!(verdict.reason().contains("R2"));
        // Typed app-marker shapes share the `@` sigil class with core lanes
        // but none of them invoke a lane today: the app namespace does not
        // exist in any grammar.
        for utterance in [
            "@shopping-list add milk",
            "start shopping-list build",
            "open my shopping list app",
            "hey shopping list add milk",
            "@youtube-search cats",
        ] {
            assert!(
                crate::magician_v2::tutor::parse_voice_guided_flow_invocation(utterance).is_none(),
                "app-shaped utterance `{utterance}` must not invoke a core lane"
            );
            assert!(
                crate::magician_v2::chat::invoke_grammar::parse_vibedev_rail_invocation(utterance)
                    .is_none(),
                "app-shaped utterance `{utterance}` must not invoke the VibeDev rail"
            );
            // The agentic policy lane parsers must fall through the same
            // way: an app-shaped marker or spoken phrase selects no
            // FeatureMode, so an app can never hijack a lane invocation.
            assert_eq!(
                crate::magician_v2::execution::agentic::policy_snapshot::parse_leading_feature_invocation(
                    utterance,
                ),
                crate::magician_v2::agents::FeatureMode::None,
                "app-shaped utterance `{utterance}` must not invoke a feature lane"
            );
            assert_eq!(
                crate::magician_v2::execution::agentic::policy_snapshot::parse_leading_feature_marker(
                    utterance,
                ),
                crate::magician_v2::agents::FeatureMode::None,
                "app-shaped marker `{utterance}` must not select a feature mode"
            );
        }
    }
}
