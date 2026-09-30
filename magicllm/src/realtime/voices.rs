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

const OPENAI_LIVE_VOICES: &[RealtimeVoiceChoice] = &[
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
        id: "quartz",
        label: "Quartz",
    },
    RealtimeVoiceChoice {
        id: "ripple",
        label: "Ripple",
    },
    RealtimeVoiceChoice {
        id: "vesper",
        label: "Vesper",
    },
    RealtimeVoiceChoice {
        id: "willow",
        label: "Willow",
    },
    RealtimeVoiceChoice {
        id: "stone",
        label: "Stone",
    },
    RealtimeVoiceChoice {
        id: "gleam",
        label: "Gleam",
    },
    RealtimeVoiceChoice {
        id: "meridian",
        label: "Meridian",
    },
    RealtimeVoiceChoice {
        id: "bossa",
        label: "Bossa",
    },
    RealtimeVoiceChoice {
        id: "tempo",
        label: "Tempo",
    },
    RealtimeVoiceChoice {
        id: "beacon",
        label: "Beacon",
    },
    RealtimeVoiceChoice {
        id: "delta",
        label: "Delta",
    },
    RealtimeVoiceChoice {
        id: "cinder",
        label: "Cinder",
    },
];

const GROK_VOICE_VOICES: &[RealtimeVoiceChoice] = &[
    RealtimeVoiceChoice {
        id: "eve",
        label: "Eve — energetic, British",
    },
    RealtimeVoiceChoice {
        id: "ara",
        label: "Ara — warm",
    },
    RealtimeVoiceChoice {
        id: "leo",
        label: "Leo — authoritative, British",
    },
    RealtimeVoiceChoice {
        id: "rex",
        label: "Rex — confident",
    },
    RealtimeVoiceChoice {
        id: "sal",
        label: "Sal — smooth",
    },
];

const GEMINI_LIVE_VOICES: &[RealtimeVoiceChoice] = &[
    RealtimeVoiceChoice {
        id: "Kore",
        label: "Kore — firm",
    },
    RealtimeVoiceChoice {
        id: "Puck",
        label: "Puck — upbeat",
    },
    RealtimeVoiceChoice {
        id: "Charon",
        label: "Charon — informative",
    },
    RealtimeVoiceChoice {
        id: "Fenrir",
        label: "Fenrir — excitable",
    },
    RealtimeVoiceChoice {
        id: "Aoede",
        label: "Aoede — breezy",
    },
    RealtimeVoiceChoice {
        id: "Leda",
        label: "Leda — youthful",
    },
    RealtimeVoiceChoice {
        id: "Orus",
        label: "Orus — firm",
    },
    RealtimeVoiceChoice {
        id: "Zephyr",
        label: "Zephyr — bright",
    },
];

pub fn voices_for_realtime_provider(provider: &str) -> &'static [RealtimeVoiceChoice] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai_realtime"
        | "openai-realtime"
        | "openai_realtime_backend"
        | "openai-realtime-backend" => OPENAI_REALTIME_VOICES,
        "openai_live" | "openai-live" | "gpt_live" | "gpt-live" => OPENAI_LIVE_VOICES,
        "gemini_live" | "gemini-live" => GEMINI_LIVE_VOICES,
        "grok_voice" | "grok-voice" | "xai_voice" => GROK_VOICE_VOICES,
        _ => &[],
    }
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
}
