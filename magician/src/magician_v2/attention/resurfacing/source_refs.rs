//! Stable source-reference contracts shared by resurfacing producers,
//! routing, backfill prioritization, and interaction adapters.
//!
//! Communication references are opaque outside the server. They encode every
//! provider-controlled identity component so `/` and `@` inside an id cannot
//! change the tuple that a later scoped lookup resolves.
//!
//! The `ChannelMessageMeta` convenience constructor stayed comms-side (it
//! lives in the `magician-comms` shim for this module) so the lib holds only
//! the string-parts contract.

/// Exact identity carried by a communication resurfacing source reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommSourceRef {
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub message_id: String,
    pub internal_date: i64,
}

pub fn comm_source_ref_parts(
    provider: &str,
    account_alias: &str,
    thread_id: &str,
    message_id: &str,
    internal_date: i64,
) -> String {
    format!(
        "{}/{}/{}/{}@{}",
        urlencoding::encode(provider),
        urlencoding::encode(account_alias),
        urlencoding::encode(thread_id),
        urlencoding::encode(message_id),
        internal_date
    )
}

/// Parse the current exact-message reference contract.
///
/// This is deliberately strict: malformed percent encoding, unescaped `/`,
/// empty identities, control characters, and non-positive timestamps are
/// rejected rather than being reinterpreted as a different account/thread.
pub fn parse_comm_source_ref(source_ref: &str) -> Option<CommSourceRef> {
    let parts = source_ref.split('/').collect::<Vec<_>>();
    if parts.len() != 4 {
        return None;
    }
    let (encoded_message_id, timestamp) = parts[3].rsplit_once('@')?;
    let internal_date = timestamp.parse::<i64>().ok()?;
    if internal_date <= 0 {
        return None;
    }

    let provider = decode_identity(parts[0])?;
    let account_alias = decode_identity(parts[1])?;
    let thread_id = decode_identity(parts[2])?;
    let message_id = decode_identity(encoded_message_id)?;
    Some(CommSourceRef {
        provider,
        account_alias,
        thread_id,
        message_id,
        internal_date,
    })
}

pub fn parse_comm_source_message_key(source_ref: &str) -> Option<(String, String, String)> {
    let parsed = parse_comm_source_ref(source_ref)?;
    Some((parsed.provider, parsed.account_alias, parsed.message_id))
}

fn decode_identity(encoded: &str) -> Option<String> {
    if encoded.is_empty() || !has_valid_percent_encoding(encoded) {
        return None;
    }
    let decoded = urlencoding::decode(encoded).ok()?.into_owned();
    if decoded.is_empty() || decoded.chars().any(char::is_control) {
        return None;
    }
    Some(decoded)
}

fn has_valid_percent_encoding(encoded: &str) -> bool {
    let bytes = encoded.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn communication_source_ref_roundtrips_encoded_identity() {
        let encoded = comm_source_ref_parts(
            "telegram",
            "public/bot",
            "thread/with/slash",
            "message@42",
            123,
        );
        assert_eq!(
            parse_comm_source_ref(&encoded),
            Some(CommSourceRef {
                provider: "telegram".to_string(),
                account_alias: "public/bot".to_string(),
                thread_id: "thread/with/slash".to_string(),
                message_id: "message@42".to_string(),
                internal_date: 123,
            })
        );
    }

    #[test]
    fn communication_source_ref_rejects_ambiguous_or_invalid_input() {
        for invalid in [
            "gmail/business",
            "gmail/business/thread/message@0",
            "gmail/business/thread/message@not-a-time",
            "whatsapp/my/account/group/thread/message@123",
            "gmail/%ZZ/thread/message@123",
            "gmail/business/thread/@123",
        ] {
            assert_eq!(parse_comm_source_ref(invalid), None, "{invalid}");
        }
    }
}
