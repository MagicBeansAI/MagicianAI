//! The screen, and the two ways of doing without one.
//!
//! # Full screen, bordered panels
//!
//! `ratatui::try_init` takes the alternate screen, and `try_restore` gives it
//! back. The selection is a full-screen layout — a title bar, a list of
//! capabilities, and a panel beside it that updates as the cursor moves to show
//! what the current selection would install and what it leaves off.
//!
//! This reverses the original design, which drew in an inline band at the
//! cursor so a long build could stream past it. Two things made that the wrong
//! trade. The plan run happens *after* this screen exits, printing to a
//! restored terminal, so nothing streams through the band that was built to
//! accommodate it; and a band is not enough room to show a consequence beside
//! the choice that causes it, which is the one thing this screen exists for.
//! The superseded argument is kept in
//! `docs/components/magician-setup/README.md` rather than here, because a file
//! should describe what it does.
//!
//! # Three ways in, one model
//!
//! Rich draws; plain prints; non-interactive prints and exits. All three read
//! the same [`Selection`], so the answer never depends on which one ran.

use std::io;

use magician_components::{Availability, ComponentState};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::model::Selection;
use crate::theme;

pub enum Outcome {
    /// Accepted, with the path chosen on the first screen. The capability plan
    /// is the same either way; the mode only decides where the binaries come
    /// from.
    Confirmed(crate::install_mode::InstallMode),
    Cancelled,
}

/// Whether drawing a screen would reach a person. Both halves matter: a pipe
/// has no reader, and an explicit opt-out is how CI asks for the quiet path.
pub fn interactive_possible() -> bool {
    if std::env::var("MAGICIAN_WIZARD_NONINTERACTIVE").is_ok_and(|v| v != "0") {
        return false;
    }
    if !std::io::IsTerminal::is_terminal(&io::stdout())
        || !std::io::IsTerminal::is_terminal(&io::stdin())
    {
        return false;
    }
    // Two bordered panes and a title bar need room. Drawing them into a window
    // this small produces something unreadable, and the printed status says the
    // same things in a form that fits anywhere.
    match ratatui::crossterm::terminal::size() {
        Ok((w, h)) => w >= MIN_WIDTH && h >= MIN_HEIGHT,
        Err(_) => false,
    }
}

/// Panels want room. An inline band tall enough for two bordered panes and a
/// title bar is most of a window anyway, so this takes the whole one and hands
/// it back on exit — with a printed summary, so leaving the screen does not
/// mean losing what it said.
const MIN_HEIGHT: u16 = 16;
const MIN_WIDTH: u16 = 72;

/// The screen before the questions.
///
/// It exists because the first thing someone met was "How should this be
/// installed?" — a question asked before anything had introduced itself. This
/// says what the thing is, what it is about to do, and what it already found on
/// the machine, which is also the first evidence that it looked.
fn welcome(terminal: &mut ratatui::DefaultTerminal, selection: &Selection) -> io::Result<bool> {
    loop {
        terminal.draw(|frame| draw_welcome(frame, selection))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
            KeyCode::Enter | KeyCode::Char(' ') => return Ok(true),
            _ => {},
        }
    }
}

