//! Comment-preserving edits to a YAML config file's text.
//!
//! Settings writers used to re-render a whole block (or the whole file) from
//! its struct, which dropped every comment, unknown key and formatting choice
//! the owner had in it. These helpers instead change only the lines that hold
//! the values that changed and leave every other byte as it was.
//!
//! - [`set_yaml_path`] sets or removes one value at a key path: a scalar leaf
//!   keeps its trailing comment and anchor, a sequence or mapping is rendered
//!   as an indented block, a missing key (or chain of keys) is added at the end
//!   of its parent, and a flow `{}` parent is opened into a block.
//! - [`yaml_value_changes`] walks a before/after pair of values and lists the
//!   leaves (or whole sequences) that differ.
//! - [`apply_yaml_changes`] applies those changes under a section path and
//!   re-reads the result to prove each change landed. Equal before/after
//!   values return the text unchanged, byte for byte.
//!
//! The editor understands the block YAML these config files are written in:
//! nested mappings, block and indentless sequences, flow values, quoted keys
//! and values, anchors and comments. A value it cannot open in place (a flow
//! mapping with content, a tagged node) is parsed, edited as a value and
//! re-rendered as a block; only that key's lines change.

use serde_yaml::{Mapping, Value};

/// A key rendered by `serde_yaml` before it is swapped for the real key text.
/// Plain-safe and short, so the emitter never quotes it or makes it complex.
const PLACEHOLDER_KEY: &str = "__magician_yaml_edit_key__";

/// Default indentation step for a block the file gives no example of.
const INDENT_STEP: usize = 2;

/// One change: a key path below the edited section, and the new value
/// (`None` removes the key).
pub type YamlChange = (Vec<String>, Option<Value>);

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

/// Every leaf that differs between `before` and `after`, as paths relative to
/// the value they were taken from. Mappings with string keys are walked key by
/// key, so an unchanged sibling never appears; anything else that differs
/// (a scalar, a sequence, a mapping with non-string keys, a type change) is
/// one change of the whole value. A mapping that becomes empty is set to
/// `{}` rather than emptied key by key, so it never reads back as null.
pub fn yaml_value_changes(before: Option<&Value>, after: Option<&Value>) -> Vec<YamlChange> {
    let mut out = Vec::new();
    collect_changes(&mut Vec::new(), before, after, &mut out);
    out
}

fn collect_changes(
    path: &mut Vec<String>,
    before: Option<&Value>,
    after: Option<&Value>,
    out: &mut Vec<YamlChange>,
) {
    let Some(after) = after else {
        if before.is_some() {
            out.push((path.clone(), None));
        }
        return;
    };
    let Value::Mapping(after_map) = after else {
        if before != Some(after) {
            out.push((path.clone(), Some(after.clone())));
        }
        return;
    };
    let before_map = match before {
        Some(Value::Mapping(map)) => Some(map),
        // Absent or null: every key of `after` is new.
        None | Some(Value::Null) => None,
        Some(_) => {
            out.push((path.clone(), Some(after.clone())));
            return;
        },
    };
    if after_map.is_empty() {
        if before_map.map_or(true, |map| !map.is_empty()) {
            out.push((path.clone(), Some(after.clone())));
        }
        return;
    }
    let string_keys = |map: &Mapping| map.keys().all(Value::is_string);
    if !string_keys(after_map) || !before_map.map_or(true, string_keys) {
        if before != Some(after) {
            out.push((path.clone(), Some(after.clone())));
        }
        return;
    }
    for (key, value) in after_map {
        let Some(name) = key.as_str() else { continue };
        path.push(name.to_string());
        collect_changes(
            path,
            before_map.and_then(|map| map.get(key)),
            Some(value),
            out,
        );
        path.pop();
    }
    if let Some(before_map) = before_map {
        for key in before_map.keys() {
            if !after_map.contains_key(key) {
                if let Some(name) = key.as_str() {
                    path.push(name.to_string());
                    out.push((path.clone(), None));
                    path.pop();
                }
            }
        }
    }
}

