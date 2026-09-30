//! What the wizard is deciding, kept apart from how it is drawn.
//!
//! The screen is a view over this. Keeping the two apart is what lets the
//! non-interactive mode answer from environment variables and still take the
//! same path: it drives this model and never constructs a terminal.

use std::collections::{BTreeMap, BTreeSet};

use magician_components::{Graph, Observed, Report};

pub use magician_components::selection::SelectionPlan as Plan;

/// The wizard's whole state.
pub struct Selection {
    pub graph: Graph,
    /// What probes found. Fixed for a run: re-probing mid-selection would make
    /// the plan shift under the cursor for reasons the reader cannot see.
    pub observed: BTreeMap<String, Observed>,
    /// Features the operator has asked for.
    wanted: BTreeSet<String>,
    /// What this machine is. Some components cannot go on some machines at all,
    /// and that is answered before offering a step rather than after failing
    /// one.
    pub host: magician_components::Host,
    pub cursor: usize,
}

impl Selection {
    /// Start from what is already working. Someone re-running the wizard on a
    /// configured machine should not have to re-choose what they already have,
    /// and someone running it fresh starts from an honest empty set.
    /// `remembered` answers "did they choose this last time", or `None` if they
    /// were never asked. A capability added since the last run is unrecorded,
    /// and falls back to whether it already works rather than defaulting to off
    /// — otherwise a new capability would arrive silently switched off.
    pub fn with_remembered(
        graph: Graph,
        observed: BTreeMap<String, Observed>,
        remembered: impl Fn(&str) -> Option<bool>,
    ) -> Self {
        let report = magician_components::resolve(&graph, &BTreeMap::new(), &observed);
        let wanted = graph
            .features
            .iter()
            .filter(|f| {
                remembered(&f.id).unwrap_or_else(|| {
                    report
                        .feature(&f.id)
                        .map(|s| s.availability.is_available())
                        .unwrap_or(false)
                })
            })
            .map(|f| f.id.clone())
            .collect();
        Selection {
            graph,
            observed,
            wanted,
            host: magician_components::probe::detect_host(),
            cursor: 0,
        }
    }

    /// Why this machine cannot have a component, if it cannot. One place, so
    /// the plan, the screen and the engine cannot disagree about it.
    pub fn unsupported(&self, component_id: &str) -> Option<String> {
        self.graph
            .component(component_id)
            .and_then(|c| c.unsupported_on(&self.host))
    }

    /// The capabilities currently chosen, for recording.
    pub fn wanted_ids(&self) -> impl Iterator<Item = &String> {
        self.wanted.iter()
    }

    pub fn features(&self) -> &[magician_components::Feature] {
        &self.graph.features
    }

    pub fn is_wanted(&self, feature_id: &str) -> bool {
        self.wanted.contains(feature_id)
    }

    /// Choose exactly these capabilities, replacing whatever was chosen.
    ///
    /// The screen never calls this — a person moves a cursor and toggles. It
    /// exists so a described machine can state its selection outright instead
    /// of being driven through the cursor, which is how the simulator covers
    /// selections nobody would sit and click through.
    pub fn set_wanted(&mut self, feature_ids: &[String]) {
        self.wanted = feature_ids.iter().cloned().collect();
    }

    pub fn toggle_at_cursor(&mut self) {
        if let Some(feature) = self.graph.features.get(self.cursor) {
            let id = feature.id.clone();
            if !self.wanted.remove(&id) {
                self.wanted.insert(id);
            }
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.graph.features.len();
        if len == 0 {
            return;
        }
        let next = self.cursor as isize + delta;
        self.cursor = next.rem_euclid(len as isize) as usize;
    }

    /// State as it stands, with nothing declared: what works right now.
    pub fn report(&self) -> Report {
        magician_components::resolve(&self.graph, &BTreeMap::new(), &self.observed)
    }

    /// The components the current selection needs and does not already have.
    ///
    /// Where a requirement has several providers and none is present, the first
    /// declared wins. Declaration order is the graph author's preference, and
    /// picking silently is only acceptable because the plan screen shows what
    /// was picked and what it costs — a choice made invisibly would not be.
    pub fn plan(&self) -> Plan {
        magician_components::selection::plan_selection(
            &self.graph,
            &self.observed,
            &self.wanted,
            &self.host,
        )
    }

    /// The report as it would be after the plan runs. This is what the plan
    /// screen shows, so a reader sees the outcome rather than the current state.
    pub fn projected_report(&self) -> Report {
        let plan = self.plan();
        magician_components::selection::projected_report(&self.graph, &self.observed, &plan)
    }
}

#[cfg(test)]
mod tests;
