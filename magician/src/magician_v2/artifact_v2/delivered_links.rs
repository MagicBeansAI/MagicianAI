//! Delivered files as the user sees them: served links, never host paths.
//!
//! A run that writes its deliverable to a file reports it by path — an
//! absolute host path under the task workdir, or a `file://` URL. Neither
//! opens on a phone or in another browser, and both leak the host layout.
//! Every user-facing text that may carry such a path (the task-user summary
//! the finalizer publishes, the chat terminal "Done." message) goes through
//! [`rewrite_host_paths_for_user`] once, so the same rule applies everywhere:
//!
//! - a path or `file://` URL that IS a delivered file — its source in the
//!   workdir, its execution copy, or its promoted copy, compared as whole
//!   paths — becomes a link to that file's served URL
//!   (`/api/magician/v3/tasks/{task_id}/outputs/<rel>`);
//! - any other path inside the task's roots, and every remaining `file://`
//!   target, is demoted to its plain file name (an image link to its alt
//!   text);
//! - absolute paths outside the task's roots (not `file://`) are left as
//!   written — they are not this task's files.
//!
//! Only prose is rewritten. Fenced code blocks are left exactly as written;
//! an inline code span whose whole content is one such path is replaced as a
//! unit (by the link, or by the file name in backticks), and any other code
//! span is left alone, so markdown is never injected inside code.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::models::{task_output_served_url, OutputRef, DELIVERED_FILE_OUTPUT_ROLE};

/// One delivered file as clients and the chat mouth see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredFileLink {
    /// The task output id of the promoted copy.
    pub output_id: String,
    /// The file name the owner sees.
    pub name: String,
    /// Served URL of the promoted copy.
    pub url: String,
    /// Bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// `true` for the run's primary deliverable (first declared).
    pub primary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// The title the run gave the file in `yield.deliverables`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Host paths that ARE this file (workdir source, execution copy,
    /// promoted copy). Matched whole against paths in text; never
    /// serialized, so no host path leaves the server through it.
    #[serde(skip)]
    pub source_paths: Vec<PathBuf>,
}

/// The delivered (`user_attachment`) outputs among `outputs`, primary first,
/// each with its served URL. `size` and `source_paths` are left empty;
/// callers that know the task directory fill them.
pub fn delivered_file_links(
    task_id: &str,
    outputs: &[OutputRef],
    primary_output_id: Option<&str>,
) -> Vec<DeliveredFileLink> {
    let mut links: Vec<DeliveredFileLink> = outputs
        .iter()
        .filter(|output| output.audience == "user" && output.role == DELIVERED_FILE_OUTPUT_ROLE)
        .filter_map(|output| {
            let name = output
                .relative_path
                .rsplit('/')
                .next()
                .map(str::trim)
                .filter(|name| !name.is_empty())?
                .to_string();
            Some(DeliveredFileLink {
                output_id: output.output_id.clone(),
                name,
                url: task_output_served_url(task_id, &output.relative_path),
                size: None,
                primary: primary_output_id == Some(output.output_id.as_str()),
                media_type: Some(output.media_type.clone()).filter(|media| !media.is_empty()),
                title: None,
                source_paths: Vec::new(),
            })
        })
        .collect();
    // Stable: the primary leads, the rest keep their order.
    links.sort_by_key(|link| !link.primary);
    links
}

