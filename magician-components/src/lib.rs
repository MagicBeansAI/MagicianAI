//! What is installed, what that enables, and what it costs to decline.
//!
//! # Why this exists
//!
//! Components were made declinable in the installer once and reverted the same
//! day, because a gate is the easy half. The hard half is that everything
//! downstream has to know how to behave when a component is absent. Without
//! that, declining one produces a silently broken install rather than a smaller
//! one: navigation entries that lead nowhere, memory retrieval that fails at
//! call time instead of being hidden.
//!
//! So this crate answers one question for every surface that asks:
//!
//! > Given what is actually running and what the operator asked for, which
//! > features work, and for the ones that do not, what exactly is missing?
//!
//! # Two kinds of truth
//!
//! [`Declared`] is intent that persists: the operator wants a local model, or
//! declined the public tunnel. [`Observed`] is what a probe found a moment ago.
//! They are separate because they disagree in ways that matter. A component can
//! be wanted and down, which is a fault worth surfacing; or present without ever
//! having been declared, which is the normal state of every install that predates
//! this registry and must not be reported as a surprise.
//!
//! Observation is ground truth for whether a feature works *now*. Declaration
//! decides whether absence is a problem or a choice.
//!
//! # No I/O
//!
//! Nothing here reads a file, opens a socket or spawns a process. Callers supply
//! [`Declared`] and [`Observed`] maps and get a [`Report`] back. That keeps
//! [`resolve`] a pure function over data, so the interesting logic — OR
//! satisfaction, transitive component requirements, the reason text attached to
//! an unavailable feature — is testable with no stack running and no terminal.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub mod setup;

// ----------------------------------------------------------------- probing --

/// How to find out whether a component is there. Declared as **data**, not as a
/// function, so the model stays I/O-free and the same declaration can be
/// executed by the runtime, by the setup wizard, or by a test with a stub.
///
/// Running these is [`probe::observe`], behind the `probe` feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProbeSpec {
    /// Present on a 2xx.
    HttpOk { url: String, timeout_ms: u64 },
    /// Present on *any* HTTP response. For hops where a GET is not the real
    /// verb — the Kapso webhook is POST-only, so a 4xx still proves the tunnel
    /// routed through to a live upstream.
    HttpAnyResponse { url: String, timeout_ms: u64 },
    /// Present on a 2xx whose body contains `needle`. Liveness is not always
    /// enough: the embedding daemon answers happily with no model resident,
    /// which is indistinguishable from working until something asks it to embed.
    HttpBodyContains {
        url: String,
        needle: String,
        timeout_ms: u64,
    },
    /// Present when the model selected at `key_path` in the runtime YAML is
    /// listed by the configured Ollama `/api/tags` endpoint.
    ConfiguredOllamaModelPresent {
        url: String,
        config_path: String,
        key_path: Vec<String>,
        timeout_ms: u64,
    },
    /// Present when the path exists.
    FileExists { path: String },
    /// Present when the path can actually be **read**.
    ///
    /// Existence and readability are the same question for most files and a
    /// different one for anything macOS puts behind Full Disk Access. The
    /// Messages database exists on every Mac that has ever sent a text, granted
    /// or not, so `FileExists` on it answers "have you used Messages" while
    /// appearing to answer "is the permission granted". A permission probe that
    /// says yes when the permission is missing is worse than no probe: it ends
    /// the step, and the failure surfaces later as an empty inbox.
    ///
    /// Denied is [`Observed::Absent`] — the thing genuinely is not available.
    /// Missing is [`Observed::Unknown`], because a file that is not there says
    /// nothing about whether the permission was granted.
    FileReadable { path: String, because: String },
    /// Present when *any* of `keys` is set in the environment or assigned a
    /// non-empty value in one of `files`. Any-of, because a component can be
    /// configured more than one way: a remote model provider is present if the
    /// operator holds a key for any provider the runtime speaks to, and
    /// demanding a particular one would report "no model" to someone who has
    /// six. A single-key probe is a one-element list.
    ///
    /// Configuration, deliberately not validity — see the module docs on why a
    /// probe must not spend someone else's rate limit.
    EnvKeyPresent {
        keys: Vec<String>,
        files: Vec<String>,
    },
    /// Present only when every named key has a non-empty value. Use this for a
    /// single integration whose credentials are a set (for example Telegram's
    /// API id plus hash), rather than alternative providers where any one key
    /// is sufficient.
    EnvKeysAllPresent {
        keys: Vec<String>,
        files: Vec<String>,
    },
    /// Cannot be determined from a process. Reports [`Observed::Unknown`], which
    /// asks a person rather than guessing.
    Manual { hint: String },
}