/// Applies the difference between `before` and `after` to the value at
/// `section` in `contents`, touching only the lines of values that changed.
///
/// `seed` is an optional whole block (`key:` line included) written first
/// when `section` is a single top-level key the file does not have yet — the
/// place for explanatory comments a fresh block should carry. The changes are
/// applied on top of it, so a seed only has to carry comments and whatever
/// fields it likes.
///
/// The result is parsed back and every change checked before it is returned;
/// an edit that would not read back is an error, never a silent bad write.
pub fn apply_yaml_changes(
    contents: &str,
    section: &[&str],
    before: &Value,
    after: &Value,
    seed: Option<&str>,
) -> std::io::Result<String> {
    let changes = yaml_value_changes(Some(before), Some(after));
    if changes.is_empty() {
        return Ok(contents.to_string());
    }
    let mut next = contents.to_string();
    if let (Some(seed), [top_key]) = (seed, section) {
        let adds_something = changes.iter().any(|(_, value)| value.is_some());
        if adds_something && !has_top_level_key(contents, top_key) {
            next = super::replace_top_level_yaml_block(contents, top_key, seed);
        }
    }
    let mut full_paths = Vec::with_capacity(changes.len());
    for (path, value) in &changes {
        let full: Vec<&str> = section
            .iter()
            .copied()
            .chain(path.iter().map(String::as_str))
            .collect();
        next = set_yaml_path(&next, &full, value.as_ref())?;
        full_paths.push(
            full.iter()
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>(),
        );
    }
    verify_changes(&next, &full_paths, &changes)?;
    Ok(next)
}

fn verify_changes(
    contents: &str,
    paths: &[Vec<String>],
    changes: &[YamlChange],
) -> std::io::Result<()> {
    let document: Value = serde_yaml::from_str(contents)
        .map_err(|error| invalid(format!("edited config does not parse: {error}")))?;
    for (path, (_, expected)) in paths.iter().zip(changes) {
        let mut current = Some(&document);
        for segment in path {
            current = current
                .and_then(Value::as_mapping)
                .and_then(|map| map.get(segment.as_str()));
        }
        // Whole floats are written as integers (`whole_floats_as_integers`).
        if current.map(whole_floats_as_integers) != expected.as_ref().map(whole_floats_as_integers)
        {
            return Err(invalid(format!(
                "edited config does not read back `{}` as written",
                path.join(".")
            )));
        }
    }
    Ok(())
}

/// Sets the value at `path` (`None` removes it), changing only that key's
/// lines. A scalar written over a scalar keeps the line's anchor and trailing
/// comment; a missing key — or chain of keys — is added at the end of its
/// parent block (a missing top-level key at the end of the file). Removing a
/// key that is not there returns `contents` unchanged.
pub fn set_yaml_path(
    contents: &str,
    path: &[&str],
    value: Option<&Value>,
) -> std::io::Result<String> {
    if path.is_empty() {
        return Err(invalid("an empty YAML key path names no value"));
    }
    let mut lines: Vec<String> = contents.split_inclusive('\n').map(str::to_string).collect();
    // The mapping being searched: its header line (None = the document), the
    // header's indentation, and the exclusive line range of its body.
    let mut parent: Option<(usize, usize)> = None;
    let mut start = 0;
    let mut end = lines.len();
    for (depth, segment) in path.iter().enumerate() {
        let last = depth + 1 == path.len();
        let parent_indent = parent.map(|(_, indent)| indent);
        let child_indent = region_child_indent(&lines[start..end], parent_indent);
        let found = (start..end).find(|&index| {
            is_content(&lines[index])
                && indent_of(&lines[index]) == child_indent
                && parse_key_line(&lines[index]).is_some_and(|key| key.key == *segment)
        });
        let Some(index) = found else {
            let Some(value) = value else {
                return Ok(contents.to_string());
            };
            let nested = nest_value(&path[depth + 1..], value.clone());
            let key_text = render_key(segment)?;
            let Some((header, header_indent)) = parent else {
                let mut out = lines.concat().trim_end_matches('\n').to_string();
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                let entry = render_entry(&key_text, &nested, 0, None, "")?;
                out.push_str(&entry.concat());
                return Ok(out);
            };
            let at = insertion_point(&lines, header, end, header_indent);
            if !lines[at - 1].ends_with('\n') {
                lines[at - 1].push('\n');
            }
            let entry = render_entry(&key_text, &nested, child_indent, None, "")?;
            lines.splice(at..at, entry);
            return Ok(lines.concat());
        };
        let key = parse_key_line(&lines[index]).expect("matched a key line");
        let inline = split_inline_value(&key.rest);
        let block_end = block_end(&lines, index, key.indent, inline.value.is_empty());
        if last {
            replace_entry(&mut lines, index, block_end, &key, &inline, value)?;
            if value.is_none() {
                if let Some((header, _)) = parent {
                    close_empty_mapping(&mut lines, header);
                }
            }
            return Ok(lines.concat());
        }
        let navigable = match inline.value.as_str() {
            "" => lines[index + 1..=block_end]
                .iter()
                .find(|line| is_content(line))
                .map_or(true, |line| {
                    indent_of(line) > key.indent && !is_seq_item(line)
                }),
            "{}" | "null" | "~" => {
                if value.is_none() {
                    return Ok(contents.to_string());
                }
                // Open the empty flow/null value into a block header.
                let mut header = format!(
                    "{}{}:{}{}",
                    " ".repeat(key.indent),
                    key.key_text,
                    inline.anchor_suffix(),
                    inline.comment
                );
                header.push('\n');
                lines[index] = header;
                true
            },
            _ => false,
        };
        if !navigable {
            rewrite_via_value(
                &mut lines,
                index,
                block_end,
                &key,
                &inline,
                &path[depth + 1..],
                value,
            )?;
            return Ok(lines.concat());
        }
        parent = Some((index, key.indent));
        start = index + 1;
        end = block_end + 1;
    }
    unreachable!("the last path segment always returns")
}