/// A `**Files**` markdown list of the delivered files whose URL `text` does
/// not already link, primary first. `None` when every file is already
/// linked (or there are none).
pub fn delivered_files_markdown_section(
    text: &str,
    delivered: &[DeliveredFileLink],
) -> Option<String> {
    let missing: Vec<&DeliveredFileLink> = delivered
        .iter()
        .filter(|link| !text.contains(link.url.as_str()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut section = String::from("**Files**\n");
    for link in missing {
        section.push_str("- [");
        section.push_str(&escape_link_label(&link.name));
        section.push_str("](");
        section.push_str(&link.url);
        section.push(')');
        if link.primary {
            section.push_str(" (main deliverable)");
        }
        section.push('\n');
    }
    Some(section)
}

/// `text` with its trailing whitespace trimmed and the delivered-files
/// section (when any file is unlinked) appended after a blank line.
pub fn append_delivered_files_section(text: &str, delivered: &[DeliveredFileLink]) -> String {
    match delivered_files_markdown_section(text, delivered) {
        Some(section) => {
            let body = text.trim_end();
            if body.is_empty() {
                section
            } else {
                format!("{body}\n\n{section}")
            }
        },
        None => text.to_string(),
    }
}

/// See the module docs. `task_roots` are the task's directories (its task
/// dir, or workdir + outputs); each is matched both as given and
/// canonicalized, since a runtime root reached through a symlink appears in
/// text under either spelling.
pub fn rewrite_host_paths_for_user(
    text: &str,
    task_roots: &[PathBuf],
    delivered: &[DeliveredFileLink],
) -> String {
    if !text.contains('/') {
        return text.to_string();
    }
    let rewriter = Rewriter {
        roots: expanded_roots(task_roots),
        delivered,
    };
    let mut out = String::with_capacity(text.len());
    let mut prose = String::new();
    // `Some((fence char, run length))` while inside a fenced block.
    let mut fence: Option<(char, usize)> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed
            .chars()
            .next()
            .filter(|ch| matches!(ch, '`' | '~'))
            .map(|ch| (ch, trimmed.chars().take_while(|c| *c == ch).count()))
            .filter(|(_, run)| *run >= 3);
        match (fence, marker) {
            (None, Some(opening)) => {
                out.push_str(&rewriter.prose(&prose));
                prose.clear();
                fence = Some(opening);
                out.push_str(line);
            },
            (Some((ch, run)), Some((closing_ch, closing_run)))
                if closing_ch == ch
                    && closing_run >= run
                    && trimmed.trim_end().chars().all(|c| c == ch) =>
            {
                fence = None;
                out.push_str(line);
            },
            (Some(_), _) => out.push_str(line),
            (None, None) => prose.push_str(line),
        }
    }
    out.push_str(&rewriter.prose(&prose));
    out
}

struct Rewriter<'a> {
    roots: Vec<PathBuf>,
    delivered: &'a [DeliveredFileLink],
}

