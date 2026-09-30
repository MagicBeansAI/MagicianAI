//! Path-component validation for api-mining stores.
//!
//! Two validators with different strictness, because the input shapes are
//! different:
//!
//! 1. **`ensure_safe_record_id`** — strict allowlist for per-record id
//!    fields like `sequence_id`, `workflow_id`, `capability_id`. These
//!    flow directly from HTTP path params and JSON field values. They
//!    have no internal structure that needs `/` or `:`. Strict.
//!
//! 2. **`ensure_safe_origin_key`** — looser validator for origin_keys.
//!    Production produces origin_keys via `router::extract_origin(url)`
//!    which returns the form `https://example.com` (with scheme + `://`).
//!    The slashes get interpreted by `PathBuf::join` so files land at
//!    `<base>/https:/example.com/...` — slightly weird but contained
//!    within the per-scope api_mining root. The HTTP routing layer
//!    (actix `{name}` path params) already rejects `/` in URL segments,
//!    so the attack surface is limited to whatever an authenticated
//!    caller could put inside the api_mining_root. We still reject
//!    `..` substrings and control chars as defense-in-depth.
//!
//! The historical `CapabilityStore::validate_id` is equivalent in spirit
//! to `ensure_safe_record_id`.

use std::io;

const MAX_COMPONENT_LEN: usize = 256;

/// Strict validator for record id fields (`sequence_id`, `workflow_id`,
/// `capability_id`, etc.) that come from HTTP path params or JSON
/// field values. Each must:
/// - Be non-empty.
/// - Be ≤ 128 chars.
/// - Match `[A-Za-z0-9._-]+` (no path separators, no other punctuation).
/// - Not literally be `.` or `..`.
/// - Not contain `..` substring (defense-in-depth).
pub fn ensure_safe_record_id(component: &str, role: &str) -> io::Result<()> {
    if component.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} must not be empty"),
        ));
    }
    if component.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} length {} exceeds cap 128", component.len()),
        ));
    }
    if component == "." || component == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} must not be '.' or '..'"),
        ));
    }
    if component.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} '{component}' must not contain '..'"),
        ));
    }
    if !component
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} '{component}' contains characters outside [A-Za-z0-9._-]"),
        ));
    }
    Ok(())
}

/// Looser validator for origin_keys, which production code currently passes
/// in the `https://example.com` form. Closes the `..` traversal vector while
/// preserving the existing on-disk layout. Each must:
/// - Be non-empty.
/// - Be ≤ 256 chars (full URL accommodation).
/// - Not literally be `.` or `..`.
/// - Not contain `..` substring (the primary traversal vector).
/// - Not contain `\` (Windows path separator).
/// - Not contain control characters (< 0x20).
pub fn ensure_safe_origin_key(component: &str, role: &str) -> io::Result<()> {
    if component.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} must not be empty"),
        ));
    }
    if component.len() > MAX_COMPONENT_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{role} length {} exceeds cap {}",
                component.len(),
                MAX_COMPONENT_LEN
            ),
        ));
    }
    if component == "." || component == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} must not be '.' or '..'"),
        ));
    }
    if component.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} '{component}' must not contain '..'"),
        ));
    }
    if component.contains('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} '{component}' must not contain backslash"),
        ));
    }
    if component.chars().any(|c| (c as u32) < 0x20) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} must not contain control characters"),
        ));
    }
    Ok(())
}

