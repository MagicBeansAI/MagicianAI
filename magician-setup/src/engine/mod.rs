//! Running a plan: probe, ask, act, verify — once per component, in order.
//!
//! # Why verification is not optional
//!
//! A step is done when its **probe** says so, never when its command exited
//! zero. `make setup-ollama` succeeding says the target ran, not that a daemon
//! is answering; granting a permission in System Settings produces no exit code
//! at all. Treating "the command worked" as "the thing is there" is how an
//! installer reports success onto a machine that cannot do the thing.
//!
//! # Why a failure is not the end of the run
//!
//! One component failing does not invalidate the rest. If the tunnel will not
//! come up, notes still installs. What a failure does invalidate is anything
//! downstream of it, and that falls out of the graph rather than needing its own
//! bookkeeping: the next step's dependency check sees an unusable component and
//! reports blocked, naming the thing that actually broke.
//!
//! # Effects are injected
//!
//! Everything that touches the world — running a command, opening a settings
//! pane, asking a person, sleeping — goes through [`Effects`]. The engine is
//! then a state machine over data, and the tests below drive a full run with no
//! subprocess, no terminal and no waiting.

use std::collections::{BTreeMap, BTreeSet};

use magician_components::{Graph, InstallAction, Observed};

pub mod dry;
pub mod env_file;
pub mod real;
pub mod state;

/// Everything the engine cannot do by itself.
pub trait Effects {
    /// Run a make target, streaming its output. Long builds are expected.
    fn run_make(&mut self, target: &str) -> Result<(), String>;
    /// Run one of the shipped scripts by name, for a machine that installed
    /// from a package and has no Makefile.
    fn run_script(&mut self, script: &str) -> Result<(), String>;
    /// Whether make targets are available here at all. A package install is not
    /// a checkout, and pretending otherwise fails at the first target.
    fn in_source_checkout(&self) -> bool;
    /// Show instructions for something only a person can do.
    fn instruct(&mut self, title: &str, steps: &[String]);
    /// Ask for a value and write it where the runtime reads it. `None` means
    /// the person skipped, which is ordinary — a key can always be pasted into
    /// the env file by hand, and several of these are any-of.
    ///
    /// Returns whether anything was written, never the value: a secret that
    /// travels back up through the engine is a secret that ends up in a log
    /// line eventually.
    fn ask_secret(&mut self, label: &str, variable: &str) -> bool;
    /// Open a settings pane or a page. Best-effort: failing to open is not a
    /// failure of the step, since the instructions still say where to go.
    fn open(&mut self, target: &str);
    /// Ask before doing anything that changes the machine.
    fn confirm(&mut self, question: &str, default: bool) -> bool;
    /// Re-probe one component. Called repeatedly while waiting on a person.
    fn probe(&mut self, component_id: &str) -> Observed;
    /// Tell the person what is happening.
    fn say(&mut self, line: &str);
    /// Between polls of a manual step. Separate so tests do not wait.
    fn pause(&mut self);
    /// Whether to keep waiting on a manual step, or give up on it for now.
    fn keep_waiting(&mut self, attempt: usize) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The probe already said yes. Nothing was run.
    AlreadyThere,
    /// Installed, and the probe agreed afterwards.
    Installed,
    /// Turned down when asked.
    Declined,
    /// Ran, but the probe still says no.
    NotVerified { detail: String },
    /// A dependency is unusable, so this could not be attempted.
    Blocked { by: String },
    /// Cannot be installed on its own — it arrives with the runtime.
    WithRuntime,
    /// This machine cannot have it, whatever anyone chooses. A backstop: the
    /// plan already excludes these, so reaching it means a step list was built
    /// somewhere else — `--step` will do exactly that.
    Unsupported { reason: String },
}

impl StepOutcome {
    /// Whether the component is usable after this step. Only two outcomes make
    /// it so, and neither of them is "the command exited zero".
    pub fn usable(&self) -> bool {
        matches!(self, StepOutcome::AlreadyThere | StepOutcome::Installed)
    }
}

#[derive(Debug, Clone)]
pub struct StepReport {
    pub component_id: String,
    pub name: String,
    pub outcome: StepOutcome,
}

/// How many times a manual step re-probes before offering to move on. The
/// pauses between are the caller's to size; the engine only counts.
const MANUAL_POLLS: usize = 40;

pub struct Engine<'a, E: Effects> {
    graph: &'a Graph,
    effects: &'a mut E,
    /// Components known usable: what the probes found at plan time, updated as
    /// steps succeed, so a later step sees the earlier one's result.
    usable: BTreeMap<String, bool>,
    /// What this machine is, so a step can be refused rather than attempted.
    host: magician_components::Host,
    /// Every variable this run has already offered to take. One key can serve
    /// two components — MiniMax's serves both the remote model and media
    /// generation — and somebody who has just declined it under one heading
    /// does not need to meet it again under the other.
    offered: BTreeSet<String>,
}

impl<'a, E: Effects> Engine<'a, E> {
    pub fn new(
        graph: &'a Graph,
        effects: &'a mut E,
        initially_usable: BTreeMap<String, bool>,
    ) -> Self {
        Self::on_host(
            graph,
            effects,
            initially_usable,
            magician_components::probe::detect_host(),
        )
    }

    /// The same engine against a stated machine. Tests describe a machine
    /// instead of owning one, which is the only way to cover the refusal.
    pub fn on_host(
        graph: &'a Graph,
        effects: &'a mut E,
        initially_usable: BTreeMap<String, bool>,
        host: magician_components::Host,
    ) -> Self {
        Engine {
            graph,
            effects,
            usable: initially_usable,
            host,
            offered: BTreeSet::new(),
        }
    }

