//! Deterministic task-shape templates and fingerprints.

pub fn deterministic_template(task_title: &str, inputs: &[(String, String)]) -> String {
    let mut template = task_title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut sorted: Vec<_> = inputs.iter().collect();
    sorted.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
    for (name, value) in sorted {
        let normalized = value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !normalized.is_empty() {
            template = replace_token_value(&template, &normalized, &format!("{{{name}}}"));
        }
    }
    template
}

fn replace_token_value(haystack: &str, needle: &str, replacement: &str) -> String {
    let starts_word = needle.chars().next().is_some_and(char::is_alphanumeric);
    let ends_word = needle
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric);
    let mut output = String::with_capacity(haystack.len());
    let mut cursor = 0;
    while let Some(offset) = haystack[cursor..].find(needle) {
        let start = cursor + offset;
        let end = start + needle.len();
        // Earlier inputs have already emitted named placeholders. A later
        // value can equal one of those names; never substitute inside it.
        let inside_placeholder = haystack[..start].rfind('{').is_some_and(|open| {
            haystack[open..]
                .find('}')
                .is_some_and(|close| start < open + close)
        });
        let left_ok = !starts_word
            || haystack[..start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric());
        let right_ok = !ends_word
            || haystack[end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_alphanumeric());
        if !inside_placeholder && left_ok && right_ok {
            output.push_str(&haystack[cursor..start]);
            output.push_str(replacement);
            // Equal captured request values share an input. Every occurrence
            // must stay linked: leaving later occurrences literal would allow
            // a task edit to change all requests while describing only one.
            cursor = end;
        } else {
            let advance = haystack[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            output.push_str(&haystack[cursor..start + advance]);
            cursor = start + advance;
        }
    }
    output.push_str(&haystack[cursor..]);
    output
}

/// Detect sample text left literal by older, first-occurrence-only templates.
/// Use the same normalization, token boundaries and placeholder exclusion as
/// learning, so input names and substrings of unrelated words do not count.
pub fn template_has_literal_value(template: &str, value: &str) -> bool {
    let template = template
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let value = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    !value.is_empty() && replace_token_value(&template, &value, "") != template
}

