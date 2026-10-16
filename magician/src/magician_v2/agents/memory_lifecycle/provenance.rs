//! Producer-owned evidence. Model output cannot mint independent observations
//! or choose an existing lifecycle record ID.
use super::*;
use crate::magician_v2::agents::memory_provenance::MemoryTrust;

fn cited_evidence(source: &Value, quote: &str) -> Value {
    let mut evidence = source.clone();
    if source.get("source_role").is_some() {
        // On re-grounding, quote is already the bounded citation. Its verified
        // producer context retains attribution that the quote alone can lose.
        if let Some(text) = source
            .get("source_context")
            .and_then(Value::as_str)
            .filter(|text| text.contains(quote))
            .or_else(|| source.get("quote").and_then(Value::as_str))
        {
            if let Some(byte) = text.find(quote) {
                let start = text[..byte].chars().count();
                let context_start = start.saturating_sub(400);
                evidence["source_context"] = json!(text
                    .chars()
                    .skip(context_start)
                    .take(start - context_start + quote.chars().count() + 400)
                    .collect::<String>());
            }
        }
    }
    evidence["quote"] = json!(quote);
    evidence
}

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
            let evidence = cited_evidence(source, quote);
            grounded
                .entry(evidence_citation_key(&evidence))
                .or_insert(evidence);
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
            let evidence = cited_evidence(source, quote);
            grounded
                .entry(evidence_citation_key(&evidence))
                .or_insert(evidence);
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
    // A proposal is model output: it may name a stated source type when an
    // owner quote grounds it, but never carry the stamps only the owner's
    // confirm action writes (`confirmed_at`, `owner_confirmed_at`, …), which
    // would read as the owner's confirmation.
    strip_confirmation_stamps(item);
    if available
        .iter()
        .any(|source| source.get("source_role").is_some())
    {
        // Complete chat sources distinguish speaker authority. A model may
        // select a literal assistant quote but cannot promote it to a user
        // assertion, nor promote an invented/uncited quote to stated memory.
        let has_stated_quote = grounded.values().any(|source| {
            source.get("quote").is_some()
                && source.get("source_role").and_then(Value::as_str) == Some("user")
                && source
                    .get("source_type")
                    .and_then(Value::as_str)
                    .map(MemoryTrust::from_source_type)
                    == Some(MemoryTrust::Stated)
        });
        if !has_stated_quote
            && item
                .get("source_type")
                .and_then(Value::as_str)
                .map(MemoryTrust::from_source_type)
                == Some(MemoryTrust::Stated)
        {
            item["source_type"] = json!("inferred_pattern");
            item["trust"] = json!("inferred");
        }
    }
    if !grounded.is_empty() {
        item["memory_evidence"] = json!(grounded.into_values().collect::<Vec<_>>());
    }
}

/// Drop the stamps only an owner confirmation writes.
fn strip_confirmation_stamps(item: &mut Value) {
    if let Some(map) = item.as_object_mut() {
        map.retain(|key, _| !(key.starts_with("confirmed") || key.starts_with("owner_confirmed")));
    }
}

