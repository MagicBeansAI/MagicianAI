//! The audio-notes UX seam (plan workstream 3.5,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Audio notes are a media-adjacent PRODUCT feature riding the notes
//! provider (Layer 1): a client captures a voice recording (often replayed
//! from a local outbox), and the backend owns an archive policy — the dated
//! `Audio Notes/<date>/` layout, the stable UUID identity that makes outbox
//! retries idempotent, the human-readable companion-page contract, the
//! newest-first listing order, and the receipt projection returned to the
//! client. Those decisions moved here verbatim from `notes.rs`
//! (behavior-identical); the provider module keeps the registry, settings,
//! idempotency write orchestration, and discovery index plumbing, and
//! re-imports these helpers under the same names — the plan-2.3
//! `notes_projection` pattern.
//!
//! Nothing here performs I/O or owns a store: the provider calls these
//! functions and persists what they decide. The idempotency BINDING rules
//! (same capture instant, same bytes/hash, same metadata or reject) stay in
//! `notes.rs::save_audio_note` beside the record reads they guard; this seam
//! owns the shape of what a retry re-submits and gets back.

use std::path::{Path, PathBuf};

use chrono::DateTime;
use uuid::Uuid;

use super::notes::{AudioNoteIndexEntry, AudioNoteRef, SaveAudioNoteRequest};

/// The archive location and identity one audio note is written under.
#[derive(Debug)]
pub struct AudioNoteLayout {
    pub note_id: String,
    pub captured_at: DateTime<chrono::FixedOffset>,
    pub target_dir: String,
    pub audio_file_name: String,
    pub note_path: String,
}

pub fn audio_note_ref_from_index(
    note: AudioNoteIndexEntry,
    note_absolute_path: PathBuf,
    audio_absolute_path: PathBuf,
) -> AudioNoteRef {
    AudioNoteRef {
        note_id: note.note_id,
        requested_provider: note.requested_provider,
        provider: note.provider,
        used_fallback: note.used_fallback,
        fallback_reason: note.fallback_reason,
        captured_at: note.captured_at,
        note_path: note.note_path,
        note_absolute_path: note_absolute_path.display().to_string(),
        audio_path: note.audio_path,
        audio_absolute_path: audio_absolute_path.display().to_string(),
        open_url: None,
        bytes: note.bytes,
        content_hash: note.content_hash,
    }
}

pub fn audio_note_newest_first(
    left: &AudioNoteIndexEntry,
    right: &AudioNoteIndexEntry,
) -> std::cmp::Ordering {
    match (
        DateTime::parse_from_rfc3339(&left.captured_at),
        DateTime::parse_from_rfc3339(&right.captured_at),
    ) {
        (Ok(left_at), Ok(right_at)) => right_at
            .timestamp_micros()
            .cmp(&left_at.timestamp_micros())
            .then_with(|| right.note_id.cmp(&left.note_id)),
        _ => right
            .captured_at
            .cmp(&left.captured_at)
            .then_with(|| right.note_id.cmp(&left.note_id)),
    }
}

pub fn audio_note_layout(request: &SaveAudioNoteRequest) -> std::io::Result<AudioNoteLayout> {
    let captured_at = request
        .captured_at
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "captured_at must be an RFC 3339 timestamp",
            )
        })?
        .unwrap_or_else(|| chrono::Local::now().fixed_offset());
    let target_dir = format!("Audio Notes/{}", captured_at.format("%Y-%m-%d"));
    let note_id = match request
        .note_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Uuid::parse_str(value)
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "note_id must be a UUID")
            })?
            .to_string(),
        None => Uuid::new_v4().to_string(),
    };
    let unique_suffix = Uuid::parse_str(&note_id)
        .expect("validated/generated audio-note UUID")
        .simple()
        .to_string();
    let base_name = format!("{}-{}", captured_at.format("%H-%M-%S-%3f"), unique_suffix);
    let extension = audio_note_extension(
        request.original_filename.as_deref(),
        request.mime_type.as_str(),
    );
    let audio_file_name = format!("{base_name}.{extension}");
    let note_path = format!("{target_dir}/{base_name}.md");
    Ok(AudioNoteLayout {
        note_id,
        captured_at,
        target_dir,
        audio_file_name,
        note_path,
    })
}

