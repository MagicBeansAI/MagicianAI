//! State hashing for page stability detection.

use std::hash::{Hash, Hasher};

use crate::magician_v2::execution::types::PageState;

/// Compute a stable hash for comparing consecutive page observations.
pub fn compute_state_hash(state: &PageState) -> u64 {
    compute_dom_hash(state)
}

fn compute_dom_hash(state: &PageState) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();

    if let Some(url) = &state.url {
        url.hash(&mut hasher);
    }

    if let Some(title) = &state.title {
        title.hash(&mut hasher);
    }

    if let Some(merkle_root) = &state.merkle_structural_root {
        merkle_root.hash(&mut hasher);
    } else if let Some(merkle_content) = &state.merkle_content_root {
        merkle_content.hash(&mut hasher);
    }

    if let Some(elements) = &state.interactive_elements_raw {
        elements.len().hash(&mut hasher);
        for element in elements.iter().take(10) {
            element.tag.hash(&mut hasher);
            element.selector.hash(&mut hasher);
        }
    }

    hasher.finish()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn compute_state_hash_changes_with_url() {
        let state1 = PageState {
            url: Some("https://example.com/a".to_string()),
            ..Default::default()
        };
        let state2 = PageState {
            url: Some("https://example.com/b".to_string()),
            ..Default::default()
        };

        assert_ne!(compute_state_hash(&state1), compute_state_hash(&state2));
    }

    #[test]
    fn compute_state_hash_is_stable_for_same_state() {
        let state = PageState {
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            ..Default::default()
        };

        assert_eq!(compute_state_hash(&state), compute_state_hash(&state));
    }
}
