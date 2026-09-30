//! The reverse drift gate: **emitted name → taxonomy row**.
//!
//! # The direction nothing checked, and what it cost
//!
//! `realtime_events.rs` carries three taxonomy gates and all three run the same
//! way round. `gaui_taxonomy_covers_every_artifact_v2_event_type` and
//! `gaui_taxonomy_covers_every_runtime_agent_event_type` check *variant → row*.
//! `every_runtime_agent_taxonomy_row_has_a_typed_variant` checks *row →
//! variant*. Between them they make the enum and the table agree with each
//! other — and say nothing at all about what the code actually emits.
//!
//! That is not a theoretical hole. `phases::outbox::gap_record` synthesises a
//! `loop.outbox.gap` record to mark records the run outbox's cap evicted, and
//! it shipped with the name spelled as a bare literal and no row anywhere. Every
//! surface router in `progress_channel_seam::surface_routing` fails CLOSED on an
//! unknown name — `chat_render_kind` answers `ChatRenderKind::Suppress`,
//! `webhook_surface_renders_agent_event` and
//! `agent_memory_surface_renders_agent_event` answer `false` — so the marker was
//! journalled, projected, and then dropped one step short of a reader. No test
//! in the tree failed, because no test looked in this direction.
//!
//! # What is enumerable, given that "every emitted string" is not
//!
//! `RuntimeTransportBroadcaster::emit_named` takes `event_type: &str`. There is
//! no type to exhaust and no registry to walk, so nothing can enumerate the
//! emitted set at compile time. What *is* enumerable is the set of **source
//! positions where a name is handed to the named rail**. There are five, they
//! all funnel into `emit_named`, and a name reaching a live surface has to pass
//! through one of them:
//!
//! | anchor                      | the name is argument |
//! |-----------------------------|----------------------|
//! | `emit_named`                | 0                    |
//! | `journal_and_emit_named`    | 4                    |
//! | `journal_and_emit_named_at` | 3                    |
//! | `named_event`               | 2                    |
//! | `named_event_measured`      | 2                    |
//!
//! So this is a source-text contract, in the style
//! `outward_gate_order_contract.rs` already uses here, and read from disk for
//! the same reason that file gives: the executor alone is ~1.5 MB and has no
//! business inside a test binary.
//!
//! # What it cannot check, stated rather than glossed
//!
//! Three residues, and none of them is small enough to leave implied:
//!
//! 1. **Forwarded names.** 24 of the 65 anchor sites take a local
//!    (`event_type`, `event_type_str`, `name`, `&key`) whose value is chosen at
//!    runtime — the chat channel forwarding a `ProgressMessage`'s type, the
//!    projector putting a journalled record back through the same entry point,
//!    the two `journal_and_emit_named*` wrappers forwarding their own parameter.
//!    A source scan cannot follow those, and this gate does not pretend to. It
//!    prints every one of them with its file and line on each run, so the
//!    residue is reviewable rather than a number nobody can act on. That is more
//!    than a third of the sites, and it is the honest ceiling on this gate:
//!    a name that only ever exists as a runtime value is out of its reach.
//! 2. **Names built by concatenation.** Nothing in the tree does this today; if
//!    something starts to, this gate will class it as unresolved rather than
//!    catch it.
//! 3. **Non-`&str` consts.** Const resolution handles `const X: &str = "…";` and
//!    `const X: &str = RuntimeAgentEventType::V.as_str();`. A name reached
//!    through a `static`, a `LazyLock`, or a function call is unresolved.
//!
//! # Why it cannot pass vacuously
//!
//! The failure mode this repo has been bitten by is a source-scan gate that
//! matched a string nobody writes and was green for months. Four guards, and
//! each one fails loudly rather than quietly finding nothing:
//!
//! * a floor on the number of anchor sites found at all;
//! * a floor on the number of names resolved to literals;
//! * a **probe per resolution leg** — one known name that must come back
//!   through each of the literal, the const, and the typed-const paths, so a
//!   leg that silently stops working takes the test with it;
//! * an **exact-match ratchet** on the known-gap list below, so an entry that
//!   gets fixed must be deleted (the list can only shrink) and the list cannot
//!   rot into a parking lot.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use magician::magician_v2::realtime_events::lookup_agent_event_taxonomy;