/// Canonicalize an `origin_key` into a single legal filesystem segment
/// for use as a directory name. Strips an optional leading `scheme://`
/// prefix, then replaces any character outside `[A-Za-z0-9._-]` with `_`.
///
/// **Why this exists**: production code produces `origin_key`s via
/// `router::extract_origin(url)` which returns the form
/// `https://example.com` — with embedded `/` characters. When that string
/// is `PathBuf::join`'d, the OS interprets the `/` as path separators,
/// scattering files across `<base>/https:/example.com/...`. The HTTP path
/// params `GET /api-mining/.../{origin_key}/...` can only carry a single
/// URL segment, so an operator passing `example.com` could never reach
/// the producer-side layout.
///
/// Canonicalizing at the store boundary fixes this: both
/// `https://example.com` (producer side) and `example.com` (HTTP side)
/// resolve to the same directory name `example.com`, so the data is
/// retrievable from either entry point.
///
/// Examples:
/// - `https://example.com` → `example.com`
/// - `example.com` → `example.com`
/// - `https://example.com:8080` → `example.com_8080`
/// - `https___example_com` → `https___example_com` (already sanitized)
/// - empty → `unknown_origin` (matches `CapabilityStore::origin_to_key` fallback)
pub fn canonical_origin_dir(origin: &str) -> String {
    if origin.is_empty() {
        return "unknown_origin".to_string();
    }
    let stripped = if let Some(idx) = origin.find("://") {
        &origin[idx + 3..]
    } else {
        origin
    };
    if stripped.is_empty() {
        return "unknown_origin".to_string();
    }
    stripped
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // ─── ensure_safe_record_id ─────────────────────────────────────────

    #[test]
    fn record_id_accepts_allowlist_shape() {
        assert!(ensure_safe_record_id("seq_01HXYZ", "sequence_id").is_ok());
        assert!(ensure_safe_record_id("wf-abc-123", "workflow_id").is_ok());
        assert!(ensure_safe_record_id("Cap_42", "capability_id").is_ok());
        assert!(ensure_safe_record_id("a.b.c", "x").is_ok());
    }

    #[test]
    fn record_id_rejects_empty() {
        assert_eq!(
            ensure_safe_record_id("", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    #[test]
    fn record_id_rejects_dot_and_dotdot() {
        assert_eq!(
            ensure_safe_record_id(".", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
        assert_eq!(
            ensure_safe_record_id("..", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    #[test]
    fn record_id_rejects_dotdot_substring() {
        for bad in &["../etc/passwd", "..\\windows", "foo/../etc", "foo..bar"] {
            assert_eq!(
                ensure_safe_record_id(bad, "x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "should reject {bad:?}",
            );
        }
    }

    #[test]
    fn record_id_rejects_path_separators() {
        for bad in &["a/b", "a\\b"] {
            assert_eq!(
                ensure_safe_record_id(bad, "x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "should reject {bad:?}",
            );
        }
    }

    #[test]
    fn record_id_rejects_unicode_and_punctuation() {
        for bad in &["examplé", "a:b", "a b", "a;b"] {
            assert_eq!(
                ensure_safe_record_id(bad, "x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "should reject {bad:?}",
            );
        }
    }

    #[test]
    fn record_id_rejects_too_long() {
        let too_long = "a".repeat(129);
        assert_eq!(
            ensure_safe_record_id(&too_long, "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    // ─── ensure_safe_origin_key ────────────────────────────────────────

    #[test]
    fn origin_key_accepts_production_url_form() {
        // Production produces these via router::extract_origin(url).
        assert!(ensure_safe_origin_key("https://example.com", "origin_key").is_ok());
        assert!(ensure_safe_origin_key("https://example.com:8080", "origin_key").is_ok());
        assert!(ensure_safe_origin_key("http://localhost:3000", "origin_key").is_ok());
        // Sanitized form from CapabilityStore::origin_to_key.
        assert!(ensure_safe_origin_key("https___example_com", "origin_key").is_ok());
        // Bare hostname (test fixtures, HTTP path params).
        assert!(ensure_safe_origin_key("example.com", "origin_key").is_ok());
    }

    #[test]
    fn origin_key_rejects_dotdot_traversal() {
        // The actual vulnerability.
        for bad in &["..", "../etc/passwd", "..\\windows", "foo/../etc"] {
            assert_eq!(
                ensure_safe_origin_key(bad, "origin_key")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput,
                "should reject {bad:?}",
            );
        }
    }

    #[test]
    fn origin_key_rejects_backslash() {
        assert_eq!(
            ensure_safe_origin_key("a\\b", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    #[test]
    fn origin_key_rejects_control_chars() {
        for bad in &["a\0b", "a\nb", "a\tb"] {
            assert_eq!(
                ensure_safe_origin_key(bad, "x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "should reject {bad:?}",
            );
        }
    }

    #[test]
    fn origin_key_rejects_empty_and_dot() {
        assert_eq!(
            ensure_safe_origin_key("", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
        assert_eq!(
            ensure_safe_origin_key(".", "x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    // ─── canonical_origin_dir ──────────────────────────────────────────

    #[test]
    fn canonical_strips_https_scheme() {
        assert_eq!(canonical_origin_dir("https://example.com"), "example.com");
    }

    #[test]
    fn canonical_strips_http_scheme() {
        assert_eq!(canonical_origin_dir("http://example.com"), "example.com");
    }

    #[test]
    fn canonical_passes_bare_host_through() {
        assert_eq!(canonical_origin_dir("example.com"), "example.com");
    }

    #[test]
    fn canonical_sanitizes_port_colon() {
        assert_eq!(
            canonical_origin_dir("https://example.com:8080"),
            "example.com_8080",
        );
    }

    #[test]
    fn canonical_preserves_already_sanitized_form() {
        // Output of CapabilityStore::origin_to_key — should round-trip
        // through canonical_origin_dir untouched.
        assert_eq!(
            canonical_origin_dir("https___example_com"),
            "https___example_com",
        );
    }

    #[test]
    fn canonical_handles_empty() {
        assert_eq!(canonical_origin_dir(""), "unknown_origin");
        assert_eq!(canonical_origin_dir("://"), "unknown_origin");
    }

    #[test]
    fn canonical_sanitizes_unicode_to_underscore() {
        assert_eq!(canonical_origin_dir("examplé.com"), "exampl_.com");
    }

    #[test]
    fn canonical_producer_and_consumer_forms_converge() {
        // The whole point: producer side (orchestrator) passes the full
        // URL form, HTTP side passes bare host — both must canonicalize
        // to the same directory.
        let producer = canonical_origin_dir("https://example.com");
        let consumer = canonical_origin_dir("example.com");
        assert_eq!(producer, consumer);
    }
}