/// Whether `contents` has `key` as a top-level mapping key.
pub fn has_top_level_key(contents: &str, key: &str) -> bool {
    contents
        .split_inclusive('\n')
        .filter_map(parse_key_line)
        .any(|line| line.indent == 0 && line.key == key)
}

fn nest_value(path: &[&str], value: Value) -> Value {
    path.iter().rev().fold(value, |inner, segment| {
        let mut map = Mapping::new();
        map.insert(Value::String(segment.to_string()), inner);
        Value::Mapping(map)
    })
}

fn set_in_value(target: &mut Value, path: &[&str], value: Option<&Value>) {
    let Some((first, rest)) = path.split_first() else {
        *target = value.cloned().unwrap_or(Value::Null);
        return;
    };
    if !target.is_mapping() {
        if value.is_none() {
            return;
        }
        *target = Value::Mapping(Mapping::new());
    }
    let map = target.as_mapping_mut().expect("just made a mapping");
    let key = Value::String(first.to_string());
    if rest.is_empty() {
        match value {
            Some(value) => {
                map.insert(key, value.clone());
            },
            None => {
                map.shift_remove(&key);
            },
        }
        return;
    }
    if !map.contains_key(&key) {
        if value.is_none() {
            return;
        }
        map.insert(key.clone(), Value::Null);
    }
    if let Some(child) = map.get_mut(&key) {
        set_in_value(child, rest, value);
    }
}

/// A key whose value cannot be opened in place: parse it, edit it as a
/// value, and write the whole key back as a block.
fn rewrite_via_value(
    lines: &mut Vec<String>,
    index: usize,
    block_end: usize,
    key: &KeyLine,
    inline: &InlineValue,
    rest: &[&str],
    value: Option<&Value>,
) -> std::io::Result<()> {
    let snippet: String = lines[index..=block_end]
        .iter()
        .map(|line| dedent(line, key.indent))
        .collect();
    let parsed: Value = serde_yaml::from_str(&snippet).map_err(|error| {
        invalid(format!(
            "cannot edit `{}`: its value does not parse: {error}",
            key.key
        ))
    })?;
    let mut current = parsed
        .as_mapping()
        .and_then(|map| map.values().next().cloned())
        .unwrap_or(Value::Null);
    let original = current.clone();
    set_in_value(&mut current, rest, value);
    if current == original {
        return Ok(());
    }
    // The old inline value is gone; keep only its anchor and comment.
    replace_entry(lines, index, block_end, key, inline, Some(&current))
}

fn dedent(line: &str, indent: usize) -> String {
    if line.len() >= indent && line.as_bytes()[..indent].iter().all(|byte| *byte == b' ') {
        line[indent..].to_string()
    } else {
        line.trim_start_matches([' ', '\t']).to_string()
    }
}

