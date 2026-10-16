//! WhatsApp Web voice calls on the meeting attendee rail.
//!
//! The browser, BlackHole microphone, and ScreenCaptureKit ears stay in
//! [`super::browser_join`]. This module only reads a join URL and an
//! agent-browser snapshot.
//!
//! An incoming call is recognized when the page shows both an Accept (or
//! Answer) button and a Decline button. Chat text that merely says "call"
//! does not. A voice call is placed from `https://web.whatsapp.com/send?phone=`.
//! A bare `https://web.whatsapp.com/` waits for the next incoming call.

use std::time::Duration;

/// How long a placed call may take to reach the in-call controls.
pub const PLACE_CALL_TIMEOUT: Duration = Duration::from_secs(90);

/// How long the joiner waits for someone to call, unless
/// `MAGICIAN_WHATSAPP_CALL_WAIT_SECS` overrides it (30–1800).
pub const DEFAULT_ANSWER_WAIT: Duration = Duration::from_secs(600);

/// Answer mode, page is open, Accept+Decline are not on screen yet.
pub const PHASE_WAITING: &str = "waiting_for_call";
/// Accept and Decline are both on screen. This is the incoming-call signal.
pub const PHASE_INCOMING: &str = "incoming_call";
/// Place mode is clicking Continue / Call / Voice.
pub const PHASE_PLACING: &str = "placing_call";
/// End call (or Hang up) is on screen. Ringing or connected.
pub const PHASE_IN_CALL: &str = "in_call";

/// What a `web.whatsapp.com` join URL is asking the bot to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppCallIntent {
    /// Stay on WhatsApp Web and answer the next incoming call.
    AnswerIncoming,
    /// Open the chat for this country-code number and start a voice call.
    PlaceVoice { phone: String },
    /// `phone=` was present and was not 8–15 digits.
    InvalidPhone,
}

/// One interactive control from an agent-browser snapshot line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotControl {
    pub role: String,
    pub name: String,
    pub ref_id: String,
}

/// What the current WhatsApp Web snapshot means for the joiner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppView {
    /// QR / link-a-device screen. The profile has never been signed in.
    NeedsLogin,
    /// Accept and Decline are both on screen. This is an incoming call.
    Incoming { accept_ref: String },
    /// The hang-up control is on screen. The call is up (ringing or connected).
    InCall { end_ref: String },
    /// A Voice control is visible. Never a Video control.
    VoiceReady { voice_ref: String },
    /// The send-by-phone interstitial, before the chat header exists.
    ContinueToChat { continue_ref: String },
    /// Header Call button. Clicking it opens the Voice / Video menu.
    CallMenuClosed { call_ref: String },
    /// Logged-in page with nothing to click yet.
    Waiting,
}

/// Digits of one WhatsApp party, when `raw` is a phone and not a display name.
///
/// A formatted number, a bare digit string, and `user[:device]@s.whatsapp.net`
/// count. A letter outside that JID is a contact name, and a name is not proof
/// of who is on the call. `@lid` and any other domain are not a phone number.
pub fn whatsapp_party_digits(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let local = if let Some((user, domain)) = trimmed.split_once('@') {
        let domain = domain.split(['/', '?', '#']).next().unwrap_or(domain);
        if !domain.eq_ignore_ascii_case("s.whatsapp.net") {
            return None;
        }
        let local = user.split(':').next().unwrap_or(user);
        if !local.chars().all(|ch| ch.is_ascii_digit() || ch == '+') {
            return None;
        }
        local
    } else if trimmed.chars().any(|ch| ch.is_alphabetic()) {
        return None;
    } else {
        trimmed
    };
    let digits: String = local.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if (8..=15).contains(&digits.len()) {
        Some(digits)
    } else {
        None
    }
}

/// `None` when `url` is not WhatsApp Web. Meet and every other host stay on
/// the existing join path.
pub fn whatsapp_call_intent(url: &str) -> Option<WhatsAppCallIntent> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let (host_and_path, query) = without_scheme
        .split_once('?')
        .unwrap_or((without_scheme, ""));
    let host = host_and_path
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host != "web.whatsapp.com" {
        return None;
    }
    match query_param(query, "phone") {
        Some(raw) => {
            let decoded = decode_query_component(raw);
            let digits: String = decoded.chars().filter(|c| c.is_ascii_digit()).collect();
            if (8..=15).contains(&digits.len()) {
                Some(WhatsAppCallIntent::PlaceVoice { phone: digits })
            } else {
                Some(WhatsAppCallIntent::InvalidPhone)
            }
        },
        None => Some(WhatsAppCallIntent::AnswerIncoming),
    }
}

