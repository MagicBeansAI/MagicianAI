//! Running whole installs against machines nobody owns.
//!
//! `--dry-run` walks one path: this laptop, these probes, the remembered
//! answers. That is the path in front of you, and it is not the flow. The flow
//! is every combination of machine, selection and answer, and most of those
//! combinations only exist on somebody else's laptop — the 8GB Intel Mac that
//! must be refused a local model, the re-run where half of it is already
//! installed, the person who says no to everything.
//!
//! A [`Scenario`] describes one of those worlds and [`run`] plays the whole
//! wizard against it: real selection, real plan, real engine, scripted effects.
//! Nothing reads a probe, runs a command or writes a file, so a hundred of them
//! cost no state and about a second.
//!
//! # Invariants, not golden transcripts
//!
//! Asserting on exact transcripts would mean rewriting them whenever a step's
//! wording changed, and a test that gets regenerated rather than read is not a
//! test. [`violations`] states what must be true of *any* run instead — never
//! plan what the host cannot have, never plan what already works, dependencies
//! before dependants, never leave a chosen capability unexplained. Those hold
//! for scenarios nobody has thought of yet, which is the point: the next person
//! to add a component gets checked against cases they never wrote.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use magician_components::{Graph, Host, InstallAction, Need, Observed};

use crate::engine::{Effects, Engine, StepOutcome};
use crate::model::Selection;

// ------------------------------------------------------------------ worlds --

/// A machine, a selection, and how the person behaves.
///
/// Readable from YAML so somebody — a person or another agent — can describe a
/// machine this file never imagined and get the whole flow played against it.
/// Only `name` is required; every other field has the sensible default, so the
/// smallest useful scenario is one line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "default_host")]
    pub host: Host,
    /// Component ids a probe would find running.
    #[serde(default)]
    pub present: Vec<String>,
    /// Component ids whose probe cannot answer — permissions, paired sessions.
    #[serde(default)]
    pub unknown: Vec<String>,
    /// Capability ids the person picked.
    #[serde(default)]
    pub wanted: Vec<String>,
    /// Variables they will actually paste when asked. Everything else they
    /// skip, which is the common case: nobody holds every key.
    #[serde(default)]
    pub will_paste: Vec<String>,
    /// Whether they say yes when asked to run a step.
    #[serde(default = "yes")]
    pub accepts: bool,
    /// Whether make targets exist here. False is a package install.
    #[serde(default = "yes")]
    pub in_checkout: bool,
    /// Whether the runtime is up by the time the component steps run.
    ///
    /// True for every ordinary run: the wizard installs and starts the binaries
    /// before it works the plan, so the components that arrive with the runtime
    /// are already answering. Setting it false describes the machine where that
    /// stage did not happen, and the cascade that follows — a browser extension
    /// cannot pair with an executor that is not running — is the correct
    /// behaviour rather than a fault.
    #[serde(default = "yes")]
    pub runtime_up: bool,
}

/// The machine the installer was written for. A described scenario that says
/// nothing about hardware means "an ordinary Mac", not "no machine at all".
fn default_host() -> Host {
    Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 36,
    }
}

/// Every behaviour flag defaults to the cooperative case: somebody in a
/// checkout, with the runtime up, saying yes. A scenario then states only its
/// departures from the ordinary run, which is what makes it readable.
fn yes() -> bool {
    true
}