impl Rewriter<'_> {
    /// Prose with its inline code spans kept apart: text between spans is
    /// rewritten, a span that is exactly one path is replaced whole, any
    /// other span is copied unchanged.
    fn prose(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut out = String::with_capacity(text.len());
        let mut plain_start = 0usize;
        let mut cursor = 0usize;
        while let Some(offset) = text[cursor..].find('`') {
            let open = cursor + offset;
            let run = text[open..].chars().take_while(|ch| *ch == '`').count();
            let content_start = open + run;
            let delimiter = "`".repeat(run);
            // The closing run must be exactly as long as the opening one.
            let mut search = content_start;
            let mut close = None;
            while let Some(found) = text[search..].find(delimiter.as_str()) {
                let at = search + found;
                let run_here = text[at..].chars().take_while(|ch| *ch == '`').count();
                if run_here == run {
                    close = Some(at);
                    break;
                }
                search = at + run_here;
            }
            let Some(close) = close else {
                // An unmatched backtick run is literal text.
                cursor = content_start;
                continue;
            };
            out.push_str(&self.plain(&text[plain_start..open], Render::Markdown));
            let content = &text[content_start..close];
            match self.classify(content.trim()) {
                Some(TargetRewrite::Link { name, url }) => {
                    out.push_str(&format!("[{}]({url})", escape_link_label(&name)));
                },
                Some(TargetRewrite::PlainName(name)) => {
                    out.push_str(&delimiter);
                    out.push_str(&name);
                    out.push_str(&delimiter);
                },
                None => out.push_str(&text[open..close + run]),
            }
            cursor = close + run;
            plain_start = cursor;
        }
        out.push_str(&self.plain(&text[plain_start..], Render::Markdown));
        out
    }

    /// Code-free text: markdown links first (markdown only), then bare
    /// paths, each rendered for the surrounding format.
    fn plain(&self, text: &str, render: Render) -> String {
        if !text.contains('/') {
            return text.to_string();
        }
        if render != Render::Markdown {
            return self.bare_paths(text, render);
        }
        let with_links = markdown_link_regex().replace_all(text, |caps: &regex::Captures<'_>| {
            let whole = caps.get(0).map_or("", |m| m.as_str());
            let image = caps.get(1).is_some_and(|m| !m.as_str().is_empty());
            let label = caps.get(2).map_or("", |m| m.as_str());
            let target = caps.get(3).map_or("", |m| m.as_str());
            let bang = if image { "!" } else { "" };
            match self.classify(target) {
                Some(TargetRewrite::Link { name, url }) => {
                    let label = if label.trim().is_empty() || looks_like_host_path(label) {
                        escape_link_label(&name)
                    } else {
                        label.to_string()
                    };
                    format!("{bang}[{label}]({url})")
                },
                // A link that cannot open becomes its words: the label, or
                // the file name when the label is itself a path. An image
                // becomes its alt text, never a stray `!`.
                Some(TargetRewrite::PlainName(name)) => {
                    if label.trim().is_empty() || looks_like_host_path(label) {
                        name
                    } else {
                        label.to_string()
                    }
                },
                None => whole.to_string(),
            }
        });
        self.bare_paths(&with_links, Render::Markdown)
    }

    fn bare_paths(&self, text: &str, render: Render) -> String {
        bare_path_regex()
            .replace_all(text, |caps: &regex::Captures<'_>| {
                let lead = caps.get(1).map_or("", |m| m.as_str());
                let raw = caps.get(2).map_or("", |m| m.as_str());
                let trimmed = raw.trim_end_matches(['.', ',', ';', ':', '!', '?']);
                let tail = &raw[trimmed.len()..];
                let replacement = match (self.classify(trimmed), render) {
                    (Some(TargetRewrite::Link { name, url }), Render::Markdown) => {
                        format!("[{}]({url})", escape_link_label(&name))
                    },
                    (Some(TargetRewrite::Link { name, url }), Render::Html) => format!(
                        "<a href=\"{}\">{}</a>",
                        escape_html(&url),
                        escape_html(&name)
                    ),
                    (Some(TargetRewrite::Link { url, .. }), Render::PlainUrl) => url,
                    (Some(TargetRewrite::PlainName(name)), Render::Html) => escape_html(&name),
                    (Some(TargetRewrite::PlainName(name)), _) => name,
                    (None, _) => trimmed.to_string(),
                };
                format!("{lead}{replacement}{tail}")
            })
            .into_owned()
    }

    /// An HTML attribute value (`href`/`src`): the served URL for a
    /// delivered file, `None` to drop an attribute that can never open (a
    /// path into the task, a `file://` link), unchanged otherwise.
    fn attribute(&self, value: &str) -> AttributeRewrite {
        let decoded = value.replace("&amp;", "&");
        match self.classify(&decoded) {
            Some(TargetRewrite::Link { url, .. }) => AttributeRewrite::Replace(url),
            Some(TargetRewrite::PlainName(_)) => AttributeRewrite::Drop,
            None => AttributeRewrite::Keep,
        }
    }
}

/// How a rewritten path is written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Render {
    /// `[name](url)` / `name`.
    Markdown,
    /// `<a href="url">name</a>` / `name`, HTML-escaped.
    Html,
    /// The bare served URL / `name` — for strings inside structured data.
    PlainUrl,
}

