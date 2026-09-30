//! Criticality and the safe alert card (plan §6.1).
//!
//! Criticality is structural — read from the live request the server
//! published, never from an urgency word a model chose. The card carries a
//! service alias derived from the server-side binding, a reason that names
//! the *kind* of thing needed, a trustworthy deadline when the request has
//! one, and the link to the exact request. Never the prompt, the hint, an
//! option label, a mailbox preview, a token or an answer.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Why a request is critical: it collects a secret, or it is time-bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Criticality {
    /// The request's own deadline (the spec's `collection_deadline_ms`, or a
    /// producer's `collection_deadline_ms`), when it has one.
    pub deadline_ms: Option<i64>,
}

impl Criticality {
    pub fn time_bound(self) -> bool {
        self.deadline_ms.is_some()
    }

    /// Decide from a `hitl.requested` `input_schema`. A request with the
    /// value-free sensitivity spec (P3) is critical; so is any request that
    /// names a collection deadline. Everything else is ordinary attention.
    pub fn of(input_schema: Option<&Value>) -> Option<Self> {
        let schema = input_schema?;
        let sensitive = schema.get("sensitive").filter(|value| value.is_object());
        let deadline_ms = sensitive
            .and_then(|spec| spec.get("collection_deadline_ms"))
            .and_then(Value::as_i64)
            .or_else(|| schema.get("collection_deadline_ms").and_then(Value::as_i64))
            .filter(|deadline| *deadline > 0);
        if sensitive.is_none() && deadline_ms.is_none() {
            return None;
        }
        Some(Self { deadline_ms })
    }
}

/// What kind of delivery a card announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Request,
    /// The owner's own test from the settings surface.
    Test,
}

impl AlertKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Test => "test",
        }
    }
}

/// The value-free alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertCard {
    pub kind: AlertKind,
    /// A host the runtime bound the request to (P4), or "a service".
    pub service_alias: String,
    /// What is needed, by kind: "a verification code", "your password", …
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
    /// The exact request behind normal authentication; absent when the
    /// runtime has no public origin, in which case the text says where to go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
}

const MAX_ALIAS_BYTES: usize = 96;

impl AlertCard {
    pub fn for_request(
        input_schema: Option<&Value>,
        correlation_id: &str,
        public_origin: Option<&str>,
    ) -> Self {
        let spec = input_schema
            .and_then(|schema| schema.get("sensitive"))
            .filter(|v| v.is_object());
        let input_type = input_schema
            .and_then(|schema| schema.get("input_type"))
            .and_then(Value::as_str);
        Self {
            kind: AlertKind::Request,
            service_alias: service_alias(spec),
            reason: reason_for(spec, input_type).to_string(),
            deadline_ms: Criticality::of(input_schema).and_then(|c| c.deadline_ms),
            open_url: open_url(public_origin, correlation_id),
        }
    }

    pub fn test(public_origin: Option<&str>, _correlation_id: &str) -> Self {
        Self {
            kind: AlertKind::Test,
            service_alias: "Magician".to_string(),
            reason: "a test alert you triggered from Settings".to_string(),
            deadline_ms: None,
            // The list, not this delivery's id: a test belongs to no request,
            // so naming it would land the owner on "Item no longer available".
            open_url: attention_list_url(public_origin),
        }
    }

    /// The one sentence every channel sends, so the owner reads the same
    /// alert everywhere.
    pub fn text(&self, now_ms: i64) -> String {
        let mut text = match self.kind {
            AlertKind::Test => format!(
                "Magician test alert: this is where {} would arrive.",
                self.reason
            ),
            AlertKind::Request => format!(
                "Magician needs {} for {} to continue.",
                self.reason, self.service_alias
            ),
        };
        match &self.open_url {
            Some(url) => text.push_str(&format!(" Open the secure request: {url}")),
            None => text.push_str(" Open Magician → Attention to answer it securely."),
        }
        if let Some(deadline_ms) = self.deadline_ms {
            text.push_str(&format!(" {}", expiry_phrase(deadline_ms, now_ms)));
        }
        text.push_str(" Don't reply with the value here.");
        text
    }

    /// The card as the transport carries it: the structured fields plus the
    /// rendered `text`.
    pub fn to_value(&self, now_ms: i64) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert("text".to_string(), Value::String(self.text(now_ms)));
        }
        value
    }
}