#[test]
fn local_transcript_telemetry_names_have_operator_taxonomy() {
    use magician::magician_v2::realtime_events::{EventCategory, EventSeverity};

    for (name, expected_severity) in [
        ("media.voice.local_transcript.state", EventSeverity::Info),
        ("media.voice.local_transcript.queue", EventSeverity::Warn),
        ("media.voice.local_transcript.turn", EventSeverity::Info),
        ("media.voice.local_transcript.fallback", EventSeverity::Warn),
    ] {
        let taxonomy = lookup_agent_event_taxonomy(name)
            .unwrap_or_else(|| panic!("{name} must be operator-classified"));
        assert_eq!(taxonomy.category, EventCategory::Media);
        assert_eq!(taxonomy.severity, expected_severity);
        assert!(!taxonomy.user_relevant);
    }
}

/// `(anchor identifier, zero-based index of the event-name argument)`.
///
/// `emit_named` is a method, so its receiver is not an argument and the name is
/// at 0. The other four are free functions whose leading arguments are the
/// context/address the record is stamped with.
const ANCHORS: &[(&str, usize)] = &[
    ("emit_named", 0),
    ("journal_and_emit_named", 4),
    ("journal_and_emit_named_at", 3),
    ("named_event", 2),
    ("named_event_measured", 2),
];

/// Names this gate found already emitted without a taxonomy row on the day it
/// was written — the same defect as `loop.outbox.gap`, in code this change did
/// not touch.
///
/// **This list is asserted to match the violation set EXACTLY, not to contain
/// it.** Registering one of these without deleting its line here fails the test
/// just as loudly as adding a new unregistered name does. That is deliberate:
/// an allowlist that may only shrink is a debt register; one that may merely be
/// appended to is the gate quietly turning itself off.
///
/// Intentionally empty after the media lifecycle names were classified. Keep
/// the exact-match ratchet: a future temporary exception must be named here,
/// and deleting its taxonomy debt must delete the exception in the same change.
const KNOWN_UNREGISTERED: &[&str] = &[];

/// One resolved name and where it was found, so a failure names a file rather
/// than a string.
#[derive(Debug, Clone)]
struct NameSite {
    name: String,
    file: String,
    line: usize,
    anchor: &'static str,
    /// Which resolution leg produced it — used by the per-leg probes.
    leg: Leg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    /// The argument was a string literal at the call site.
    Literal,
    /// The argument was a `SCREAMING_SNAKE` const whose value is a literal.
    Const,
}

/// An anchor whose name argument this scan could not reduce to a string.
#[derive(Debug, Clone)]
struct UnresolvedSite {
    expression: String,
    file: String,
    line: usize,
    anchor: &'static str,
}

// ─── Workspace walking ───────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the magician crate must sit inside the workspace root")
        .to_path_buf()
}

/// Every workspace member's `src` directory, read out of the root manifest.
///
/// Read rather than hardcoded so a crate added later is covered the day it is
/// added. A hardcoded list is the other way this kind of gate goes quiet: the
/// scan keeps passing while the code it was supposed to watch moves house.
fn member_src_dirs() -> Vec<PathBuf> {
    let root = workspace_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml"))
        .expect("the workspace manifest must be readable");
    let start = manifest
        .find("members = [")
        .expect("the workspace manifest must declare `members = [`");
    let rest = &manifest[start..];
    let end = rest.find(']').expect("the `members` array must be closed");
    let members: Vec<&str> = rest[..end]
        .match_indices('"')
        .map(|(i, _)| i)
        .collect::<Vec<_>>()
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| &rest[pair[0] + 1..pair[1]])
        .collect();

    assert!(
        members.len() >= 10 && members.contains(&"magician"),
        "parsed {} workspace members and `magician` present = {}: the manifest's \
         `members` array did not parse, so this gate would have scanned nothing. \
         Fix the parse rather than the assertion.",
        members.len(),
        members.contains(&"magician"),
    );

    members
        .iter()
        .map(|member| root.join(member).join("src"))
        .filter(|dir| dir.is_dir())
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