enum AttributeRewrite {
    Keep,
    Replace(String),
    Drop,
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The HTML form of [`rewrite_host_paths_for_user`]: `href`/`src` values
/// and text nodes are rewritten by the same rules; comments, tags' other
/// attributes, and everything inside `<pre>`, `<code>`, `<script>`,
/// `<style>` and `<textarea>` are copied exactly. An attribute that names a
/// task path or `file://` target that is not a delivered file is dropped,
/// so the element stays and only the dead link goes. A hand-written
/// scanner, not a parser: it never re-serializes markup it does not touch.
pub fn rewrite_host_paths_in_html(
    html: &str,
    task_roots: &[PathBuf],
    delivered: &[DeliveredFileLink],
) -> String {
    if !html.contains('/') {
        return html.to_string();
    }
    let rewriter = Rewriter {
        roots: expanded_roots(task_roots),
        delivered,
    };
    const RAW_ELEMENTS: [&str; 5] = ["pre", "code", "script", "style", "textarea"];
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    while cursor < html.len() {
        let Some(offset) = html[cursor..].find('<') else {
            out.push_str(&rewriter.plain(&html[cursor..], Render::Html));
            break;
        };
        let open = cursor + offset;
        out.push_str(&rewriter.plain(&html[cursor..open], Render::Html));
        if lower[open..].starts_with("<!--") {
            let end = lower[open + 4..]
                .find("-->")
                .map_or(html.len(), |found| open + 4 + found + 3);
            out.push_str(&html[open..end]);
            cursor = end;
            continue;
        }
        let Some(tag_end) = tag_end(html, open) else {
            // An unterminated `<` is text.
            out.push_str(&rewriter.plain(&html[open..], Render::Html));
            break;
        };
        let tag = &html[open..tag_end];
        let name: String = tag[1..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        out.push_str(&rewrite_tag_attributes(tag, &rewriter));
        cursor = tag_end;
        if RAW_ELEMENTS.contains(&name.as_str()) && !tag.ends_with("/>") {
            let closing = format!("</{name}");
            let end = lower[cursor..]
                .find(closing.as_str())
                .map_or(html.len(), |found| cursor + found);
            out.push_str(&html[cursor..end]);
            cursor = end;
        }
    }
    out
}

/// Byte index just past the `>` closing the tag that opens at `open`,
/// skipping `>` inside quoted attribute values.
fn tag_end(html: &str, open: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, ch) in html[open + 1..].char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {},
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => return Some(open + 1 + index + 1),
            (None, '<') => return None,
            _ => {},
        }
    }
    None
}

