//! The one way memory and recall text is shortened for a prompt.
//!
//! Every bound a recall path applies — an episode's outcome or result, an
//! intent, a tier item, a passage window, a lane share, a whole memory
//! section — goes through these functions, so shortened text always ends at
//! a sentence end or a word boundary (then `…`) and never mid-word, and a
//! section keeps whole entries and its closing tag. Raw character slicing of
//! recall text anywhere else is a bug.

/// The elision mark a shortened text ends with.
pub const ELISION: char = '…';

/// The line a shortened section ends with, before its closing tag.
pub const SECTION_ELISION: &str = "…(more memory omitted)";

/// `value` from its start, at most `max_chars` characters including a
/// closing [`ELISION`] when shortened.
///
/// The kept head ends at its last sentence end (`.`, `!`, `?` or `;` followed
/// by whitespace), else at its last word boundary, whichever keeps at least
/// half of it. Only a head whose second half is one unbroken run (a long
/// identifier, or a script written without spaces) is cut inside it — there
/// is no boundary there to cut at.
pub fn bound_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars < 2 {
        return String::new();
    }
    let head = value.chars().take(max_chars - 1).collect::<String>();
    let sentence_end = head
        .char_indices()
        .filter(|&(at, ch)| {
            matches!(ch, '.' | '!' | '?' | ';')
                && head[at + ch.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(at, ch)| at + ch.len_utf8())
        .last()
        .filter(|&end| end * 2 >= head.len());
    let end = sentence_end
        .or_else(|| {
            head.rfind(char::is_whitespace)
                .filter(|&at| at > 0 && at * 2 >= head.len())
        })
        .unwrap_or(head.len());
    // A kept part that was itself already shortened carries one mark, not two.
    format!(
        "{}{ELISION}",
        head[..end].trim_end().trim_end_matches(ELISION).trim_end()
    )
}

/// Shortest stored text [`repair_dangling_cut`] treats as possibly cut by a
/// legacy writer: those cut only at caps of a few hundred characters or
/// more, and a short text without a final stop is far more likely whole.
const MIN_LEGACY_CUT_CHARS: usize = 160;

/// Longest trailing fragment, in words, that [`repair_dangling_cut`] reads as
/// the stub of a cut sentence rather than a sentence written without a stop.
const MAX_DANGLING_FRAGMENT_WORDS: usize = 3;

/// `value` with a legacy mid-word cut at its end repaired for display.
///
/// Writers before [`bound_text`] cut stored text at a character count —
/// mid-word, mid-sentence — and marked it `...` (or not at all), so an old
/// record can end `… It a`. Read paths repair that without rewriting the
/// record: a text ending in a word glued to a legacy `...` loses that
/// possibly partial word, and a text ending in a short unterminated stub after
/// its last sentence (`Done. It a`) loses the stub; either then ends at the
/// sentence end or word boundary [`bound_text`] would choose, marked with
/// [`ELISION`]. Text that already ends on a stop, a closing mark or `…`, short
/// text, and text with no earlier boundary are returned unchanged, so the
/// repair is idempotent and never touches what [`bound_text`] produced.
pub fn repair_dangling_cut(value: &str) -> String {
    let text = value.trim_end();
    if text.chars().count() < MIN_LEGACY_CUT_CHARS {
        return value.to_string();
    }
    let repaired = if let Some(head) = text.strip_suffix("...") {
        // A legacy marker glued to a word: that word may be partial.
        if !head.chars().last().is_some_and(char::is_alphanumeric) {
            return value.to_string();
        }
        let Some(word_start) = head.rfind(char::is_whitespace).filter(|&at| at > 0) else {
            return value.to_string();
        };
        // Ends on a word boundary already; a sentence end keeping at least
        // half of it is preferred, as in `bound_text`.
        let kept = &head[..word_start];
        let end = last_sentence_end(kept)
            .filter(|&end| end * 2 >= kept.len())
            .unwrap_or(kept.len());
        &kept[..end]
    } else {
        if !text.chars().last().is_some_and(char::is_alphanumeric) {
            return value.to_string();
        }
        let Some(sentence_end) = last_sentence_end(text) else {
            return value.to_string();
        };
        let fragment = text[sentence_end..].trim();
        let words = fragment.split_whitespace().count();
        let list_item = fragment.starts_with(['-', '*', '#', '>'])
            || fragment.split_once('.').is_some_and(|(lead, _)| {
                !lead.is_empty() && lead.chars().all(|c| c.is_ascii_digit())
            });
        if words == 0 || words > MAX_DANGLING_FRAGMENT_WORDS || list_item {
            return value.to_string();
        }
        &text[..sentence_end]
    };
    let repaired = repaired.trim_end().trim_end_matches(ELISION).trim_end();
    if repaired.is_empty() {
        return value.to_string();
    }
    format!("{repaired}{ELISION}")
}

/// Byte offset just past the last sentence end in `text` (`.`, `!`, `?` or
/// `;` followed by whitespace or the end).
fn last_sentence_end(text: &str) -> Option<usize> {
    text.char_indices()
        .filter(|&(at, ch)| {
            matches!(ch, '.' | '!' | '?' | ';')
                && text[at + ch.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(at, ch)| at + ch.len_utf8())
        .last()
}

/// The farthest a window edge moves to reach a word boundary. Text written
/// without spaces has none nearby, and its edge stays where it was.
const MAX_WINDOW_ALIGN_SHIFT: usize = 32;

/// The window `start..end` (character indices into `chars`) narrowed to whole
/// words: a start inside a word moves to the next word's start, an end inside
/// a word moves back to the previous word's end — each by at most
/// [`MAX_WINDOW_ALIGN_SHIFT`]. A window holding no whole word is returned
/// unchanged, since narrowing it would leave nothing.
pub fn word_aligned_window(chars: &[char], start: usize, end: usize) -> (usize, usize) {
    let end = end.min(chars.len());
    let start = start.min(end);
    let inside = |at: usize| {
        at > 0 && at < chars.len() && !chars[at - 1].is_whitespace() && !chars[at].is_whitespace()
    };
    let mut aligned_start = start;
    if inside(start) {
        while aligned_start < end && !chars[aligned_start].is_whitespace() {
            aligned_start += 1;
        }
        if aligned_start - start > MAX_WINDOW_ALIGN_SHIFT {
            aligned_start = start;
        }
    }
    while aligned_start < end && chars[aligned_start].is_whitespace() {
        aligned_start += 1;
    }
    let mut aligned_end = end;
    if inside(end) {
        while aligned_end > aligned_start && !chars[aligned_end - 1].is_whitespace() {
            aligned_end -= 1;
        }
        if end - aligned_end > MAX_WINDOW_ALIGN_SHIFT {
            aligned_end = end;
        }
    }
    while aligned_end > aligned_start && chars[aligned_end - 1].is_whitespace() {
        aligned_end -= 1;
    }
    if aligned_start >= aligned_end {
        (start, end)
    } else {
        (aligned_start, aligned_end)
    }
}

/// A rendered memory section bounded to `max_chars` characters by whole
/// entries.
///
/// A section is a heading, entries separated by blank lines, and (usually) a
/// closing tag line such as `</agent_memory>`. Entries are kept in order
/// while they fit; the first that does not is shortened with [`bound_text`]
/// when a meaningful part of it fits, then [`SECTION_ELISION`] marks the
/// rest as omitted and the closing tag is kept, so the section stays well
/// formed.
pub fn bound_section(section: &str, max_chars: usize) -> String {
    let section = section.trim();
    if section.chars().count() <= max_chars {
        return section.to_string();
    }
    let (body, closing) = match section.rsplit_once('\n') {
        Some((body, last)) if last.trim_start().starts_with("</") => (body, Some(last)),
        _ => (section, None),
    };
    let reserve = SECTION_ELISION.chars().count()
        + 1
        + closing.map_or(0, |closing| closing.chars().count() + 1);
    let budget = max_chars.saturating_sub(reserve);
    let mut kept = String::new();
    let mut kept_chars = 0usize;
    for block in body.split("\n\n") {
        let separator = if kept.is_empty() { 0 } else { 2 };
        let block_chars = block.chars().count();
        if kept_chars + separator + block_chars <= budget {
            if separator > 0 {
                kept.push_str("\n\n");
            }
            kept.push_str(block);
            kept_chars += separator + block_chars;
            continue;
        }
        let room = budget.saturating_sub(kept_chars + separator);
        if room >= 80 {
            let shortened = bound_text(block, room);
            if !shortened.is_empty() {
                if separator > 0 {
                    kept.push_str("\n\n");
                }
                kept.push_str(&shortened);
            }
        }
        break;
    }
    let mut out = kept.trim_end().to_string();
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(SECTION_ELISION);
    if let Some(closing) = closing {
        out.push('\n');
        out.push_str(closing.trim());
    }
    out
}

/// Whether `text` ends where a bound may end it: unshortened, or with
/// [`ELISION`] right after a token that is a whole word of `source` (trailing
/// punctuation allowed). For tests of every recall path.
pub fn ends_on_boundary(text: &str, source: &str) -> bool {
    let Some(head) = text.trim_end().strip_suffix(ELISION) else {
        return true;
    };
    let Some(last) = head.split_whitespace().last() else {
        return true;
    };
    source.split_whitespace().any(|word| {
        word == last
            || word
                .strip_prefix(last)
                .is_some_and(|rest| rest.chars().all(|ch| !ch.is_alphanumeric()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_text_ends_at_a_sentence_or_word_boundary() {
        let line = "label (as of 2026-10-19): First sentence here. Second sentence follows";
        assert_eq!(bound_text(line, 500), line);
        assert_eq!(
            bound_text(line, 60),
            "label (as of 2026-10-19): First sentence here.…"
        );
        assert_eq!(
            bound_text("label: alpha beta gamma delta", 20),
            "label: alpha beta…"
        );
        assert_eq!(bound_text(&"x".repeat(10), 5), "xxxx…");
        assert_eq!(bound_text("anything", 1), "");
        // From the first size whose head holds a word boundary.
        for max in 7..80 {
            let bounded = bound_text(line, max);
            assert!(bounded.chars().count() <= max, "{max}: {bounded}");
            assert!(ends_on_boundary(&bounded, line), "{max}: {bounded:?}");
        }
    }

    #[test]
    fn legacy_mid_word_cuts_are_repaired_at_read_time() {
        let lead = "The run opened the project, read NOTES.md and confirmed the tracked items \
                    match the request. It verified the build and the checks passed cleanly. \
                    Then it wrote a short summary for the owner. ";
        assert!(lead.chars().count() >= MIN_LEGACY_CUT_CHARS);
        // A stub cut after the last sentence, with no marker.
        let stub = format!("{lead}It a");
        let repaired = repair_dangling_cut(&stub);
        assert_eq!(repaired, format!("{}{ELISION}", lead.trim_end()));
        // A word glued to the legacy `...` marker may be partial; the rest
        // ends where `bound_text` would end it (here, the last sentence).
        let marked = format!("{lead}It also upda...");
        let repaired = repair_dangling_cut(&marked);
        assert_eq!(repaired, format!("{}{ELISION}", lead.trim_end()));
        let one_sentence = format!("{}It also upda...", lead.replace(". ", ", "));
        let repaired_words = repair_dangling_cut(&one_sentence);
        assert!(
            repaired_words.ends_with(&format!("It also{ELISION}")),
            "{repaired_words}"
        );
        // Idempotent, and bound output is never touched.
        assert_eq!(repair_dangling_cut(&repaired), repaired);
        let bounded = bound_text(&stub, 120);
        assert_eq!(repair_dangling_cut(&bounded), bounded);
        // Whole text, a final stop, a list item, a long last sentence, short text.
        for whole in [
            format!("{lead}It also passed."),
            format!("{lead}\n- verify the build"),
            format!("{lead}Then it reported the final status to the owner"),
            "Done. It a".to_string(),
        ] {
            assert_eq!(repair_dangling_cut(&whole), whole, "{whole}");
        }
    }

    #[test]
    fn windows_align_to_whole_words() {
        let chars = "alpha beta gamma delta".chars().collect::<Vec<_>>();
        let (start, end) = word_aligned_window(&chars, 2, 13);
        assert_eq!(chars[start..end].iter().collect::<String>(), "beta");
        let (start, end) = word_aligned_window(&chars, 0, chars.len());
        assert_eq!((start, end), (0, chars.len()));
        // A window inside one word stays as it is: there is no whole word.
        assert_eq!(word_aligned_window(&chars, 1, 3), (1, 3));
    }

    #[test]
    fn a_section_keeps_whole_entries_and_its_closing_tag() {
        let section = "## AGENT MEMORY\n<agent_memory>\n### Agent Context\n[a]\nFirst entry is \
                       short.\n\n[b]\nSecond entry runs on with many words to fill the space \
                       available.\n\n[c]\nThird entry.\n</agent_memory>";
        assert_eq!(bound_section(section, 10_000), section);
        let bounded = bound_section(section, 140);
        assert!(bounded.chars().count() <= 140, "{bounded}");
        assert!(bounded.contains("First entry is short."), "{bounded}");
        assert!(bounded.ends_with(&format!("{SECTION_ELISION}\n</agent_memory>")));
        assert!(!bounded.contains("Third entry"), "{bounded}");
        for block in bounded.split("\n\n") {
            assert!(ends_on_boundary(
                block.lines().last().unwrap_or(""),
                section
            ));
        }
    }
}
