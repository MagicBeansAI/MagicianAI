//! Built-in speakable voices for Magician realtime / Live engines.
//!
//! These are the vendor ids each transport accepts at session start.
//! A missing or unknown preference falls back to the profile YAML / provider default.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeVoiceChoice {
    pub id: &'static str,
    pub label: &'static str,
}

const OPENAI_REALTIME_VOICES: &[RealtimeVoiceChoice] = &[
    RealtimeVoiceChoice {
        id: "marin",
        label: "Marin — natural, conversational",
    },
    RealtimeVoiceChoice {
        id: "cedar",
        label: "Cedar — natural",
    },
    RealtimeVoiceChoice {
        id: "alloy",
        label: "Alloy — neutral",
    },
    RealtimeVoiceChoice {
        id: "ash",
        label: "Ash — warm, mid-range",
    },
    RealtimeVoiceChoice {
        id: "ballad",
        label: "Ballad — softer, narrative",
    },
    RealtimeVoiceChoice {
        id: "coral",
        label: "Coral — calm, grounded",
    },
    RealtimeVoiceChoice {
        id: "echo",
        label: "Echo — neutral, mid-range",
    },
    RealtimeVoiceChoice {
        id: "sage",
        label: "Sage — calm, measured",
    },
    RealtimeVoiceChoice {
        id: "shimmer",
        label: "Shimmer — warm, friendly",
    },
    RealtimeVoiceChoice {
        id: "verse",
        label: "Verse — expressive",
    },
];

// https://developers.openai.com/api/reference/resources/live/methods/create
const OPENAI_LIVE_VOICES: &[RealtimeVoiceChoice] = &[
    RealtimeVoiceChoice {
        id: "alloy",
        label: "Alloy",
    },
    RealtimeVoiceChoice {
        id: "ash",
        label: "Ash",
    },
    RealtimeVoiceChoice {
        id: "ballad",
        label: "Ballad",
    },
    RealtimeVoiceChoice {
        id: "beacon",
        label: "Beacon",
    },
    RealtimeVoiceChoice {
        id: "bossa",
        label: "Bossa",
    },
    RealtimeVoiceChoice {
        id: "brise",
        label: "Brise",
    },
    RealtimeVoiceChoice {
        id: "cedar",
        label: "Cedar",
    },
    RealtimeVoiceChoice {
        id: "cinder",
        label: "Cinder",
    },
    RealtimeVoiceChoice {
        id: "coral",
        label: "Coral",
    },
    RealtimeVoiceChoice {
        id: "delta",
        label: "Delta",
    },
    RealtimeVoiceChoice {
        id: "echo",
        label: "Echo",
    },
    RealtimeVoiceChoice {
        id: "flitz",
        label: "Flitz",
    },
    RealtimeVoiceChoice {
        id: "gleam",
        label: "Gleam",
    },
    RealtimeVoiceChoice {
        id: "harema",
        label: "Harema",
    },
    RealtimeVoiceChoice {
        id: "juni",
        label: "Juni",
    },
    RealtimeVoiceChoice {
        id: "marin",
        label: "Marin",
    },
    RealtimeVoiceChoice {
        id: "meridian",
        label: "Meridian",
    },
    RealtimeVoiceChoice {
        id: "nira",
        label: "Nira",
    },
    RealtimeVoiceChoice {
        id: "noeul",
        label: "Noeul",
    },
    RealtimeVoiceChoice {
        id: "nuri",
        label: "Nuri",
    },
    RealtimeVoiceChoice {
        id: "quartz",
        label: "Quartz",
    },
    RealtimeVoiceChoice {
        id: "ripple",
        label: "Ripple",
    },
    RealtimeVoiceChoice {
        id: "sage",
        label: "Sage",
    },
    RealtimeVoiceChoice {
        id: "shida",
        label: "Shida",
    },
    RealtimeVoiceChoice {
        id: "shimmer",
        label: "Shimmer",
    },
    RealtimeVoiceChoice {
        id: "sillage",
        label: "Sillage",
    },
    RealtimeVoiceChoice {
        id: "stone",
        label: "Stone",
    },
    RealtimeVoiceChoice {
        id: "tempo",
        label: "Tempo",
    },
    RealtimeVoiceChoice {
        id: "verse",
        label: "Verse",
    },
    RealtimeVoiceChoice {
        id: "vesper",
        label: "Vesper",
    },
    RealtimeVoiceChoice {
        id: "willow",
        label: "Willow",
    },
];

fn grok_choices() -> &'static [RealtimeVoiceChoice] {
    static CHOICES: std::sync::OnceLock<Vec<RealtimeVoiceChoice>> = std::sync::OnceLock::new();
    CHOICES
        .get_or_init(|| {
            crate::speech_voices::GROK_VOICES
                .iter()
                .map(|(id, label)| RealtimeVoiceChoice { id, label })
                .collect()
        })
        .as_slice()
}

fn gemini_choices(extended: bool) -> &'static [RealtimeVoiceChoice] {
    static STUDIO: std::sync::OnceLock<Vec<RealtimeVoiceChoice>> = std::sync::OnceLock::new();
    static EXTENDED: std::sync::OnceLock<Vec<RealtimeVoiceChoice>> = std::sync::OnceLock::new();
    let cache = if extended { &EXTENDED } else { &STUDIO };
    cache
        .get_or_init(|| {
            crate::speech_voices::gemini_voices(extended)
                .iter()
                .map(|id| RealtimeVoiceChoice { id, label: id })
                .collect()
        })
        .as_slice()
}

