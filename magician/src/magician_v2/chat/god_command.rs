//! `/god` and `/godoff` typed into a chat.
//!
//! The owner turns god mode on or off for one chat by typing the command into
//! it — on WhatsApp (Kapso), Telegram, any channel bot, or the web composer.
//! The command sets the same per-chat override as the composer's Magician orb
//! (`god_mode::set_chat_override`); the agent never sees it.
//!
//! Who may: only the owner, decided server-side from facts the sender cannot
//! supply.
//!
//! - **Channel bots** ([`channel_sender_may_toggle`]): the sender must be the
//!   owner by [`envoy::is_owner`] — the owner allowlist plus server-derived
//!   verification — and must be the address the session itself was opened for.
//!   A bot token alone is not the owner.
//! - **Web**: the chat API applies the orb's own rule (owner login session or
//!   verified paired owner phone), then [`session_is_owner_lane`].
//!
//! Both refuse a session that belongs to a guest (bound to the envoy agent) or
//! to an engagement (`eng:` thread). A refused command is not answered: the
//! text goes to the agent like any other message, so a guest learns nothing
//! about the command from trying it.

use super::envoy;
use crate::config::EnvoyConfig;

/// A god-mode command typed into a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GodModeCommand {
    On,
    Off,
}

impl GodModeCommand {
    /// The command the whole message is, if it is one.
    ///
    /// Trimmed and case-insensitive: `/god`, `/god on`, `/godoff`, `/god off`.
    /// Telegram's `/god@BotName` form and a control prefix a bot did not strip
    /// (`/magic /god`, `@magic /god`) are accepted. Anything more in the
    /// message (`/god please`) is ordinary text.
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text.trim();
        for prefix in ["/magic", "@magic"] {
            if let Some(after) = strip_prefix_ignore_case(rest, prefix) {
                if after.starts_with(char::is_whitespace) {
                    rest = after.trim_start();
                    break;
                }
            }
        }
        let rest = rest.strip_prefix('/')?;
        let mut words = rest.split_whitespace();
        let first = words.next()?;
        let name = first
            .split('@')
            .next()
            .unwrap_or(first)
            .to_ascii_lowercase();
        let argument = words.next().map(str::to_ascii_lowercase);
        if words.next().is_some() {
            return None;
        }
        match (name.as_str(), argument.as_deref()) {
            ("god", None | Some("on")) => Some(Self::On),
            ("god", Some("off")) | ("godoff", None) => Some(Self::Off),
            _ => None,
        }
    }

    /// The per-chat override this command sets.
    pub fn enabled(self) -> bool {
        matches!(self, Self::On)
    }

    /// The assistant's reply. `auto_approve` is whether the chat's
    /// auto-approve applies, which changes what "on" means.
    pub fn reply(self, auto_approve: bool) -> &'static str {
        match (self, auto_approve) {
            (Self::On, true) => {
                "God mode is on for this chat. Magician's shell, file and web tools can go \
                 past the sandbox and run without asking, except deletes, root, code the \
                 line does not show and blocked commands. Send /godoff to turn it off."
            },
            (Self::On, false) => {
                "God mode is on for this chat. Magician's shell, file and web tools can go \
                 past the sandbox; each step outside it asks you first. Send /godoff to \
                 turn it off."
            },
            (Self::Off, _) => {
                "God mode is off for this chat. Magician's tools stay inside the sandbox. \
                 Send /god to turn it on."
            },
        }
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// The chat session a command would change.
#[derive(Debug, Clone, Copy)]
pub struct GodCommandSession<'a> {
    pub agent_id: &'a str,
    pub ui_thread_id: &'a str,
    pub origin_channel_type: &'a str,
    pub origin_address: Option<&'a str>,
}

/// Whether the session is the owner's own conversation: not a guest's (bound
/// to the envoy agent) and not an engagement's.
pub fn session_is_owner_lane(
    envoy_agent_id: Option<&str>,
    session: &GodCommandSession<'_>,
) -> bool {
    let agent = session.agent_id.trim();
    if envoy_agent_id
        .map(str::trim)
        .is_some_and(|envoy| !envoy.is_empty() && envoy == agent)
    {
        return false;
    }
    !session
        .ui_thread_id
        .trim()
        .starts_with(envoy::engagement_thread_id("").as_str())
}

