//! Renders the screens into a buffer and reads them back.
//!
//! The interactive screen shipped with a cursor that could not move, and
//! nothing caught it because nothing had ever drawn it. A terminal is needed to
//! *watch* a screen, not to render one, so these draw the real widgets and
//! assert on the result.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::install_mode::{InstallMode, Offer, OfferState};

fn offers() -> Vec<Offer> {
    // The shape of this machine today, and the shape that broke: exactly one
    // choice is takeable.
    vec![
        Offer {
            mode: InstallMode::Prebuilt,
            label: "Prebuilt",
            detail: "Install what is already built.".into(),
            state: OfferState::Unavailable("no published release for macos yet".into()),
        },
        Offer {
            mode: InstallMode::FromSource,
            label: "From source",
            detail: "Installs what is missing, then builds.".into(),
            state: OfferState::Ready,
        },
        Offer {
            mode: InstallMode::Container,
            label: "Container",
            detail: "The whole runtime in one image.".into(),
            state: OfferState::Unavailable("coming soon".into()),
        },
    ]
}

fn render(cursor: usize, refused: Option<&str>) -> String {
    // A realistic window: the screens are full-screen now, not a band.
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test backend");
    terminal
        .draw(|frame| super::draw_modes(frame, &offers(), cursor, refused))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Not an assertion — prints the screen so a person can read it. `cargo test
/// -- --nocapture the_screen_as_drawn` is the closest thing to looking at it
/// without a terminal.
#[test]
fn the_screen_as_drawn() {
    println!("\n{}\n", render(1, None));
    println!(
        "{}\n",
        render(0, Some("Prebuilt — no published release for macos yet"))
    );
}

/// The welcome screen against a real probe of this machine — the right pane is
/// only worth having if it is live, so this renders it for real.
#[test]
fn the_welcome_as_drawn() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let paths = magician_components::loader::Paths::from_env();
    let graph = magician_components::loader::try_load(&paths).expect("graph");
    let observed = runtime.block_on(magician_components::probe::observe_all(&graph.components));
    let selection = crate::model::Selection::with_remembered(graph, observed, |_| None);

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test backend");
    terminal
        .draw(|frame| super::draw_welcome(frame, &selection))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let screen = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    println!("\n{screen}\n");
    assert!(screen.contains("Welcome"), "{screen}");
    assert!(
        screen.contains("more power in your own hands"),
        "the thesis: {screen}"
    );
    assert!(
        screen.contains("already work"),
        "the live summary: {screen}"
    );
}

#[test]
fn the_wordmark_is_drawn_large_and_gives_up_when_it_cannot_be() {
    // Five rows of block characters is the only way to be large in a terminal.
    // The width guard matters more than the banner: a wordmark that wraps is
    // worse than no wordmark, so a narrow window gets plain text instead.
    let wide = crate::theme::wordmark(100);
    assert_eq!(wide.len(), 5, "five rows or nothing");
    assert!(
        wide.iter().all(|r| r.contains('█')),
        "drawn in blocks: {wide:?}"
    );
    assert!(wide[0].len() >= 40, "large enough to read as a wordmark");

    assert!(
        crate::theme::wordmark(20).is_empty(),
        "too narrow: give up rather than wrap"
    );
    assert!(
        crate::theme::wordmark(41).is_empty(),
        "one column short is still short"
    );
    assert!(
        !crate::theme::wordmark(42).is_empty(),
        "exactly enough is enough"
    );
}

#[test]
fn the_first_screen_says_what_this_is() {
    // It is the only screen that introduces the product; after it the person
    // knows what they are installing.
    let screen = render(1, None);
    assert!(screen.contains("Magican"), "no name:\n{screen}");
    assert!(
        screen.contains("Superpowers for Work"),
        "no tagline:\n{screen}"
    );
}

#[test]
fn the_keys_that_work_are_written_on_the_screen() {
    // The reported failure was "arrow keys not working". They were being read;
    // the cursor refused to move. Either way the screen never said what to press.
    let screen = render(1, None);
    for hint in ["up/down", "j/k", "enter", "q"] {
        assert!(screen.contains(hint), "{hint} missing from:\n{screen}");
    }
}

#[test]
fn every_choice_is_shown_and_the_unavailable_ones_say_why() {
    let screen = render(1, None);
    for label in ["Prebuilt", "From source", "Container"] {
        assert!(screen.contains(label), "{label} missing:\n{screen}");
    }
    assert!(
        screen.contains("no published release"),
        "reason missing:\n{screen}"
    );
    assert!(screen.contains("coming soon"), "reason missing:\n{screen}");
}

#[test]
fn the_cursor_reaches_a_row_that_cannot_be_chosen() {
    // The bug: `step` only landed on selectable rows, so with one selectable
    // row every key press moved nowhere and the screen looked frozen.
    let offers = offers();
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = 1;
    for _ in 0..offers.len() {
        cursor = super::step(&offers, cursor, 1);
        seen.insert(cursor);
    }
    assert_eq!(
        seen.len(),
        offers.len(),
        "the cursor must reach every row, got {seen:?}"
    );
}

#[test]
fn pressing_enter_on_a_greyed_row_explains_rather_than_ignoring() {
    let screen = render(0, Some("Prebuilt — no published release for macos yet"));
    assert!(
        screen.contains("Cannot use Prebuilt"),
        "no refusal shown:\n{screen}"
    );
}