// ─── A lexer, because an anchor inside a comment is not an emit ──────────────

/// A byte-for-byte mask over `bytes`: `true` where the byte is code.
///
/// Needed and not fussiness. `emit_named(...)` appears in prose in
/// `realtime_events.rs`'s own docs, `[`JournalAppend::named_event`]` appears in
/// dozens of doc links, and this very file writes the anchor identifiers out as
/// string literals in `ANCHORS`. Searching raw text finds all of those and
/// parses garbage out of them.
fn code_mask(bytes: &[u8]) -> Vec<bool> {
    let n = bytes.len();
    let mut mask = vec![false; n];
    let mut i = 0usize;
    while i < n {
        let c = bytes[i];
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == b'r'
            && i + 1 < n
            && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#')
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && bytes[j] == b'"' {
                j += 1;
                let mut close = Vec::with_capacity(hashes + 1);
                close.push(b'"');
                close.extend(std::iter::repeat_n(b'#', hashes));
                let mut k = j;
                while k < n && !bytes[k..].starts_with(&close) {
                    k += 1;
                }
                i = if k < n { k + close.len() } else { n };
            } else {
                mask[i] = true;
                i += 1;
            }
        } else if c == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if c == b'\'' {
            // A char literal, or a lifetime. `'a` and `'static` are code;
            // `'x'` and `'\n'` are not.
            if i + 1 < n && bytes[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < n && bytes[j] != b'\'' {
                    j += 1;
                }
                i = j + 1;
            } else if i + 2 < n && bytes[i + 2] == b'\'' {
                i += 3;
            } else {
                mask[i] = true;
                i += 1;
            }
        } else {
            mask[i] = true;
            i += 1;
        }
    }
    mask
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Split a call's argument list into byte ranges, one per depth-0 argument.
///
/// Paren/bracket/brace aware and string aware, so `serde_json::json!({ … })`
/// and `RenderHint::new(a, b)` do not split at their inner commas.
fn split_args(bytes: &[u8], open_paren: usize) -> Vec<(usize, usize)> {
    let n = bytes.len();
    let mut i = open_paren + 1;
    let mut depth = 0i32;
    let mut args = Vec::new();
    let mut start = i;
    while i < n {
        let c = bytes[i];
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let mut d = 1usize;
            i += 2;
            while i < n && d > 0 {
                if bytes[i..].starts_with(b"/*") {
                    d += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    d -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if c == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == b'(' || c == b'[' || c == b'{' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == b')' || c == b']' || c == b'}' {
            if depth == 0 && c == b')' {
                args.push((start, i));
                return args;
            }
            depth -= 1;
            i += 1;
            continue;
        }
        if c == b',' && depth == 0 {
            args.push((start, i));
            i += 1;
            start = i;
            continue;
        }
        i += 1;
    }
    args
}

/// `true` when the identifier at `at` is a **definition** rather than a call.
fn is_definition(bytes: &[u8], at: usize) -> bool {
    let mut j = at;
    while j > 0 && (bytes[j - 1] as char).is_whitespace() {
        j -= 1;
    }
    j >= 2 && &bytes[j - 2..j] == b"fn"
}

/// The text of a `const NAME: &str = …;` right-hand side, for every such const
/// in the scanned tree.
fn const_values(sources: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for text in sources.values() {
        let bytes = text.as_bytes();
        let mask = code_mask(bytes);
        for (idx, _) in text.match_indices("const ") {
            if !mask[idx] {
                continue;
            }
            let rest = &text[idx + "const ".len()..];
            let Some(colon) = rest.find(':') else {
                continue;
            };
            let ident = rest[..colon].trim();
            if ident.is_empty()
                || !ident
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || !ident.starts_with(|c: char| c.is_ascii_uppercase())
            {
                continue;
            }
            let after_colon = &rest[colon + 1..];
            let Some(eq) = after_colon.find('=') else {
                continue;
            };
            let ty = after_colon[..eq].trim();
            if ty != "&str" && ty != "&'static str" {
                continue;
            }
            let value_region = &after_colon[eq + 1..];
            let Some(semi) = value_region.find(';') else {
                continue;
            };
            out.insert(ident.to_string(), value_region[..semi].trim().to_string());
        }
    }
    out
}

/// Everything a `&str` literal token means once it is unquoted, or `None` when
/// the expression is not a plain literal.
fn as_string_literal(expression: &str) -> Option<String> {
    let trimmed = expression.trim();
    let inner = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('"') || inner.contains('\\') {
        return None;
    }
    Some(inner.to_string())
}

fn is_typed_variant_expression(expression: &str) -> bool {
    expression.contains("RuntimeAgentEventType::") && expression.contains(".as_str()")
}

/// The whole scan, in one pass, returning what it resolved and what it did not.
fn scan() -> (Vec<NameSite>, Vec<UnresolvedSite>, Vec<String>, usize) {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in member_src_dirs() {
        rust_files(&dir, &mut files);
    }

    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for path in &files {
        if let Ok(text) = fs::read_to_string(path) {
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            sources.insert(relative, text);
        }
    }

    let consts = const_values(&sources);

    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut typed_sites = Vec::new();
    let mut anchor_sites = 0usize;

    for (file, text) in &sources {
        if !ANCHORS.iter().any(|(anchor, _)| text.contains(anchor)) {
            continue;
        }
        let bytes = text.as_bytes();
        let mask = code_mask(bytes);

        for (anchor, name_index) in ANCHORS {
            let needle = format!("{anchor}(");
            for (idx, _) in text.match_indices(&needle) {
                if !mask[idx] {
                    continue;
                }
                if idx > 0 && is_ident_byte(bytes[idx - 1]) {
                    continue;
                }
                if is_definition(bytes, idx) {
                    continue;
                }
                let args = split_args(bytes, idx + anchor.len());
                let Some((start, end)) = args.get(*name_index).copied() else {
                    continue;
                };
                anchor_sites += 1;
                let expression = String::from_utf8_lossy(&bytes[start..end])
                    .trim()
                    .to_string();
                let line = text[..idx].matches('\n').count() + 1;

                if let Some(name) = as_string_literal(&expression) {
                    resolved.push(NameSite {
                        name,
                        file: file.clone(),
                        line,
                        anchor,
                        leg: Leg::Literal,
                    });
                    continue;
                }
                if is_typed_variant_expression(&expression) {
                    // Safe by construction: every `RuntimeAgentEventType`
                    // variant is proved to have a row by
                    // `gaui_taxonomy_covers_every_runtime_agent_event_type`.
                    // Recorded rather than skipped so the four buckets sum to
                    // `anchor_sites` — an accounting that does not add up is a
                    // scanner losing sites somewhere it does not say.
                    typed_sites.push(expression);
                    continue;
                }
                let is_const_ident = !expression.is_empty()
                    && expression
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    && expression.starts_with(|c: char| c.is_ascii_uppercase());
                if is_const_ident {
                    match consts.get(&expression) {
                        Some(value) if is_typed_variant_expression(value) => {
                            typed_sites.push(expression.clone());
                            continue;
                        },
                        Some(value) => {
                            if let Some(name) = as_string_literal(value) {
                                resolved.push(NameSite {
                                    name,
                                    file: file.clone(),
                                    line,
                                    anchor,
                                    leg: Leg::Const,
                                });
                                continue;
                            }
                        },
                        None => {},
                    }
                }
                unresolved.push(UnresolvedSite {
                    expression,
                    file: file.clone(),
                    line,
                    anchor,
                });
            }
        }
    }

    (resolved, unresolved, typed_sites, anchor_sites)
}

// ─── The gate ────────────────────────────────────────────────────────────────

/// Every event name this codebase hands to the named rail resolves to a
/// `GAUI_EVENT_TAXONOMY` row — or is one of the knowns in
/// [`KNOWN_UNREGISTERED`], which must match the violation set exactly.
///
/// This is the direction that would have caught `loop.outbox.gap` before it
/// shipped: the name was a bare literal reaching `JournalAppend::named_event`,
/// with no row, and no existing gate looks at emit sites at all.
#[test]
fn every_emitted_event_name_resolves_to_a_taxonomy_row() {
    let (resolved, unresolved, typed_sites, anchor_sites) = scan();

    // ── Anti-vacuity, before any conclusion is drawn from the scan ──
    //
    // A scan that found nothing must fail rather than pass. Floors are set
    // well under the counts at the time of writing (65 anchors, 22 names) so
    // ordinary churn does not trip them, and well over zero so a broken
    // matcher does.
    assert!(
        anchor_sites >= 45,
        "found only {anchor_sites} named-rail anchor sites across the workspace. \
         This gate is a source scan, and a source scan that stops matching \
         passes vacuously. Either the anchors in `ANCHORS` were renamed \
         (update them) or the scanner broke (fix it) — do not lower this floor."
    );

    let names: BTreeSet<&str> = resolved.iter().map(|site| site.name.as_str()).collect();
    assert!(
        names.len() >= 15,
        "resolved only {} distinct event names from {anchor_sites} anchor sites: {names:?}. \
         The scan is matching call sites but failing to read their name argument.",
        names.len()
    );

    // Every anchor site lands in exactly one of the three buckets. A scanner
    // that drops sites on the floor would otherwise shrink the violation set
    // without shrinking any number this test prints — the quietest way for a
    // gate like this to stop working.
    assert_eq!(
        resolved.len() + typed_sites.len() + unresolved.len(),
        anchor_sites,
        "the scan's buckets do not account for every anchor site: {} resolved + \
         {} typed + {} unresolved != {anchor_sites}",
        resolved.len(),
        typed_sites.len(),
        unresolved.len(),
    );

    // One probe per resolution leg. Each names a real emit site that exists
    // today; if a leg silently stops working its probe goes missing and this
    // fails instead of the gate going quiet.
    let literal_names: BTreeSet<&str> = resolved
        .iter()
        .filter(|site| site.leg == Leg::Literal)
        .map(|site| site.name.as_str())
        .collect();
    let const_names: BTreeSet<&str> = resolved
        .iter()
        .filter(|site| site.leg == Leg::Const)
        .map(|site| site.name.as_str())
        .collect();
    let anchors_hit: BTreeSet<&str> = resolved.iter().map(|site| site.anchor).collect();

    for probe in [
        "reasoning.start",
        "plan.step.finished",
        "tool.result.projected",
    ] {
        assert!(
            literal_names.contains(probe),
            "the string-literal leg did not resolve `{probe}`, which is emitted as a \
             literal in this tree. The scanner is not reading literal arguments."
        );
    }
    assert!(
        const_names.contains("media.voice.session.minted"),
        "the const leg did not resolve `media.voice.session.minted` \
         (`MEDIA_VOICE_SESSION_MINTED` in magician-media). Const resolution is \
         the leg that reaches names spelled once and used elsewhere — exactly \
         the shape `OUTBOX_GAP_EVENT` had — so a broken const leg is this gate \
         missing its own motivating case."
    );
    for anchor in ["emit_named", "named_event"] {
        assert!(
            anchors_hit.contains(anchor),
            "no name resolved through the `{anchor}` anchor. Anchors reached: \
             {anchors_hit:?}"
        );
    }

    // ── The gate itself ──
    let mut unregistered: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for site in &resolved {
        if lookup_agent_event_taxonomy(&site.name).is_none() {
            unregistered
                .entry(site.name.as_str())
                .or_default()
                .push(format!("{}:{} ({})", site.file, site.line, site.anchor));
        }
    }

    let found: BTreeSet<&str> = unregistered.keys().copied().collect();
    let known: BTreeSet<&str> = KNOWN_UNREGISTERED.iter().copied().collect();

    let newly_broken: Vec<&&str> = found.difference(&known).collect();
    assert!(
        newly_broken.is_empty(),
        "These event names are emitted onto the named rail with no \
         `GAUI_EVENT_TAXONOMY` row. Every surface router in \
         `progress_channel_seam::surface_routing` fails CLOSED on an unknown \
         name, so each of these is journalled, projected, and then dropped one \
         step short of a reader — silently.\n\n{}\n\nFix: add a \
         `RuntimeAgentEventType` variant and a `GAUI_EVENT_TAXONOMY` row in \
         `magician-event-taxonomy` (state the render decision at the row — which \
         surfaces should see it, and why), then run \
         `make event-taxonomy-codegen`.",
        newly_broken
            .iter()
            .map(|name| format!("  {name}\n    {}", unregistered[**name].join("\n    ")))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let now_fixed: Vec<&&str> = known.difference(&found).collect();
    assert!(
        now_fixed.is_empty(),
        "These names are listed in `KNOWN_UNREGISTERED` but now have taxonomy \
         rows (or are no longer emitted): {now_fixed:?}. Delete their lines. The \
         list is a debt register that may only shrink — an allowlist that is \
         merely appended to is this gate turning itself off."
    );

    // The typed-variant probe is AFTER the gate, deliberately, and the ordering
    // was chosen by watching this test fail.
    //
    // The floors and the literal/const probes have to run first: each of them
    // guards against the scan going empty, and an empty scan produces an empty
    // violation set that would sail through the assertions above. This one
    // cannot cause a false pass — a typed const the scanner stops recognising
    // lands in `unresolved`, which is noise, never silence — so running it
    // early only bought one thing: when the original defect was reproduced
    // wholesale (the const put back to a bare literal AND the row deleted),
    // this probe fired first and hid the message that names the actual problem.
    // A gate whose loudest failure is about its own bookkeeping teaches the
    // wrong lesson to whoever is reading the output at 2am.
    assert!(
        typed_sites
            .iter()
            .any(|name| name == "RuntimeAgentEventType::MediaPreferencesUpdated.as_str()"),
        "the typed-variant leg did not recognise \
         `RuntimeAgentEventType::MediaPreferencesUpdated.as_str()`. The scanner \
         stopped reading typed event expressions. Found typed variants: \
         {typed_sites:?}"
    );

    // Not an assertion: the forwarded-name residue is real and this gate cannot
    // see through it. Printed — with the sites, not just the count — so a run
    // that is about to be trusted says out loud how much it did not check, and
    // so the list is reviewable rather than a number nobody can act on.
    eprintln!(
        "[emitted-name gate] {anchor_sites} anchor sites = {} resolved + {} \
         typed-by-variant + {} forwarded | those resolved sites carry {} \
         distinct names ({} reached as a literal, {} through a const). The \
         forwarded sites are names this scan CANNOT follow:",
        resolved.len(),
        typed_sites.len(),
        unresolved.len(),
        names.len(),
        literal_names.len(),
        const_names.len(),
    );
    for site in &unresolved {
        eprintln!(
            "    {}:{} {}(… {} …)",
            site.file, site.line, site.anchor, site.expression
        );
    }
}

/// The narrow gate, kept beside the broad one because the broad one is a source
/// scan and this is not.
///
/// If the scanner above ever breaks in a way its own probes do not catch, this
/// still fails for the case that motivated the whole change: the outbox gap
/// marker must resolve to a row, and the routers must stop failing closed on it.
#[test]
fn the_outbox_gap_marker_is_registered_and_no_longer_unknown_to_the_routers() {
    let taxonomy = lookup_agent_event_taxonomy("loop.outbox.gap").expect(
        "`loop.outbox.gap` must have a taxonomy row: without one every surface \
         router answers `Suppress` / `false` on the unknown name and the hole \
         marker reaches no reader at all",
    );
    assert_eq!(
        taxonomy.severity,
        magician::magician_v2::realtime_events::EventSeverity::Warn,
        "the gap marker must be Warn: it is what makes a hole findable by a \
         `severity >= warn` filter on the operator stream. At Info it is \
         indistinguishable from the catch-all an unregistered name already got."
    );
}