pub fn voices_for_realtime_provider(provider: &str) -> &'static [RealtimeVoiceChoice] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai_realtime"
        | "openai-realtime"
        | "openai_realtime_backend"
        | "openai-realtime-backend" => OPENAI_REALTIME_VOICES,
        "openai_live" | "openai-live" | "gpt_live" | "gpt-live" => OPENAI_LIVE_VOICES,
        "gemini_live" | "gemini-live" => gemini_choices(false),
        "grok_voice" | "grok-voice" | "xai_voice" => grok_choices(),
        _ => &[],
    }
}

/// Gemini 3.8 Live accepts extended IDs; 3.1 rejects them with close code 1007.
/// Keep discovery, preference validation, and session setup on the same roster.
pub fn voices_for_realtime_model(provider: &str, model: &str) -> &'static [RealtimeVoiceChoice] {
    if model.to_ascii_lowercase().contains("translate") {
        return &[];
    }
    if matches!(
        provider.trim().to_ascii_lowercase().as_str(),
        "gemini_live" | "gemini-live"
    ) && model.to_ascii_lowercase().contains("3.8")
    {
        gemini_choices(true)
    } else {
        voices_for_realtime_provider(provider)
    }
}

pub fn canonical_realtime_voice_for_model(
    provider: &str,
    model: &str,
    voice: &str,
) -> Option<String> {
    voices_for_realtime_model(provider, model)
        .iter()
        .find(|choice| choice.id.eq_ignore_ascii_case(voice.trim()))
        .map(|choice| choice.id.to_string())
}

pub fn voice_is_valid_for_realtime_provider(provider: &str, voice: &str) -> bool {
    let trimmed = voice.trim();
    if trimmed.is_empty() {
        return false;
    }
    voices_for_realtime_provider(provider)
        .iter()
        .any(|choice| choice.id.eq_ignore_ascii_case(trimmed))
}

pub fn canonical_realtime_voice(provider: &str, voice: &str) -> Option<String> {
    let trimmed = voice.trim();
    voices_for_realtime_provider(provider)
        .iter()
        .find(|choice| choice.id.eq_ignore_ascii_case(trimmed))
        .map(|choice| choice.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_and_gemini_catalogs_are_nonempty() {
        assert!(voices_for_realtime_provider("openai_realtime").len() >= 8);
        assert!(voices_for_realtime_provider("openai_live")
            .iter()
            .any(|v| v.id == "marin"));
        assert!(voice_is_valid_for_realtime_provider("gemini_live", "kore"));
        assert!(voice_is_valid_for_realtime_provider("grok_voice", "Eve"));
        assert_eq!(
            canonical_realtime_voice("grok_voice", "ARA").as_deref(),
            Some("ara")
        );
        assert_eq!(
            canonical_realtime_voice("gemini_live", "puck").as_deref(),
            Some("Puck")
        );
        assert!(!voice_is_valid_for_realtime_provider(
            "openai_realtime",
            "Puck"
        ));
    }
    #[test]
    fn published_grok_and_gemini_studio_catalogs_are_complete() {
        assert_eq!(voices_for_realtime_provider("grok_voice").len(), 28);
        assert!(voice_is_valid_for_realtime_provider("grok_voice", "altair"));
        assert_eq!(voices_for_realtime_provider("gemini_live").len(), 30);
        assert!(voice_is_valid_for_realtime_provider(
            "gemini_live",
            "Sulafat"
        ));
    }
    #[test]
    fn gemini_extended_voices_are_model_specific() {
        assert_eq!(
            voices_for_realtime_model("gemini_live", "gemini-3.8-live").len(),
            2089
        );
        assert_eq!(
            voices_for_realtime_model("gemini_live", "gemini-3.8-live-extended-thinking").len(),
            2089
        );
        assert_eq!(
            voices_for_realtime_model("gemini_live", "gemini-3.1-flash-live-preview").len(),
            30
        );
        assert_eq!(
            canonical_realtime_voice_for_model("gemini_live", "gemini-3.8-live", "EN-US-ADVISOR-1")
                .as_deref(),
            Some("en-us-advisor-1")
        );
        assert!(canonical_realtime_voice_for_model(
            "gemini_live",
            "gemini-3.1-flash-live-preview",
            "en-us-advisor-1"
        )
        .is_none());
    }
    #[test]
    fn openai_live_has_its_complete_distinct_roster() {
        let live = voices_for_realtime_provider("openai_live");
        assert_eq!(live.len(), 31);
        assert!(canonical_realtime_voice("openai_live", "sillage").is_some());
        assert!(canonical_realtime_voice("openai_live", "ash").is_some());
        assert!(canonical_realtime_voice("openai_realtime", "sillage").is_none());
        assert_eq!(voices_for_realtime_provider("openai_realtime").len(), 10);
    }
    #[test]
    fn translation_models_do_not_advertise_selectable_speaking_voices() {
        assert!(
            voices_for_realtime_model("gemini_live", "gemini-3.5-live-translate-preview")
                .is_empty()
        );
        assert!(
            voices_for_realtime_model("openai_realtime_backend", "gpt-realtime-translate")
                .is_empty()
        );
    }
}