fn draw_welcome(frame: &mut Frame, selection: &Selection) {
    // The welcome gets a taller head than the other screens: it is the one
    // place a wordmark is worth the rows it costs, and the only screen someone
    // sees before deciding whether to trust this with their machine.
    let banner = theme::wordmark(frame.area().width.saturating_sub(4));
    let head_height = if banner.is_empty() {
        3
    } else {
        banner.len() as u16 + 4
    };
    let [head, body, footer] = Layout::vertical([
        Constraint::Length(head_height),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let mut head_lines: Vec<Line> = banner
        .into_iter()
        .map(|row| Line::styled(row, theme::on(theme::BANNER)))
        .collect();
    if head_lines.is_empty() {
        // Narrow terminal: the name in plain text rather than a wrapped banner.
        head_lines.push(Line::styled(theme::NAME, theme::brand()));
    }
    head_lines.push(Line::raw(""));
    head_lines.push(Line::styled(theme::TAGLINE, theme::dim()));
    frame.render_widget(
        Paragraph::new(head_lines).block(
            Block::bordered()
                .border_style(theme::on(theme::ACCENT))
                .padding(Padding::horizontal(1)),
        ),
        head,
    );
    frame.render_widget(
        Paragraph::new(Line::styled("enter to begin · q to leave", theme::dim())).block(
            Block::bordered()
                .title(" Keys ")
                .padding(Padding::horizontal(1)),
        ),
        footer,
    );

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);

    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::styled(theme::THESIS, theme::heading()),
            Line::raw(""),
            Line::styled(
                "This sets up Magican on this machine: the runtime, and whichever capabilities you want it to have.",
                theme::dim(),
            ),
            Line::raw(""),
            Line::styled(
                "You pick what it should be able to do. It works out what that needs, in what order, and what declining any of it costs you.",
                theme::dim(),
            ),
            Line::raw(""),
            Line::styled("Nothing is installed without asking first.", theme::on(theme::OK)),
        ])
        .block(Block::bordered().title(" Welcome ").padding(Padding::horizontal(1)))
        .wrap(Wrap { trim: true }),
        left,
    );

    // Live, not decorative. The point of showing it here is that the wizard has
    // already looked, so the first screen is evidence rather than a splash.
    let report = selection.report();
    let working = report
        .features
        .iter()
        .filter(|f| f.availability.is_available())
        .count();
    let present = report
        .components
        .iter()
        .filter(|c| c.state.usable())
        .count();

    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{working}"), theme::brand()),
            Span::styled(
                format!(" of {} capabilities already work", report.features.len()),
                theme::dim(),
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{present}"), theme::brand()),
            Span::styled(
                format!(" of {} components are here", report.components.len()),
                theme::dim(),
            ),
        ]),
        Line::raw(""),
    ];
    match crate::install_mode::installed_version() {
        Some(version) => lines.push(Line::from(vec![
            Span::styled(theme::MARK_OK, theme::on(theme::OK)),
            Span::styled(format!(" backend {version} installed"), theme::dim()),
        ])),
        None => lines.push(Line::from(vec![
            Span::styled(theme::MARK_ABSENT, theme::dim()),
            Span::styled(" no backend installed yet", theme::dim()),
        ])),
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Working already:", theme::bold()));
    let mut named = 0;
    for feature in &report.features {
        if feature.availability.is_available() && named < 6 {
            lines.push(Line::from(vec![
                Span::styled(theme::MARK_OK, theme::on(theme::OK)),
                Span::raw(format!(" {}", feature.name)),
            ]));
            named += 1;
        }
    }
    if named == 0 {
        lines.push(Line::styled(
            " nothing yet — that is what this is for",
            theme::dim(),
        ));
    } else if working > named {
        lines.push(Line::styled(
            format!(" and {} more", working - named),
            theme::dim(),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" What is already here ")
                    .padding(Padding::horizontal(1)),
            )
            .wrap(Wrap { trim: true }),
        right,
    );
}

/// How is asked before what, because the answer changes how long the rest
/// takes and whether anything can happen at all.
fn choose_mode(
    terminal: &mut ratatui::DefaultTerminal,
    offers: &[crate::install_mode::Offer],
) -> io::Result<Option<crate::install_mode::InstallMode>> {
    // Start on something takeable, but move over everything. Skipping the
    // greyed rows was worse than a dead first keypress: with one selectable
    // choice — which is this machine today — it made *every* keypress do
    // nothing, and a screen that ignores the arrow keys reads as broken.
    let mut cursor = offers.iter().position(|o| o.state.is_ready()).unwrap_or(0);
    let mut refused: Option<String> = None;
    loop {
        terminal.draw(|frame| draw_modes(frame, offers, cursor, refused.as_deref()))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
            KeyCode::Enter => match &offers[cursor].state {
                crate::install_mode::OfferState::Ready => return Ok(Some(offers[cursor].mode)),
                // Chosen and refused, with the reason, rather than silently
                // ignored — the row is visible, so pressing enter on it is a
                // fair thing to try.
                crate::install_mode::OfferState::Unavailable(why) => {
                    refused = Some(format!("{} — {why}", offers[cursor].label));
                },
            },
            KeyCode::Up | KeyCode::Char('k') => {
                refused = None;
                cursor = step(offers, cursor, -1);
            },
            KeyCode::Down | KeyCode::Char('j') => {
                refused = None;
                cursor = step(offers, cursor, 1);
            },
            _ => {},
        }
    }
}