fn replace_entry(
    lines: &mut Vec<String>,
    index: usize,
    block_end: usize,
    key: &KeyLine,
    inline: &InlineValue,
    value: Option<&Value>,
) -> std::io::Result<()> {
    let had_final_newline = lines[block_end].ends_with('\n');
    let Some(value) = value else {
        lines.drain(index..=block_end);
        return Ok(());
    };
    let mut entry = render_entry(
        &key.key_text,
        value,
        key.indent,
        inline.anchor.as_deref(),
        &inline.comment,
    )?;
    if !had_final_newline && block_end + 1 == lines.len() {
        if let Some(last) = entry.last_mut() {
            if last.ends_with('\n') {
                last.pop();
            }
        }
    }
    lines.splice(index..=block_end, entry);
    Ok(())
}

/// After a removal, a nested mapping left without keys is written `{}` so it
/// reads back as an empty mapping, not null.
fn close_empty_mapping(lines: &mut [String], header: usize) {
    let Some(key) = parse_key_line(&lines[header]) else {
        return;
    };
    let end = block_end(lines, header, key.indent, true);
    if lines[header + 1..=end].iter().any(|line| is_content(line)) {
        return;
    }
    let inline = split_inline_value(&key.rest);
    if !inline.value.is_empty() {
        return;
    }
    lines[header] = format!(
        "{}{}:{} {{}}{}\n",
        " ".repeat(key.indent),
        key.key_text,
        inline.anchor_suffix(),
        inline.comment
    );
}

/// The lines for `key_text: value` at `indent`, each ending in `\n`.
fn render_entry(
    key_text: &str,
    value: &Value,
    indent: usize,
    anchor: Option<&str>,
    comment: &str,
) -> std::io::Result<Vec<String>> {
    let mut wrapper = Mapping::new();
    wrapper.insert(
        Value::String(PLACEHOLDER_KEY.to_string()),
        whole_floats_as_integers(value),
    );
    let rendered = serde_yaml::to_string(&Value::Mapping(wrapper))
        .map_err(|error| invalid(format!("cannot render `{key_text}`: {error}")))?;
    let rendered = rendered.strip_prefix("---\n").unwrap_or(&rendered);
    let mut rendered_lines = rendered.lines();
    let first_rest = rendered_lines
        .next()
        .and_then(|line| line.strip_prefix(PLACEHOLDER_KEY))
        .and_then(|line| line.strip_prefix(':'))
        .ok_or_else(|| invalid(format!("unexpected rendering for `{key_text}`")))?;
    let pad = " ".repeat(indent);
    let anchor = anchor
        .map(|anchor| format!(" {anchor}"))
        .unwrap_or_default();
    let mut out = vec![format!("{pad}{key_text}:{anchor}{first_rest}{comment}\n")];
    out.extend(rendered_lines.map(|line| {
        if line.is_empty() {
            "\n".to_string()
        } else {
            format!("{pad}{line}\n")
        }
    }));
    Ok(out)
}