/// A tool call is model output. It may say the owner stated something
/// (`explicit_user_statement`, which only counts with host-recorded owner-turn
/// evidence), but it must not claim the owner *confirmed* it, or assert a
/// trust level: those come from the owner's confirm action and from
/// provenance, never from arguments.
fn clamp_tool_asserted_authority(item: &mut Value) {
    let Some(map) = item.as_object_mut() else {
        return;
    };
    map.retain(|key, _| {
        !(key == "trust" || key.starts_with("confirmed") || key.starts_with("owner_confirmed"))
    });
    if map
        .get("source_type")
        .and_then(Value::as_str)
        .is_some_and(|source| source.eq_ignore_ascii_case("owner_confirmed"))
    {
        map.insert(
            "source_type".to_string(),
            json!("explicit_user_statement"),
        );
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
                clamp_tool_asserted_authority(value);
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
    fn a_tool_cannot_claim_owner_confirmation_or_trust() {
        let fields = serde_json::Map::from_iter([(
            "diet".to_string(),
            json!({
                "key": "diet",
                "value": "vegetarian",
                "source_type": "owner_confirmed",
                "confirmed_from": "forged",
                "owner_confirmed_at": "2026-10-09T00:00:00Z",
                "trust": "stated",
            }),
        )]);
        let prepared = tool_fields(&fields, Some("exec-1"), Utc::now());
        let item = &prepared["diet"];
        assert_eq!(item["source_type"], "explicit_user_statement");
        assert!(item.get("confirmed_from").is_none());
        assert!(item.get("owner_confirmed_at").is_none());
        assert!(item.get("trust").is_none());
        assert_eq!(item["value"], "vegetarian");
    }
    #[test]
    fn memory_recall_distinct_citations_keep_both_speakers_under_one_root() {
        let assistant = "Paris is only a possible holiday suggestion.";
        let user = "I still live in Mumbai and have not chosen a destination.";
        let sources = vec![
            json!({"id":"turn-one","source_role":"assistant","source_id":"assistant-message","source_revision":"assistant-v1","source_type":"inferred_pattern","quote":assistant}),
            json!({"id":"turn-one","source_role":"user","source_id":"user-message","source_revision":"user-v1","source_type":"explicit_user_statement","quote":user}),
        ];
        let mut proposal = json!({"key":"holiday","value":"No destination chosen; Paris was suggested","source_type":"explicit_user_statement",
            "memory_evidence":[{"id":"turn-one","quote":assistant},{"id":"turn-one","quote":user}],"source_quote":user});
        ground_proposal(&mut proposal, &sources);
        assert_eq!(proposal["memory_evidence"].as_array().unwrap().len(), 2);
        assert_eq!(independent_observations(&proposal), 1);
        assert_eq!(proposal["source_type"], "explicit_user_statement");
        for role in ["user", "assistant"] {
            assert!(proposal["memory_evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["source_role"] == role));
        }
        let mut retry = proposal.clone();
        ground_proposal(&mut retry, proposal["memory_evidence"].as_array().unwrap());
        assert_eq!(retry["memory_evidence"], proposal["memory_evidence"]);
        assert_eq!(independent_observations(&retry), 1);
    }

    #[test]
    fn memory_recall_stated_aliases_require_a_grounded_owner_quote() {
        let sources = vec![
            json!({"id":"assistant-turn","source_role":"assistant","source_type":"inferred_pattern","quote":"I suggest Paris as a future vacation option."}),
        ];
        for declared in [
            "explicit_user_statement",
            " EXPLICIT_USER_STATEMENT ",
            "correction",
            "Correction",
            "owner_confirmed",
            " OWNER_CONFIRMED ",
        ] {
            for quote in [
                "I suggest Paris as a future vacation option.",
                "I live in Paris permanently.",
            ] {
                let mut proposal = json!({"key":"city","value":"Paris","source_type":declared,"source_quote":quote});
                ground_proposal(&mut proposal, &sources);
                assert_eq!(proposal["source_type"], "inferred_pattern", "{declared}");
                assert_eq!(proposal["trust"], "inferred");
            }
            let owner = json!({"id":"owner-turn","source_role":"user","source_type":declared,"quote":"I now live in Mumbai; this replaces my old Delhi address."});
            let mut valid = json!({"key":"city","value":"Mumbai","source_type":declared,"source_quote":"I now live in Mumbai; this replaces my old Delhi address."});
            ground_proposal(&mut valid, &[owner]);
            assert_eq!(valid["source_type"], declared);
            assert_eq!(valid["memory_evidence"][0]["source_role"], "user");
        }
    }
    #[test]
    fn memory_recall_speaker_authority_and_quote_grounding_cannot_be_invented() {
        let sources = vec![
            json!({"id":"turn-one","source_role":"user","source_type":"explicit_user_statement","quote":"I now live in Mumbai; this replaces my old Delhi address."}),
            json!({"id":"turn-one","source_role":"assistant","source_type":"inferred_pattern","quote":"I suggest Paris as an option for a future vacation."}),
        ];
        let mut valid = json!({"key":"city","value":"Mumbai","source_type":"explicit_user_statement","source_quote":"I now live in Mumbai; this replaces my old Delhi address."});
        ground_proposal(&mut valid, &sources);
        assert_eq!(valid["source_type"], "explicit_user_statement");
        assert_eq!(independent_observations(&valid), 1);
        for quote in [
            "I suggest Paris as an option for a future vacation.",
            "I permanently moved to Paris yesterday.",
        ] {
            let mut proposal = json!({"key":"city","value":"Paris","source_type":"explicit_user_statement","source_quote":quote});
            ground_proposal(&mut proposal, &sources);
            assert_eq!(proposal["source_type"], "inferred_pattern");
            assert_eq!(independent_observations(&proposal), 1);
        }
    }
    #[test]
    fn memory_recall_quoted_person_retains_producer_context_and_one_root() {
        let source = json!({"id":"turn-one","source_role":"user","source_type":"explicit_user_statement",
            "source_revision":"rev-one","quote":"My colleague Ana wrote: 'I live in Paris.' I still live in Mumbai."});
        let mut item = json!({"key":"ana-city","value":"Ana lives in Paris","source_quote":"I live in Paris."});
        ground_proposal(&mut item, &[source]);
        let evidence = &item["memory_evidence"][0];
        assert_eq!(evidence["source_role"], "user");
        assert_eq!(evidence["source_revision"], "rev-one");
        assert!(evidence["source_context"]
            .as_str()
            .unwrap()
            .contains("My colleague Ana wrote"));
        assert!(evidence["source_context"]
            .as_str()
            .unwrap()
            .contains("I still live in Mumbai"));
        assert_eq!(independent_observations(&item), 1);
        let mut retry = item.clone();
        ground_proposal(
            &mut retry,
            &item["memory_evidence"].as_array().unwrap().clone(),
        );
        assert_eq!(independent_observations(&retry), 1);
        assert!(retry["memory_evidence"][0]["source_context"]
            .as_str()
            .unwrap()
            .contains("My colleague Ana wrote"));
        assert!(retry["memory_evidence"][0]["source_context"]
            .as_str()
            .unwrap()
            .contains("I still live in Mumbai"));
    }
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
