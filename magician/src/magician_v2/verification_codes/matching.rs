//! The challenge context, the message evidence, and the verdict (plan §6.2).
//!
//! Matching uses every trusted fact and nothing the message merely says
//! about itself: the scope; the receiving account; the provider's own receive
//! time inside the challenge's window (never the message's `Date`); the
//! sender's registrable domain against the destination a challenge bound the
//! ask to; provider authentication when there is no binding to check against;
//! message identity for dedup. Sender display names, SMS labels and app
//! package names are hints, not identity. When the evidence cannot separate
//! two candidates — or two live challenges — the answer is `Ambiguous` and a
//! person decides.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::extract::{extract_code, ExpectedFormat, Extraction};

/// How far before the challenge started a message may have arrived and
/// still be this challenge's: a "Send code" the adapter pressed a moment
/// before the ask was raised.
pub const DEFAULT_LOOKBACK_MS: i64 = 90_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Gmail,
    AgentMail,
    Messages,
    AndroidNotification,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::AgentMail => "agentmail",
            Self::Messages => "messages",
            Self::AndroidNotification => "android_notification",
        }
    }

    /// Whether the transport authenticates the sender at all. SMS and app
    /// notifications never do; mail does when the provider says so.
    pub fn can_authenticate_sender(self) -> bool {
        matches!(self, Self::Gmail | Self::AgentMail)
    }
}

/// The challenge as the resolver knows it: the pending `otp` ask's
/// published, value-free facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeContext {
    pub principal: String,
    pub workspace: String,
    pub correlation_id: String,
    /// `hitl.requested` timestamp — server-observed challenge start.
    pub started_at_ms: i64,
    pub deadline_ms: Option<i64>,
    /// The host of the destination a challenge bound the ask to (P4), when
    /// there is one: the service the code must come from.
    pub expected_host: Option<String>,
    pub expected: ExpectedFormat,
    pub lookback_ms: i64,
    /// The moment the previous code challenge FOR THIS SERVICE was decided,
    /// when there was one. The lookback may not reach behind it: a code that
    /// answered — or was shown to — the last challenge is spent, and whether
    /// the resolver, the owner or the phone spent it is not knowable here.
    /// Without this clamp the lookback quietly re-offered it to the next
    /// challenge, which is how a code typed by hand could be replayed. Keyed
    /// per bound host, so one site's login does not truncate another's window.
    pub not_before_ms: Option<i64>,
    /// The lane that owns the ask (`agentic`, `user_request`, …), as the
    /// announcement named it. A source that answers the ask ITSELF — the
    /// Android companion — has to post it back, and guessing is how every
    /// deposit was refused.
    pub lane: String,
}

impl ChallengeContext {
    pub fn window_start_ms(&self) -> i64 {
        let lookback_start = self.started_at_ms - self.lookback_ms;
        match self.not_before_ms {
            Some(not_before) => lookback_start.max(not_before),
            None => lookback_start,
        }
    }

    pub fn window_contains(&self, received_at_ms: i64) -> bool {
        received_at_ms >= self.window_start_ms()
            && self
                .deadline_ms
                .map_or(true, |deadline| received_at_ms <= deadline)
    }
}

/// One message as a source watcher saw it. In memory only; dropped after
/// matching. `Debug` never prints the body or the subject.
#[derive(Clone)]
pub struct MessageEvidence {
    pub source: SourceKind,
    /// The receiving account as the source registry names it.
    pub account: String,
    pub message_id: String,
    /// The provider's receive time (Gmail `internalDate`, the Messages
    /// store's date, the notification's post time).
    pub received_at_ms: i64,
    pub sender_address: Option<String>,
    /// Provider-verified authentication (`dkim=pass`/`spf=pass`); `None`
    /// when the transport offers none.
    pub authenticated: Option<bool>,
    pub subject: Option<String>,
    pub body: String,
}

impl std::fmt::Debug for MessageEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageEvidence")
            .field("source", &self.source)
            .field("message_id", &self.message_id)
            .field("received_at_ms", &self.received_at_ms)
            .field("sender_domain", &self.sender_domain())
            .field("authenticated", &self.authenticated)
            .finish_non_exhaustive()
    }
}