pub fn audio_note_extension(original_filename: Option<&str>, mime_type: &str) -> &'static str {
    let filename_extension = original_filename
        .and_then(|name| Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match filename_extension.as_deref() {
        Some("m4a") => "m4a",
        Some("wav") => "wav",
        Some("mp3") => "mp3",
        Some("webm") => "webm",
        Some("ogg") | Some("oga") => "ogg",
        Some("caf") => "caf",
        _ => match mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "audio/wav" | "audio/x-wav" => "wav",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/webm" => "webm",
            "audio/ogg" => "ogg",
            "audio/x-caf" => "caf",
            _ => "m4a",
        },
    }
}

pub fn render_audio_note_markdown(
    request: &SaveAudioNoteRequest,
    layout: &AudioNoteLayout,
) -> String {
    let captured_at = layout.captured_at.to_rfc3339();
    let recorded_date = layout.captured_at.format("%Y-%m-%d").to_string();
    let recorded_time = layout.captured_at.format("%H:%M:%S %:z").to_string();
    let date_tag = layout.captured_at.format("audio-note/date/%Y-%m-%d");
    let time_tag = layout.captured_at.format("audio-note/time/%H-%M");
    let display_time = layout.captured_at.format("%e %b %Y, %l:%M:%S %p");
    let source_surface = serde_json::to_string(request.source_surface.trim())
        .unwrap_or_else(|_| "\"unknown\"".to_string());
    let mime_type = serde_json::to_string(request.mime_type.trim())
        .unwrap_or_else(|_| "\"application/octet-stream\"".to_string());
    let duration = request
        .duration_ms
        .map(|duration_ms| format!("duration_ms: {duration_ms}\n"))
        .unwrap_or_default();
    let transcript = request
        .transcript
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("_No transcript was available._");

    format!(
        "---\nmagician_kind: audio_note\ntags:\n  - audio-note\n  - voice-note\n  - \"{date_tag}\"\n  - \"{time_tag}\"\nrecorded_at: {captured_at}\nrecorded_date: {recorded_date}\nrecorded_time: \"{recorded_time}\"\nsource_surface: {source_surface}\nmime_type: {mime_type}\n{duration}audio_file: \"{}\"\n---\n\n# Audio Note — {display_time}\n\n<audio controls src=\"{}\"></audio>\n\n[Open recording]({})\n\n## Transcript\n\n{transcript}\n",
        layout.audio_file_name, layout.audio_file_name, layout.audio_file_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(note_id: Option<&str>, captured_at: Option<&str>) -> SaveAudioNoteRequest {
        SaveAudioNoteRequest {
            provider: None,
            note_id: note_id.map(str::to_string),
            captured_at: captured_at.map(str::to_string),
            source_surface: "voice_note".to_string(),
            transcript: Some("  ship it  ".to_string()),
            original_filename: Some("memo.m4a".to_string()),
            mime_type: "audio/x-m4a".to_string(),
            duration_ms: Some(1_500),
            bytes: vec![1, 2, 3],
        }
    }

    #[test]
    fn layout_normalizes_identity_and_files_under_the_dated_archive_dir() {
        let layout = audio_note_layout(&request(
            Some("  6f9d7ac0-1111-4111-8111-3c9a1b2f0011  "),
            None,
        ))
        .expect("layout");
        // A supplied UUID is normalized (trimmed, canonical form) and reused,
        // keeping outbox retries idempotent.
        assert_eq!(layout.note_id, "6f9d7ac0-1111-4111-8111-3c9a1b2f0011");
        assert!(layout.target_dir.starts_with("Audio Notes/2"));
        assert_eq!(
            layout.note_path,
            format!(
                "{}/{}.md",
                layout.target_dir,
                layout.audio_file_name.trim_end_matches(".m4a")
            )
        );
        assert!(layout.audio_file_name.ends_with(".m4a"));
        assert!(layout
            .audio_file_name
            .contains("6f9d7ac01111411181113c9a1b2f0011"));
    }

    #[test]
    fn layout_rejects_bad_identity_and_timestamps() {
        let bad_uuid = audio_note_layout(&request(Some("not-a-uuid"), None))
            .expect_err("non-UUID note_id must fail");
        assert_eq!(bad_uuid.kind(), std::io::ErrorKind::InvalidInput);
        let bad_time = audio_note_layout(&request(None, Some("yesterday")))
            .expect_err("non-RFC3339 captured_at must fail");
        assert_eq!(bad_time.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn extension_prefers_filename_and_falls_back_to_mime_then_m4a() {
        assert_eq!(audio_note_extension(Some("a.WAV"), "audio/mp3"), "wav");
        assert_eq!(audio_note_extension(Some("a.OGA"), ""), "ogg");
        assert_eq!(
            audio_note_extension(None, "audio/mpeg; charset=utf8"),
            "mp3"
        );
        assert_eq!(audio_note_extension(None, "audio/x-caf"), "caf");
        assert_eq!(
            audio_note_extension(None, "application/octet-stream"),
            "m4a"
        );
        assert_eq!(audio_note_extension(Some("memo.dat"), "text/plain"), "m4a");
    }

    #[test]
    fn markdown_page_carries_frontmatter_audio_element_and_transcript() {
        let layout =
            audio_note_layout(&request(None, Some("2026-08-26T09:30:00+05:30"))).expect("layout");
        let markdown =
            render_audio_note_markdown(&request(None, Some("2026-08-26T09:30:00+05:30")), &layout);
        assert!(markdown.contains("magician_kind: audio_note"));
        assert!(markdown.contains("- \"audio-note/date/2026-08-26\""));
        assert!(markdown.contains("- \"audio-note/time/09-30\""));
        assert!(markdown.contains("source_surface: \"voice_note\""));
        assert!(markdown.contains("duration_ms: 1500\n"));
        assert!(markdown.contains("<audio controls src=\""));
        assert!(markdown.contains("## Transcript\n\nship it"));
        // A missing transcript gets the explicit fallback, never an empty
        // section.
        let mut silent = request(None, None);
        silent.transcript = Some("   ".to_string());
        let silent_markdown = render_audio_note_markdown(&silent, &layout);
        assert!(silent_markdown.contains("_No transcript was available._"));
    }

    #[test]
    fn listing_orders_newest_first_with_note_id_tiebreak() {
        let entry = |note_id: &str, captured_at: &str| AudioNoteIndexEntry {
            note_id: note_id.to_string(),
            requested_provider: "local".to_string(),
            provider: "local".to_string(),
            used_fallback: false,
            fallback_reason: None,
            captured_at: captured_at.to_string(),
            source_surface: "voice_note".to_string(),
            transcript: None,
            duration_ms: None,
            mime_type: "audio/x-m4a".to_string(),
            note_path: "Audio Notes/2026-08-26/a.md".to_string(),
            audio_path: "Audio Notes/2026-08-26/a.m4a".to_string(),
            bytes: 3,
            content_hash: "blake3:x".to_string(),
        };
        let newer = entry("b", "2026-08-26T10:00:00Z");
        let older = entry("a", "2026-08-26T09:00:00Z");
        assert_eq!(
            audio_note_newest_first(&newer, &older),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            audio_note_newest_first(&older, &newer),
            std::cmp::Ordering::Greater
        );
        // Same instant: higher note_id first (descending).
        let same_a = entry("a", "2026-08-26T10:00:00Z");
        let same_b = entry("b", "2026-08-26T10:00:00Z");
        assert_eq!(
            audio_note_newest_first(&same_b, &same_a),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn receipt_projection_from_index_omits_open_url_and_keeps_paths_absolute() {
        let index = AudioNoteIndexEntry {
            note_id: "n1".to_string(),
            requested_provider: "local".to_string(),
            provider: "local".to_string(),
            used_fallback: true,
            fallback_reason: Some("sb down".to_string()),
            captured_at: "2026-08-26T10:00:00Z".to_string(),
            source_surface: "voice_note".to_string(),
            transcript: None,
            duration_ms: None,
            mime_type: "audio/x-m4a".to_string(),
            note_path: "Audio Notes/2026-08-26/n1.md".to_string(),
            audio_path: "Audio Notes/2026-08-26/n1.m4a".to_string(),
            bytes: 9,
            content_hash: "blake3:y".to_string(),
        };
        let receipt = audio_note_ref_from_index(
            index,
            PathBuf::from("/root/Audio Notes/2026-08-26/n1.md"),
            PathBuf::from("/root/Audio Notes/2026-08-26/n1.m4a"),
        );
        assert_eq!(receipt.note_id, "n1");
        assert!(receipt.used_fallback);
        assert_eq!(
            receipt.note_absolute_path,
            "/root/Audio Notes/2026-08-26/n1.md"
        );
        assert_eq!(receipt.open_url, None);
    }
}