// --------------------------------------------------------------- installing --

/// How a component gets there. Declared as data beside its probe, so the same
/// declaration can be executed, printed as instructions, or asserted in a test.
///
/// Deliberately narrow. Every automatic action is an existing `make` target,
/// because the wizard's job is to sequence and explain the installer, not to
/// become a second one that drifts from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InstallAction {
    /// Run a make target and wait for it. Long output is expected and streams.
    ///
    /// `script` names the script that target actually runs, relative to
    /// `scripts/`. A machine that installed from a package has no Makefile, so
    /// without this the capability steps could only ever run from a checkout —
    /// which would leave a downloaded release able to place the backend and
    /// nothing else. Absent means the target genuinely needs a checkout, and
    /// the wizard says so rather than failing at the step.
    Make {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script: Option<String>,
    },
    /// Something only a person can do: grant a permission, load an extension,
    /// paste a key. The wizard shows the steps, optionally opens the right
    /// place, then re-probes until it is done or the person skips.
    Manual {
        /// One instruction per line, in the order they happen.
        steps: Vec<String>,
        /// Opened for them if present — a Settings pane or a browser page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<String>,
        /// Values the wizard can take and write, rather than telling someone to
        /// go and edit a dotfile. Empty for a step that wants a file or a
        /// permission, which no text field can supply.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        secrets: Vec<SecretPrompt>,
    },
    /// Arrives with the runtime and cannot be installed on its own. Core
    /// components use this; offering to "install" one would be a lie.
    WithRuntime,
}

// ---------------------------------------------------------------- the graph --

/// A thing that gets installed or configured. An API key is a component: it is
/// present or it is not, something has to put it there, and features depend on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    /// Human label, shown wherever this component is named to a person.
    pub name: String,
    /// Core components are not declinable and never appear as a choice. The
    /// macOS tray is one: it binds the host gateway that relays the mac skills,
    /// so "install without it" is not a smaller install, it is a broken one.
    /// Core also means "arrives with the runtime", so there is nothing to pull.
    pub core: bool,
    /// Not declinable, but not free either: something still has to install it.
    /// The local embedding model is the case that forced this apart from
    /// `core` — memory is indexed on this machine in both processing modes, so
    /// it cannot be declined, yet it is a multi-gigabyte pull rather than
    /// something that ships with the binary.
    #[serde(default)]
    pub required: bool,
    /// Requirement ids this component satisfies. Several components may provide
    /// the same requirement; that is what makes it a choice rather than a step.
    pub provides: Vec<String>,
    /// Other components that must be usable for this one to work.
    pub requires: Vec<String>,
    /// What the operator trades by choosing this, in their words. Shown at the
    /// moment of choice, which is the only moment it is useful.
    pub cost: String,
    /// What it costs in money, as a field rather than a sentence, so every
    /// surface can say it the same way. Prose alone made "is this going to
    /// charge me" a question you had to read three lines to answer.
    #[serde(default)]
    pub pricing: Pricing,
    /// How it gets installed.
    pub install: InstallAction,
    /// How to find out whether it is there.
    pub probe: ProbeSpec,
    /// Optional data-defined interactive setup flow. The setup catalog owns
    /// provider names and UI fields; callers execute only typed drivers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<setup::ResolvedSetup>,
    /// The machine this component needs before it can be offered at all.
    /// Deliberately not part of [`resolve`]: the resolver answers "does this
    /// work", and whether a machine *can have* something is a different
    /// question, asked before an install rather than after one.
    #[serde(default)]
    pub host: Option<HostRequirement>,
}