/// `tag` with its `href`/`src` values rewritten (see
/// [`rewrite_host_paths_in_html`]); every other byte is kept.
fn rewrite_tag_attributes(tag: &str, rewriter: &Rewriter<'_>) -> String {
    if !tag.contains('/') || tag.starts_with("</") || tag.starts_with("<!") {
        return tag.to_string();
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let attribute = RE.get_or_init(|| {
        Regex::new(r#"(?i)(\s+)(href|src)(\s*=\s*)("([^"]*)"|'([^']*)'|([^\s"'>]+))"#)
            .expect("attribute pattern compiles")
    });
    attribute
        .replace_all(tag, |caps: &regex::Captures<'_>| {
            let whole = caps.get(0).map_or("", |m| m.as_str());
            let value = caps
                .get(5)
                .or_else(|| caps.get(6))
                .or_else(|| caps.get(7))
                .map_or("", |m| m.as_str());
            match rewriter.attribute(value) {
                AttributeRewrite::Keep => whole.to_string(),
                AttributeRewrite::Drop => String::new(),
                AttributeRewrite::Replace(url) => format!(
                    "{}{}{}\"{}\"",
                    caps.get(1).map_or(" ", |m| m.as_str()),
                    caps.get(2).map_or("href", |m| m.as_str()),
                    caps.get(3).map_or("=", |m| m.as_str()),
                    escape_html(&url)
                ),
            }
        })
        .into_owned()
}

/// The JSON form of [`rewrite_host_paths_for_user`]: every string VALUE is
/// rewritten (a delivered file's host path becomes its bare served URL, any
/// other task path or `file://` target its file name); keys, numbers and
/// structure are untouched. Returns whether anything changed.
pub fn rewrite_host_paths_in_json(
    value: &mut serde_json::Value,
    task_roots: &[PathBuf],
    delivered: &[DeliveredFileLink],
) -> bool {
    let rewriter = Rewriter {
        roots: expanded_roots(task_roots),
        delivered,
    };
    let mut changed = false;
    let mut pending: Vec<&mut serde_json::Value> = vec![value];
    while let Some(current) = pending.pop() {
        match current {
            serde_json::Value::String(text) if text.contains('/') => {
                let rewritten = match rewriter.classify(text.as_str()) {
                    Some(TargetRewrite::Link { url, .. }) => url,
                    Some(TargetRewrite::PlainName(name)) => name,
                    None => rewriter.plain(text.as_str(), Render::PlainUrl),
                };
                if rewritten != *text {
                    *text = rewritten;
                    changed = true;
                }
            },
            serde_json::Value::Array(items) => pending.extend(items.iter_mut()),
            serde_json::Value::Object(map) => pending.extend(map.values_mut()),
            _ => {},
        }
    }
    changed
}

impl Rewriter<'_> {
    fn classify(&self, target: &str) -> Option<TargetRewrite> {
        let target = target.trim().trim_start_matches('<').trim_end_matches('>');
        if target.is_empty() || target.contains(char::is_whitespace) && !is_file_url(target) {
            return None;
        }
        let is_file_url = is_file_url(target);
        let path = if is_file_url {
            file_url_path(target)?
        } else if target.starts_with('/') {
            lexically_normalized(Path::new(target))
        } else {
            return None;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "file".to_string());
        if let Some(link) = self.delivered_at(&path) {
            return Some(TargetRewrite::Link {
                name: link.name.clone(),
                url: link.url.clone(),
            });
        }
        if is_file_url || self.roots.iter().any(|root| path.starts_with(root)) {
            // Inside the task but not a delivered file, or a `file://` link
            // that never opens for the reader: at most its name means
            // something.
            return Some(TargetRewrite::PlainName(name));
        }
        None
    }

    /// The delivered file whose host path is exactly `path` (as written, or
    /// canonicalized when it exists).
    fn delivered_at(&self, path: &Path) -> Option<&DeliveredFileLink> {
        let canonical = std::fs::canonicalize(path).ok();
        self.delivered.iter().find(|link| {
            link.source_paths.iter().any(|source| {
                let source = lexically_normalized(source);
                source == path
                    || canonical.as_deref() == Some(source.as_path())
                    || std::fs::canonicalize(&source)
                        .ok()
                        .is_some_and(|resolved| {
                            resolved == path || canonical.as_deref() == Some(resolved.as_path())
                        })
            })
        })
    }
}

enum TargetRewrite {
    Link { name: String, url: String },
    PlainName(String),
}

fn file_url_path(target: &str) -> Option<PathBuf> {
    if let Ok(url) = url::Url::parse(target) {
        if let Ok(path) = url.to_file_path() {
            return Some(lexically_normalized(&path));
        }
    }
    // A malformed file URL (`file://relative/x`) still names a file.
    let rest = target.get(7..)?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let path = if rest.starts_with('/') {
        PathBuf::from(rest)
    } else {
        PathBuf::from(format!("/{rest}"))
    };
    Some(lexically_normalized(&path))
}

fn expanded_roots(task_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(task_roots.len() * 2);
    for root in task_roots {
        if root.as_os_str().is_empty() || !root.is_absolute() {
            continue;
        }
        let normalized = lexically_normalized(root);
        if !roots.contains(&normalized) {
            roots.push(normalized);
        }
        if let Ok(canonical) = std::fs::canonicalize(root) {
            if !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
    }
    roots
}

/// `.` and `..` resolved without touching the filesystem, so a path in text
/// that climbs out of a root (`<root>/../x`) is judged where it points.
fn lexically_normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            },
            Component::CurDir => {},
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn is_file_url(text: &str) -> bool {
    text.get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
}

fn looks_like_host_path(label: &str) -> bool {
    let label = label.trim();
    label.starts_with('/') || is_file_url(label) || label.starts_with("~/")
}

fn escape_link_label(name: &str) -> String {
    name.replace(['\n', '\r'], " ")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn markdown_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(!?)\[([^\]\n]*)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"\n]*")?\s*\)"#)
            .expect("markdown link pattern compiles")
    })
}