pub fn answer_wait_timeout() -> Duration {
    std::env::var("MAGICIAN_WHATSAPP_CALL_WAIT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| secs.clamp(30, 1800))
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_ANSWER_WAIT)
}

pub fn classify_whatsapp_snapshot(snapshot: &str) -> WhatsAppView {
    if needs_login(snapshot) {
        return WhatsAppView::NeedsLogin;
    }
    let controls = parse_controls(snapshot);
    if let Some(accept) = controls.iter().find(|c| is_button(c) && is_accept(&c.name)) {
        if controls.iter().any(|c| is_button(c) && is_decline(&c.name)) {
            return WhatsAppView::Incoming {
                accept_ref: accept.ref_id.clone(),
            };
        }
    }
    if let Some(end) = controls.iter().find(|c| is_button(c) && is_end(&c.name)) {
        return WhatsAppView::InCall {
            end_ref: end.ref_id.clone(),
        };
    }
    if let Some(voice) = controls
        .iter()
        .find(|c| is_invokable(c) && is_voice(&c.name))
    {
        return WhatsAppView::VoiceReady {
            voice_ref: voice.ref_id.clone(),
        };
    }
    if let Some(cont) = controls
        .iter()
        .find(|c| is_button(c) && is_continue_chat(&c.name))
    {
        return WhatsAppView::ContinueToChat {
            continue_ref: cont.ref_id.clone(),
        };
    }
    if let Some(call) = controls
        .iter()
        .find(|c| is_button(c) && is_call_opener(&c.name))
    {
        return WhatsAppView::CallMenuClosed {
            call_ref: call.ref_id.clone(),
        };
    }
    WhatsAppView::Waiting
}

/// Phase string the joiner stores while it is still inside `join`.
/// `MeetingSession::status` turns [`PHASE_INCOMING`] into `IncomingCall`
/// so a poll of `GET /meetings/{id}` can see the ring. Accept alone does
/// not: the Decline button has to be there too.
pub fn whatsapp_call_phase(view: &WhatsAppView, placing: bool) -> &'static str {
    match view {
        WhatsAppView::InCall { .. } => PHASE_IN_CALL,
        WhatsAppView::Incoming { .. } if !placing => PHASE_INCOMING,
        _ if placing => PHASE_PLACING,
        _ => PHASE_WAITING,
    }
}

