//! Ring for a WhatsApp call that is already open in the user's Chrome.
//!
//! The extension reports the ring. Observe joins, listens, or dismisses it.
//! This module only decides what happens next. Starting and stopping audio
//! stays with the HTTP handler, so a test never opens a call.

use std::sync::{Mutex, OnceLock};

pub const PROFILE_REALTIME_2: &str = "voice_realtime_openai_backend";
pub const PROFILE_GPT_LIVE_1: &str = "voice_realtime_gpt_live_1";
const DEFAULT_URL: &str = "https://web.whatsapp.com/";
const DEFAULT_TITLE: &str = "WhatsApp call";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatsAppRingDecision {
    Incoming,
    Ongoing,
    Approved,
    Listen,
    Ignored,
    Cleared,
    Cancelled,
    Ended,
    Profile,
}

/// Talk answers and speaks. Listen answers and only transcribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatsAppAttendMode {
    Talk,
    Listen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsAppRingEvent {
    pub decision: WhatsAppRingDecision,
    pub url: Option<String>,
    pub title: Option<String>,
    pub caller: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppRingEffect {
    None,
    Start {
        profile: String,
        mode: WhatsAppAttendMode,
    },
    Stop {
        session_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Incoming,
    Ongoing,
    Starting,
    Live,
    Listening,
    Failed,
    Stopping,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    None,
    Ringing,
    Ongoing,
}

impl Surface {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ringing => "ringing",
            Self::Ongoing => "ongoing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsAppRingState {
    phase: Phase,
    surface: Surface,
    mode: WhatsAppAttendMode,
    profile: String,
    url: String,
    title: String,
    caller: Option<String>,
    session_id: Option<String>,
    error: Option<String>,
    stop_after_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsAppRingView {
    pub phase: &'static str,
    pub surface: &'static str,
    pub profile: String,
    pub url: String,
    pub title: String,
    pub caller: Option<String>,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub starting: bool,
    pub leaving: bool,
}

pub fn parse_whatsapp_ring_decision(raw: &str) -> Option<WhatsAppRingDecision> {
    match raw.trim() {
        "incoming" => Some(WhatsAppRingDecision::Incoming),
        "ongoing" => Some(WhatsAppRingDecision::Ongoing),
        "approved" | "join" => Some(WhatsAppRingDecision::Approved),
        "listen" => Some(WhatsAppRingDecision::Listen),
        "ignored" => Some(WhatsAppRingDecision::Ignored),
        "cleared" => Some(WhatsAppRingDecision::Cleared),
        "cancel" | "cancelled" => Some(WhatsAppRingDecision::Cancelled),
        "ended" => Some(WhatsAppRingDecision::Ended),
        "profile" => Some(WhatsAppRingDecision::Profile),
        _ => None,
    }
}

/// A `voice_realtime_*` profile id. Anything else stays on GPT Realtime 2.
pub fn normalize_whatsapp_profile(raw: &str) -> String {
    let trimmed = raw.trim();
    if is_whatsapp_profile_id(trimmed) {
        trimmed.to_string()
    } else {
        PROFILE_REALTIME_2.to_string()
    }
}

fn is_whatsapp_profile_id(raw: &str) -> bool {
    const PREFIX: &str = "voice_realtime_";
    let len = raw.len();
    if len <= PREFIX.len() || len > 80 {
        return false;
    }
    raw.starts_with(PREFIX)
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Keep a posted id when the catalog lists it. An empty catalog trusts the
/// id shape. A catalog that does not list the id stays on GPT Realtime 2.
pub fn accept_whatsapp_profile<'a>(
    raw: &str,
    catalog_ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let normalized = normalize_whatsapp_profile(raw);
    let mut seen = false;
    for id in catalog_ids {
        seen = true;
        if id == normalized {
            return normalized;
        }
    }
    if seen {
        PROFILE_REALTIME_2.to_string()
    } else {
        normalized
    }
}

impl Default for WhatsAppRingState {
    fn default() -> Self {
        Self::new()
    }
}

impl WhatsAppRingState {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            surface: Surface::None,
            mode: WhatsAppAttendMode::Talk,
            profile: PROFILE_REALTIME_2.to_string(),
            url: DEFAULT_URL.to_string(),
            title: DEFAULT_TITLE.to_string(),
            caller: None,
            session_id: None,
            error: None,
            stop_after_start: false,
        }
    }

    /// Name shown on the active-session card. A page title such as
    /// "(161) WhatsApp" is not a caller.
    pub fn session_title(&self) -> String {
        self.caller
            .as_deref()
            .map(str::trim)
            .filter(|caller| !caller.is_empty())
            .unwrap_or(DEFAULT_TITLE)
            .to_string()
    }

    pub fn apply(&mut self, event: WhatsAppRingEvent) -> WhatsAppRingEffect {
        let profile_frozen = matches!(
            self.phase,
            Phase::Starting | Phase::Live | Phase::Listening | Phase::Stopping
        );
        if !profile_frozen && event.decision == WhatsAppRingDecision::Profile {
            if let Some(profile) = event.profile.as_deref() {
                self.profile = normalize_whatsapp_profile(profile);
            }
        }

        match event.decision {
            WhatsAppRingDecision::Profile | WhatsAppRingDecision::Ignored => {
                WhatsAppRingEffect::None
            },
            WhatsAppRingDecision::Incoming => {
                if self.blocks_offer() {
                    return WhatsAppRingEffect::None;
                }
                self.remember_offer(&event);
                self.phase = Phase::Incoming;
                self.surface = Surface::Ringing;
                self.session_id = None;
                self.error = None;
                self.stop_after_start = false;
                WhatsAppRingEffect::None
            },
            WhatsAppRingDecision::Ongoing => {
                if self.blocks_offer() {
                    return WhatsAppRingEffect::None;
                }
                self.remember_offer(&event);
                self.phase = Phase::Ongoing;
                self.surface = Surface::Ongoing;
                self.session_id = None;
                self.error = None;
                self.stop_after_start = false;
                WhatsAppRingEffect::None
            },
            WhatsAppRingDecision::Approved => {
                if !self.can_talk() {
                    return WhatsAppRingEffect::None;
                }
                if let Some(profile) = event.profile.as_deref() {
                    self.profile = normalize_whatsapp_profile(profile);
                }
                self.remember_offer(&event);
                self.mode = WhatsAppAttendMode::Talk;
                self.phase = Phase::Starting;
                self.session_id = None;
                self.error = None;
                self.stop_after_start = false;
                WhatsAppRingEffect::Start {
                    profile: self.profile.clone(),
                    mode: WhatsAppAttendMode::Talk,
                }
            },
            WhatsAppRingDecision::Listen => {
                if !self.can_listen() {
                    return WhatsAppRingEffect::None;
                }
                if let Some(profile) = event.profile.as_deref() {
                    self.profile = normalize_whatsapp_profile(profile);
                }
                self.remember_offer(&event);
                self.mode = WhatsAppAttendMode::Listen;
                self.phase = Phase::Starting;
                self.session_id = None;
                self.error = None;
                self.stop_after_start = false;
                WhatsAppRingEffect::Start {
                    profile: self.profile.clone(),
                    mode: WhatsAppAttendMode::Listen,
                }
            },
            WhatsAppRingDecision::Cancelled => match self.phase {
                // The WhatsApp call is going away. A session that already
                // started, or is still starting, leaves with it.
                Phase::Live | Phase::Listening => {
                    self.phase = Phase::Stopping;
                    match self.session_id.take() {
                        Some(session_id) => WhatsAppRingEffect::Stop { session_id },
                        None => {
                            self.settle_idle();
                            WhatsAppRingEffect::None
                        },
                    }
                },
                Phase::Starting => {
                    self.phase = Phase::Stopping;
                    self.stop_after_start = true;
                    WhatsAppRingEffect::None
                },
                Phase::Incoming | Phase::Ongoing | Phase::Failed => {
                    self.phase = Phase::Cancelled;
                    self.error = None;
                    self.session_id = None;
                    WhatsAppRingEffect::None
                },
                Phase::Idle | Phase::Stopping | Phase::Cancelled => WhatsAppRingEffect::None,
            },
            WhatsAppRingDecision::Cleared => {
                if matches!(
                    self.phase,
                    Phase::Incoming | Phase::Ongoing | Phase::Failed | Phase::Cancelled
                ) {
                    self.settle_idle();
                }
                WhatsAppRingEffect::None
            },
            WhatsAppRingDecision::Ended => match self.phase {
                Phase::Live | Phase::Listening => {
                    self.phase = Phase::Stopping;
                    match self.session_id.take() {
                        Some(session_id) => WhatsAppRingEffect::Stop { session_id },
                        None => {
                            self.settle_idle();
                            WhatsAppRingEffect::None
                        },
                    }
                },
                Phase::Starting => {
                    self.phase = Phase::Stopping;
                    self.stop_after_start = true;
                    WhatsAppRingEffect::None
                },
                Phase::Incoming | Phase::Ongoing | Phase::Failed | Phase::Cancelled => {
                    self.settle_idle();
                    WhatsAppRingEffect::None
                },
                Phase::Idle | Phase::Stopping => WhatsAppRingEffect::None,
            },
        }
    }

    /// Record the session that `Start` launched. A leave that arrived while
    /// startup was in flight comes back as `Stop`.
    pub fn commit_started(&mut self, result: Result<String, String>) -> WhatsAppRingEffect {
        match result {
            Ok(session_id) => {
                if self.stop_after_start || self.phase == Phase::Stopping {
                    self.stop_after_start = false;
                    self.session_id = None;
                    self.phase = Phase::Stopping;
                    WhatsAppRingEffect::Stop { session_id }
                } else if self.phase == Phase::Starting {
                    self.session_id = Some(session_id);
                    self.phase = match self.mode {
                        WhatsAppAttendMode::Talk => Phase::Live,
                        WhatsAppAttendMode::Listen => Phase::Listening,
                    };
                    self.error = None;
                    WhatsAppRingEffect::None
                } else {
                    WhatsAppRingEffect::Stop { session_id }
                }
            },
            Err(error) => {
                self.session_id = None;
                self.stop_after_start = false;
                if self.phase == Phase::Stopping {
                    self.settle_idle();
                } else {
                    self.phase = Phase::Failed;
                    self.error = Some(error);
                }
                WhatsAppRingEffect::None
            },
        }
    }

    pub fn commit_stopped(&mut self) {
        if self.phase == Phase::Stopping {
            self.settle_idle();
        }
    }

    pub fn view(&self) -> WhatsAppRingView {
        let (phase, starting, leaving) = match self.phase {
            Phase::Idle => ("idle", false, false),
            Phase::Incoming => ("incoming", false, false),
            Phase::Ongoing => ("ongoing", false, false),
            Phase::Starting => match self.mode {
                WhatsAppAttendMode::Talk => ("approved", true, false),
                WhatsAppAttendMode::Listen => ("listening", true, false),
            },
            Phase::Live => ("approved", false, false),
            Phase::Listening => ("listening", false, false),
            Phase::Failed => ("failed", false, false),
            Phase::Stopping => ("leaving", false, true),
            Phase::Cancelled => ("cancelled", false, false),
        };
        WhatsAppRingView {
            phase,
            surface: self.surface.as_str(),
            profile: self.profile.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
            caller: self.caller.clone(),
            session_id: self.session_id.clone(),
            error: self.error.clone(),
            starting,
            leaving,
        }
    }

    fn can_talk(&self) -> bool {
        match self.phase {
            Phase::Incoming => true,
            Phase::Failed => self.surface == Surface::Ringing,
            _ => false,
        }
    }

    fn can_listen(&self) -> bool {
        match self.phase {
            Phase::Incoming | Phase::Ongoing => true,
            Phase::Failed => self.surface != Surface::None,
            _ => false,
        }
    }

    fn blocks_offer(&self) -> bool {
        matches!(
            self.phase,
            Phase::Starting | Phase::Live | Phase::Listening | Phase::Stopping | Phase::Cancelled
        )
    }

    fn remember_offer(&mut self, event: &WhatsAppRingEvent) {
        if let Some(url) = clean_whatsapp_url(event.url.as_deref()) {
            self.url = url;
        }
        if let Some(title) = clean_title(event.title.as_deref()) {
            self.title = title;
        }
        if let Some(caller) = clean_caller(event.caller.as_deref()) {
            self.caller = Some(caller);
        }
    }

    fn settle_idle(&mut self) {
        self.phase = Phase::Idle;
        self.surface = Surface::None;
        self.session_id = None;
        self.error = None;
        self.stop_after_start = false;
        self.caller = None;
        self.title = DEFAULT_TITLE.to_string();
    }
}

pub fn whatsapp_ring_state() -> &'static Mutex<WhatsAppRingState> {
    static CELL: OnceLock<Mutex<WhatsAppRingState>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(WhatsAppRingState::new()))
}