/// `100.0` written as `100`: an `f64` setting set back to a whole number
/// would otherwise change the owner's `100` to `100.0` (same value, a diff
/// nobody made). Float fields read a whole number back unchanged.
fn whole_floats_as_integers(value: &Value) -> Value {
    match value {
        Value::Number(number) if number.is_f64() => match number.as_f64() {
            Some(float) if float.is_finite() && float.fract() == 0.0 && float.abs() < 1e15 => {
                Value::Number((float as i64).into())
            },
            _ => value.clone(),
        },
        Value::Sequence(items) => {
            Value::Sequence(items.iter().map(whole_floats_as_integers).collect())
        },
        Value::Mapping(map) => Value::Mapping(
            map.iter()
                .map(|(key, item)| (key.clone(), whole_floats_as_integers(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A new key as YAML text: plain where `serde_yaml` would write it plain,
/// quoted otherwise.
fn render_key(key: &str) -> std::io::Result<String> {
    let mut wrapper = Mapping::new();
    wrapper.insert(Value::String(key.to_string()), Value::Null);
    let rendered = serde_yaml::to_string(&Value::Mapping(wrapper))
        .map_err(|error| invalid(format!("cannot render key `{key}`: {error}")))?;
    let rendered = rendered.strip_prefix("---\n").unwrap_or(&rendered);
    match rendered.strip_suffix(": null\n") {
        Some(text) if !text.contains('\n') && !text.starts_with('?') => Ok(text.to_string()),
        _ => serde_json::to_string(key).map_err(|error| invalid(error.to_string())),
    }
}

/// Where a new child of the mapping headed at `header` goes: after its last
/// content line, or after a comment indented under it (a fresh block's
/// explanatory comments stay above the keys added to it).
fn insertion_point(
    lines: &[String],
    header: usize,
    region_end: usize,
    header_indent: usize,
) -> usize {
    let mut last = header;
    for (index, line) in lines.iter().enumerate().take(region_end).skip(header + 1) {
        if is_content(line) || (is_comment(line) && indent_of(line) > header_indent) {
            last = index;
        }
    }
    for (index, line) in lines.iter().enumerate().skip(region_end) {
        if line.trim().is_empty() {
            continue;
        }
        if is_comment(line) && indent_of(line) > header_indent {
            last = index;
            continue;
        }
        break;
    }
    last + 1
}

/// The indentation of a region's keys: its first content line's, else one
/// step in from the header.
fn region_child_indent(region: &[String], parent_indent: Option<usize>) -> usize {
    region
        .iter()
        .filter(|line| is_content(line))
        .map(|line| indent_of(line))
        .find(|indent| parent_indent.map_or(true, |parent| *indent > parent))
        .unwrap_or_else(|| parent_indent.map_or(0, |parent| parent + INDENT_STEP))
}

/// The last line of the value that starts on the key line at `index`:
/// following content lines indented deeper, plus sequence items at the key's
/// own indentation when the value is a block.
fn block_end(lines: &[String], index: usize, indent: usize, block_value: bool) -> usize {
    let mut end = index;
    for (offset, line) in lines.iter().enumerate().skip(index + 1) {
        if !is_content(line) {
            continue;
        }
        let line_indent = indent_of(line);
        if line_indent > indent || (block_value && line_indent == indent && is_seq_item(line)) {
            end = offset;
        } else {
            break;
        }
    }
    end
}

fn line_body(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn is_content(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#')
}

fn is_seq_item(line: &str) -> bool {
    let trimmed = line_body(line).trim_start();
    trimmed == "-" || trimmed.starts_with("- ") || trimmed.starts_with("-\t")
}

/// A `key: ...` line: the key's value, its text as written, and what follows
/// the colon.
#[derive(Debug)]
struct KeyLine {
    indent: usize,
    key: String,
    key_text: String,
    rest: String,
}

fn parse_key_line(line: &str) -> Option<KeyLine> {
    let body = line_body(line);
    let indent = indent_of(body);
    let text = &body[indent..];
    if text.is_empty() || text.starts_with('#') || is_seq_item(text) {
        return None;
    }
    if text.starts_with(['?', '{', '[', '&', '*', '!', '|', '>', '%', '@', '`']) {
        return None;
    }
    let bytes = text.as_bytes();
    let (key, key_end) = if bytes[0] == b'"' {
        let mut index = 1;
        let mut close = None;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => index += 2,
                b'"' => {
                    close = Some(index);
                    break;
                },
                _ => index += 1,
            }
        }
        let close = close?;
        let key: String = serde_yaml::from_str(&text[..=close]).ok()?;
        (key, close + 1)
    } else if bytes[0] == b'\'' {
        let mut index = 1;
        let mut close = None;
        while index < bytes.len() {
            if bytes[index] == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                close = Some(index);
                break;
            }
            index += 1;
        }
        let close = close?;
        (text[1..close].replace("''", "'"), close + 1)
    } else {
        // A plain key ends at the first `: ` (or a line-ending `:`), unless a
        // ` #` comment starts first.
        let colon = (0..bytes.len())
            .take_while(|&index| {
                !(bytes[index] == b'#' && index > 0 && matches!(bytes[index - 1], b' ' | b'\t'))
            })
            .find(|&index| {
                bytes[index] == b':' && matches!(bytes.get(index + 1), None | Some(b' ' | b'\t'))
            })?;
        let key = text[..colon].trim_end();
        if key.is_empty() {
            return None;
        }
        (key.to_string(), colon)
    };
    let after_key = &text[key_end..];
    let spaces = after_key.len() - after_key.trim_start_matches([' ', '\t']).len();
    let colon = key_end + spaces;
    if bytes.get(colon) != Some(&b':') {
        return None;
    }
    if !matches!(bytes.get(colon + 1), None | Some(b' ' | b'\t')) {
        return None;
    }
    Some(KeyLine {
        indent,
        key,
        key_text: text[..colon].to_string(),
        rest: text[colon + 1..].to_string(),
    })
}

/// What follows a key's colon: an optional anchor, the inline value (empty
/// for a block or null value) and a trailing comment with its leading
/// whitespace.
#[derive(Debug, Default)]
struct InlineValue {
    anchor: Option<String>,
    value: String,
    comment: String,
}

impl InlineValue {
    fn anchor_suffix(&self) -> String {
        self.anchor
            .as_deref()
            .map(|anchor| format!(" {anchor}"))
            .unwrap_or_default()
    }
}

fn split_inline_value(rest: &str) -> InlineValue {
    let skip_space = |text: &str| text.len() - text.trim_start_matches([' ', '\t']).len();
    let mut body = &rest[skip_space(rest)..];
    let mut comment_from = rest;
    let mut out = InlineValue::default();
    if body.starts_with('&') {
        let end = body.find([' ', '\t']).unwrap_or(body.len());
        out.anchor = Some(body[..end].to_string());
        comment_from = &body[end..];
        body = &comment_from[skip_space(comment_from)..];
    }
    if body.is_empty() {
        return out;
    }
    if body.starts_with('#') {
        out.comment = comment_from.to_string();
        return out;
    }
    let bytes = body.as_bytes();
    let value_end = match bytes[0] {
        b'"' => {
            let mut index = 1;
            let mut end = body.len();
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index += 2,
                    b'"' => {
                        end = index + 1;
                        break;
                    },
                    _ => index += 1,
                }
            }
            end
        },
        b'\'' => {
            let mut index = 1;
            let mut end = body.len();
            while index < bytes.len() {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    end = index + 1;
                    break;
                }
                index += 1;
            }
            end
        },
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut quote: Option<u8> = None;
            let mut end = body.len();
            let mut index = 0;
            while index < bytes.len() {
                let byte = bytes[index];
                match quote {
                    Some(b'"') if byte == b'\\' => index += 1,
                    Some(open) if byte == open => quote = None,
                    Some(_) => {},
                    None => match byte {
                        b'"' | b'\'' => quote = Some(byte),
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                end = index + 1;
                                break;
                            }
                        },
                        _ => {},
                    },
                }
                index += 1;
            }
            end
        },
        _ => (1..bytes.len())
            .find(|&index| bytes[index] == b'#' && matches!(bytes[index - 1], b' ' | b'\t'))
            .unwrap_or(body.len()),
    };
    let value = body[..value_end.min(body.len())].trim_end();
    let tail = &body[value.len()..];
    out.value = value.to_string();
    if tail.trim_start().starts_with('#') {
        out.comment = tail.to_string();
    }
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Value {
        serde_yaml::from_str(text).unwrap()
    }

    fn set(contents: &str, path: &[&str], value: Option<&str>) -> String {
        let value = value.map(yaml);
        set_yaml_path(contents, path, value.as_ref()).unwrap()
    }

    const NESTED: &str = "\
# top comment
execution:
  # which engine thinks a run
  harness_engine: magician  # owner note
  harness_auth:
    # login first
    mode: login_first  # keep
    engines:
      codex: magician
  file_sandbox:
    mode: workspace_write
    allowed_roots:
    - .
    # the scratch dir too
    - /tmp
  harness_turn_max_seconds: 2400
