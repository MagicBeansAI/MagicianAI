//! Redaction of the owner's preference wording for the internal analyst.
//!
//! The analyst debugs checks from structure: kinds, evidence strengths,
//! lifecycles, ids, revisions and timings. The wording of a preference can be
//! intimate (health, therapy, where the owner lives), so analyst reads replace
//! it with a placeholder unless the owner granted wording for that run
//! (`wording_grant.rs`). The placeholder keeps a short digest, so two mentions
//! of the same wording are recognisable without revealing it.

use serde_json::Value;

/// Fields that carry the owner's words, anywhere in a view, a history entry
/// or a check record.
const WORDING_FIELDS: &[&str] = &[
    "statement",
    "conditions",
    "wording_before",
    "wording_after",
    "explanation",
    "quote",
    "unresolved_facts",
];

/// `[taste · 6 words · #a3f9]`: kind, word count and a 4-hex digest.
pub fn redact_wording(kind: Option<&str>, text: &str) -> String {
    let words = text.split_whitespace().count();
    let digest = &blake3::hash(text.as_bytes()).to_hex()[..4];
    match kind {
        Some(kind) => format!("[{kind} · {words} words · #{digest}]"),
        None => format!("[{words} words · #{digest}]"),
    }
}

fn redact_field(value: &mut Value, kind: Option<&str>) {
    match value {
        Value::String(text) => *text = redact_wording(kind, text),
        Value::Array(items) => {
            for item in items {
                redact_field(item, kind);
            }
        },
        Value::Null => {},
        other => *other = Value::String(redact_wording(kind, &other.to_string())),
    }
}

/// Replace every wording field in `value`, at any depth, with its
/// placeholder. An object's own `kind` labels its placeholders.
pub fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let kind = map.get("kind").and_then(Value::as_str).map(str::to_owned);
            for (key, field) in map.iter_mut() {
                if WORDING_FIELDS.contains(&key.as_str()) {
                    redact_field(field, kind.as_deref());
                } else {
                    redact_value(field);
                }
            }
        },
        Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        },
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_word_of_a_preference_survives_and_the_structure_does() {
        let mut view = json!({
            "intentions": [{
                "id": "a", "kind": "standing_instruction", "assertion_status": "confirmed",
                "statement": "therapy: Don't mention my therapy appointments to anyone at work",
                "conditions": "when messaging colleagues",
                "grounds": [{"role": "owner", "source_id": "turn-1"}]
            }],
            "entries": [{"op": "edited", "wording_before": "therapy: old words", "wording_after": null}],
            "decision": {"explanation": "This mentions therapy.", "unresolved_facts": ["Whether therapy applies"]}
        });
        redact_value(&mut view);
        let text = view.to_string();
        for word in ["therapy", "colleagues", "old words", "appointments"] {
            assert!(!text.contains(word), "`{word}` leaked: {text}");
        }
        assert_eq!(view["intentions"][0]["assertion_status"], "confirmed");
        assert_eq!(view["intentions"][0]["grounds"][0]["source_id"], "turn-1");
        assert!(view["intentions"][0]["statement"]
            .as_str()
            .unwrap()
            .starts_with("[standing_instruction · 10 words · #"));
        assert!(view["entries"][0]["wording_after"].is_null());
    }

    #[test]
    fn the_same_wording_gets_the_same_placeholder() {
        assert_eq!(
            redact_wording(Some("taste"), "No meat"),
            redact_wording(Some("taste"), "No meat")
        );
        assert_ne!(
            redact_wording(None, "No meat"),
            redact_wording(None, "No fish")
        );
    }
}