impl MessageEvidence {
    pub fn sender_domain(&self) -> Option<String> {
        let address = self.sender_address.as_deref()?;
        let domain = address
            .rsplit('@')
            .next()?
            .trim()
            .trim_end_matches('>')
            .to_ascii_lowercase();
        (!domain.is_empty() && domain != address.to_ascii_lowercase()).then_some(domain)
    }

    fn text(&self) -> String {
        match &self.subject {
            Some(subject) => format!("{subject}\n{}", self.body),
            None => self.body.clone(),
        }
    }

    pub fn identity(&self) -> (SourceKind, String, String) {
        (self.source, self.account.clone(), self.message_id.clone())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The message is this challenge's and carries exactly one code.
    Match { code: String },
    /// Not this challenge's message, with the fact that ruled it out.
    NoMatch { reason: String },
    /// Eligible but undecidable: the person answers.
    Ambiguous { reason: String },
}

/// A verdict prints its kind and reason, never the code.
impl std::fmt::Debug for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Match { .. } => f.write_str("Match { code: <redacted> }"),
            Self::NoMatch { reason } => f.debug_struct("NoMatch").field("reason", reason).finish(),
            Self::Ambiguous { reason } => {
                f.debug_struct("Ambiguous").field("reason", reason).finish()
            },
        }
    }
}

/// A message's identity for dedup: the source, the receiving account and
/// the provider's message id.
pub type MessageIdentity = (SourceKind, String, String);

/// Judge one message against one challenge.
pub fn judge(challenge: &ChallengeContext, message: &MessageEvidence) -> Verdict {
    if !challenge.window_contains(message.received_at_ms) {
        return Verdict::NoMatch {
            reason: if message.received_at_ms < challenge.window_start_ms() {
                "received before the challenge's window (a stale code)".to_string()
            } else {
                "received after the challenge's deadline".to_string()
            },
        };
    }
    match (&challenge.expected_host, message.sender_domain()) {
        (Some(host), Some(domain)) => {
            if !domain_matches_host(&domain, host) {
                return Verdict::NoMatch {
                    reason: format!(
                        "sender domain does not belong to the bound destination {host}"
                    ),
                };
            }
            // A sender that claims the destination's domain is exactly the
            // forgery this guards, so for a source whose provider CAN
            // authenticate a sender, the verdict must be a pass — an absent
            // verdict is not consent. Accepting `None` here meant a mailbox
            // with no authentication at all (AgentMail never has one) decided
            // on a `From` header anybody can write: send mail claiming the
            // destination's domain into the window and its digits were used.
            if message.source.can_authenticate_sender() && message.authenticated != Some(true) {
                return Verdict::NoMatch {
                    reason: if message.authenticated.is_some() {
                        "the provider failed the sender's authentication".to_string()
                    } else {
                        "the provider wrote no authentication verdict for a sender that claims the bound destination".to_string()
                    },
                };
            }
        },
        (Some(_), None) if message.source.can_authenticate_sender() => {
            return Verdict::NoMatch {
                reason: "the message names no sender to check against the bound destination"
                    .to_string(),
            };
        },
        (Some(_), None) => {
            // SMS / notifications: no sender identity to check; the window
            // and the cue carry the decision, the person keeps the last word
            // when two candidates exist.
        },
        (None, _) => {
            // No binding to check against: mail must at least be
            // authenticated by its provider; a failed check is a refusal.
            if message.source.can_authenticate_sender() && message.authenticated != Some(true) {
                return Verdict::NoMatch {
                    reason: "no bound destination and the provider did not authenticate the sender"
                        .to_string(),
                };
            }
        },
    }
    match extract_code(&message.text(), challenge.expected) {
        Extraction::Code(code) => Verdict::Match { code },
        Extraction::Ambiguous { candidates } => Verdict::Ambiguous {
            reason: format!("the message carries {candidates} candidate codes"),
        },
        Extraction::None { reason } => Verdict::NoMatch {
            reason: reason.to_string(),
        },
    }
}