chat:
  harness_engine: magician
";

    #[test]
    fn a_deep_scalar_changes_on_its_own_line_keeping_comments() {
        let next = set(
            NESTED,
            &["execution", "harness_auth", "engines", "codex"],
            Some("pi"),
        );
        assert_eq!(
            next,
            NESTED.replace("      codex: magician", "      codex: pi")
        );
        let next = set(
            NESTED,
            &["execution", "harness_auth", "mode"],
            Some("login_only"),
        );
        assert_eq!(
            next,
            NESTED.replace("mode: login_first  # keep", "mode: login_only  # keep")
        );
    }

    #[test]
    fn a_trailing_comment_survives_quoted_and_anchored_values() {
        let contents = "a:\n  b: \"x # not a comment\"  # real\n  c: &pick one # chosen\n";
        let next = set(contents, &["a", "b"], Some("y"));
        assert_eq!(next, "a:\n  b: y  # real\n  c: &pick one # chosen\n");
        let next = set(contents, &["a", "c"], Some("two"));
        assert_eq!(
            next,
            "a:\n  b: \"x # not a comment\"  # real\n  c: &pick two # chosen\n"
        );
    }

    #[test]
    fn a_changed_list_is_replaced_and_its_siblings_kept() {
        let next = set(
            NESTED,
            &["execution", "file_sandbox", "allowed_roots"],
            Some("[., /work]"),
        );
        assert!(
            next.contains(
                "    mode: workspace_write\n    allowed_roots:\n    - .\n    - /work\n  harness_turn_max_seconds: 2400\n"
            ),
            "{next}"
        );
        assert!(next.starts_with("# top comment\nexecution:\n  # which engine thinks a run\n"));
        let parsed = yaml(&next);
        assert_eq!(
            parsed["execution"]["file_sandbox"]["allowed_roots"][1],
            "/work"
        );
        assert_eq!(parsed["chat"]["harness_engine"], "magician");
    }

    #[test]
    fn a_missing_nested_path_is_created_at_the_end_of_its_parent() {
        let next = set(
            NESTED,
            &["execution", "budgets", "native_max_cost_usd"],
            Some("15.0"),
        );
        assert!(
            next.contains(
                "  harness_turn_max_seconds: 2400\n  budgets:\n    native_max_cost_usd: 15\nchat:\n"
            ),
            "{next}"
        );
        let next = set(NESTED, &["media", "surface", "default"], Some("voice"));
        assert!(next
            .ends_with("  harness_engine: magician\n\nmedia:\n  surface:\n    default: voice\n"));
    }

    #[test]
    fn removal_drops_only_the_key_and_reverses_an_insert() {
        let added = set(NESTED, &["execution", "pi_profile"], Some("openai"));
        assert_eq!(set(&added, &["execution", "pi_profile"], None), NESTED);
        assert_eq!(set(NESTED, &["execution", "pi_profile"], None), NESTED);
        assert_eq!(set(NESTED, &["nothing", "here"], None), NESTED);
        let next = set(NESTED, &["execution", "harness_auth", "engines"], None);
        assert!(
            next.contains("    mode: login_first  # keep\n  file_sandbox:\n"),
            "{next}"
        );
    }

    #[test]
    fn removing_the_last_key_leaves_an_empty_mapping_not_null() {
        let next = set(
            NESTED,
            &["execution", "harness_auth", "engines", "codex"],
            None,
        );
        assert!(
            next.contains("    engines: {}\n  file_sandbox:\n"),
            "{next}"
        );
        assert!(yaml(&next)["execution"]["harness_auth"]["engines"].is_mapping());
    }

    #[test]
    fn flow_values_are_opened_or_rewritten_as_blocks() {
        let contents = "enabled: true\nmedia: {}  # audio\nfrontend:\n  host: 127.0.0.1\n";
        let next = set(contents, &["media", "enabled"], Some("true"));
        assert_eq!(
            next,
            "enabled: true\nmedia:  # audio\n  enabled: true\nfrontend:\n  host: 127.0.0.1\n"
        );
        let contents = "chat: {harness_engine: magician}\nnext: 1\n";
        let next = set(contents, &["chat", "harness_model"], Some("default"));
        assert_eq!(
            next,
            "chat:\n  harness_engine: magician\n  harness_model: default\nnext: 1\n"
        );
        assert_eq!(set(contents, &["chat", "missing"], None), contents);
    }

    #[test]
    fn indentless_and_flow_lists_are_replaced_whole() {
        let contents = "roots:\n- a\n- b\nafter: 1\n";
        let next = set(contents, &["roots"], Some("[c]"));
        assert_eq!(next, "roots:\n- c\nafter: 1\n");
        let contents = "x:\n  roots: [a, b] # list\n  y: 2\n";
        let next = set(contents, &["x", "roots"], Some("[]"));
        assert_eq!(next, "x:\n  roots: [] # list\n  y: 2\n");
    }

    /// Found live: a cap set to 99 and back to 100 left `100.0` where the
    /// owner had written `100`.
    #[test]
    fn a_whole_float_round_trips_to_the_owners_integer_spelling() {
        let original = "execution:\n  budgets:\n    harness_max_cost_usd: 100\n";
        let ninety_nine = set(
            original,
            &["execution", "budgets", "harness_max_cost_usd"],
            Some("99.0"),
        );
        assert!(
            ninety_nine.contains("harness_max_cost_usd: 99\n"),
            "{ninety_nine}"
        );
        let back = set(
            &ninety_nine,
            &["execution", "budgets", "harness_max_cost_usd"],
            Some("100.0"),
        );
        assert_eq!(back, original);
        let fraction = set(
            original,
            &["execution", "budgets", "harness_max_cost_usd"],
            Some("2.5"),
        );
        assert!(
            fraction.contains("harness_max_cost_usd: 2.5\n"),
            "{fraction}"
        );
    }

    #[test]
    fn writing_the_same_value_is_byte_identical() {
        let next = set(NESTED, &["execution", "harness_engine"], Some("magician"));
        assert_eq!(next, NESTED);
        let section = yaml(NESTED)["execution"].clone();
        assert_eq!(
            apply_yaml_changes(NESTED, &["execution"], &section, &section, None).unwrap(),
            NESTED
        );
    }

    #[test]
    fn the_diff_writer_touches_only_changed_leaves() {
        let before = yaml(NESTED)["execution"].clone();
        let mut after = before.clone();
        after["harness_engine"] = Value::from("pi");
        after["harness_auth"]["engines"]["grok"] = Value::from("login");
        after["file_sandbox"]
            .as_mapping_mut()
            .unwrap()
            .shift_remove("mode");
        let next = apply_yaml_changes(NESTED, &["execution"], &before, &after, None).unwrap();
        let expected = NESTED
            .replace(
                "harness_engine: magician  # owner note",
                "harness_engine: pi  # owner note",
            )
            .replace(
                "      codex: magician\n",
                "      codex: magician\n      grok: login\n",
            )
            .replace("    mode: workspace_write\n", "");
        assert_eq!(next, expected);
    }

    #[test]
    fn a_seed_block_is_written_only_when_the_section_is_new() {
        let before = yaml("enabled: false\ntimeout: 30");
        let after = yaml("enabled: true\ntimeout: 30");
        let seed = "deploy:\n  # Static publishing targets.\n";
        let next =
            apply_yaml_changes("other: 1\n", &["deploy"], &before, &after, Some(seed)).unwrap();
        assert_eq!(
            next,
            "other: 1\n\ndeploy:\n  # Static publishing targets.\n  enabled: true\n"
        );
        let existing = "deploy:\n  enabled: false  # off for now\n";
        let next = apply_yaml_changes(existing, &["deploy"], &before, &after, Some(seed)).unwrap();
        assert_eq!(next, "deploy:\n  enabled: true  # off for now\n");
    }

    #[test]
    fn value_changes_list_leaves_sequences_and_removals() {
        let before = yaml("a: 1\nb: {c: 2, d: 3}\nlist: [1, 2]\ngone: x");
        let after = yaml("a: 1\nb: {c: 2, d: 4}\nlist: [1]\nnew: {e: 5}");
        let changes = yaml_value_changes(Some(&before), Some(&after));
        let names: Vec<(String, bool)> = changes
            .iter()
            .map(|(path, value)| (path.join("."), value.is_some()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("b.d".to_string(), true),
                ("list".to_string(), true),
                ("new.e".to_string(), true),
                ("gone".to_string(), false),
            ]
        );
    }

    #[test]
    fn quoted_keys_are_found_and_awkward_new_keys_are_quoted() {
        let contents = "map:\n  \"a: b\": 1\n  'it''s': 2\n";
        let next = set(contents, &["map", "a: b"], Some("3"));
        assert_eq!(next, "map:\n  \"a: b\": 3\n  'it''s': 2\n");
        let next = set(contents, &["map", "it's"], Some("4"));
        assert_eq!(next, "map:\n  \"a: b\": 1\n  'it''s': 4\n");
        let next = set(contents, &["map", "true"], Some("5"));
        assert_eq!(yaml(&next)["map"]["true"], 5);
    }

    #[test]
    fn multi_line_strings_render_as_blocks_under_their_key() {
        let mut value = Mapping::new();
        value.insert(Value::from("note"), Value::from("line one\nline two"));
        let next =
            set_yaml_path("a:\n  b: 1\n", &["a", "c"], Some(&Value::Mapping(value))).unwrap();
        let parsed = yaml(&next);
        assert_eq!(parsed["a"]["c"]["note"], "line one\nline two");
        assert_eq!(parsed["a"]["b"], 1);
    }
}
