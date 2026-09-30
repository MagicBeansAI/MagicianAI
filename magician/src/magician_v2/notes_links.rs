//! Wiki links and Markdown links between notes, and the notes that point back.
//!
//! A link written `[[Inbox]]` or `[Inbox](Inbox.md)` names a page. Resolution
//! tries that path, the `.md` file, and `index.md` inside a folder of that
//! name, from the notes root and from the linking note's folder.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedLink {
    pub target: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoundBacklink {
    pub relative_path: String,
    pub line: usize,
    pub text: String,
}

pub(crate) fn extract_note_links(markdown: &str) -> Vec<ExtractedLink> {
    let mut found = Vec::new();
    let mut in_fence = false;
    for (index, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let line_no = index + 1;
        let snippet = bounded(&line.trim());
        collect_wiki(line, line_no, &snippet, &mut found);
        collect_markdown(line, line_no, &snippet, &mut found);
    }
    found
}

pub(crate) fn resolve_note_target(paths: &[String], from: &str, target: &str) -> Option<String> {
    let target = target.trim().trim_start_matches('/');
    let target = target.split('#').next().unwrap_or("").trim();
    if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
        return None;
    }
    let mut candidates = Vec::new();
    push_variants(&mut candidates, &target.replace('\\', "/"));
    if let Some((parent, _)) = from.rsplit_once('/') {
        let relative = format!("{parent}/{}", target.replace('\\', "/"));
        push_variants(&mut candidates, &relative);
    }
    let known = paths
        .iter()
        .filter_map(|path| normalize_note_path(path).map(|key| (key.to_lowercase(), path.clone())))
        .collect::<Vec<_>>();
    for candidate in candidates {
        let Some(key) = normalize_note_path(&candidate) else {
            continue;
        };
        let lowered = key.to_lowercase();
        if let Some((_, original)) = known.iter().find(|(known_key, _)| known_key == &lowered) {
            return Some(original.clone());
        }
    }
    None
}

pub(crate) fn backlinks_to(notes: &[(&str, &str)], current: &str) -> Vec<FoundBacklink> {
    let paths = notes
        .iter()
        .map(|(path, _)| (*path).to_string())
        .collect::<Vec<_>>();
    let Some(current_key) = normalize_note_path(current).map(|path| path.to_lowercase()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (path, markdown) in notes {
        let Some(path_key) = normalize_note_path(path) else {
            continue;
        };
        if path_key.to_lowercase() == current_key {
            continue;
        }
        let Some(link) = extract_note_links(markdown).into_iter().find(|link| {
            resolve_note_target(&paths, path, &link.target)
                .and_then(|resolved| normalize_note_path(&resolved))
                .is_some_and(|resolved| resolved.to_lowercase() == current_key)
        }) else {
            continue;
        };
        found.push(FoundBacklink {
            relative_path: (*path).to_string(),
            line: link.line,
            text: link.text,
        });
    }
    found.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    found.truncate(50);
    found
}

fn collect_wiki(line: &str, line_no: usize, snippet: &str, found: &mut Vec<ExtractedLink>) {
    let mut rest = line;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        if let Some(target) = wiki_target(&after[..end]) {
            found.push(ExtractedLink {
                target,
                line: line_no,
                text: snippet.to_string(),
            });
        }
        rest = &after[end + 2..];
    }
}

fn wiki_target(inner: &str) -> Option<String> {
    let head = inner.split('|').next()?.trim();
    let page = head.split('#').next()?.trim();
    if page.is_empty() {
        None
    } else {
        Some(page.to_string())
    }
}

fn collect_markdown(line: &str, line_no: usize, snippet: &str, found: &mut Vec<ExtractedLink>) {
    let mut index = 0;
    while let Some(relative) = line[index..].find("](") {
        let close_at = index + relative;
        let before = &line[..close_at];
        let Some(open) = before.rfind('[') else {
            index = close_at + 2;
            continue;
        };
        if open > 0 && before.as_bytes()[open - 1] == b'[' {
            index = close_at + 2;
            continue;
        }
        let after = &line[close_at + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let url = after[..end].trim();
        if is_note_url(url) {
            let target = url.split('#').next().unwrap_or(url).trim();
            if !target.is_empty() {
                found.push(ExtractedLink {
                    target: target.to_string(),
                    line: line_no,
                    text: snippet.to_string(),
                });
            }
        }
        index = close_at + 2 + end + 1;
    }
}

fn is_note_url(url: &str) -> bool {
    if url.is_empty() || url.contains("://") || url.to_lowercase().starts_with("mailto:") {
        return false;
    }
    let path = url.split('#').next().unwrap_or(url).trim();
    if path.is_empty() || path.starts_with('#') {
        return false;
    }
    let lower = path.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn push_variants(candidates: &mut Vec<String>, target: &str) {
    candidates.push(target.to_string());
    let lower = target.to_lowercase();
    if !lower.ends_with(".md") && !lower.ends_with(".markdown") {
        candidates.push(format!("{target}.md"));
        candidates.push(format!("{target}/index.md"));
    }
}

fn normalize_note_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop()?;
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn bounded(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 180 {
        return trimmed.to_string();
    }
    let mut end = 0;
    for (index, _) in trimmed.char_indices().take(180) {
        end = index;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_link_resolves_to_the_markdown_file_and_comes_back() {
        let notes = [
            (
                "index.md",
                "# Magican Notes\n\n- [[Inbox]]\n- [[Programs/harness_reliability|Harness]]\n",
            ),
            ("Inbox.md", "# Inbox\n\nSee [[index]].\n"),
            ("Programs/harness_reliability.md", "# Harness\n"),
        ];
        let paths = notes
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            resolve_note_target(&paths, "index.md", "Inbox").as_deref(),
            Some("Inbox.md")
        );
        assert_eq!(
            resolve_note_target(&paths, "index.md", "Programs/harness_reliability").as_deref(),
            Some("Programs/harness_reliability.md")
        );
        let back = backlinks_to(&notes, "Inbox.md");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].relative_path, "index.md");
        assert!(back[0].text.contains("[[Inbox]]"));
    }

    #[test]
    fn markdown_file_links_count_and_web_links_do_not() {
        let notes = [(
            "Samples/index.md",
            "See [tomatoes](Garden/Beds/tomatoes.md) and [web](https://example.com/a.md).\n",
        ), (
            "Samples/Garden/Beds/tomatoes.md",
            "# Tomatoes\n",
        )];
        let back = backlinks_to(&notes, "Samples/Garden/Beds/tomatoes.md");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].relative_path, "Samples/index.md");
    }
}
