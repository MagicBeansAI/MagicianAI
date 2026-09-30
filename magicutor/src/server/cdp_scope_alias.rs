//! Reversible transport spelling for trusted clients with strict CDP URL grammars.
//! This changes no tab ownership or authorization: both paths name the same scope.
const PREFIX: &str = "magicvault-scope-";
fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 128
        && scope != "magicutor-proxy"
        && scope
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn encode(scope: &str) -> Option<String> {
    if !valid_scope(scope) {
        return None;
    }
    let mut id = String::from(PREFIX);
    use std::fmt::Write;
    for byte in scope.bytes() {
        write!(&mut id, "{byte:02x}").ok()?;
    }
    Some(id)
}
/// `None` is an ordinary path; `Some(None)` is a malformed reserved alias.
/// Malformed aliases must not silently acquire a fresh browser scope.
pub(super) fn decode(id: &str) -> Option<Option<String>> {
    let encoded = id.strip_prefix(PREFIX)?;
    if encoded.is_empty()
        || encoded.len() > 256
        || encoded.len() % 2 != 0
        || !encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Some(None);
    }
    let bytes: Option<Vec<u8>> = (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).ok())
        .collect();
    Some(
        bytes
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| valid_scope(s)),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jit_scope_alias_preserves_exact_session_identity() {
        for scope in [
            "magician-exec_012345",
            "magician-chat-user_thread--cdp",
            "magician-exec-a",
        ] {
            let alias = encode(scope).unwrap();
            assert!(alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'));
            assert_eq!(decode(&alias), Some(Some(scope.into())));
        }
        assert_ne!(encode("scope_a"), encode("scope-a"));
    }
    #[test]
    fn jit_scope_alias_rejects_unscoped_or_malformed_routes() {
        for bad in ["", "magicutor-proxy", "../scope", "scope/other"] {
            assert!(encode(bad).is_none());
        }
        for bad in [
            "magicvault-scope-",
            "magicvault-scope-1",
            "magicvault-scope-ff",
            "magicvault-scope-2f",
            "magicvault-scope-6D",
        ] {
            assert_eq!(decode(bad), Some(None));
        }
        assert_eq!(decode("magician-ordinary"), None);
    }
}