pub fn shape_fingerprint(
    template: &str,
    agent_id: &str,
    principal: &str,
    workspace: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [template, agent_id, principal, workspace] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

pub fn source_task_fingerprint(title: &str, description: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [title, description] {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        hasher.update(normalized.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

pub fn contextual_shape_fingerprint(
    template: &str,
    description: Option<&str>,
    agent_id: &str,
    principal: &str,
    workspace: &str,
) -> String {
    match description {
        Some(description) => shape_fingerprint(
            &format!("{template}\0{description}"),
            agent_id,
            principal,
            workspace,
        ),
        None => shape_fingerprint(template, agent_id, principal, workspace),
    }
}

/// Build the syntactic pattern. Matching callers must use `template_captures`
/// as well: regex alone cannot enforce equality of repeated input slots.
pub fn template_regex(template: &str) -> Option<regex::Regex> {
    let mut pattern = String::from("^\\s*");
    let mut rest = template;
    let mut names = std::collections::HashSet::new();
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}')?;
        append_literal_pattern(&mut pattern, &rest[..start]);
        let name = &rest[start + 1..start + end];
        if name.is_empty()
            || !name.chars().enumerate().all(|(index, character)| {
                character == '_'
                    || character.is_ascii_alphanumeric()
                        && (index > 0 || !character.is_ascii_digit())
            })
        {
            return None;
        }
        if names.insert(name) {
            pattern.push_str(&format!("(?P<{name}>.+?)"));
        } else {
            pattern.push_str("(.+?)");
        }
        rest = &rest[start + end + 1..];
    }
    append_literal_pattern(&mut pattern, rest);
    pattern.push_str("\\s*$");
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

/// Ordered slots in a template already validated by `template_regex`.
pub fn template_input_names(template: &str) -> impl Iterator<Item = &str> {
    template
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
}

pub fn template_captures<'text>(
    template: &str,
    expression: &regex::Regex,
    text: &'text str,
) -> Option<regex::Captures<'text>> {
    let captures = expression.captures(text)?;
    // Each slot emits exactly one capture; all literal text is escaped.
    // Reuse the regex's named capture lookup instead of allocating a second
    // map of values for each candidate/task match.
    for (index, name) in template_input_names(template).enumerate() {
        let value = captures.get(index + 1)?.as_str().trim();
        if captures.name(name)?.as_str().trim() != value {
            return None;
        }
    }
    Some(captures)
}

fn append_literal_pattern(pattern: &mut String, literal: &str) {
    // Learning collapses whitespace, while task descriptions routinely have
    // paragraphs and bullet lists. Match that same equivalence without making
    // whitespace optional or weakening any non-whitespace intent text.
    let mut start = 0;
    let mut in_whitespace = false;
    for (offset, character) in literal.char_indices() {
        if character.is_whitespace() {
            if !in_whitespace {
                pattern.push_str(&regex::escape(&literal[start..offset]));
                pattern.push_str(r"\s+");
                in_whitespace = true;
            }
        } else if in_whitespace {
            start = offset;
            in_whitespace = false;
        }
    }
    if !in_whitespace {
        pattern.push_str(&regex::escape(&literal[start..]));
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn deterministic_template_handles_short_inputs_without_replacing_substrings() {
        assert_eq!(
            deterministic_template("Search x in example", &[("query".into(), "x".into())]),
            "search {query} in example"
        );
        assert_eq!(
            deterministic_template("Open google", &[("verb".into(), "go".into())]),
            "open google"
        );
    }

    #[test]
    fn deterministic_template_normalizes_multiword_input_spacing() {
        assert_eq!(
            deterministic_template(
                "Find New York weather",
                &[("city".into(), "New   York".into())]
            ),
            "find {city} weather"
        );
    }

    #[test]
    fn deterministic_template_requires_repeated_inputs_to_remain_equal() {
        let template = deterministic_template(
            "Compare Paris with Paris",
            &[("city".into(), "Paris".into())],
        );
        assert_eq!(template, "compare {city} with {city}");
        let expression = template_regex(&template).unwrap();
        let captures = template_captures(&template, &expression, "Compare Rome with Rome").unwrap();
        assert_eq!(&captures["city"], "Rome");
        assert!(template_captures(&template, &expression, "Compare Rome with Paris").is_none());
        assert!(template_captures(&template, &expression, "Compare Rome with rome").is_none());
    }

    #[test]
    fn repeated_and_distinct_slots_preserve_capture_order() {
        let template = "compare {a} with {a} and {b} with {b}";
        let expression = template_regex(template).unwrap();
        let captures = template_captures(
            template,
            &expression,
            "Compare BTC with BTC and ETH with ETH",
        )
        .unwrap();
        assert_eq!(&captures["a"], "BTC");
        assert_eq!(&captures["b"], "ETH");
        assert!(template_captures(
            template,
            &expression,
            "Compare BTC with BTC and ETH with DOGE"
        )
        .is_none());
    }

    #[test]
    fn legacy_literal_detection_ignores_slots_and_unrelated_substrings() {
        assert!(template_has_literal_value("compare {q} with btc", "BTC"));
        assert!(template_has_literal_value(
            "compare {city} with new york",
            "New\tYork"
        ));
        assert!(!template_has_literal_value("compare {q} with {q}", "q"));
        assert!(!template_has_literal_value("open google for {verb}", "go"));
        assert!(!template_has_literal_value("find {q}", ""));
    }

    #[test]
    fn input_values_cannot_rewrite_previously_emitted_placeholders() {
        let template = deterministic_template(
            "Compare blue with q",
            &[("q".into(), "blue".into()), ("other".into(), "q".into())],
        );
        assert_eq!(template, "compare {q} with {other}");
        let regex = template_regex(&template).unwrap();
        let captures = regex.captures("Compare red with green").unwrap();
        assert_eq!(&captures["q"], "red");
        assert_eq!(&captures["other"], "green");
    }

    #[test]
    fn template_literals_match_multiline_whitespace_without_dropping_word_boundaries() {
        let expression = template_regex("find {query} and return its title").unwrap();
        let captures = expression
            .captures("Find\tRust\n\nand return\tits title")
            .unwrap();
        assert_eq!(&captures["query"], "Rust");
        assert!(!expression.is_match("FindRust and return its title"));
        assert!(!expression.is_match("Find Rust and delete its title"));
        assert!(template_regex(" ").unwrap().is_match("\n\t"));
    }
}