fn bare_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // No backtick lead: code spans never reach this pattern, and a stray
    // backtick must not make a path look like prose.
    RE.get_or_init(|| {
        Regex::new(r#"(^|[\s(\["'<])((?i:file://)[^\s)\]>"'`]+|/[^\s)\]>"'`]+)"#)
            .expect("bare path pattern compiles")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/data/internal_tasks/task-1";
    const URL: &str = "/api/magician/v3/tasks/task-1/outputs/delivered/exec-1/report.md";

    fn report_link() -> DeliveredFileLink {
        DeliveredFileLink {
            output_id: "out_task_user_attachment:tool_output_file:a".into(),
            name: "report.md".into(),
            url: URL.into(),
            size: Some(12),
            primary: true,
            media_type: Some("text/markdown".into()),
            title: None,
            source_paths: vec![
                PathBuf::from(format!("{ROOT}/workdir/report.md")),
                PathBuf::from(format!("{ROOT}/outputs/delivered/exec-1/report.md")),
            ],
        }
    }

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from(ROOT)]
    }

    fn rewrite(text: &str) -> String {
        rewrite_host_paths_for_user(text, &roots(), &[report_link()])
    }

    #[test]
    fn an_absolute_path_to_a_delivered_file_becomes_its_served_link() {
        assert_eq!(
            rewrite(&format!("The full list is in {ROOT}/workdir/report.md.")),
            format!("The full list is in [report.md]({URL}).")
        );
    }

    #[test]
    fn a_file_url_markdown_link_points_at_the_served_copy() {
        let text = format!(
            "See [the list](file://{ROOT}/workdir/report.md) and [{ROOT}/workdir/report.md](file://{ROOT}/workdir/report.md)."
        );
        assert_eq!(
            rewrite(&text),
            format!("See [the list]({URL}) and [report.md]({URL}).")
        );
    }

    #[test]
    fn a_same_named_file_elsewhere_in_the_task_is_not_the_delivered_one() {
        assert_eq!(
            rewrite(&format!("Draft at {ROOT}/workdir/drafts/report.md.")),
            "Draft at report.md."
        );
    }

    #[test]
    fn undelivered_and_foreign_file_urls_are_demoted_to_names() {
        let text = format!(
            "Scratch at {ROOT}/workdir/notes.txt, [old](file:///Volumes/x/elsewhere/old.md), file:///tmp/a%20b.csv and /usr/bin/env stays."
        );
        assert_eq!(
            rewrite(&text),
            "Scratch at notes.txt, old, a b.csv and /usr/bin/env stays."
        );
    }

    #[test]
    fn image_links_keep_or_drop_their_bang_cleanly() {
        let text = format!(
            "![chart]({ROOT}/workdir/chart.png) and ![report]({ROOT}/workdir/report.md)"
        );
        assert_eq!(rewrite(&text), format!("chart and ![report]({URL})"));
    }

    #[test]
    fn code_is_never_given_markdown() {
        let text = format!(
            "Run:\n```sh\npython {ROOT}/workdir/x.py > {ROOT}/workdir/report.md\n```\n\
             Inline `cat {ROOT}/workdir/report.md` stays; `{ROOT}/workdir/report.md` is the file, \
             `{ROOT}/workdir/notes.txt` is scratch.\n"
        );
        assert_eq!(
            rewrite(&text),
            format!(
                "Run:\n```sh\npython {ROOT}/workdir/x.py > {ROOT}/workdir/report.md\n```\n\
                 Inline `cat {ROOT}/workdir/report.md` stays; [report.md]({URL}) is the file, \
                 `notes.txt` is scratch.\n"
            )
        );
    }

    #[test]
    fn html_links_and_text_are_rewritten_without_touching_code() {
        let html = format!(
            "<p>See <a class=\"x\" href=\"file://{ROOT}/workdir/report.md\">the report</a> \
             or {ROOT}/workdir/report.md.</p><img alt=\"c\" src='{ROOT}/workdir/chart.png'>\
             <pre>{ROOT}/workdir/report.md</pre><a href=\"https://example.com/a\">ext</a>\
             <!-- {ROOT}/workdir/report.md -->"
        );
        assert_eq!(
            rewrite_host_paths_in_html(&html, &roots(), &[report_link()]),
            format!(
                "<p>See <a class=\"x\" href=\"{URL}\">the report</a> \
                 or <a href=\"{URL}\">report.md</a>.</p><img alt=\"c\">\
                 <pre>{ROOT}/workdir/report.md</pre><a href=\"https://example.com/a\">ext</a>\
                 <!-- {ROOT}/workdir/report.md -->"
            )
        );
    }

    #[test]
    fn json_string_values_are_rewritten_and_keys_kept() {
        let mut value = serde_json::json!({
            "report": format!("{ROOT}/workdir/report.md"),
            "notes": [format!("see file://{ROOT}/workdir/scratch.txt for details"), 3],
            "nested": {"other": "/usr/share/dict"},
        });
        value
            .as_object_mut()
            .unwrap()
            .insert(format!("{ROOT}/workdir/key.md"), serde_json::json!(1));
        assert!(rewrite_host_paths_in_json(&mut value, &roots(), &[report_link()]));
        assert_eq!(value["report"], URL);
        assert_eq!(value["notes"][0], "see scratch.txt for details");
        assert_eq!(value["notes"][1], 3);
        assert_eq!(value["nested"]["other"], "/usr/share/dict");
        assert!(value.get(format!("{ROOT}/workdir/key.md")).is_some(), "keys stay");
        let mut untouched = serde_json::json!({"a": "plain"});
        assert!(!rewrite_host_paths_in_json(&mut untouched, &roots(), &[report_link()]));
    }

    #[test]
    fn served_urls_and_climbing_paths_are_judged_where_they_point() {
        let text = format!("[report.md]({URL}) {ROOT}/../task-2/secret.md");
        assert_eq!(rewrite(&text), text, "neither is this task's host path");
    }

    #[test]
    fn the_files_section_lists_only_unlinked_files_primary_first() {
        let mut other = report_link();
        other.name = "data.csv".into();
        other.url = "/api/magician/v3/tasks/task-1/outputs/delivered/exec-1/data.csv".into();
        other.primary = false;
        let delivered = vec![report_link(), other];
        let text = format!("Done. [report.md]({URL})");
        assert_eq!(
            append_delivered_files_section(&text, &delivered),
            format!(
                "{text}\n\n**Files**\n- [data.csv](/api/magician/v3/tasks/task-1/outputs/delivered/exec-1/data.csv)\n"
            )
        );
        assert_eq!(
            append_delivered_files_section("", &delivered[..1]),
            format!("**Files**\n- [report.md]({URL}) (main deliverable)\n")
        );
    }

    #[test]
    fn delivered_links_come_from_attachment_outputs_primary_first() {
        let output = |id: &str, role: &str, path: &str| OutputRef {
            output_id: id.into(),
            scope: "task".into(),
            audience: "user".into(),
            role: role.into(),
            relative_path: path.into(),
            media_type: "text/markdown".into(),
            created_at: "2026-10-30T00:00:00Z".into(),
            source_execution_id: None,
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let outputs = vec![
            output("answer", "primary_task_user", "outputs/out_task_user.md"),
            output("a", DELIVERED_FILE_OUTPUT_ROLE, "outputs/delivered/e/notes.md"),
            output("b", DELIVERED_FILE_OUTPUT_ROLE, "outputs/delivered/e/report.md"),
        ];
        let links = delivered_file_links("task-1", &outputs, Some("b"));
        assert_eq!(
            links.iter().map(|l| (l.name.as_str(), l.primary)).collect::<Vec<_>>(),
            vec![("report.md", true), ("notes.md", false)]
        );
        assert_eq!(
            links[0].url,
            "/api/magician/v3/tasks/task-1/outputs/delivered/e/report.md"
        );
        assert!(serde_json::to_value(&links[0])
            .unwrap()
            .get("source_paths")
            .is_none());
    }
}