/// The alias is the runtime's own binding (the host of the destination a
/// challenge named, P4) — never a producer- or model-authored label, which
/// could carry anything.
fn service_alias(spec: Option<&Value>) -> String {
    spec.and_then(|spec| spec.get("expected_destination"))
        .and_then(Value::as_str)
        .and_then(host_of)
        .map(|host| bounded(&host, MAX_ALIAS_BYTES))
        .unwrap_or_else(|| "a service".to_string())
}

fn host_of(destination: &str) -> Option<String> {
    let parsed = url::Url::parse(destination.trim()).ok()?;
    let host = parsed.host_str()?;
    if host.is_empty() {
        return None;
    }
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn reason_for(spec: Option<&Value>, input_type: Option<&str>) -> &'static str {
    let kind = spec.and_then(|s| s.get("kind")).and_then(Value::as_str);
    let field_kinds: Vec<&str> = spec
        .and_then(|s| s.get("fields"))
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.get("kind").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if kind == Some("otp") || field_kinds.contains(&"otp") || input_type == Some("otp") {
        "a verification code"
    } else if field_kinds.len() > 1 || field_kinds.contains(&"login_identifier") {
        "your sign-in details"
    } else if kind == Some("password")
        || field_kinds.contains(&"password")
        || input_type == Some("password")
    {
        "your password"
    } else if spec.is_some() {
        "a private value"
    } else {
        "a decision"
    }
}

/// `{origin}/attention?attention=1&attention_item={id}` — the id alone; the
/// page is behind login and scope, and a GET is read-only. `attention_item`
/// is what the web UI's Attention center opens as an exact item (a
/// correlation id is one of a row's aliases); `attention=1` keeps the list
/// behind it so an item that already resolved shows "no longer available"
/// instead of an empty page.
pub fn open_url(public_origin: Option<&str>, correlation_id: &str) -> Option<String> {
    attention_url(public_origin, Some(correlation_id))
}

/// The Attention centre with no item named — where the owner's own test alert
/// points. A test delivery's correlation id belongs to no request, so naming it
/// as `attention_item` sends the owner to a page that truthfully answers "Item
/// no longer available", which reads as a broken link rather than as a working
/// test.
pub fn attention_list_url(public_origin: Option<&str>) -> Option<String> {
    attention_url(public_origin, None)
}

fn attention_url(public_origin: Option<&str>, correlation_id: Option<&str>) -> Option<String> {
    let origin = public_origin?.trim().trim_end_matches('/');
    if origin.is_empty() {
        return None;
    }
    let mut url = url::Url::parse(origin).ok()?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return None;
    }
    url.set_path("/attention");
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut().append_pair("attention", "1");
    if let Some(correlation_id) = correlation_id {
        url.query_pairs_mut()
            .append_pair("attention_item", correlation_id);
    }
    Some(url.to_string())
}

fn expiry_phrase(deadline_ms: i64, now_ms: i64) -> String {
    let remaining_ms = deadline_ms - now_ms;
    if remaining_ms <= 0 {
        return "It may already have expired.".to_string();
    }
    let minutes = (remaining_ms + 59_999) / 60_000;
    if minutes <= 1 {
        "It expires within a minute.".to_string()
    } else if minutes < 120 {
        format!("It expires in about {minutes} minutes.")
    } else {
        format!("It expires in about {} hours.", (minutes + 30) / 60)
    }
}