/// Move one row, wrapping. Every row, selectable or not.
fn step(offers: &[crate::install_mode::Offer], from: usize, delta: isize) -> usize {
    let len = offers.len() as isize;
    if len == 0 {
        return from;
    }
    ((from as isize + delta).rem_euclid(len)) as usize
}

pub fn run(
    selection: &mut Selection,
    offers: &[crate::install_mode::Offer],
) -> io::Result<Outcome> {
    let mut terminal = ratatui::try_init()?;

    if !welcome(&mut terminal, selection)? {
        ratatui::try_restore()?;
        return Ok(Outcome::Cancelled);
    }

    let Some(mode) = choose_mode(&mut terminal, offers)? else {
        ratatui::try_restore()?;
        return Ok(Outcome::Cancelled);
    };

    let outcome = loop {
        terminal.draw(|frame| draw(frame, selection))?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Outcome::Cancelled,
                KeyCode::Enter => break Outcome::Confirmed(mode),
                KeyCode::Char(' ') => selection.toggle_at_cursor(),
                KeyCode::Up | KeyCode::Char('k') => selection.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => selection.move_cursor(1),
                _ => {},
            },
            _ => {},
        }
    };

    ratatui::try_restore()?;
    Ok(outcome)
}

/// The three paths, with the ones this machine cannot take dimmed and carrying
/// their reason. Greying rather than hiding is the point: the reader learns
/// the product has a container path and that it is not ready, which is a
/// different thing from learning nothing.
/// The frame every screen sits in: a title bar carrying the product, and a key
/// bar carrying what to press. Returns the area left for the screen itself.
///
/// Shared rather than repeated, because a title that differs by screen makes
/// the wizard feel like several programs.
fn chrome(frame: &mut Frame, keys: &str, note: Option<Span>) -> ratatui::layout::Rect {
    let [title, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(theme::NAME, theme::brand()),
            Span::styled(format!("{:4}{}", "", theme::TAGLINE), theme::dim()),
        ]))
        .block(
            Block::bordered()
                .border_style(theme::on(theme::ACCENT))
                .padding(Padding::horizontal(1)),
        ),
        title,
    );

    let footer_line = match note {
        Some(span) => Line::from(span),
        None => Line::from(Span::styled(keys, theme::dim())),
    };
    frame.render_widget(
        Paragraph::new(footer_line).block(
            Block::bordered()
                .title(" Keys ")
                .padding(Padding::horizontal(1)),
        ),
        footer,
    );

    body
}