/// What a component costs in money.
///
/// Deliberately coarse. Real prices change, vary by usage and are the
/// provider's to state; a graph that carried numbers would be wrong within
/// weeks. What someone needs before choosing is only whether reaching for this
/// means reaching for a card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pricing {
    /// No account and no money. The default, because most of the graph is the
    /// operator's own machine.
    #[default]
    Free,
    /// Free to start and metered after that. Enough to try without deciding.
    FreeTier,
    /// Billed from the first call. Nothing here charges without you putting a
    /// key in, but this is the one that eventually appears on a statement.
    Paid,
}

impl Pricing {
    /// A word for it, short enough to sit beside a component name.
    pub fn label(&self) -> &'static str {
        match self {
            Pricing::Free => "free",
            Pricing::FreeTier => "free tier",
            Pricing::Paid => "paid",
        }
    }

    /// Whether choosing this can put a charge on someone's card.
    pub fn costs_money(&self) -> bool {
        matches!(self, Pricing::Paid | Pricing::FreeTier)
    }
}

/// One value the wizard can ask for and write into the env file.
///
/// The variable is named here rather than parsed out of the step text: prose is
/// for the reader, and a writer that guesses which key a sentence meant is a
/// writer that eventually puts a token under the wrong name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPrompt {
    /// The environment variable to write.
    pub variable: String,
    /// What to call it when asking — the provider's name, not the variable.
    pub label: String,
    /// Optional managed bot whose scoped environment receives this value too.
    /// This keeps routing in the graph instead of a provider-name switch in
    /// Desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
}

/// What a machine has to be for a component to be installable on it.
///
/// Declared as data beside the component so the same sentence is used by the
/// installer that refuses, the wizard that explains, and the page that shows
/// what a smaller machine gives up.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostRequirement {
    /// Minimum physical memory, in whole GiB.
    #[serde(default)]
    pub min_memory_gb: Option<u32>,
    /// Acceptable CPU architectures as `uname -m` reports them. Empty is any.
    #[serde(default)]
    pub arch: Vec<String>,
    /// Acceptable operating systems, lowercased. Empty is any.
    #[serde(default)]
    pub os: Vec<String>,
    /// Why the limit exists, in the operator's words. Shown when it refuses,
    /// because "unsupported" on its own tells nobody what to do next.
    #[serde(default)]
    pub because: String,
}

/// What the machine actually is. Gathered by the caller — this crate does no
/// I/O — so a test can describe any machine without owning one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub memory_gb: u32,
}

impl HostRequirement {
    /// The reason this machine cannot have the component, or `None` if it can.
    /// One sentence, ending in the declared `because`, so a caller never has to
    /// compose the explanation itself.
    pub fn unmet_on(&self, host: &Host) -> Option<String> {
        let mut faults = Vec::new();
        if let Some(min) = self.min_memory_gb {
            if host.memory_gb < min {
                faults.push(format!(
                    "needs {min} GB of memory and this machine has {}",
                    host.memory_gb
                ));
            }
        }
        if !self.arch.is_empty() && !self.arch.iter().any(|a| a == &host.arch) {
            faults.push(format!(
                "needs {} and this machine is {}",
                self.arch.join(" or "),
                host.arch
            ));
        }
        if !self.os.is_empty() && !self.os.iter().any(|o| o == &host.os) {
            faults.push(format!(
                "needs {} and this machine is {}",
                self.os.join(" or "),
                host.os
            ));
        }
        if faults.is_empty() {
            return None;
        }
        let mut reason = faults.join(", and ");
        if !self.because.is_empty() {
            reason.push_str(". ");
            reason.push_str(&self.because);
        }
        Some(reason)
    }
}