impl Scenario {
    pub fn new(name: &str) -> Self {
        Scenario {
            name: name.to_string(),
            host: default_host(),
            present: Vec::new(),
            unknown: Vec::new(),
            wanted: Vec::new(),
            will_paste: Vec::new(),
            accepts: true,
            in_checkout: true,
            runtime_up: true,
        }
    }
    pub fn on(mut self, os: &str, arch: &str, memory_gb: u32) -> Self {
        self.host = Host {
            os: os.into(),
            arch: arch.into(),
            memory_gb,
        };
        self
    }
    pub fn with(mut self, ids: &[&str]) -> Self {
        self.present = ids.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn cannot_tell(mut self, ids: &[&str]) -> Self {
        self.unknown = ids.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn wanting(mut self, ids: &[&str]) -> Self {
        self.wanted = ids.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn pasting(mut self, vars: &[&str]) -> Self {
        self.will_paste = vars.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn declining(mut self) -> Self {
        self.accepts = false;
        self
    }
    pub fn from_a_package(mut self) -> Self {
        self.in_checkout = false;
        self
    }
    pub fn before_the_runtime_is_up(mut self) -> Self {
        self.runtime_up = false;
        self
    }
}

/// What happened, in a shape worth asserting on.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub scenario: String,
    pub install: Vec<String>,
    pub unsupported: Vec<(String, String)>,
    pub unresolved: Vec<String>,
    pub left_off: Vec<(String, String)>,
    pub steps: Vec<(String, StepOutcome)>,
    /// Variables the run asked for, in order.
    pub asked: Vec<String>,
    /// Everything that would have touched the machine.
    pub acted: Vec<String>,
    pub working_before: usize,
    pub working_after: usize,
    pub capabilities: usize,
}

impl Transcript {
    pub fn outcome(&self, component_id: &str) -> Option<&StepOutcome> {
        self.steps
            .iter()
            .find(|(id, _)| id == component_id)
            .map(|(_, o)| o)
    }

    /// The whole run as a person would read it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("── {} ──\n", self.scenario));
        out.push_str(&format!(
            "   capabilities {} → {} of {}\n",
            self.working_before, self.working_after, self.capabilities
        ));
        if self.install.is_empty() {
            out.push_str("   plan: nothing to install\n");
        } else {
            out.push_str(&format!("   plan: {}\n", self.install.join(" → ")));
        }
        for (name, why) in &self.unsupported {
            out.push_str(&format!("   not on this machine: {name} — {why}\n"));
        }
        for id in &self.unresolved {
            out.push_str(&format!("   NOTHING PROVIDES: {id}\n"));
        }
        for (id, outcome) in &self.steps {
            out.push_str(&format!("   {id}: {outcome:?}\n"));
        }
        // Names, not reasons: this list is every capability nobody asked for,
        // and its reasons ran seventeen lines deep over a two-line plan. The
        // reason that matters — why something somebody *did* ask for did not
        // arrive — comes back as a violation instead. Complete, not truncated:
        // a list that quietly stops at five reads as a shorter list.
        if !self.left_off.is_empty() {
            let names: Vec<&str> = self
                .left_off
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            out.push_str(&format!(
                "   left off ({}): {}\n",
                names.len(),
                names.join(", ")
            ));
        }
        if !self.asked.is_empty() {
            out.push_str(&format!("   asked for: {}\n", self.asked.join(", ")));
        }
        out
    }
}

// ----------------------------------------------------------------- effects --

/// Effects that answer from a script and record what they were asked to do.
///
/// Not the test fake: this ships, so `--simulate` works in a release binary and
/// an agent with only the installer can drive it.
struct Scripted {
    present: BTreeSet<String>,
    unknown: BTreeSet<String>,
    provides: BTreeMap<String, Vec<String>>,
    will_paste: BTreeSet<String>,
    accepts: bool,
    in_checkout: bool,
    asked: Vec<String>,
    acted: Vec<String>,
}

impl Effects for Scripted {
    fn in_source_checkout(&self) -> bool {
        self.in_checkout
    }
    fn run_make(&mut self, target: &str) -> Result<(), String> {
        self.acted.push(format!("make {target}"));
        self.satisfy(target);
        Ok(())
    }
    fn run_script(&mut self, script: &str) -> Result<(), String> {
        self.acted.push(format!("scripts/{script}"));
        self.satisfy(script);
        Ok(())
    }
    fn ask_secret(&mut self, _label: &str, variable: &str) -> bool {
        self.asked.push(variable.to_string());
        if !self.will_paste.contains(variable) {
            return false;
        }
        self.acted.push(format!("wrote {variable}"));
        self.satisfy(variable);
        true
    }
    fn instruct(&mut self, _title: &str, _steps: &[String]) {}
    fn open(&mut self, target: &str) {
        self.acted.push(format!("open {target}"));
    }
    fn confirm(&mut self, _question: &str, _default: bool) -> bool {
        self.accepts
    }
    fn probe(&mut self, component_id: &str) -> Observed {
        if self.present.contains(component_id) {
            Observed::Present("the scenario says so".into())
        } else if self.unknown.contains(component_id) {
            Observed::Unknown("the scenario cannot say".into())
        } else {
            Observed::Absent("the scenario says so".into())
        }
    }
    fn say(&mut self, _line: &str) {}
    fn pause(&mut self) {}
    fn keep_waiting(&mut self, _attempt: usize) -> bool {
        false
    }
}

impl Scripted {
    /// A step that ran makes whatever it installs present, which is how a
    /// scenario models "the command worked". A scenario that wants the other
    /// outcome — ran and still absent — leaves the mapping out.
    fn satisfy(&mut self, key: &str) {
        if let Some(ids) = self.provides.get(key).cloned() {
            self.present.extend(ids);
        }
    }
}

/// What a world already has before any step runs: what the scenario declares,
/// plus whatever arrives with the runtime. Shared by [`run`] and [`violations`]
/// because a checker working from a different idea of the starting machine
/// reports faults that are its own.
fn already_there(graph: &Graph, scenario: &Scenario) -> BTreeSet<String> {
    graph
        .components
        .iter()
        .filter(|c| scenario.runtime_up && matches!(c.install, InstallAction::WithRuntime))
        .map(|c| c.id.clone())
        .chain(scenario.present.iter().cloned())
        .collect()
}

/// What each make target, script and secret would make present. Derived from
/// the graph so a scenario never restates it and a new component is covered the
/// day it is declared.
fn provisions(graph: &Graph) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for component in &graph.components {
        match &component.install {
            InstallAction::Make { target, script } => {
                out.entry(target.clone())
                    .or_default()
                    .push(component.id.clone());
                if let Some(script) = script {
                    out.entry(script.clone())
                        .or_default()
                        .push(component.id.clone());
                }
            },
            InstallAction::Manual { secrets, .. } => {
                for secret in secrets {
                    out.entry(secret.variable.clone())
                        .or_default()
                        .push(component.id.clone());
                }
            },
            InstallAction::WithRuntime => {},
        }
    }
    out
}

// ------------------------------------------------------------------ running --

/// Play the whole wizard against one described world. Touches nothing.
pub fn run(graph: &Graph, scenario: &Scenario) -> Transcript {
    // What arrives with the runtime is present once the runtime is, and the
    // stage that installs it runs before the plan does. Left out, every
    // component that depends on one of these reports blocked, and the whole
    // simulation describes a machine mid-install rather than one being set up.
    let start = already_there(graph, scenario);

    let observed: BTreeMap<String, Observed> = start
        .iter()
        .map(|id| (id.clone(), Observed::Present("the scenario says so".into())))
        .chain(scenario.unknown.iter().map(|id| {
            (
                id.clone(),
                Observed::Unknown("the scenario cannot say".into()),
            )
        }))
        .collect();

    // `Some(false)` for everything: a scenario states what it wants outright
    // rather than inheriting the remembered answers of whoever is running it.
    let mut selection = Selection::with_remembered(graph.clone(), observed, |_| Some(false));
    selection.host = scenario.host.clone();
    selection.set_wanted(&scenario.wanted);

    let before = selection.report();
    let working_before = before
        .features
        .iter()
        .filter(|f| f.availability.is_available())
        .count();
    let plan = selection.plan();

    let mut effects = Scripted {
        present: start.clone(),
        unknown: scenario.unknown.iter().cloned().collect(),
        provides: provisions(graph),
        will_paste: scenario.will_paste.iter().cloned().collect(),
        accepts: scenario.accepts,
        in_checkout: scenario.in_checkout,
        asked: Vec::new(),
        acted: Vec::new(),
    };
    let usable = before
        .components
        .iter()
        .map(|c| (c.id.clone(), c.state.usable()))
        .collect();
    let steps = Engine::on_host(graph, &mut effects, usable, scenario.host.clone())
        .run(&plan.install)
        .into_iter()
        .map(|r| (r.component_id, r.outcome))
        .collect();

    // The machine as the run left it, read from the same probe state the engine
    // finished with rather than from what the plan hoped for.
    let after: BTreeMap<String, Observed> = effects
        .present
        .iter()
        .map(|id| (id.clone(), Observed::Present("installed".into())))
        .collect();
    let after = magician_components::resolve(graph, &BTreeMap::new(), &after);

    Transcript {
        scenario: scenario.name.clone(),
        install: plan.install,
        unsupported: plan.unsupported,
        unresolved: plan.unresolved,
        left_off: plan.left_off,
        steps,
        asked: effects.asked,
        acted: effects.acted,
        working_before,
        working_after: after
            .features
            .iter()
            .filter(|f| f.availability.is_available())
            .count(),
        capabilities: after.features.len(),
    }
}

// --------------------------------------------------------------- invariants --

/// Everything that must hold of any run. An empty list means the scenario
/// behaved; each string is a sentence the installer broke.
pub fn violations(graph: &Graph, scenario: &Scenario, t: &Transcript) -> Vec<String> {
    let mut out = Vec::new();
    let by_id = |id: &str| graph.components.iter().find(|c| c.id == id);

    // Never plan what this machine cannot have. Offering a step and then
    // refusing it wastes the one answer the person gave, and the plan screen
    // has already promised it.
    for id in &t.install {
        if let Some(why) = by_id(id).and_then(|c| c.unsupported_on(&scenario.host)) {
            out.push(format!("planned {id}, which this host cannot have: {why}"));
        }
    }

    // Never plan what is already working. A re-run on a configured machine
    // should be quiet.
    let start = already_there(graph, scenario);
    for id in &t.install {
        if start.contains(id) {
            out.push(format!("planned {id}, which was already present"));
        }
    }

    // Dependencies before dependants, or the step fails on ordering alone.
    for id in &t.install {
        let Some(component) = by_id(id) else { continue };
        for dep in &component.requires {
            let (Some(at_dep), Some(at_id)) = (position(&t.install, dep), position(&t.install, id))
            else {
                continue;
            };
            if at_dep > at_id {
                out.push(format!("{id} is planned before its dependency {dep}"));
            }
        }
    }

    // A required component is planned, present, or refused with a reason.
    // Waiting to be asked for one is how an install ends up unable to remember
    // anything.
    for component in graph.components.iter().filter(|c| c.required && !c.core) {
        let handled = t.install.contains(&component.id)
            || scenario.present.contains(&component.id)
            || t.unsupported
                .iter()
                .any(|(name, _)| name == &component.name);
        if !handled {
            out.push(format!(
                "required component {} was neither planned, present, nor refused",
                component.id
            ));
        }
    }

    // Nothing is asked for that no component declares, and nothing is asked for
    // twice — a wizard that asks for the same key on two screens looks broken
    // even when it is not.
    for variable in &t.asked {
        if !declares_secret(graph, variable) {
            out.push(format!("asked for {variable}, which no component declares"));
        }
    }
    let mut seen_asks = BTreeSet::new();
    for variable in &t.asked {
        if !seen_asks.insert(variable) {
            out.push(format!("asked for {variable} more than once"));
        }
    }

    // No step runs for a component outside the plan.
    for (id, _) in &t.steps {
        if !t.install.contains(id) {
            out.push(format!("ran a step for {id}, which was not in the plan"));
        }
    }

    // Every planned component produced a step. `Engine::run` skips ids it
    // cannot find in the graph, so a plan naming something that is not there
    // would disappear without a word — and the plan screen would have counted
    // it.
    for id in &t.install {
        if t.outcome(id).is_none() {
            out.push(format!("planned {id}, but no step ran for it"));
        }
    }

    // A chosen capability either works afterwards, or something in the plan
    // says why not. Silence about something somebody asked for is the worst
    // outcome available here.
    let projected: BTreeMap<String, Observed> = t
        .install
        .iter()
        .chain(start.iter())
        .map(|id| (id.clone(), Observed::Present("planned".into())))
        .collect();
    let projected = magician_components::resolve(graph, &BTreeMap::new(), &projected);
    for id in &scenario.wanted {
        let Some(feature) = graph.features.iter().find(|f| &f.id == id) else {
            out.push(format!(
                "chose {id}, which is not a capability in the graph"
            ));
            continue;
        };
        let works = projected
            .feature(id)
            .map(|f| f.availability.is_available())
            .unwrap_or(false);
        if works {
            continue;
        }
        if !explained(graph, t, feature) {
            out.push(format!(
                "chose {id}, which the plan neither delivers nor explains away"
            ));
        }
    }

    // Saying no changes nothing. The one promise a nervous person is actually
    // testing when they run this.
    if !scenario.accepts && !t.acted.is_empty() {
        out.push(format!(
            "said no to everything, but {:?} still ran",
            t.acted
        ));
    }

    out
}

fn position(list: &[String], id: &str) -> Option<usize> {
    list.iter().position(|x| x == id)
}

fn declares_secret(graph: &Graph, variable: &str) -> bool {
    graph.components.iter().any(|c| match &c.install {
        InstallAction::Manual { secrets, .. } => secrets.iter().any(|s| &s.variable == variable),
        _ => false,
    })
}

/// Whether the plan accounts for a capability it did not deliver: something it
/// needs was refused for this machine, or nothing provides it at all.
fn explained(graph: &Graph, t: &Transcript, feature: &magician_components::Feature) -> bool {
    feature.needs.iter().any(|need| match need {
        Need::Component(id) => graph
            .component(id)
            .map(|c| t.unsupported.iter().any(|(name, _)| name == &c.name))
            .unwrap_or(false),
        Need::Requirement(id) => {
            let providers = graph.providers(id);
            providers.is_empty() && t.unresolved.contains(id)
                || (!providers.is_empty()
                    && providers
                        .iter()
                        .all(|p| t.unsupported.iter().any(|(name, _)| name == &p.name)))
        },
    })
}

// ------------------------------------------------------------------- sweeps --

/// A spread of worlds worth checking every time. Not exhaustive — the graph has
/// more subsets than atoms — but chosen so each one is a different *kind* of
/// machine rather than a different list.
pub fn standard_scenarios(graph: &Graph) -> Vec<Scenario> {
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let first_three: Vec<&str> = every.iter().take(3).copied().collect();

    vec![
        // The machine the installer was written for.
        Scenario::new("a bare Apple Silicon Mac, everything wanted")
            .on("macos", "arm64", 36)
            .wanting(&every),
        // The machine it must refuse a local model to, and route around.
        Scenario::new("an 8GB Intel Mac, everything wanted")
            .on("macos", "x86_64", 8)
            .wanting(&every),
        // Enough memory, wrong silicon.
        Scenario::new("a 64GB Intel Mac, everything wanted")
            .on("macos", "x86_64", 64)
            .wanting(&every),
        // Right silicon, not enough of it.
        Scenario::new("a 16GB Apple Silicon Mac, everything wanted")
            .on("macos", "arm64", 16)
            .wanting(&every),
        // Nobody picked anything. The required components still have to arrive.
        Scenario::new("a bare Mac, nothing wanted").on("macos", "arm64", 36),
        // Somebody who says no to every prompt.
        Scenario::new("a bare Mac, everything wanted, every prompt declined")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .declining(),
        // A person with every key in hand.
        Scenario::new("a bare Mac, everything wanted, every key pasted")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .pasting(&every_secret(graph)),
        // No Makefile: the package install, where a step without a script is
        // refused rather than attempted.
        Scenario::new("a package install, everything wanted")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .from_a_package(),
        // A partial re-run: half the machine is already up.
        Scenario::new("a re-run over a half-installed machine")
            .on("macos", "arm64", 36)
            .with(&every_other(graph))
            .wanting(&every),
        // A probe that cannot answer, which is every permission on macOS.
        Scenario::new("a Mac where the permission probes cannot answer")
            .on("macos", "arm64", 36)
            .cannot_tell(&unknowable_ids(graph))
            .wanting(&every),
        // The stage before the plan did not happen. Everything downstream of
        // the runtime is then blocked, and correctly so.
        Scenario::new("a Mac where the runtime is not up yet")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .before_the_runtime_is_up(),
        // A narrow pick, which is what most people actually do.
        Scenario::new("a bare Mac, three capabilities wanted")
            .on("macos", "arm64", 36)
            .wanting(&first_three),
    ]
}

/// One scenario per capability, each choosing exactly that one. This is the
/// sweep that catches a capability whose needs nothing can satisfy, because it
/// is never masked by another capability dragging the same component in.
pub fn one_per_capability(graph: &Graph) -> Vec<Scenario> {
    graph
        .features
        .iter()
        .map(|f| {
            Scenario::new(&format!("only {}", f.id))
                .on("macos", "arm64", 36)
                .wanting(&[f.id.as_str()])
        })
        .collect()
}

fn every_secret(graph: &Graph) -> Vec<&str> {
    graph
        .components
        .iter()
        .filter_map(|c| match &c.install {
            InstallAction::Manual { secrets, .. } => Some(secrets),
            _ => None,
        })
        .flatten()
        .map(|s| s.variable.as_str())
        .collect()
}

/// Every other component, for the half-installed machine. Which half hardly
/// matters; that the halves interleave does — a contiguous block would leave
/// the dependency ordering untested.
fn every_other(graph: &Graph) -> Vec<&str> {
    graph
        .components
        .iter()
        .step_by(2)
        .map(|c| c.id.as_str())
        .collect()
}

/// Components whose probe reads a permission rather than a process. Those are
/// the ones that come back `Unknown` on a real machine.
fn unknowable_ids(graph: &Graph) -> Vec<&str> {
    graph
        .components
        .iter()
        .filter(|c| matches!(c.probe, magician_components::ProbeSpec::Manual { .. }))
        .map(|c| c.id.as_str())
        .collect()
}

/// Read machines somebody else described.
///
/// The point of the whole file: the standard sweep is what this author thought
/// of, and the interesting machine is always the one nobody thought of.
pub fn read_scenarios(yaml: &str) -> Result<Vec<Scenario>, String> {
    serde_yaml::from_str(yaml).map_err(|e| format!("{e}"))
}

/// Play a described set and report, same shape as the standard sweep.
pub fn play(graph: &Graph, scenarios: &[Scenario]) -> (String, usize) {
    let mut out = String::new();
    let mut broken = 0;
    out.push_str(&format!(
        "{} scenarios, nothing touched\n\n",
        scenarios.len()
    ));
    for scenario in scenarios {
        let transcript = run(graph, scenario);
        out.push_str(&transcript.render());
        for violation in violations(graph, scenario, &transcript) {
            broken += 1;
            out.push_str(&format!("   ✗ {violation}\n"));
        }
        out.push('\n');
    }
    out.push_str(&match broken {
        0 => "no invariant was broken\n".to_string(),
        1 => "1 invariant broken\n".to_string(),
        n => format!("{n} invariants broken\n"),
    });
    (out, broken)
}

/// Run every standard sweep and report. Returns the rendered text and whether
/// anything was violated, so a caller can print it and set an exit code.
pub fn sweep(graph: &Graph) -> (String, usize) {
    let scenarios: Vec<Scenario> = standard_scenarios(graph)
        .into_iter()
        .chain(one_per_capability(graph))
        .collect();
    play(graph, &scenarios)
}

#[cfg(test)]
mod tests;
