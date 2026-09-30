//! Meeting session vocabulary consumed cross-module.

use std::collections::BTreeMap;

use crate::magician_v2::media_seam::meeting_session_engine::{
    meet_code_from_url, short_url_hash, slugify_thread_label,
};
use crate::magician_v2::media_seam::AudioStage;

/// Configuration for a meeting session.
#[derive(Debug, Clone)]
pub struct MeetingConfig {
    /// The Google-Meet link to join.
    pub meet_url: String,
    /// Display name the bot joins as.
    pub display_name: String,
    /// Wake phrases that engage the responder (matched case-insensitively).
    pub wake_phrases: Vec<String>,
    /// Re-summarize after this many new transcript turns (debounced cadence).
    pub summarize_every_turns: usize,
    /// How many recent turns to include as responder context.
    pub responder_tail_turns: usize,
    /// Calendar event title for this meeting, when the caller has it (the agent
    /// usually found the link on a calendar event). Used to key the bot's chat
    /// thread: `meeting-<slug(title)>-<date>`. `None` → fall back to the Meet code.
    pub title: Option<String>,
    /// Scheduled meeting date as `YYYY-MM-DD`, when known (from the calendar
    /// event). `None` → the join date (today) is used. Always part of the thread id.
    pub meeting_date: Option<String>,
    /// Optional canonical Meeting profile override for this session.
    pub audio_profile: Option<String>,
    /// Optional canonical per-stage selections for this session.
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

/// Derive the bot's per-meeting chat-thread id: `meeting-<label>-<date>`.
///
/// `label` is the slug of the calendar `title` when one was passed, else the Meet
/// code parsed from the URL (`meet.google.com/abc-defg-hij` → `abc-defg-hij`),
/// else a short stable hash of the URL. `date` is ALWAYS appended, so each day's
/// occurrence of a recurring meeting gets its own thread while a same-day rejoin
/// reuses it. Pure: the caller resolves `date` (the event date, else today).
pub fn derive_meeting_thread_id(meet_url: &str, title: Option<&str>, date: &str) -> String {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    let label = title
        .map(slugify_thread_label)
        .filter(|s| !s.is_empty())
        .or_else(|| meet_code_from_url(meet_url))
        // Hash whatever identity we DO have: a non-ASCII-only title (Hindi/
        // CJK/emoji) slugifies to empty, and hashing the (possibly empty) URL
        // instead would collapse every such meeting into one constant thread.
        .unwrap_or_else(|| short_url_hash(title.unwrap_or(meet_url)));
    format!("meeting-{label}-{date}")
}
