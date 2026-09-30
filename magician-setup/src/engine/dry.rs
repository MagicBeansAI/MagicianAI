//! Effects that narrate instead of acting.
//!
//! The point is not to test the engine — the [`Fake`](super::tests) does that.
//! It is to walk the whole install on a real machine and read every screen,
//! decision and consequence before any of it is wired to something that
//! changes the disk. A flow argued about on paper is a flow nobody has seen.
//!
//! Probing stays real. A dry run against invented state would rehearse a plan
//! for a machine that does not exist, and the plan is the thing worth reading.

use magician_components::{probe, Component, Observed, ProbeSpec};
use tokio::runtime::Runtime;

use super::Effects;
use crate::theme;

pub struct DryRunEffects<'a> {
    runtime: &'a Runtime,
    probes: Vec<(String, ProbeSpec)>,
    in_checkout: bool,
    /// Components a step has already pretended to install. The probe before a
    /// step is real; the probe after one answers for the pretence, so a dry
    /// run shows the flow it is rehearsing rather than a cascade of failures.
    pretended: std::collections::BTreeSet<String>,
    /// Set by a would-run, cleared by the probe that follows it.
    installing: bool,
    /// Everything it would have done, in order, so a caller can assert on the
    /// shape of a run rather than on its prose.
    pub would: Vec<String>,
}

impl<'a> DryRunEffects<'a> {
    pub fn new(runtime: &'a Runtime, components: &[Component], in_checkout: bool) -> Self {
        DryRunEffects {
            runtime,
            probes: components
                .iter()
                .map(|c| (c.id.clone(), c.probe.clone()))
                .collect(),
            in_checkout,
            pretended: std::collections::BTreeSet::new(),
            installing: false,
            would: Vec::new(),
        }
    }

    fn note(&mut self, line: String) {
        println!("  {} {}", theme::MARK_ABSENT, line);
        self.would.push(line);
    }
}

impl Effects for DryRunEffects<'_> {
    fn in_source_checkout(&self) -> bool {
        self.in_checkout
    }

    fn run_make(&mut self, target: &str) -> Result<(), String> {
        self.note(format!("would run: make {target}"));
        self.installing = true;
        Ok(())
    }

    fn run_script(&mut self, script: &str) -> Result<(), String> {
        self.note(format!("would run: scripts/{script}"));
        self.installing = true;
        Ok(())
    }

    /// Never asks. A rehearsal that collected a real key and then wrote
    /// nothing would be the worst of both.
    fn ask_secret(&mut self, label: &str, variable: &str) -> bool {
        self.note(format!("would ask for {label} and write {variable}"));
        false
    }

    fn instruct(&mut self, title: &str, steps: &[String]) {
        self.note(format!("would walk you through: {title}"));
        for step in steps {
            println!("        {step}");
        }
    }

    fn open(&mut self, target: &str) {
        self.note(format!("would open: {target}"));
    }

    /// Answers the default rather than asking. A dry run exists to show the
    /// whole path; stopping to ask at every branch would show one path and
    /// call it the flow.
    fn confirm(&mut self, question: &str, default: bool) -> bool {
        println!(
            "  {} {question}  [assuming {}]",
            theme::MARK_UNKNOWN,
            if default { "yes" } else { "no" }
        );
        default
    }

    /// Real before a step, simulated after one. The first answer is what makes
    /// the plan this machine's; the second answers for what the step pretended
    /// to do, because reporting "ran, but still not answering" about something
    /// that never ran is the one thing a rehearsal must not say.
    fn probe(&mut self, component_id: &str) -> Observed {
        if self.pretended.contains(component_id) {
            return Observed::Present("would be installed by the step above".into());
        }
        if self.installing {
            // The engine re-probes immediately after a step, so this is that
            // probe and this is the component whose step just ran.
            self.installing = false;
            self.pretended.insert(component_id.to_string());
            return Observed::Present("would be installed by the step above".into());
        }
        match self.probes.iter().find(|(id, _)| id == component_id) {
            Some((_, spec)) => self.runtime.block_on(probe::observe(spec)),
            None => Observed::Unknown(format!("no probe declared for {component_id}")),
        }
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    /// Nothing is waited for. A dry run that slept between polls would take as
    /// long as the real install and change nothing at the end of it.
    fn pause(&mut self) {}

    fn keep_waiting(&mut self, _attempt: usize) -> bool {
        false
    }
}