fn draw_modes(
    frame: &mut Frame,
    offers: &[crate::install_mode::Offer],
    cursor: usize,
    refused: Option<&str>,
) {
    use crate::install_mode::OfferState;

    let note =
        refused.map(|why| Span::styled(format!("Cannot use {why}"), theme::on(theme::UNKNOWN)));
    let body = chrome(
        frame,
        "up/down or j/k move · enter chooses · q cancels",
        note,
    );
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body);

    let items: Vec<ListItem> = offers
        .iter()
        .map(|offer| {
            let (mark, style) = match offer.state {
                OfferState::Ready => (theme::MARK_ABSENT, theme::bold()),
                OfferState::Unavailable(_) => (theme::MARK_DECLINED, theme::dim()),
            };
            let mut line = vec![
                Span::styled(format!("{mark} "), theme::dim()),
                Span::styled(offer.label, style),
            ];
            if let OfferState::Unavailable(reason) = &offer.state {
                // Enough to know it is out, not the whole sentence — that is
                // what the pane beside it is for.
                let short = reason.split(" — ").next().unwrap_or(reason);
                let short = short.split(',').next().unwrap_or(short);
                line.push(Span::styled(format!(" {short}"), theme::on(theme::UNKNOWN)));
            }
            ListItem::new(Line::from(line))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::bordered()
                    .title(" How to install ")
                    .padding(Padding::horizontal(1)),
            )
            .highlight_style(theme::selected()),
        left,
        &mut state,
    );

    // The right pane explains whichever row the cursor is on, so the list can
    // stay one line per choice and the reasoning still has somewhere to live.
    let offer = &offers[cursor];
    let mut lines = vec![
        Line::styled(offer.label, theme::heading()),
        Line::raw(""),
        Line::styled(offer.detail.clone(), theme::dim()),
    ];
    match &offer.state {
        OfferState::Ready => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Press enter to take this path.",
                theme::on(theme::OK),
            ));
        },
        OfferState::Unavailable(reason) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Not available here",
                theme::on(theme::UNKNOWN),
            ));
            lines.push(Line::styled(reason.clone(), theme::dim()));
        },
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Everything after this is the same either way — only where the binaries come from differs.",
        theme::dim(),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" What that means ")
                    .padding(Padding::horizontal(1)),
            )
            .wrap(Wrap { trim: true }),
        right,
    );
}

fn draw(frame: &mut Frame, selection: &Selection) {
    let body = chrome(
        frame,
        "up/down move · space toggles · enter accepts · q cancels",
        None,
    );
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(body);

    let plan = selection.plan();

    // Left: the capabilities, each with what it costs you today.
    let report = selection.report();
    let items: Vec<ListItem> = selection
        .features()
        .iter()
        .map(|feature| {
            let wanted = selection.is_wanted(&feature.id);
            let available = report
                .feature(&feature.id)
                .map(|f| f.availability.is_available())
                .unwrap_or(false);
            let mark = if wanted { "[x]" } else { "[ ]" };
            // Already working is worth saying: it is the difference between
            // "this will be installed" and "this is fine".
            let note = if available {
                Span::styled(" working", theme::on(theme::OK))
            } else if wanted {
                Span::styled(" needs setup", theme::on(theme::UNKNOWN))
            } else {
                Span::raw("")
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{mark} {}", feature.name)),
                note,
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selection.cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::bordered()
                    .title(" What do you want this to do? ")
                    .padding(Padding::horizontal(1)),
            )
            .highlight_style(theme::selected()),
        left,
        &mut list_state,
    );

    // Right: the consequence of the current selection, which is the whole
    // reason this is a screen rather than a sequence of prompts.
    let mut lines: Vec<Line> = Vec::new();
    if plan.install.is_empty() {
        lines.push(Line::styled("Nothing to install.", theme::on(theme::OK)));
        lines.push(Line::styled(
            "Everything selected already works here.",
            theme::dim(),
        ));
    } else {
        lines.push(Line::styled(
            format!("Will install {} component(s):", plan.install.len()),
            theme::bold(),
        ));
        for id in &plan.install {
            let component = selection.graph.component(id);
            let name = component.map(|c| c.name.as_str()).unwrap_or(id.as_str());
            lines.push(Line::from(Span::styled(
                format!(" {} {name}", theme::MARK_ABSENT),
                theme::on(theme::ACCENT),
            )));
            if let Some(cost) = component.map(|c| c.cost.as_str()).filter(|c| !c.is_empty()) {
                lines.push(Line::styled(format!(" {cost}"), theme::dim()));
            }
        }
    }
    for requirement in &plan.unresolved {
        lines.push(Line::styled(
            format!(" nothing can provide {requirement}"),
            theme::on(theme::FAULT),
        ));
    }
    if !plan.left_off.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Left off:", theme::bold()));
        for (name, reason) in plan.left_off.iter().take(6) {
            lines.push(Line::styled(format!(" {name} — {reason}"), theme::dim()));
        }
    }

    // The outcome, not the present. "12 of 13 will work" is the sentence
    // someone is actually trying to get to.
    let after = selection.projected_report();
    let working = after
        .features
        .iter()
        .filter(|f| f.availability.is_available())
        .count();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "After this: {working} of {} capabilities work.",
            after.features.len()
        ),
        theme::bold(),
    ));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" What that means ")
                    .padding(Padding::horizontal(1)),
            )
            .wrap(Wrap { trim: true }),
        right,
    );
}