fn bounded(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn criticality_comes_from_the_spec_or_a_deadline_never_from_wording() {
        let sensitive = json!({"sensitive": {"kind": "otp", "one_time": true, "collection_deadline_ms": 1_700_000_000_000_i64}});
        let critical = Criticality::of(Some(&sensitive)).expect("a code ask is critical");
        assert_eq!(critical.deadline_ms, Some(1_700_000_000_000));
        assert!(critical.time_bound());
        let password = json!({"sensitive": {"kind": "password"}});
        assert!(!Criticality::of(Some(&password)).unwrap().time_bound());
        let deadline_only = json!({"collection_deadline_ms": 42_000});
        assert!(Criticality::of(Some(&deadline_only)).unwrap().time_bound());
        let urgent_words = json!({"input_type": "text", "prompt": "URGENT!!! reply now"});
        assert!(Criticality::of(Some(&urgent_words)).is_none());
        assert!(Criticality::of(None).is_none());
    }

    #[test]
    fn the_card_names_the_kind_and_the_bound_host_and_nothing_the_producer_wrote() {
        let schema = json!({
            "input_type": "otp",
            "prompt": "Enter the 6-digit code we sent to +1 555 0100 for account alice@example.test",
            "hint": "the code is 123456",
            "options": [{"id": "a", "label": "secret option"}],
            "context": {"account_label": "model-authored label", "session_id": "s1"},
            "sensitive": {"kind": "otp", "one_time": true, "expected_destination": "https://accounts.example.test:8443", "collection_deadline_ms": 600_000}
        });
        let card = AlertCard::for_request(
            Some(&schema),
            "req-1",
            Some("https://magician.example.test/"),
        );
        assert_eq!(card.service_alias, "accounts.example.test:8443");
        assert_eq!(card.reason, "a verification code");
        assert_eq!(
            card.open_url.as_deref(),
            Some("https://magician.example.test/attention?attention=1&attention_item=req-1")
        );
        let text = card.text(60_000);
        assert!(
            text.contains("a verification code for accounts.example.test:8443"),
            "{text}"
        );
        assert!(text.contains("about 9 minutes"), "{text}");
        for leaked in [
            "123456",
            "alice",
            "555",
            "model-authored",
            "secret option",
            "6-digit",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked into {text}");
            assert!(!card.to_value(60_000).to_string().contains(leaked));
        }
        assert!(text.contains("Don't reply with the value here"));
    }

    #[test]
    fn a_card_without_a_public_origin_points_at_attention_and_never_builds_a_bad_link() {
        let schema = json!({"sensitive": {"kind": "password", "fields": [{"id":"u","kind":"login_identifier"},{"id":"p","kind":"password"}]}});
        let card = AlertCard::for_request(Some(&schema), "req-2", None);
        assert_eq!(card.reason, "your sign-in details");
        assert_eq!(card.service_alias, "a service");
        assert!(card.open_url.is_none());
        assert!(card.text(0).contains("Open Magician → Attention"));
        assert!(open_url(Some("ftp://x"), "id").is_none());
        assert!(open_url(Some("   "), "id").is_none());
        assert_eq!(
            open_url(Some("http://localhost:5173/some/path?x=1#frag"), "a b").as_deref(),
            Some("http://localhost:5173/attention?attention=1&attention_item=a+b")
        );
    }

    #[test]
    fn the_owner_test_alert_links_the_list_because_it_belongs_to_no_request() {
        // The owner's test delivery has a correlation id of its own, but no
        // request behind it. Naming it as `attention_item` sent the owner to a
        // page that truthfully answered "Item no longer available" — a working
        // test that reads as a broken link.
        let card = AlertCard::test(Some("https://magician.example.test/"), "test-abc123");
        assert_eq!(
            card.open_url.as_deref(),
            Some("https://magician.example.test/attention?attention=1"),
            "the test alert opens the Attention list, naming no item",
        );
        assert!(!card.open_url.as_deref().unwrap().contains("attention_item"));
        assert!(card.text(0).contains("Open the secure request:"));

        // A real request still opens its exact item.
        let schema = json!({"sensitive": {"kind": "otp"}});
        assert_eq!(
            AlertCard::for_request(
                Some(&schema),
                "req-9",
                Some("https://magician.example.test")
            )
            .open_url
            .as_deref(),
            Some("https://magician.example.test/attention?attention=1&attention_item=req-9"),
        );

        // And the list builder refuses what `open_url` refuses.
        assert!(attention_list_url(None).is_none());
        assert!(attention_list_url(Some("ftp://x")).is_none());
        assert!(attention_list_url(Some("   ")).is_none());
    }

    #[test]
    fn a_decision_with_a_deadline_is_worded_as_a_decision() {
        let schema = json!({"input_type": "choice", "collection_deadline_ms": 90_000});
        let card = AlertCard::for_request(Some(&schema), "req-3", None);
        assert_eq!(card.reason, "a decision");
        assert!(card.text(0).contains("about 2 minutes"));
        assert!(card.text(100_000).contains("already have expired"));
    }
}
