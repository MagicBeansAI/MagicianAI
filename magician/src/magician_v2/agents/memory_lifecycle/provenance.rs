//! Producer-owned evidence. Model output cannot mint independent observations
//! or choose an existing lifecycle record ID.
use super::*;

pub fn reset_proposal_metadata(item: &mut Value) {
    if let Some(map) = item.as_object_mut() {
        map.retain(|key, _| {
            !(key.starts_with("memory_")
                || key.starts_with("supersed")
                || key.starts_with("retire")
                || key == "resolves_conflict"
                || key == "root_source_id"
                || key == "source_event_id"
                || key == "source_id"
                || key == "source_quote")
        });
    }
}

/// Evidence claims must cite a producer-provided ID and a literal quote from
/// that source. Dates and root IDs always come from the producer. Without a
/// citation, inherit one source conservatively; never count the whole batch as
/// support for every distilled claim.
pub fn ground_proposal(item: &mut Value, available: &[Value]) {
    if !item.is_object() {
        return;
    }
    let claims = item
        .get("memory_evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut grounded = BTreeMap::new();
    for claim in claims {
        let Some(quote) = claim
            .get("quote")
            .and_then(Value::as_str)
            .filter(|q| (8..=1000).contains(&q.chars().count()))
        else {
            continue;
        };
        if let Some(source) = available.iter().find(|source| {
            source.get("id") == claim.get("id")
                && source
                    .get("quote")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(quote))
        }) {
            let mut evidence = source.clone();
            evidence["quote"] = json!(quote);
            grounded.entry(source["id"].to_string()).or_insert(evidence);
        }
    }
    // Extraction also accepts an exact supporting excerpt without asking the
    // model to reproduce producer IDs. Only a literal source match is retained.
    if let Some(quote) = item
        .get("source_quote")
        .and_then(Value::as_str)
        .filter(|q| (8..=1000).contains(&q.chars().count()))
    {
        if let Some(source) = available.iter().find(|source| {
            source
                .get("quote")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(quote))
        }) {
            let mut evidence = source.clone();
            evidence["quote"] = json!(quote);
            grounded.entry(source["id"].to_string()).or_insert(evidence);
        }
    }
    if grounded.is_empty() {
        if let Some(source) = available
            .iter()
            .min_by_key(|source| source["id"].to_string())
        {
            // An uncited fallback is attribution, not a verbatim quote. Do not
            // copy arbitrary raw event payloads (which may contain secrets or
            // unrelated private text) into a durable memory's evidence sample.
            let inherited = json!({"id":source.get("id"),"at":source.get("at"),
                "source_type":source.get("source_type"),"attribution":"uncited_source"});
            grounded.insert(source["id"].to_string(), inherited);
        }
    }
    reset_proposal_metadata(item);
    if !grounded.is_empty() {
        item["memory_evidence"] = json!(grounded.into_values().collect::<Vec<_>>());
    }
}

/// Tool-invocation metadata is injected by the dispatcher, outside its schema.
/// All records from one execution count as one observation, including retries.
pub fn tool_fields(
    fields: &serde_json::Map<String, Value>,
    event: Option<&str>,
    at: DateTime<Utc>,
) -> serde_json::Map<String, Value> {
    fn walk(value: &mut Value, event: Option<&str>, at: DateTime<Utc>) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, event, at);
                }
            },
            Value::Object(_) => {
                reset_proposal_metadata(value);
                if let Some(event) = event {
                    value["source_event_id"] = json!(format!("memory_tool:{event}"));
                }
                value["updated_at"] = json!(at.to_rfc3339());
            },
            _ => {},
        }
    }
    let mut fields = fields.clone();
    for value in fields.values_mut() {
        walk(value, event, at);
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_lifecycle_verified_excerpt_preserves_correction_context() {
        let sources = vec![json!({"id":"turn-2","at":"2026-09-12T00:00:00Z",
            "quote":"The owner said: I moved permanently to Mumbai; Mumbai replaces Delhi."})];
        let mut item = json!({"key":"home","value":"Mumbai","source_type":"explicit_user_statement",
            "source_quote":"I moved permanently to Mumbai; Mumbai replaces Delhi."});
        ground_proposal(&mut item, &sources);
        assert_eq!(
            item["memory_evidence"][0]["quote"],
            "I moved permanently to Mumbai; Mumbai replaces Delhi."
        );
        assert_eq!(item["memory_evidence"][0]["id"], "turn-2");
        assert!(item.get("source_quote").is_none());
        item["source_quote"] = json!("I permanently moved to Paris.");
        item.as_object_mut().unwrap().remove("memory_evidence");
        ground_proposal(&mut item, &sources);
        assert!(item["memory_evidence"][0].get("quote").is_none());
    }
    #[test]
    fn memory_lifecycle_generated_ids_and_dates_cannot_inflate_evidence() {
        let sources = vec![
            json!({"id":"root-one","at":"2026-09-01T00:00:00Z","quote":"The owner started work at ten today."}),
        ];
        let mut item = json!({"key":"habit","value":"Works at ten","source_type":"inferred",
            "memory_record_id":"victim","memory_evidence":[
                {"id":"fake-one","at":"2026-09-01T00:00:00Z"},
                {"id":"fake-two","at":"2026-09-02T00:00:00Z"},
                {"id":"fake-three","at":"2026-09-03T00:00:00Z"}]});
        ground_proposal(&mut item, &sources);
        assert!(item.get("memory_record_id").is_none());
        assert_eq!(independent_observations(&item), 1);
        assert_eq!(item["memory_evidence"][0]["id"], "root-one");
        let mut copy = item.clone();
        copy["memory_evidence"][0]["at"] = json!("2099-01-01T00:00:00Z");
        ground_proposal(&mut copy, &sources);
        assert_eq!(copy["memory_evidence"][0]["at"], "2026-09-01T00:00:00Z");
    }
}
