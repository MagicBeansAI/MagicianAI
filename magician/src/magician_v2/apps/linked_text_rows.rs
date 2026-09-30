//! Pure validation of a reviewed JSON dictionary and its linked text rows.
//! Field names belong to the declaration; no App or host capability is implied.
use std::{collections::BTreeSet, fmt};

use serde::{
    de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLinkedTextRowsSchema {
    pub dictionary_field: String,
    pub rows_field: String,
    pub link_field: String,
    pub text_field: String,
    pub max_keys: u16,
    pub max_rows: u16,
    pub max_bytes: u32,
}

impl AppLinkedTextRowsSchema {
    pub fn valid_declaration(&self) -> bool {
        self.dictionary_field != self.rows_field
            && self.link_field != self.text_field
            && [
                &self.dictionary_field,
                &self.rows_field,
                &self.link_field,
                &self.text_field,
            ]
            .iter()
            .all(|name| !name.is_empty() && name.len() <= 128)
            && (1..=256).contains(&self.max_keys)
            && (1..=4096).contains(&self.max_rows)
            && (1..=262_144).contains(&self.max_bytes)
    }

    pub fn accepts(&self, text: &str) -> bool {
        if !self.valid_declaration() || text.len() > self.max_bytes as usize {
            return false;
        }
        // Bound nesting before recursion and reject repeated keys before they
        // can be collapsed by a map, including equivalent escaped JSON names.
        let mut decoder = serde_json::Deserializer::from_str(text);
        let Ok(value) = StrictValue(16).deserialize(&mut decoder) else {
            return false;
        };
        if decoder.end().is_err() {
            return false;
        }
        let Some(root) = value.as_object().filter(|root| root.len() == 2) else {
            return false;
        };
        let Some(dictionary) = root.get(&self.dictionary_field).and_then(Value::as_object) else {
            return false;
        };
        let Some(rows) = root.get(&self.rows_field).and_then(Value::as_array) else {
            return false;
        };
        dictionary.len() <= usize::from(self.max_keys)
            && rows.len() <= usize::from(self.max_rows)
            && dictionary.iter().all(|(key, label)| {
                !key.trim().is_empty()
                    && label.as_str().is_some_and(|label| !label.trim().is_empty())
            })
            && rows.iter().all(|row| {
                let Some(row) = row.as_object().filter(|row| row.len() == 2) else {
                    return false;
                };
                row.get(&self.link_field)
                    .and_then(Value::as_str)
                    .is_some_and(|key| dictionary.contains_key(key))
                    && row
                        .get(&self.text_field)
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty())
            })
    }
}

struct StrictValue(u8);

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.0 == 0 {
            return Err(D::Error::custom(
                "JSON depth exceeds declaration validator limit",
            ));
        }
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }
    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_string<E: Error>(self, value: String) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut rows = Vec::new();
        while let Some(row) = sequence.next_element_seed(StrictValue(self.0 - 1))? {
            if rows.len() >= 4096 {
                return Err(A::Error::custom("too many JSON array entries"));
            }
            rows.push(row);
        }
        Ok(Value::Array(rows))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut fields = Map::new();
        let mut names = BTreeSet::new();
        while let Some(name) = object.next_key::<String>()? {
            if !names.insert(name.clone()) || fields.len() >= 4096 {
                return Err(A::Error::custom("duplicate or excessive JSON object keys"));
            }
            fields.insert(name, object.next_value_seed(StrictValue(self.0 - 1))?);
        }
        Ok(Value::Object(fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn schema() -> AppLinkedTextRowsSchema {
        AppLinkedTextRowsSchema {
            dictionary_field: "speakers".into(),
            rows_field: "utterances".into(),
            link_field: "speaker".into(),
            text_field: "text".into(),
            max_keys: 64,
            max_rows: 1000,
            max_bytes: 262_144,
        }
    }
    #[test]
    fn linked_rows_keep_explicit_attribution_and_reject_ambiguous_json() {
        let s = schema();
        assert!(s.accepts(
            r#"{"speakers":{"a":"Alice"},"utterances":[{"speaker":"a","text":"Hello"}]}"#
        ));
        for invalid in [
            r#"{"speakers":{"a":"Alice","\u0061":"Bob"},"utterances":[]}"#,
            r#"{"speakers":{"a":"Alice"},"utterances":[{"speaker":"b","text":"Hi"}]}"#,
            r#"{"speakers":{"a":" "},"utterances":[]}"#,
            r#"{"speakers":{"a":"Alice"},"utterances":[{"speaker":"a","text":" "}]}"#,
            r#"{"speakers":{},"utterances":[],"extra":true}"#,
            r#"{"speakers":{},"utterances":[],"utterances":[]}"#,
            r#"{"speakers":{"a":"Alice"},"utterances":[{"speaker":"a","text":"Hi","actor":"owner"}]}"#,
            r#"{"speakers":{},"utterances":[]} {}"#,
        ] {
            assert!(!s.accepts(invalid), "accepted {invalid}");
        }
        let row = json!({"speaker":"a","text":"Hi"});
        assert!(s.accepts(
            &json!({"speakers":{"a":"Alice"},"utterances":vec![row.clone(); 1000]}).to_string()
        ));
        assert!(
            !s.accepts(&json!({"speakers":{"a":"Alice"},"utterances":vec![row; 1001]}).to_string())
        );
        let deep = format!("{}0{}", "[".repeat(10000), "]".repeat(10000));
        assert!(!s.accepts(&deep));
        let mut other = s;
        other.dictionary_field = "authors".into();
        other.rows_field = "comments".into();
        other.link_field = "author".into();
        other.text_field = "body".into();
        assert!(other
            .accepts(r#"{"authors":{"b":"Bea"},"comments":[{"author":"b","body":"Reviewed"}]}"#));
    }
}