// ------------------------------------------------------------- plain output --

/// What is here now, and what it enables. The same information the screen
/// opens with, for a terminal that cannot draw one.
/// The money word for a component, or nothing when it is free. Free is the
/// default across most of the graph, so labelling every free line would be
/// noise and would bury the few that are not.
fn money(pricing: magician_components::Pricing) -> String {
    if pricing.costs_money() {
        format!(" [{}]", pricing.label())
    } else {
        String::new()
    }
}

pub fn print_status(selection: &Selection) {
    // The backend is the one thing every route installs and nothing probes:
    // the component probes answer "is it running", which a stopped install
    // fails while still being installed.
    match crate::install_mode::installed_version() {
        Some(version) => println!("\nInstalled backend: {version}"),
        None => println!("\nInstalled backend: none found at the install prefix"),
    }

    let report = selection.report();

    println!("\nComponents");
    // Width from the longest name: a hardcoded column silently ragged-edges the
    // moment someone adds a component with a longer label.
    let width = report
        .components
        .iter()
        .map(|c| c.name.len() + money(c.pricing).len())
        .max()
        .unwrap_or(0);
    for component in &report.components {
        let (mark, note) = match &component.state {
            ComponentState::Present { detail } => (theme::MARK_OK, detail.clone()),
            ComponentState::Declined => (theme::MARK_DECLINED, "declined".into()),
            ComponentState::Missing { detail } => (theme::MARK_MISSING, detail.clone()),
            ComponentState::NotInstalled { detail } => (theme::MARK_ABSENT, detail.clone()),
            ComponentState::BlockedBy { component } => {
                (theme::MARK_MISSING, format!("blocked by {component}"))
            },
            ComponentState::Unknown { detail } => (theme::MARK_UNKNOWN, detail.clone()),
        };
        let name = format!("{}{}", component.name, money(component.pricing));
        println!(" {mark} {name:<width$} {note}");
    }

    println!("\nWhat works");
    let width = report
        .features
        .iter()
        .map(|f| f.name.len())
        .max()
        .unwrap_or(0);
    for feature in &report.features {
        match &feature.availability {
            Availability::Available => println!(" {} {}", theme::MARK_OK, feature.name),
            Availability::Unavailable { reason, .. } => {
                println!(" {} {:<width$} {reason}", theme::MARK_ABSENT, feature.name)
            },
        }
    }
}

/// The accepted plan, printed after the screen closes so it stays in scrollback
/// once the band is gone.
pub fn print_plan(selection: &Selection) {
    let plan = selection.plan();
    if plan.install.is_empty() {
        println!("\nNothing to install — everything you chose already works here.");
    } else {
        println!("\nWould install:");
        for id in &plan.install {
            let name = selection
                .graph
                .component(id)
                .map(|c| c.name.as_str())
                .unwrap_or(id.as_str());
            println!(" - {name}");
        }
    }
    if !plan.unsupported.is_empty() {
        println!("\nNot possible on this machine:");
        for (name, reason) in &plan.unsupported {
            println!(" - {name} — {reason}");
        }
    }
    if !plan.left_off.is_empty() {
        println!("\nLeft off:");
        for (name, reason) in &plan.left_off {
            println!(" - {name} — {reason}");
        }
    }
}

#[cfg(test)]
mod tests;
