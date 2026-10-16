//! Compact, size-checked control snapshots. Formatting must not make a
//! successfully published checkpoint unreadable by its next owner.
use serde::Serialize;

pub(crate) fn compact_json_within_limit<T: Serialize>(
    value: &T,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok((bytes.len() as u64 <= max_bytes).then_some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_round_fits_even_when_pretty_formatting_exceeds_read_limit() {
        let row = json!({"fields": {"body": "a".repeat(220), "id": "participant"},
            "checkpoint": {"digest": "b".repeat(64), "kind": "query"}});
        let value = json!({"seal_version": 1, "hmac_sha256": "c".repeat(64),
            "payload": {"queries": vec![row; 4500]}});
        let limit = 2 * 1024 * 1024;
        assert!(serde_json::to_vec_pretty(&value).unwrap().len() as u64 > limit);
        let compact = compact_json_within_limit(&value, limit).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&compact).unwrap(),
            value
        );
    }

    #[test]
    fn rejects_oversized_content_before_publication_and_accepts_exact_limit() {
        let value = json!({"body": "bounded"});
        let size = serde_json::to_vec(&value).unwrap().len() as u64;
        assert!(compact_json_within_limit(&value, size).unwrap().is_some());
        assert!(compact_json_within_limit(&value, size - 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn legacy_whitespace_does_not_hide_oversized_content_or_change_seal_fields() {
        let original = json!({"seal_version": 1, "hmac_sha256": "unchanged", "payload": {"x": "z".repeat(200)}});
        let pretty = serde_json::to_vec_pretty(&original).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&pretty).unwrap();
        let normalized = compact_json_within_limit(&parsed, 1024).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&normalized).unwrap(),
            original
        );
        assert!(compact_json_within_limit(&parsed, 100).unwrap().is_none());
    }
}