/// Copy that means the WhatsApp call itself is over. Checked on the
/// interactive snapshot, so a chat message with the same words is not a button.
pub fn whatsapp_call_ended(snapshot: &str) -> bool {
    let lower = snapshot.to_lowercase();
    [
        "call ended",
        "this call has ended",
        "call declined",
        "you left the call",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn needs_login(snapshot: &str) -> bool {
    let lower = snapshot.to_lowercase();
    let login = [
        "scan the qr",
        "scan this qr",
        "log in with phone number",
        "link with phone number",
        "use whatsapp on your phone to scan",
        "log into whatsapp web",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if !login {
        return false;
    }
    // A signed-in chat can quote those words. The login screen has neither
    // the composer nor the chat search box.
    !lower.contains("type a message") && !lower.contains("search or start a new chat")
}

fn parse_controls(snapshot: &str) -> Vec<SnapshotControl> {
    snapshot.lines().filter_map(parse_control_line).collect()
}

fn parse_control_line(line: &str) -> Option<SnapshotControl> {
    let ref_id = extract_ref(line)?;
    let (role, name) = role_and_name(line)?;
    if name.is_empty() {
        return None;
    }
    Some(SnapshotControl { role, name, ref_id })
}

fn extract_ref(line: &str) -> Option<String> {
    let start = line.find("[ref=")? + "[ref=".len();
    let rest = &line[start..];
    let end = rest.find(']')?;
    let token = rest[..end].trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn role_and_name(line: &str) -> Option<(String, String)> {
    let q1 = line.find('"')?;
    let q2 = line[q1 + 1..].find('"')? + q1 + 1;
    let name = line[q1 + 1..q2].trim().to_lowercase();
    let before = line[..q1].trim();
    let role = before
        .split_whitespace()
        .next_back()?
        .trim_start_matches('-')
        .to_lowercase();
    Some((role, name))
}

fn is_button(control: &SnapshotControl) -> bool {
    control.role == "button"
}

fn is_invokable(control: &SnapshotControl) -> bool {
    control.role == "button" || control.role == "menuitem"
}

fn is_accept(name: &str) -> bool {
    matches!(name, "accept" | "answer" | "accept call" | "answer call")
}

fn is_decline(name: &str) -> bool {
    matches!(name, "decline" | "reject" | "decline call" | "reject call")
}

fn is_end(name: &str) -> bool {
    name == "end call" || name == "hang up" || name == "leave call" || name.starts_with("end call")
}

fn is_voice(name: &str) -> bool {
    if name.contains("video") {
        return false;
    }
    matches!(
        name,
        "voice" | "voice call" | "audio" | "audio call" | "start voice call"
    ) || name.starts_with("voice call")
}

fn is_continue_chat(name: &str) -> bool {
    name == "continue to chat"
}

/// The chat-header button that opens Voice / Video. The sidebar tab is
/// "Calls" and must not be clicked — it leaves the conversation.
fn is_call_opener(name: &str) -> bool {
    name == "call"
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Enough decoding for a `phone` value. `%2B` is `+`; other bytes stay as
/// characters so the digit filter can drop them.
fn decode_query_component(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_jid_is_the_party_and_a_name_or_lid_is_not() {
        let phone = "12025550101";
        assert_eq!(
            whatsapp_party_digits("12025550101:12@s.whatsapp.net").as_deref(),
            Some(phone)
        );
        assert_eq!(
            whatsapp_party_digits("+1 (202) 555-0101").as_deref(),
            Some(phone)
        );
        assert_eq!(whatsapp_party_digits(&format!("{phone}@lid")), None);
        assert_eq!(whatsapp_party_digits("Ada"), None);
        assert_eq!(whatsapp_party_digits("Ada 12025550101"), None);
        assert_eq!(whatsapp_party_digits("(161) WhatsApp"), None);
    }

    #[test]
    fn meet_url_is_not_whatsapp() {
        assert!(whatsapp_call_intent("https://meet.google.com/abc-defg-hij").is_none());
    }

    #[test]
    fn bare_whatsapp_waits_for_an_incoming_call() {
        assert_eq!(
            whatsapp_call_intent("https://web.whatsapp.com/"),
            Some(WhatsAppCallIntent::AnswerIncoming)
        );
        assert_eq!(
            whatsapp_call_intent("https://web.whatsapp.com"),
            Some(WhatsAppCallIntent::AnswerIncoming)
        );
    }

    #[test]
    fn send_url_places_a_voice_call_with_digits_only() {
        assert_eq!(
            whatsapp_call_intent("https://web.whatsapp.com/send?phone=+1 202 555-0101"),
            Some(WhatsAppCallIntent::PlaceVoice {
                phone: "12025550101".into()
            })
        );
        assert_eq!(
            whatsapp_call_intent("https://web.whatsapp.com/send?phone=%2B12025550101"),
            Some(WhatsAppCallIntent::PlaceVoice {
                phone: "12025550101".into()
            })
        );
    }

    #[test]
    fn short_phone_is_rejected() {
        assert_eq!(
            whatsapp_call_intent("https://web.whatsapp.com/send?phone=123"),
            Some(WhatsAppCallIntent::InvalidPhone)
        );
    }

    #[test]
    fn agent_browser_snapshot_format_reports_an_incoming_call() {
        // Captured from `agent-browser snapshot` of a local page, 2026-10-02.
        // Full snapshot keeps StaticText; `snapshot -i` drops it. Accept and
        // Decline outrank End call, Voice, and the Calls tab.
        let snapshot = r#"
- button "Calls" [ref=e1]
- button "Video call" [ref=e2]
- button "Call" [ref=e3]
- menuitem "Voice" [ref=e4]
- button "Accept" [ref=e5]
- button "Decline" [ref=e6]
- button "End call" [ref=e7]
- paragraph
  - StaticText "Scan the QR code to pay"
- textbox "Type a message" [ref=e8]
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::Incoming {
                accept_ref: "e5".into()
            }
        );
        assert_eq!(
            whatsapp_call_phase(&classify_whatsapp_snapshot(snapshot), false),
            PHASE_INCOMING
        );
    }

    #[test]
    fn accept_and_decline_together_are_an_incoming_call() {
        let snapshot = r#"
- button "Mute" [ref=e1]
- button "Accept" [ref=e4]
- button "Decline" [ref=e5]
- button "Calls" [ref=e2]
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::Incoming {
                accept_ref: "e4".into()
            }
        );
    }

    #[test]
    fn answer_and_reject_are_an_incoming_call() {
        let snapshot = r#"
- button "Answer" [ref=e7]
- button "Reject" [ref=e8]
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::Incoming {
                accept_ref: "e7".into()
            }
        );
        assert_eq!(
            whatsapp_call_phase(&classify_whatsapp_snapshot(snapshot), false),
            PHASE_INCOMING
        );
    }

    #[test]
    fn an_incoming_pair_while_placing_stays_on_the_place_phase() {
        let snapshot = r#"
- button "Accept" [ref=e4]
- button "Decline" [ref=e5]
"#;
        assert_eq!(
            whatsapp_call_phase(&classify_whatsapp_snapshot(snapshot), true),
            PHASE_PLACING
        );
    }

    #[test]
    fn accept_without_decline_is_not_an_incoming_call() {
        let snapshot = r#"- button "Accept" [ref=e4]"#;
        assert_eq!(classify_whatsapp_snapshot(snapshot), WhatsAppView::Waiting);
        assert_eq!(
            whatsapp_call_phase(&classify_whatsapp_snapshot(snapshot), false),
            PHASE_WAITING
        );
    }

    #[test]
    fn voice_menu_is_chosen_over_video_and_the_calls_tab() {
        let snapshot = r#"
- button "Calls" [ref=e1]
- menuitem "Video" [ref=e8]
- menuitem "Voice" [ref=e9]
- button "Call" [ref=e3]
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::VoiceReady {
                voice_ref: "e9".into()
            }
        );
    }

    #[test]
    fn video_call_button_is_never_the_voice_control() {
        let snapshot = "- button \"Video call\" [ref=e3]\n- button \"Call\" [ref=e4]\n";
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::CallMenuClosed {
                call_ref: "e4".into()
            }
        );
    }

    #[test]
    fn end_call_means_the_call_is_up() {
        let snapshot = r#"- button "End call" [ref=e20] - button "Mute" [ref=e21]"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::InCall {
                end_ref: "e20".into()
            }
        );
        assert_eq!(
            whatsapp_call_phase(&classify_whatsapp_snapshot(snapshot), false),
            PHASE_IN_CALL
        );
    }

    #[test]
    fn qr_screen_needs_login() {
        let snapshot = "Scan the QR code\n- button \"Log in with phone number\" [ref=e2]";
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::NeedsLogin
        );
    }

    #[test]
    fn live_whatsapp_web_login_snapshot_needs_login() {
        // `agent-browser snapshot` of https://web.whatsapp.com/ in the meet-bot
        // profile, headed Chrome 148, 2026-10-02. HeadlessChrome is rejected
        // before this page. The profile was not linked.
        let snapshot = r#"
- button "Link with phone number instead." [ref=e1]
  - StaticText "Link with phone number instead."
- StaticText "Scan the QR code with your phone's camera"
- image "Scan this QR code to link a device!"
- button "Log in with phone number" [ref=e2]
  - StaticText "Log in with phone number"
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::NeedsLogin
        );
    }

    #[test]
    fn a_signed_in_chat_quoting_qr_words_is_not_the_login_screen() {
        let snapshot = r#"
Scan the QR code to pay
- textbox "Type a message" [ref=e3]
- button "Call" [ref=e4]
"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::CallMenuClosed {
                call_ref: "e4".into()
            }
        );
    }

    #[test]
    fn continue_to_chat_is_its_own_step() {
        let snapshot = r#"- button "Continue to chat" [ref=e6]"#;
        assert_eq!(
            classify_whatsapp_snapshot(snapshot),
            WhatsAppView::ContinueToChat {
                continue_ref: "e6".into()
            }
        );
    }

    #[test]
    fn call_ended_copy_ends_the_session() {
        assert!(whatsapp_call_ended("- button \"Call ended\" [ref=e1]"));
        assert!(!whatsapp_call_ended("- button \"End call\" [ref=e1]"));
    }
}
