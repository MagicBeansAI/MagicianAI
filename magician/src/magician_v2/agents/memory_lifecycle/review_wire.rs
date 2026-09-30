//! Provider-independent review decoding. Redundant identical JSON members do
//! not change a decision; conflicting members never get a last-writer winner.
use super::Review;
use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use std::fmt;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub(super) fn parse(text: &str) -> Result<Review, String> {
    if text.len() > MAX_RESPONSE_BYTES {
        return Err("memory review exceeds response size limit".into());
    }
    let mut decoder = serde_json::Deserializer::from_str(text);
    let value = UnambiguousValue::deserialize(&mut decoder).map_err(|e| e.to_string())?;
    decoder.end().map_err(|e| e.to_string())?;
    // Retain the same typed schema, enum checks and unknown-field rejection.
    serde_json::from_value(value.0).map_err(|e| e.to_string())
}

struct UnambiguousValue(Value);

impl<'de> Deserialize<'de> for UnambiguousValue {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        decoder.deserialize_any(JsonVisitor).map(Self)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON without conflicting duplicate members")
    }

    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_string<E: Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Value, A::Error> {
        let mut result = Vec::new();
        while let Some(value) = values.next_element::<UnambiguousValue>()? {
            result.push(value.0);
        }
        Ok(Value::Array(result))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Value, A::Error> {
        let mut result = Map::new();
        while let Some((key, value)) = entries.next_entry::<String, UnambiguousValue>()? {
            if let Some(previous) = result.get(&key) {
                if previous != &value.0 {
                    return Err(A::Error::custom(format!(
                        "conflicting duplicate JSON field `{key}`"
                    )));
                }
            } else {
                result.insert(key, value.0);
            }
        }
        Ok(Value::Object(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW: &str = r#"{"kind":"observation","subject":"owner","aspect":"exercise timing","context":null,"valid_until":null,"validity_quote":null,"relationships":[{"existing_id":"m1","relation":"clarify","same_subject_and_aspect":true,"same_context":true,"explicit_correction":false,"confidence":0.98,"rationale":"A sustained pattern has unexplained applicability.","question":"Has your preference changed?","same_context":true}]}"#;

    #[test]
    fn memory_lifecycle_wire_identical_duplicate_preserves_the_decision() {
        for wire in [
            REVIEW.to_owned(),
            REVIEW.replace("\"relation\":\"clarify\"", "\"relation\":\"ask_owner\""),
        ] {
            let review = parse(&wire).unwrap();
            assert_eq!(
                review.relationships[0].relation,
                super::super::Relation::Clarify
            );
            assert!(review.relationships[0].same_context);
            assert_eq!(
                review.relationships[0].incoming_coverage,
                super::super::IncomingCoverage::Unknown
            );
            assert_eq!(
                review.relationships[0].question.as_deref(),
                Some("Has your preference changed?")
            );
            assert_eq!(
                serde_json::to_value(&review).unwrap()["relationships"][0]["relation"],
                "ask_owner"
            );
        }
    }

    #[test]
    fn memory_lifecycle_wire_conflicting_duplicates_never_choose_a_winner() {
        for reply in [
            REVIEW.replacen("\"same_context\":true", "\"same_context\":false", 1),
            REVIEW.replacen(
                "\"kind\":\"observation\"",
                "\"kind\":\"observation\",\"kind\":\"fact\"",
                1,
            ),
        ] {
            assert!(parse(&reply)
                .unwrap_err()
                .contains("conflicting duplicate JSON field"));
        }
    }

    #[test]
    fn memory_lifecycle_wire_keeps_schema_and_input_bounds() {
        assert!(parse(&(REVIEW.to_owned() + " {}")).is_err());
        assert!(parse(&" ".repeat(MAX_RESPONSE_BYTES + 1)).is_err());
        assert!(parse(&REVIEW.replace("\"confidence\":0.98", "\"confidence\":\"high\"")).is_err());
        assert!(parse(&REVIEW.replace("\"kind\":", "\"unexpected\":true,\"kind\":")).is_err());
        assert!(parse(&REVIEW.replace(
            "\"relation\":\"clarify\"",
            "\"relation\":\"clarify\",\"incoming_coverage\":\"mostly\""
        ))
        .is_err());
    }
}