fn clean_whatsapp_url(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw == "https://web.whatsapp.com" || raw.starts_with("https://web.whatsapp.com/") {
        Some(raw.to_string())
    } else {
        None
    }
}

fn clean_title(raw: Option<&str>) -> Option<String> {
    let title = raw?.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = title.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(180).collect())
    }
}

/// The person or phone number on the call screen. The unread page title is
/// not a caller. A phone is kept so seating can compare digits. A name is
/// kept for the card, and a name does not prove the caller.
fn clean_caller(raw: Option<&str>) -> Option<String> {
    let title = clean_title(raw)?;
    if title.chars().count() > 80 {
        return None;
    }
    let ascii_letters: String = title
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if ascii_letters == "whatsapp" {
        return None;
    }
    let has_letter = title.chars().any(|ch| ch.is_alphabetic());
    if has_letter
        || crate::magician_v2::media_seam::whatsapp_web_call::whatsapp_party_digits(&title)
            .is_some()
    {
        Some(title)
    } else {
        None
    }
}

/// Caller on an offer that is still this call. Idle and cancelled rings
/// contribute nothing, so a finished call cannot seat the next one.
pub fn active_ring_caller() -> Option<String> {
    let state = whatsapp_ring_state().lock().ok()?;
    caller_on_active_offer(&state)
}