impl Component {
    /// Whether declining this is a choice the operator actually has.
    pub fn declinable(&self) -> bool {
        !self.core && !self.required
    }

    /// The reason this machine cannot have it, if it cannot.
    pub fn unsupported_on(&self, host: &Host) -> Option<String> {
        self.host.as_ref().and_then(|h| h.unmet_on(host))
    }
}

/// A named join point that several components can satisfy — "a model", "a
/// browser driver". Named once so that two features needing a model cannot
/// drift apart about what counts as one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub name: String,
    /// Asked when nothing already satisfies this and a choice must be made.
    pub question: String,
}

/// What a feature needs: either one specific component, or any provider of a
/// requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    Component(String),
    Requirement(String),
}

/// Where a feature appears. Surfaces ask for their own list so each can hide
/// what it cannot deliver instead of rendering an entry that fails on click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Web,
    Ios,
    Android,
    Desktop,
    Cli,
}

/// An outcome a person recognises. Nobody wants a browser executor; they want
/// the agent to read their tabs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub name: String,
    pub description: String,
    /// All of these must be satisfied. Alternatives live inside a requirement.
    pub needs: Vec<Need>,
    pub surfaces: Vec<Surface>,
}

/// The declared graph. Data, not behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    pub components: Vec<Component>,
    pub requirements: Vec<Requirement>,
    pub features: Vec<Feature>,
}

impl Graph {
    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    pub fn requirement(&self, id: &str) -> Option<&Requirement> {
        self.requirements.iter().find(|r| r.id == id)
    }

    /// Components that satisfy a requirement, in declaration order. The order is
    /// the graph author's preference and is what a chooser should offer first.
    pub fn providers(&self, requirement: &str) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|c| c.provides.iter().any(|p| p == requirement))
            .collect()
    }

    /// Every id referenced by something that does not exist. A graph that
    /// references a missing component would otherwise fail as a confusing
    /// "unavailable" at runtime instead of a loud error at load.
    pub fn dangling(&self) -> Vec<String> {
        let components: BTreeSet<&str> = self.components.iter().map(|c| c.id.as_str()).collect();
        let requirements: BTreeSet<&str> =
            self.requirements.iter().map(|r| r.id.as_str()).collect();
        let mut bad = Vec::new();
        for c in &self.components {
            for dep in &c.requires {
                if !components.contains(dep.as_str()) {
                    bad.push(format!(
                        "component '{}' requires unknown component '{dep}'",
                        c.id
                    ));
                }
            }
            for provided in &c.provides {
                if !requirements.contains(provided.as_str()) {
                    bad.push(format!(
                        "component '{}' provides unknown requirement '{provided}'",
                        c.id
                    ));
                }
            }
        }
        for f in &self.features {
            for need in &f.needs {
                match need {
                    Need::Component(id) if !components.contains(id.as_str()) => {
                        bad.push(format!("feature '{}' needs unknown component '{id}'", f.id))
                    },
                    Need::Requirement(id) if !requirements.contains(id.as_str()) => bad.push(
                        format!("feature '{}' needs unknown requirement '{id}'", f.id),
                    ),
                    _ => {},
                }
            }
        }
        bad
    }
}

// ------------------------------------------------------------------- state --

/// Operator intent, persisted. Absence of an entry is [`Declared::Unset`], which
/// is what every install predating this registry looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Declared {
    /// Asked for. Absence is a fault worth reporting.
    Wanted,
    /// Turned down. Absence is expected and must not read as an error.
    Declined,
    /// Never expressed either way.
    #[default]
    Unset,
}

