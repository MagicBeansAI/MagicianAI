//! The [`Effects`] a real run needs: subprocesses, a terminal, and a person.
//!
//! Probing is async while the engine is not, so this holds the runtime and
//! blocks on each probe. That is deliberate rather than incidental: the engine
//! reads as the sequence it is, and a step genuinely cannot proceed until its
//! probe has answered, so there is nothing to overlap.

use std::io::{self, Write};
use std::process::Command;

use magician_components::{probe, Component, Observed, ProbeSpec};
use tokio::runtime::Runtime;

use super::Effects;
use crate::theme;

/// Read a line with the terminal echo off.
///
/// Nothing is printed as it is typed — not even asterisks, whose count is a
/// hint about the value. Backspace works because a mistyped key that cannot be
/// corrected is a key pasted into a dotfile by hand instead.
fn read_hidden() -> io::Result<String> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    enable_raw_mode()?;
    let mut value = String::new();
    let outcome = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => break Ok(value.clone()),
                KeyCode::Esc => break Ok(String::new()),
                KeyCode::Backspace => {
                    value.pop();
                },
                KeyCode::Char(c) => value.push(c),
                _ => {},
            },
            Ok(_) => {},
            Err(e) => break Err(e),
        }
    };
    // Restored before returning, and before any print, so an error path cannot
    // leave the terminal in raw mode with the echo off.
    disable_raw_mode()?;
    outcome
}

pub struct RealEffects<'a> {
    runtime: &'a Runtime,
    probes: Vec<(String, ProbeSpec)>,
    repo_root: std::path::PathBuf,
    /// Where the env file lives. Separate from the checkout: a package install
    /// has a data root and no repository at all.
    data_root: std::path::PathBuf,
    /// Seconds between polls while waiting on a person.
    poll_seconds: u64,
}

impl<'a> RealEffects<'a> {
    pub fn new(
        runtime: &'a Runtime,
        components: &[Component],
        repo_root: std::path::PathBuf,
        data_root: std::path::PathBuf,
    ) -> Self {
        RealEffects {
            runtime,
            probes: components
                .iter()
                .map(|c| (c.id.clone(), c.probe.clone()))
                .collect(),
            repo_root,
            data_root,
            poll_seconds: 3,
        }
    }
}

impl RealEffects<'_> {
    /// A checkout has a Makefile at its root; an install prefix does not. That
    /// one file is the whole difference between the two worlds this runs in.
    fn makefile(&self) -> std::path::PathBuf {
        self.repo_root.join("Makefile")
    }
}

impl Effects for RealEffects<'_> {
    fn in_source_checkout(&self) -> bool {
        self.makefile().is_file()
    }

    fn run_script(&mut self, script: &str) -> Result<(), String> {
        let path = self.repo_root.join("scripts").join(script);
        if !path.is_file() {
            return Err(format!(
                "{script} is not in this install — it should have shipped with the package"
            ));
        }
        println!("\n  running {script} — output follows\n");
        // Same inherited stdio as make: these stream for minutes and the person
        // watching deserves to see it.
        let status = Command::new("bash")
            .arg(&path)
            .current_dir(&self.repo_root)
            .status()
            .map_err(|e| format!("could not start {script}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(match status.code() {
                Some(code) => format!("{script} exited {code}"),
                None => format!("{script} was killed"),
            })
        }
    }

    fn run_make(&mut self, target: &str) -> Result<(), String> {
        println!("\n  running make {target} — output follows\n");
        // Inherited stdio on purpose: a build streams for minutes and the person
        // watching deserves to see it, which is also why the wizard never takes
        // the alternate screen.
        let status = Command::new("make")
            .arg(target)
            .current_dir(&self.repo_root)
            .status()
            .map_err(|e| format!("could not start make: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(match status.code() {
                Some(code) => format!("make {target} exited {code}"),
                None => format!("make {target} was killed"),
            })
        }
    }

    /// Read a value without echoing it, write it, and say only whether it
    /// landed. Raw mode is entered for the read and left before anything is
    /// printed, so a panic between the two cannot leave the terminal mute.
    fn ask_secret(&mut self, label: &str, variable: &str) -> bool {
        let env = super::env_file::EnvFile::at(&self.data_root);
        if env.is_set(variable) {
            println!("    {} {label} is already set", theme::MARK_OK);
            return false;
        }

        print!("    {label} — paste it, or press enter to skip: ");
        let _ = io::stdout().flush();
        let value = match read_hidden() {
            Ok(v) => v,
            Err(e) => {
                println!("\n    could not read that ({e}); add {variable} by hand");
                return false;
            },
        };
        println!();
        if value.trim().is_empty() {
            return false;
        }
        match env.set(variable, value.trim()) {
            Ok(super::env_file::Wrote::Added) => {
                println!(
                    "    {} wrote {variable} to {}",
                    theme::MARK_OK,
                    env.path().display()
                );
                true
            },
            Ok(super::env_file::Wrote::AlreadySet) => false,
            Err(e) => {
                println!("    could not write {variable}: {e}");
                false
            },
        }
    }

    fn instruct(&mut self, title: &str, steps: &[String]) {
        println!("\n  {title} — this one is yours to do:");
        for (n, step) in steps.iter().enumerate() {
            println!("    {}. {step}", n + 1);
        }
    }

    fn open(&mut self, target: &str) {
        // Best effort. If it does not open, the instructions still say where to
        // go, so a failure here is not a failure of the step.
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = Command::new(opener).arg(target).status();
    }

    fn confirm(&mut self, question: &str, default: bool) -> bool {
        let hint = if default { "Y/n" } else { "y/N" };
        print!("\n  {question} [{hint}] ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return default;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        }
    }

    fn probe(&mut self, component_id: &str) -> Observed {
        let Some((_, spec)) = self.probes.iter().find(|(id, _)| id == component_id) else {
            return Observed::Unknown(format!("no probe declared for {component_id}"));
        };
        let spec = spec.clone();
        self.runtime.block_on(probe::observe(&spec))
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    fn pause(&mut self) {
        std::thread::sleep(std::time::Duration::from_secs(self.poll_seconds));
    }

    fn keep_waiting(&mut self, attempt: usize) -> bool {
        // Say so once, then stay quiet: reprinting every few seconds while
        // someone is in System Settings is noise, not progress.
        if attempt == 0 {
            println!(
                "  {} waiting — this notices on its own when you are done.",
                theme::MARK_UNKNOWN
            );
        }
        true
    }
}
