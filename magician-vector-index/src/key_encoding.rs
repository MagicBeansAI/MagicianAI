//! Unambiguous composite keys for durable memory identifiers.
//!
//! Memory identifiers — agent ids, goal ids, tier names, item keys — are
//! persisted user/planner data. They routinely contain the characters a naive
//! `a:b:c` join uses as a separator: goal ids arrive as `chat:<uuid>` or as the
//! planner's full free-text goal, and seeded agent ids such as
//! `system:scheduler` carry one directly. Joining those with a bare separator
//! is not injective, so two distinct sources can collapse onto one durable key
//! and a reader cannot recover the original segments.
//!
//! Length-prefixing every segment makes the encoding injective and the parse
//! exact, whatever the segment contains.

/// Encode one segment as `<byte-len>:<value>`.
pub fn length_prefixed_key_segment(value: &str) -> String {
    format!("{}:{value}", value.len())
}

/// Decode one `<byte-len>:<value>` segment, returning it with the remainder
/// after the segment's trailing separator.
///
/// Returns `None` when the input is not a well-formed segment — a missing
/// delimiter, an unparsable length, a length running past the end of the
/// input, or a length landing inside a UTF-8 character.
pub fn parse_length_prefixed_key_segment(input: &str) -> Option<(&str, &str)> {
    let delimiter = input.find(':')?;
    let value_len = input[..delimiter].parse::<usize>().ok()?;
    let value_start = delimiter.checked_add(1)?;
    let value_end = value_start.checked_add(value_len)?;
    let value = input.get(value_start..value_end)?;
    let remainder = input.get(value_end..)?;
    if remainder.is_empty() {
        Some((value, remainder))
    } else {
        Some((value, remainder.strip_prefix(':')?))
    }
}

/// Longest identifier segment kept verbatim inside a composite key.
///
/// Sized so a five-segment key stays under a kilobyte even in the worst case.
pub const MAX_KEY_SEGMENT_BYTES: usize = 160;

/// Marker prefix for a segment that was replaced by a digest.
const HASHED_SEGMENT_PREFIX: &str = "~h";

/// Cap a segment's contribution to a composite key.
///
/// Identifiers are supposed to identify. Some do not: `goal_id` is documented
/// as the planner's *semantic* goal key and in practice arrives as the entire
/// free-text goal — the longest one observed was 3,037 characters of a
/// multi-line personality-override prompt. Embedding that verbatim makes keys
/// enormous, and a key that carries a payload is a key nobody can log, diff, or
/// bound the storage of.
///
/// Over-long segments collapse to a digest of themselves. Equality — all a key
/// needs — is preserved; only human readability of that one segment is lost,
/// and the full value still lives on the candidate it came from.
pub fn bounded_key_segment(value: &str) -> std::borrow::Cow<'_, str> {
    if value.len() <= MAX_KEY_SEGMENT_BYTES {
        return std::borrow::Cow::Borrowed(value);
    }
    let digest = blake3::hash(value.as_bytes()).to_hex();
    std::borrow::Cow::Owned(format!("{HASHED_SEGMENT_PREFIX}{}", &digest[..32]))
}

/// Encode an optional segment, distinguishing "absent" from "present but empty".
pub fn optional_key_segment(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("some:{}", length_prefixed_key_segment(value)),
        None => "none".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_segments_containing_the_separator() {
        let encoded = format!(
            "{}:{}",
            length_prefixed_key_segment("chat:2705464a-3f98-4261-a5f7-e4353b275509"),
            length_prefixed_key_segment("episodes")
        );
        let (first, rest) = parse_length_prefixed_key_segment(&encoded).expect("first segment");
        assert_eq!(first, "chat:2705464a-3f98-4261-a5f7-e4353b275509");
        let (second, rest) = parse_length_prefixed_key_segment(rest).expect("second segment");
        assert_eq!(second, "episodes");
        assert!(rest.is_empty());
    }

    #[test]
    fn distinct_segment_splits_do_not_collapse_onto_one_key() {
        // `a:b` + `c` and `a` + `b:c` are the same string under a bare join.
        let left = format!(
            "{}:{}",
            length_prefixed_key_segment("a:b"),
            length_prefixed_key_segment("c")
        );
        let right = format!(
            "{}:{}",
            length_prefixed_key_segment("a"),
            length_prefixed_key_segment("b:c")
        );
        assert_ne!(left, right);
    }

    #[test]
    fn round_trips_multiline_and_unicode_segments() {
        let value = "## Personality Override\n\nadopt the **witty** personality — ephemeral";
        let encoded = length_prefixed_key_segment(value);
        let (decoded, rest) = parse_length_prefixed_key_segment(&encoded).expect("segment");
        assert_eq!(decoded, value);
        assert!(rest.is_empty());
    }

    #[test]
    fn rejects_a_length_landing_inside_a_utf8_character() {
        // "é" is two bytes; a length of 1 would split it.
        assert!(parse_length_prefixed_key_segment("1:é").is_none());
    }

    #[test]
    fn rejects_malformed_segments() {
        assert!(parse_length_prefixed_key_segment("nodelimiter").is_none());
        assert!(parse_length_prefixed_key_segment("x:value").is_none());
        assert!(parse_length_prefixed_key_segment("99:short").is_none());
    }

    #[test]
    fn over_long_segments_collapse_to_a_stable_digest() {
        let prose = "## Personality Override\n\n".repeat(200);
        let bounded = bounded_key_segment(&prose);
        assert!(bounded.len() <= MAX_KEY_SEGMENT_BYTES);
        assert!(bounded.starts_with("~h"));
        assert_eq!(
            bounded,
            bounded_key_segment(&prose),
            "must be deterministic"
        );

        let other = format!("{prose}x");
        assert_ne!(bounded, bounded_key_segment(&other));
    }

    #[test]
    fn short_segments_are_left_verbatim() {
        assert_eq!(bounded_key_segment("task_1234"), "task_1234");
        assert_eq!(bounded_key_segment(""), "");
        let exact = "a".repeat(MAX_KEY_SEGMENT_BYTES);
        assert_eq!(bounded_key_segment(&exact), exact);
    }

    #[test]
    fn a_five_segment_key_stays_under_a_kilobyte() {
        let huge = "x".repeat(4_000);
        let key = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|_| length_prefixed_key_segment(&bounded_key_segment(&huge)))
            .collect::<Vec<_>>()
            .join(":");
        assert!(key.len() < 1_024, "key was {} bytes", key.len());
    }

    #[test]
    fn optional_segments_distinguish_absent_from_empty() {
        assert_ne!(optional_key_segment(None), optional_key_segment(Some("")));
    }
}