/// Whether a channel bot's message may toggle god mode for `session`.
///
/// `verified` must be the server-derived [`envoy::channel_is_verified`], never
/// the caller's claim. `web` is refused here: the web composer has no address
/// to match, and the chat API checks the owner's login instead.
pub fn channel_sender_may_toggle(
    cfg: &EnvoyConfig,
    channel_type: &str,
    address: &str,
    verified: bool,
    session: &GodCommandSession<'_>,
) -> bool {
    let channel = channel_type.trim();
    let address = address.trim();
    if channel.is_empty() || channel.eq_ignore_ascii_case("web") || address.is_empty() {
        return false;
    }
    if !session_is_owner_lane(Some(cfg.envoy_agent_id.as_str()), session) {
        return false;
    }
    let same_origin = session
        .origin_channel_type
        .trim()
        .eq_ignore_ascii_case(channel)
        && session.origin_address.map(str::trim) == Some(address);
    same_origin && envoy::is_owner(cfg, channel, address, verified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EnvoyConfig {
        let mut owners = std::collections::HashMap::new();
        owners.insert("kapso".into(), vec!["+15551234567".into()]);
        owners.insert("telegram".into(), vec!["12345".into()]);
        EnvoyConfig {
            owner_identities: owners,
            envoy_agent_id: "envoy".into(),
            ..EnvoyConfig::default()
        }
    }

    fn owner_session<'a>(channel: &'a str, address: &'a str) -> GodCommandSession<'a> {
        GodCommandSession {
            agent_id: "personal-assistant",
            ui_thread_id: "ext:kapso:+15551234567:magic",
            origin_channel_type: channel,
            origin_address: Some(address),
        }
    }

    #[test]
    fn the_commands_parse_trimmed_and_case_insensitive() {
        for text in [
            "/god",
            "  /GOD ",
            "/god on",
            "/God@MagicianBot",
            "/magic /god",
            "@Magic /god",
        ] {
            assert_eq!(
                GodModeCommand::parse(text),
                Some(GodModeCommand::On),
                "{text}"
            );
        }
        for text in [
            "/godoff",
            "/GodOff\n",
            "/god off",
            "/godoff@MagicianBot",
            "/magic /godoff",
        ] {
            assert_eq!(
                GodModeCommand::parse(text),
                Some(GodModeCommand::Off),
                "{text}"
            );
        }
    }

    #[test]
    fn anything_else_is_ordinary_text() {
        for text in [
            "",
            "god",
            "/gods",
            "/godmode",
            "/god please",
            "/god off now",
            "please /god",
            "/magic",
            "/magicgod",
            "/new",
        ] {
            assert_eq!(GodModeCommand::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_command_sets_the_matching_override() {
        assert!(GodModeCommand::On.enabled());
        assert!(!GodModeCommand::Off.enabled());
        assert!(GodModeCommand::On.reply(false).contains("/godoff"));
        assert!(GodModeCommand::On.reply(true).contains("without asking"));
        assert!(GodModeCommand::Off
            .reply(true)
            .starts_with("God mode is off"));
    }

    #[test]
    fn the_verified_owner_may_toggle_their_own_chat() {
        let session = owner_session("kapso", "+15551234567");
        assert!(channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+15551234567",
            true,
            &session
        ));
        let telegram = GodCommandSession {
            ui_thread_id: "ext:telegram:12345",
            ..owner_session("telegram", "12345")
        };
        assert!(channel_sender_may_toggle(
            &cfg(),
            "telegram",
            "12345",
            true,
            &telegram
        ));
    }

    #[test]
    fn a_guest_never_toggles() {
        let session = owner_session("kapso", "+19998887777");
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+19998887777",
            true,
            &session
        ));
    }

    #[test]
    fn an_unverified_owner_address_never_toggles() {
        let session = owner_session("kapso", "+15551234567");
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+15551234567",
            false,
            &session
        ));
    }

    #[test]
    fn the_owner_cannot_toggle_a_session_opened_for_someone_else() {
        let guest_origin = owner_session("kapso", "+19998887777");
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+15551234567",
            true,
            &guest_origin
        ));
        let other_channel = owner_session("telegram", "+15551234567");
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+15551234567",
            true,
            &other_channel
        ));
    }

    #[test]
    fn a_guest_or_engagement_session_is_not_the_owners_lane() {
        let envoy = GodCommandSession {
            agent_id: "envoy",
            ui_thread_id: "ext:kapso:+15551234567",
            ..owner_session("kapso", "+15551234567")
        };
        assert!(!session_is_owner_lane(Some("envoy"), &envoy));
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "kapso",
            "+15551234567",
            true,
            &envoy
        ));
        let engagement = GodCommandSession {
            ui_thread_id: "eng:acme",
            ..owner_session("kapso", "+15551234567")
        };
        assert!(!session_is_owner_lane(Some("envoy"), &engagement));
        assert!(session_is_owner_lane(None, &owner_session("web", "")));
    }

    #[test]
    fn web_is_never_a_channel_sender() {
        let session = GodCommandSession {
            origin_channel_type: "web",
            origin_address: None,
            ..owner_session("web", "")
        };
        assert!(!channel_sender_may_toggle(
            &cfg(),
            "web",
            "",
            true,
            &session
        ));
        assert!(!channel_sender_may_toggle(&cfg(), "", "", true, &session));
    }
}