/// What a probe found. `Unknown` is a real answer, not a failure to get one:
/// macOS gives no way to read microphone or screen-recording permission from a
/// process that does not hold it, so the honest report is "ask the person".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum Observed {
    Present(String),
    Absent(String),
    Unknown(String),
}

impl Observed {
    pub fn is_present(&self) -> bool {
        matches!(self, Observed::Present(_))
    }

    pub fn detail(&self) -> &str {
        match self {
            Observed::Present(d) | Observed::Absent(d) | Observed::Unknown(d) => d,
        }
    }
}

/// A component's state once intent and observation are combined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ComponentState {
    /// Running. Everything that depends on it works.
    Present { detail: String },
    /// Turned down and absent, exactly as asked. Not a fault.
    Declined,
    /// Asked for but not running. This is the one that deserves attention.
    Missing { detail: String },
    /// Never set up, never declined.
    NotInstalled { detail: String },
    /// A dependency of this component is not usable, so it cannot be either.
    BlockedBy { component: String },
    /// Could not be determined; a person has to confirm.
    Unknown { detail: String },
}

impl ComponentState {
    /// Whether things that depend on this component can work right now. Only
    /// observation grants this; intent never does.
    pub fn usable(&self) -> bool {
        matches!(self, ComponentState::Present { .. })
    }
}

/// Whether a feature works, and if not, precisely what is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Availability {
    Available,
    /// `reason` is written to be shown to a person as-is. `missing` carries the
    /// unsatisfied need ids so a caller can offer to fix them.
    Unavailable {
        reason: String,
        missing: Vec<String>,
    },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub surfaces: Vec<Surface>,
    pub availability: Availability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub id: String,
    pub name: String,
    pub core: bool,
    pub cost: String,
    /// Carried through to the surfaces, because "will this charge me" is asked
    /// of the status list rather than of the graph.
    pub pricing: Pricing,
    pub install: InstallAction,
    pub state: ComponentState,
}

/// What every surface reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub components: Vec<ComponentStatus>,
    pub features: Vec<FeatureStatus>,
}

impl Report {
    /// Features a given surface should render. A surface that renders anything
    /// else is promising something it cannot deliver.
    pub fn available_on(&self, surface: Surface) -> Vec<&FeatureStatus> {
        self.features
            .iter()
            .filter(|f| f.surfaces.contains(&surface) && f.availability.is_available())
            .collect()
    }

    /// Features this surface would show if something were installed, with the
    /// reason. This is what turns a hidden menu entry into an offer.
    pub fn blocked_on(&self, surface: Surface) -> Vec<&FeatureStatus> {
        self.features
            .iter()
            .filter(|f| f.surfaces.contains(&surface) && !f.availability.is_available())
            .collect()
    }

    pub fn component(&self, id: &str) -> Option<&ComponentStatus> {
        self.components.iter().find(|c| c.id == id)
    }

    pub fn feature(&self, id: &str) -> Option<&FeatureStatus> {
        self.features.iter().find(|f| f.id == id)
    }
}

// ---------------------------------------------------------------- resolving --

/// Combine the graph, operator intent and fresh observations into the answer
/// every surface needs.
///
/// Components resolve first, because a component whose own dependency is
/// unusable is itself unusable however healthy its own probe looked. Features
/// resolve second, over usable components only.
pub fn resolve(
    graph: &Graph,
    declared: &BTreeMap<String, Declared>,
    observed: &BTreeMap<String, Observed>,
) -> Report {
    let mut states: BTreeMap<String, ComponentState> = BTreeMap::new();
    for component in &graph.components {
        states.insert(
            component.id.clone(),
            base_state(component, declared, observed),
        );
    }
    propagate_blocked(graph, &mut states);

    let components = graph
        .components
        .iter()
        .map(|c| ComponentStatus {
            id: c.id.clone(),
            name: c.name.clone(),
            core: c.core,
            cost: c.cost.clone(),
            pricing: c.pricing,
            install: c.install.clone(),
            state: states
                .get(&c.id)
                .cloned()
                .unwrap_or(ComponentState::Unknown {
                    detail: "not resolved".to_string(),
                }),
        })
        .collect();

    let features = graph
        .features
        .iter()
        .map(|f| FeatureStatus {
            id: f.id.clone(),
            name: f.name.clone(),
            description: f.description.clone(),
            surfaces: f.surfaces.clone(),
            availability: feature_availability(graph, f, &states),
        })
        .collect();

    Report {
        components,
        features,
    }
}