fn caller_on_active_offer(state: &WhatsAppRingState) -> Option<String> {
    if !matches!(
        state.phase,
        Phase::Incoming
            | Phase::Ongoing
            | Phase::Starting
            | Phase::Live
            | Phase::Listening
            | Phase::Failed
    ) {
        return None;
    }
    state.caller.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(decision: WhatsAppRingDecision, profile: Option<&str>) -> WhatsAppRingEvent {
        WhatsAppRingEvent {
            decision,
            url: Some("https://web.whatsapp.com/".to_string()),
            title: Some("(161) WhatsApp".to_string()),
            caller: None,
            profile: profile.map(str::to_string),
        }
    }

    fn talk(profile: &str) -> WhatsAppRingEffect {
        WhatsAppRingEffect::Start {
            profile: profile.to_string(),
            mode: WhatsAppAttendMode::Talk,
        }
    }

    #[test]
    fn approve_starts_once_on_the_selected_engine() {
        let mut state = WhatsAppRingState::new();
        assert!(matches!(
            state.apply(event(
                WhatsAppRingDecision::Profile,
                Some(PROFILE_GPT_LIVE_1)
            )),
            WhatsAppRingEffect::None
        ));
        assert_eq!(state.view().profile, PROFILE_GPT_LIVE_1);

        state.apply(event(WhatsAppRingDecision::Incoming, None));
        let start = state.apply(event(WhatsAppRingDecision::Approved, None));
        assert_eq!(start, talk(PROFILE_GPT_LIVE_1));
        assert!(state.view().starting);

        let again = state.apply(event(WhatsAppRingDecision::Approved, None));
        assert_eq!(again, WhatsAppRingEffect::None);
        assert_eq!(
            state.commit_started(Ok("meet-1".to_string())),
            WhatsAppRingEffect::None
        );
        let view = state.view();
        assert_eq!(view.phase, "approved");
        assert_eq!(view.session_id.as_deref(), Some("meet-1"));
        assert!(!view.starting);
    }

    #[test]
    fn catalog_profile_sticks_and_garbage_falls_back_to_realtime_2() {
        let mut state = WhatsAppRingState::new();
        state.apply(event(
            WhatsAppRingDecision::Profile,
            Some("  voice_realtime_grok  "),
        ));
        assert_eq!(state.view().profile, "voice_realtime_grok");

        state.apply(event(
            WhatsAppRingDecision::Profile,
            Some("voice_realtime_default"),
        ));
        assert_eq!(state.view().profile, "voice_realtime_default");

        for garbage in [
            "join",
            "../etc",
            "voice_realtime_",
            "voice_realtime_grok;drop",
        ] {
            state.apply(event(WhatsAppRingDecision::Profile, Some(garbage)));
            assert_eq!(state.view().profile, PROFILE_REALTIME_2, "{garbage}");
        }
        let too_long = format!("voice_realtime_{}", "a".repeat(70));
        state.apply(event(
            WhatsAppRingDecision::Profile,
            Some(too_long.as_str()),
        ));
        assert_eq!(state.view().profile, PROFILE_REALTIME_2);

        assert_eq!(
            accept_whatsapp_profile("voice_realtime_grok", ["voice_realtime_grok"]),
            "voice_realtime_grok"
        );
        assert_eq!(
            accept_whatsapp_profile("voice_realtime_missing", ["voice_realtime_grok"]),
            PROFILE_REALTIME_2
        );
        assert_eq!(
            accept_whatsapp_profile("voice_realtime_missing", std::iter::empty::<&str>()),
            "voice_realtime_missing"
        );
    }

    #[test]
    fn profile_is_frozen_while_the_call_is_starting_or_live() {
        let mut state = WhatsAppRingState::new();
        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(
            state.apply(event(
                WhatsAppRingDecision::Approved,
                Some(PROFILE_REALTIME_2)
            )),
            talk(PROFILE_REALTIME_2)
        );
        state.apply(event(
            WhatsAppRingDecision::Profile,
            Some("voice_realtime_grok"),
        ));
        assert_eq!(state.view().profile, PROFILE_REALTIME_2);

        state.commit_started(Ok("meet-live".to_string()));
        state.apply(event(
            WhatsAppRingDecision::Profile,
            Some("voice_realtime_gemini_38_live"),
        ));
        state.apply(event(
            WhatsAppRingDecision::Approved,
            Some("voice_realtime_grok"),
        ));
        let view = state.view();
        assert_eq!(view.phase, "approved");
        assert_eq!(view.profile, PROFILE_REALTIME_2);
    }

    #[test]
    fn ignore_leaves_the_ring_and_ended_stops_a_live_call() {
        let mut state = WhatsAppRingState::new();
        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Ignored, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(state.view().phase, "incoming");

        state.apply(event(WhatsAppRingDecision::Approved, None));
        state.commit_started(Ok("meet-2".to_string()));
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Ignored, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(state.view().phase, "approved");

        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(state.view().session_id.as_deref(), Some("meet-2"));

        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Ended, None)),
            WhatsAppRingEffect::Stop {
                session_id: "meet-2".to_string()
            }
        );
        assert!(state.view().leaving);
        state.commit_stopped();
        assert_eq!(state.view().phase, "idle");
    }

    #[test]
    fn cleared_incoming_does_not_start_and_a_late_leave_stops_startup() {
        let mut state = WhatsAppRingState::new();
        state.apply(event(WhatsAppRingDecision::Incoming, None));
        state.apply(event(WhatsAppRingDecision::Cleared, None));
        assert_eq!(state.view().phase, "idle");
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Approved, None)),
            WhatsAppRingEffect::None
        );

        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Approved, None)),
            talk(PROFILE_REALTIME_2)
        );
        state.apply(event(WhatsAppRingDecision::Ended, None));
        assert_eq!(
            state.commit_started(Ok("meet-3".to_string())),
            WhatsAppRingEffect::Stop {
                session_id: "meet-3".to_string()
            }
        );
    }

    #[test]
    fn idle_approve_does_not_start_and_a_failed_ring_can_retry() {
        let mut state = WhatsAppRingState::new();
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Approved, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(state.view().phase, "idle");

        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Approved, None)),
            talk(PROFILE_REALTIME_2)
        );
        state.commit_started(Err("OPENAI_API_KEY is required".to_string()));
        let view = state.view();
        assert_eq!(view.phase, "failed");
        assert_eq!(view.surface, "ringing");
        assert_eq!(view.error.as_deref(), Some("OPENAI_API_KEY is required"));

        assert_eq!(
            state.apply(event(
                WhatsAppRingDecision::Approved,
                Some(PROFILE_REALTIME_2)
            )),
            talk(PROFILE_REALTIME_2)
        );
    }

    #[test]
    fn a_ring_offers_join_and_listen_and_an_ongoing_call_offers_listen() {
        let mut state = WhatsAppRingState::new();
        let mut incoming = event(WhatsAppRingDecision::Incoming, None);
        incoming.caller = Some("Ada".to_string());
        state.apply(incoming);
        let view = state.view();
        assert_eq!(view.phase, "incoming");
        assert_eq!(view.caller.as_deref(), Some("Ada"));
        assert_eq!(state.session_title(), "Ada");

        let mut page_title = event(WhatsAppRingDecision::Incoming, None);
        page_title.caller = Some("(161) WhatsApp".to_string());
        state.apply(page_title);
        assert_eq!(state.view().caller.as_deref(), Some("Ada"));

        let mut phone = WhatsAppRingState::new();
        let mut numbered = event(WhatsAppRingDecision::Incoming, None);
        numbered.caller = Some("+1 (555) 121-2001".to_string());
        phone.apply(numbered);
        assert_eq!(phone.view().caller.as_deref(), Some("+1 (555) 121-2001"));
        assert_eq!(
            caller_on_active_offer(&phone).as_deref(),
            Some("+1 (555) 121-2001")
        );
        phone.settle_idle();
        assert_eq!(caller_on_active_offer(&phone), None);

        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Listen, None)),
            WhatsAppRingEffect::Start {
                profile: PROFILE_REALTIME_2.to_string(),
                mode: WhatsAppAttendMode::Listen,
            }
        );
        assert_eq!(
            state.commit_started(Ok("listen-1".to_string())),
            WhatsAppRingEffect::None
        );
        assert_eq!(state.view().phase, "listening");
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Ended, None)),
            WhatsAppRingEffect::Stop {
                session_id: "listen-1".to_string()
            }
        );

        let mut again = WhatsAppRingState::new();
        let mut ongoing = event(WhatsAppRingDecision::Ongoing, None);
        ongoing.caller = Some("Ada".to_string());
        again.apply(ongoing);
        assert_eq!(again.view().phase, "ongoing");
        assert_eq!(
            again.apply(event(WhatsAppRingDecision::Approved, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(
            again.apply(event(WhatsAppRingDecision::Listen, None)),
            WhatsAppRingEffect::Start {
                profile: PROFILE_REALTIME_2.to_string(),
                mode: WhatsAppAttendMode::Listen,
            }
        );
        assert_eq!(
            again.apply(event(WhatsAppRingDecision::Listen, None)),
            WhatsAppRingEffect::None
        );
    }

    #[test]
    fn cancel_dismisses_an_offer_and_stops_a_live_call() {
        let mut state = WhatsAppRingState::new();
        state.apply(event(WhatsAppRingDecision::Incoming, None));
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Cancelled, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(state.view().phase, "cancelled");
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Approved, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(
            state.apply(event(WhatsAppRingDecision::Incoming, None)),
            WhatsAppRingEffect::None
        );

        let mut starting = WhatsAppRingState::new();
        starting.apply(event(WhatsAppRingDecision::Incoming, None));
        starting.apply(event(WhatsAppRingDecision::Approved, None));
        assert_eq!(
            starting.apply(event(WhatsAppRingDecision::Cancelled, None)),
            WhatsAppRingEffect::None
        );
        assert_eq!(starting.view().phase, "leaving");
        assert_eq!(
            starting.commit_started(Ok("meet-5".to_string())),
            WhatsAppRingEffect::Stop {
                session_id: "meet-5".to_string()
            }
        );

        let mut live = WhatsAppRingState::new();
        live.apply(event(WhatsAppRingDecision::Incoming, None));
        live.apply(event(WhatsAppRingDecision::Approved, None));
        live.commit_started(Ok("meet-4".to_string()));
        assert_eq!(
            live.apply(event(WhatsAppRingDecision::Cancelled, None)),
            WhatsAppRingEffect::Stop {
                session_id: "meet-4".to_string()
            }
        );
        assert_eq!(live.view().phase, "leaving");
        assert_eq!(
            live.apply(event(WhatsAppRingDecision::Ongoing, None)),
            WhatsAppRingEffect::None
        );
    }

    #[test]
    fn decisions_match_the_extension_vocabulary() {
        assert_eq!(
            parse_whatsapp_ring_decision("ended"),
            Some(WhatsAppRingDecision::Ended)
        );
        assert_eq!(
            parse_whatsapp_ring_decision("join"),
            Some(WhatsAppRingDecision::Approved)
        );
        assert_eq!(
            parse_whatsapp_ring_decision("approved"),
            Some(WhatsAppRingDecision::Approved)
        );
        assert_eq!(
            parse_whatsapp_ring_decision("listen"),
            Some(WhatsAppRingDecision::Listen)
        );
        assert_eq!(
            parse_whatsapp_ring_decision("ongoing"),
            Some(WhatsAppRingDecision::Ongoing)
        );
        assert_eq!(
            parse_whatsapp_ring_decision("cancel"),
            Some(WhatsAppRingDecision::Cancelled)
        );
        assert_eq!(parse_whatsapp_ring_decision("decline"), None);
    }
}
