//! Boundary-tag neutralization for memory candidate text.
//!
//! Copied verbatim from `magician::magician_v2::prompt_identity` (see
//! `neutralize_boundary_tags`) so this crate has no reverse dependency on
//! magician. Keep the two implementations in sync; both replace `<tag` /
//! `</tag` occurrences with fullwidth `\u{FF1C}` to defeat tag-closing
//! attacks in untrusted content.

const BOUNDARY_TAGS: &[&str] = &[
    "external_content",
    "tool_output",
    "task_context",
    "agent_identity",
    "environment_knowledge",
    "capability_guide",
    "user_memory",
    "agent_memory",
    "agent_goal_memory",
    "runtime_ledger",
    "user_message",
    "synthesis_bundle",
];

pub fn neutralize_boundary_tags(content: &str) -> String {
    let mut result = content.to_string();
    for tag in BOUNDARY_TAGS {
        result = replace_tag_case_insensitive(&result, tag);
    }
    result
}

fn replace_tag_case_insensitive(content: &str, tag: &str) -> String {
    let tag_lower = tag.to_lowercase();
    let mut result = String::with_capacity(content.len());
    let chars = content.char_indices();

    for (i, ch) in chars {
        if ch == '<' {
            let rest = &content[i + 1..];
            let rest_lower = rest.to_lowercase();
            if rest_lower.starts_with(&format!("/{}", tag_lower)) {
                result.push('\u{FF1C}');
            } else if rest_lower.starts_with(&tag_lower) {
                result.push('\u{FF1C}');
            } else {
                result.push('<');
            }
        } else {
            result.push(ch);
        }
    }
    result
}