fn base_state(
    component: &Component,
    declared: &BTreeMap<String, Declared>,
    observed: &BTreeMap<String, Observed>,
) -> ComponentState {
    // Observation wins when it is positive: a component that is running is
    // usable whatever the file says, which is what makes this work on installs
    // that predate any declaration.
    match observed.get(&component.id) {
        Some(Observed::Present(detail)) => {
            return ComponentState::Present {
                detail: detail.clone(),
            }
        },
        Some(Observed::Unknown(detail)) => {
            return ComponentState::Unknown {
                detail: detail.clone(),
            }
        },
        _ => {},
    }

    let detail = observed
        .get(&component.id)
        .map(|o| o.detail().to_string())
        .unwrap_or_else(|| "not probed".to_string());

    // Core components are never "declined"; declining one is not expressible.
    if component.core {
        return ComponentState::Missing { detail };
    }

    match declared.get(&component.id).copied().unwrap_or_default() {
        Declared::Declined => ComponentState::Declined,
        Declared::Wanted => ComponentState::Missing { detail },
        Declared::Unset => ComponentState::NotInstalled { detail },
    }
}

/// A component with an unusable dependency is unusable. Repeats to a fixed
/// point so a chain longer than one link resolves, and because the graph is
/// authored by hand and its order is not guaranteed topological.
fn propagate_blocked(graph: &Graph, states: &mut BTreeMap<String, ComponentState>) {
    loop {
        let mut changed = false;
        for component in &graph.components {
            if !states
                .get(&component.id)
                .map(|s| s.usable())
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(blocker) = component
                .requires
                .iter()
                .find(|dep| !states.get(*dep).map(|s| s.usable()).unwrap_or(false))
            {
                states.insert(
                    component.id.clone(),
                    ComponentState::BlockedBy {
                        component: blocker.clone(),
                    },
                );
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

fn feature_availability(
    graph: &Graph,
    feature: &Feature,
    states: &BTreeMap<String, ComponentState>,
) -> Availability {
    let usable = |id: &str| states.get(id).map(|s| s.usable()).unwrap_or(false);

    let mut missing = Vec::new();
    let mut reasons = Vec::new();

    for need in &feature.needs {
        match need {
            Need::Component(id) => {
                if usable(id) {
                    continue;
                }
                missing.push(id.clone());
                let name = graph.component(id).map(|c| c.name.as_str()).unwrap_or(id);
                reasons.push(format!("needs {name}"));
            },
            Need::Requirement(id) => {
                let providers = graph.providers(id);
                if providers.iter().any(|p| usable(&p.id)) {
                    continue;
                }
                missing.push(id.clone());
                let name = graph.requirement(id).map(|r| r.name.as_str()).unwrap_or(id);
                if providers.is_empty() {
                    reasons.push(format!("needs {name}, which nothing provides"));
                } else {
                    // Requirement names carry their article ("a model", "an
                    // embedding model") so this reads as a sentence rather than
                    // as a field label.
                    let options: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
                    reasons.push(format!("needs {name} — install {}", options.join(" or ")));
                }
            },
        }
    }

    if missing.is_empty() {
        Availability::Available
    } else {
        Availability::Unavailable {
            reason: reasons.join("; "),
            missing,
        }
    }
}

pub mod loader;
pub mod probe;
pub mod selection;

#[cfg(test)]
mod tests;
