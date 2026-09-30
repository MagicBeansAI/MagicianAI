//! One place for every colour and glyph, so the three UI modes agree.
//!
//! Semantic colour is separate from emphasis: green, red and yellow mean
//! present, missing and unknown, and nothing else uses them. Emphasis is
//! weight and reversal. A terminal that renders sixteen colours badly still
//! reads correctly, because state is carried by the glyph as well as the hue.

use ratatui::style::{Color, Modifier, Style};

pub const OK: Color = Color::Green;
pub const FAULT: Color = Color::Red;
pub const UNKNOWN: Color = Color::Yellow;
pub const MUTED: Color = Color::DarkGray;
pub const ACCENT: Color = Color::Cyan;

pub fn dim() -> Style {
    Style::default().fg(MUTED)
}

pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn on(color: Color) -> Style {
    Style::default().fg(color)
}

pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// The name, and the line that ships on the landing page. One place, because a
/// tagline that differs between the site and the installer reads as two
/// products.
pub const NAME: &str = "Magican";
pub const TAGLINE: &str = "Superpowers for Work, Play, and all your side quests";
/// The manifesto's closing line, which is the argument for the whole thing.
pub const THESIS: &str =
    "Not another intelligence at the heart of your life — more power in your own hands.";

pub fn brand() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// A terminal has one font at one size, so hierarchy is weight, colour and
/// space — there is no heading size to reach for. Sub-headings are the accent
/// without the bold, which reads as a step down rather than as another title.
pub fn heading() -> Style {
    Style::default().fg(ACCENT)
}

/// The wordmark, drawn in block characters because a terminal has one font at
/// one size and this is the only way to be large in it.
///
/// Its own colour rather than the semantic yellow: the rule below is that
/// green, red and yellow mean present, missing and unknown. A five-row banner
/// is not a state glyph, but reusing the exact hue would still weaken the rule,
/// so the brand gets a warmer one of its own.
pub const BANNER: Color = Color::LightYellow;

/// Five rows per letter, five columns wide. Data rather than a dependency —
/// seven letters do not justify a figlet crate, and a table can be read.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'M' => ["█   █", "██ ██", "█ █ █", "█   █", "█   █"],
        'A' => [" ███ ", "█   █", "█████", "█   █", "█   █"],
        'G' => [" ████", "█    ", "█  ██", "█   █", " ████"],
        'I' => ["█████", "  █  ", "  █  ", "  █  ", "█████"],
        'C' => [" ████", "█    ", "█    ", "█    ", " ████"],
        'N' => ["█   █", "██  █", "█ █ █", "█  ██", "█   █"],
        _ => ["     ", "     ", "     ", "     ", "     "],
    }
}

/// The name as five lines of block characters.
///
/// Returns nothing when the width cannot hold it — a wordmark that wraps is
/// worse than no wordmark, and the caller falls back to plain text.
pub fn wordmark(available: u16) -> Vec<String> {
    let letters: Vec<[&str; 5]> = NAME.to_uppercase().chars().map(glyph).collect();
    let width = letters.len() * 6;
    if letters.is_empty() || width as u16 > available {
        return Vec::new();
    }
    (0..5)
        .map(|row| {
            letters
                .iter()
                .map(|g| g[row])
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end()
                .to_string()
        })
        .collect()
}

/// State marks. Deliberately ASCII: a wizard runs over ssh, in CI logs and in
/// terminals with no font fallback, where a box-drawing glyph becomes a
/// question mark and the meaning is lost.
pub const MARK_OK: &str = "[+]";
pub const MARK_MISSING: &str = "[!]";
pub const MARK_ABSENT: &str = "[ ]";
pub const MARK_UNKNOWN: &str = "[?]";
pub const MARK_DECLINED: &str = "[-]";