    /// Work the plan in order. Never stops early: a component that fails costs
    /// the things downstream of it, not the ones beside it.
    pub fn run(&mut self, order: &[String]) -> Vec<StepReport> {
        let mut reports = Vec::new();
        for id in order {
            let Some(component) = self.graph.component(id) else {
                continue;
            };
            let outcome = self.step(id);
            self.usable.insert(id.clone(), outcome.usable());
            self.effects.say(&summary_line(&component.name, &outcome));
            reports.push(StepReport {
                component_id: id.clone(),
                name: component.name.clone(),
                outcome,
            });
        }
        reports
    }

    fn step(&mut self, id: &str) -> StepOutcome {
        let Some(component) = self.graph.component(id) else {
            return StepOutcome::Blocked {
                by: "unknown component".to_string(),
            };
        };
        let (name, install, requires) = (
            component.name.clone(),
            component.install.clone(),
            component.requires.clone(),
        );

        // The machine is asked before the dependencies: no ordering of other
        // components makes a component installable on hardware that cannot run
        // it, so this is the first thing that can rule a step out.
        if let Some(reason) = component.unsupported_on(&self.host) {
            return StepOutcome::Unsupported { reason };
        }

        // A dependency that is not usable makes this unreachable, and says which.
        if let Some(blocker) = requires.iter().find(|d| !self.is_usable(d)) {
            let blocker_name = self
                .graph
                .component(blocker)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| blocker.clone());
            return StepOutcome::Blocked { by: blocker_name };
        }

        // Ask the machine before asking the person: re-running a wizard on a
        // configured install should be quiet, not a second interrogation.
        if self.effects.probe(id).is_present() {
            return StepOutcome::AlreadyThere;
        }

        match install {
            InstallAction::WithRuntime => StepOutcome::WithRuntime,

            InstallAction::Make { target, script } => {
                // Decided before asking. Offering to set something up and then
                // discovering it is impossible here wastes the one answer the
                // person gave.
                let in_checkout = self.effects.in_source_checkout();
                if !in_checkout && script.is_none() {
                    return StepOutcome::Unsupported {
                        reason: format!(
                            "{name} can only be set up from a source checkout — its step runs a \
                             build that no package can carry."
                        ),
                    };
                }
                if !self.effects.confirm(&format!("Set up {name} now?"), true) {
                    return StepOutcome::Declined;
                }
                let attempt = if in_checkout {
                    self.effects.run_make(&target)
                } else {
                    self.effects
                        .run_script(script.as_deref().unwrap_or_default())
                };
                if let Err(e) = attempt {
                    // Still probe: a target can report failure having already
                    // done the part that matters, and the probe is the authority.
                    self.effects.say(&format!("  {target} reported: {e}"));
                }
                self.verify(id)
            },

            InstallAction::Manual {
                steps,
                open,
                secrets,
            } => {
                if !self.effects.confirm(&format!("Set up {name} now?"), true) {
                    return StepOutcome::Declined;
                }
                self.effects.instruct(&name, &steps);
                if let Some(target) = open {
                    self.effects.open(&target);
                }
                // Offered after the instructions and the page, because the
                // value does not exist until someone has followed them.
                for secret in &secrets {
                    // Asked once per run, however many components want it. The
                    // ones already pasted are handled by the probe above; this
                    // is for the ones turned down.
                    if !self.offered.insert(secret.variable.clone()) {
                        continue;
                    }
                    if self.effects.ask_secret(&secret.label, &secret.variable) {
                        // Any-of: one accepted value is enough, and asking for
                        // the other five providers afterwards is noise.
                        if self.effects.probe(id).is_present() {
                            return StepOutcome::Installed;
                        }
                    }
                }
                self.wait_for(id)
            },
        }
    }

    fn verify(&mut self, id: &str) -> StepOutcome {
        match self.effects.probe(id) {
            Observed::Present(_) => StepOutcome::Installed,
            other => StepOutcome::NotVerified {
                detail: other.detail().to_string(),
            },
        }
    }

    /// Poll while the person works somewhere else. They are in System Settings,
    /// not looking at this, so it has to notice on its own rather than making
    /// them come back and press a key.
    fn wait_for(&mut self, id: &str) -> StepOutcome {
        for attempt in 0..MANUAL_POLLS {
            if self.effects.probe(id).is_present() {
                return StepOutcome::Installed;
            }
            if !self.effects.keep_waiting(attempt) {
                break;
            }
            self.effects.pause();
        }
        // A probe that cannot answer must not be reported as a failure — mic and
        // screen permissions are unreadable from a process that lacks them, so
        // the person's word is the only evidence available.
        match self.effects.probe(id) {
            Observed::Unknown(detail) => {
                if self
                    .effects
                    .confirm(&format!("Did you finish this? ({detail})"), true)
                {
                    StepOutcome::Installed
                } else {
                    StepOutcome::Declined
                }
            },
            other => StepOutcome::NotVerified {
                detail: other.detail().to_string(),
            },
        }
    }

    fn is_usable(&self, id: &str) -> bool {
        self.usable.get(id).copied().unwrap_or(false)
    }
}

fn summary_line(name: &str, outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::AlreadyThere => format!("  {name} — already here"),
        StepOutcome::Installed => format!("  {name} — done"),
        StepOutcome::Declined => format!("  {name} — skipped"),
        StepOutcome::WithRuntime => format!("  {name} — arrives with the runtime"),
        StepOutcome::Blocked { by } => format!("  {name} — not attempted, {by} is not available"),
        StepOutcome::Unsupported { reason } => format!("  {name} — not on this machine: {reason}"),
        StepOutcome::NotVerified { detail } => {
            format!("  {name} — ran, but still not answering ({detail})")
        },
    }
}

#[cfg(test)]
mod tests;