/// `mail.accounts.example.test` belongs to `accounts.example.test`; so does
/// `example.test` (a service often mails from its apex or a sibling
/// subdomain). Registrable-domain comparison against the public-suffix
/// list: `attacker.co.uk` never matches `bank.co.uk`, and a public suffix
/// on its own is nobody's domain.
pub fn domain_matches_host(sender_domain: &str, expected_host: &str) -> bool {
    let host = host_of(expected_host);
    let sender = sender_domain.trim().to_ascii_lowercase();
    if host.is_empty() || sender.is_empty() {
        return false;
    }
    match (registrable_domain(&host), registrable_domain(&sender)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The registrable domain of a host name (`accounts.example.co.uk` →
/// `example.co.uk`) per the public-suffix list, lowercased; `None` for an
/// IP literal, a bare label or a public suffix itself.
pub fn registrable_domain(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() || name.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    // An unlisted suffix (`.test`, a private label) still has a registrable
    // domain by the list's own rule: the label before it.
    let domain = psl::domain(name.as_bytes())?;
    std::str::from_utf8(domain.as_bytes())
        .ok()
        .map(str::to_string)
}

/// The host of a destination given as an origin (`https://h:8443`), a
/// `host:port` or a bare host, lowercased.
pub fn host_of(destination: &str) -> String {
    let trimmed = destination.trim();
    if trimmed.contains("://") {
        if let Ok(url) = url::Url::parse(trimmed) {
            return url.host_str().unwrap_or_default().to_ascii_lowercase();
        }
    }
    trimmed
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// One poll's decision: the verdict and the identities of the messages
/// that produced a match (the ones a challenge consumes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    pub matched: Vec<MessageIdentity>,
}

/// Decide across every eligible message of one poll: exactly one distinct
/// code wins; two distinct codes, or a message the extractor found
/// ambiguous, hand the decision to the person. Messages already judged
/// (`seen`) are skipped — a message is judged once per challenge — and a
/// code `is_stale` says answered an earlier challenge is not a candidate,
/// so the fresh code beside it still answers.
pub fn decide(
    challenge: &ChallengeContext,
    messages: &[MessageEvidence],
    seen: &mut HashSet<MessageIdentity>,
    is_stale: &dyn Fn(&str) -> bool,
) -> Decision {
    let mut codes: HashSet<String> = HashSet::new();
    let mut matched = Vec::new();
    let mut ambiguous: Option<String> = None;
    let mut last_reason = "no eligible message".to_string();
    for message in messages {
        if !seen.insert(message.identity()) {
            continue;
        }
        match judge(challenge, message) {
            Verdict::Match { code } if is_stale(&code) => {
                last_reason = "the code answered an earlier challenge".to_string();
            },
            Verdict::Match { code } => {
                codes.insert(code);
                matched.push(message.identity());
            },
            Verdict::Ambiguous { reason } => {
                ambiguous = Some(reason);
                matched.push(message.identity());
            },
            Verdict::NoMatch { reason } => last_reason = reason,
        }
    }
    let verdict = match (codes.len(), ambiguous) {
        (1, None) => Verdict::Match {
            code: codes.into_iter().next().unwrap_or_default(),
        },
        (0, None) => Verdict::NoMatch {
            reason: last_reason,
        },
        (0, Some(reason)) => Verdict::Ambiguous { reason },
        (_, _) => Verdict::Ambiguous {
            reason: "several eligible messages carry different codes".to_string(),
        },
    };
    Decision { verdict, matched }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(expected_host: Option<&str>) -> ChallengeContext {
        ChallengeContext {
            principal: "owner".into(),
            workspace: "ws".into(),
            correlation_id: "req-1".into(),
            started_at_ms: 1_000_000,
            deadline_ms: Some(1_600_000),
            expected_host: expected_host.map(host_of),
            expected: ExpectedFormat::default(),
            lookback_ms: DEFAULT_LOOKBACK_MS,
            not_before_ms: None,
            lane: "agentic".to_string(),
        }
    }

    fn mail(
        id: &str,
        received_at_ms: i64,
        from: &str,
        authenticated: Option<bool>,
        body: &str,
    ) -> MessageEvidence {
        MessageEvidence {
            source: SourceKind::Gmail,
            account: "personal".into(),
            message_id: id.into(),
            received_at_ms,
            sender_address: Some(from.into()),
            authenticated,
            subject: Some("Your code".into()),
            body: body.into(),
        }
    }

    #[test]
    fn the_window_is_anchored_on_the_servers_challenge_start_not_the_messages_date() {
        let c = challenge(Some("https://accounts.example.test"));
        let fresh = mail(
            "m1",
            1_010_000,
            "no-reply@accounts.example.test",
            Some(true),
            "Your verification code is 042917",
        );
        assert_eq!(
            judge(&c, &fresh),
            Verdict::Match {
                code: "042917".into()
            }
        );
        let just_before = mail(
            "m2",
            1_000_000 - 60_000,
            "no-reply@accounts.example.test",
            Some(true),
            "Your verification code is 131313",
        );
        assert!(
            matches!(judge(&c, &just_before), Verdict::Match { .. }),
            "a code sent a minute before the ask is inside the lookback"
        );
        let stale = mail(
            "m3",
            1_000_000 - 120_000,
            "no-reply@accounts.example.test",
            Some(true),
            "Your verification code is 242424",
        );
        assert!(
            matches!(judge(&c, &stale), Verdict::NoMatch { reason } if reason.contains("stale"))
        );
        let late = mail(
            "m4",
            1_700_000,
            "no-reply@accounts.example.test",
            Some(true),
            "Your verification code is 353535",
        );
        assert!(
            matches!(judge(&c, &late), Verdict::NoMatch { reason } if reason.contains("deadline"))
        );
        // The lookback never reaches behind the previous challenge's decision:
        // a code the owner typed (or the phone deposited) for that one is spent
        // whether or not this process matched it.
        let after_a_resolution = ChallengeContext {
            not_before_ms: Some(1_000_000 - 30_000),
            ..challenge(Some("https://accounts.example.test"))
        };
        assert_eq!(after_a_resolution.window_start_ms(), 1_000_000 - 30_000);
        assert!(
            matches!(judge(&after_a_resolution, &just_before), Verdict::NoMatch { reason } if reason.contains("stale")),
            "a code from before the last resolution is not a candidate"
        );
        assert!(matches!(
            judge(&after_a_resolution, &fresh),
            Verdict::Match { .. }
        ));
    }

    #[test]
    fn the_sender_must_belong_to_the_bound_destination_and_pass_authentication() {
        let c = challenge(Some("https://accounts.example.test:8443"));
        assert!(domain_matches_host(
            "mail.example.test",
            "accounts.example.test:8443"
        ));
        assert!(domain_matches_host(
            "mail.example.test",
            "https://accounts.example.test:8443"
        ));
        assert_eq!(
            host_of("https://Accounts.Example.test:8443/x"),
            "accounts.example.test"
        );
        assert!(domain_matches_host("example.test", "accounts.example.test"));
        assert!(!domain_matches_host(
            "example.test.evil.net",
            "accounts.example.test"
        ));
        assert!(!domain_matches_host("examp1e.test", "example.test"));
        // Public suffixes: the last two labels are not a registrable domain.
        assert!(domain_matches_host(
            "mail.bank.co.uk",
            "https://online.bank.co.uk"
        ));
        assert!(!domain_matches_host(
            "attacker.co.uk",
            "https://online.bank.co.uk"
        ));
        assert!(!domain_matches_host("co.uk", "https://online.bank.co.uk"));
        assert!(
            !domain_matches_host("evil.github.io", "https://bank.github.io"),
            "a private suffix separates its tenants"
        );
        assert_eq!(
            registrable_domain("Accounts.Example.co.uk."),
            Some("example.co.uk".to_string())
        );
        assert_eq!(registrable_domain("10.0.0.1"), None);
        assert_eq!(registrable_domain("localhost"), None);
        let spoof = mail(
            "m1",
            1_010_000,
            "security@examp1e.test",
            Some(true),
            "Your verification code is 042917",
        );
        assert!(
            matches!(judge(&c, &spoof), Verdict::NoMatch { reason } if reason.contains("bound destination"))
        );
        let forged = mail(
            "m2",
            1_010_000,
            "security@example.test",
            Some(false),
            "Your verification code is 042917",
        );
        assert!(
            matches!(judge(&c, &forged), Verdict::NoMatch { reason } if reason.contains("authentication"))
        );
        // A sender claiming the bound destination is exactly the forgery this
        // guards: from a provider that CAN authenticate, no verdict is not
        // consent. (A source that cannot — SMS, a notification — is judged by
        // the window and the cue; see the next test.)
        let unknown_auth = mail(
            "m3",
            1_010_000,
            "security@example.test",
            None,
            "Your verification code is 042917",
        );
        assert!(
            matches!(judge(&c, &unknown_auth), Verdict::NoMatch { reason } if reason.contains("no authentication verdict")),
            "an absent verdict from a provider that can authenticate is refused"
        );
        let anonymous = MessageEvidence {
            sender_address: None,
            ..mail(
                "m4",
                1_010_000,
                "x@y",
                Some(true),
                "Your verification code is 042917",
            )
        };
        assert!(matches!(judge(&c, &anonymous), Verdict::NoMatch { .. }));
    }

    #[test]
    fn without_a_binding_mail_needs_provider_authentication_and_sms_needs_only_the_window() {
        let c = challenge(None);
        let unauthenticated = mail(
            "m1",
            1_010_000,
            "codes@anything.test",
            None,
            "Your verification code is 042917",
        );
        assert!(
            matches!(judge(&c, &unauthenticated), Verdict::NoMatch { reason } if reason.contains("authenticate"))
        );
        let authenticated = mail(
            "m2",
            1_010_000,
            "codes@anything.test",
            Some(true),
            "Your verification code is 042917",
        );
        assert_eq!(
            judge(&c, &authenticated),
            Verdict::Match {
                code: "042917".into()
            }
        );
        let sms = MessageEvidence {
            source: SourceKind::AndroidNotification,
            account: "pixel".into(),
            message_id: "n1".into(),
            received_at_ms: 1_010_000,
            sender_address: Some("VERIFY".into()),
            authenticated: None,
            subject: None,
            body: "Your login code is 771122".into(),
        };
        assert_eq!(
            judge(&c, &sms),
            Verdict::Match {
                code: "771122".into()
            }
        );
        assert_eq!(
            judge(&challenge(Some("https://shop.example.test")), &sms),
            Verdict::Match {
                code: "771122".into()
            },
            "an SMS label cannot be checked against a host; the window decides"
        );
    }

    #[test]
    fn a_poll_with_two_different_codes_or_a_seen_message_is_handled_by_a_person_or_skipped() {
        let c = challenge(Some("https://accounts.example.test"));
        let a = mail(
            "m1",
            1_010_000,
            "a@example.test",
            Some(true),
            "Your verification code is 131313",
        );
        let b = mail(
            "m2",
            1_011_000,
            "a@example.test",
            Some(true),
            "Your verification code is 242424",
        );
        let mut seen = HashSet::new();
        let never_stale = |_: &str| false;
        let both = decide(&c, &[a.clone(), b.clone()], &mut seen, &never_stale);
        assert!(matches!(both.verdict, Verdict::Ambiguous { .. }));
        assert_eq!(
            both.matched.len(),
            2,
            "both eligible messages are the challenge's, whichever way it ends"
        );
        // A message judged once is never judged again for this challenge.
        assert!(matches!(
            decide(&c, &[a.clone()], &mut seen, &never_stale).verdict,
            Verdict::NoMatch { .. }
        ));
        let mut fresh = HashSet::new();
        let once = decide(&c, &[a.clone(), a.clone()], &mut fresh, &never_stale);
        assert_eq!(
            once.verdict,
            Verdict::Match {
                code: "131313".into()
            },
            "the same message twice is one"
        );
        assert_eq!(once.matched, vec![a.identity()]);
        let ambiguous_body = mail(
            "m3",
            1_012_000,
            "a@example.test",
            Some(true),
            "Your verification code is 353535 or 464646",
        );
        assert!(matches!(
            decide(&c, &[ambiguous_body], &mut HashSet::new(), &never_stale).verdict,
            Verdict::Ambiguous { .. }
        ));
        // A stale code beside the fresh one is skipped, not a second candidate.
        let stale_is_a = |code: &str| code == "131313";
        let retry = decide(
            &c,
            &[a.clone(), b.clone()],
            &mut HashSet::new(),
            &stale_is_a,
        );
        assert_eq!(
            retry.verdict,
            Verdict::Match {
                code: "242424".into()
            }
        );
        assert_eq!(retry.matched, vec![b.identity()]);
        assert!(
            matches!(decide(&c, &[a.clone()], &mut HashSet::new(), &stale_is_a).verdict, Verdict::NoMatch { reason } if reason.contains("earlier"))
        );
        let debug = format!("{:?}", a);
        assert!(
            !debug.contains("131313") && !debug.contains("Your code"),
            "{debug}"
        );
        let verdict = format!(
            "{:?}",
            Verdict::Match {
                code: "131313".into()
            }
        );
        assert!(!verdict.contains("131313"), "{verdict}");
    }
}
