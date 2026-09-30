//! Planning a capability selection against the component graph.
//!
//! Setup surfaces should not each invent dependency closure or provider
//! preference. This module is the shared, I/O-free planner used by the terminal
//! installer and Desktop onboarding. Probes and persistence remain with the
//! caller; this code only combines their data.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{resolve, Availability, Declared, Graph, Host, Need, Observed, Report};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionPlan {
    /// Components to install or configure, dependencies before dependants.
    pub install: Vec<String>,
    /// Components this host cannot run, with the reason.
    pub unsupported: Vec<(String, String)>,
    /// Requirements that have no provider in the graph.
    pub unresolved: Vec<String>,
    /// Features the selection leaves unavailable, with the reason.
    pub left_off: Vec<(String, String)>,
}

/// Resolve the work needed for exactly `wanted_features` on `host`.
///
/// Already-present components are never put in `install`. Required non-core
/// components are always planned. When a requirement has several providers,
/// the first supported provider in graph order wins; graph order is the
/// product's declared preference and every setup surface therefore makes the
/// same choice.
pub fn plan_selection(
    graph: &Graph,
    observed: &BTreeMap<String, Observed>,
    wanted_features: &BTreeSet<String>,
    host: &Host,
) -> SelectionPlan {
    let report = resolve(graph, &BTreeMap::new(), observed);
    let usable = |id: &str| {
        report
            .component(id)
            .map(|component| component.state.usable())
            .unwrap_or(false)
    };

    let mut install = Vec::new();
    let mut seen = BTreeSet::new();
    let mut unsupported = Vec::new();
    let mut unresolved = Vec::new();

    for component in graph
        .components
        .iter()
        .filter(|component| component.required && !component.core)
    {
        if !usable(&component.id) {
            add_with_dependencies(
                graph,
                host,
                &component.id,
                &usable,
                &mut install,
                &mut seen,
                &mut unsupported,
            );
        }
    }

    for feature in graph
        .features
        .iter()
        .filter(|feature| wanted_features.contains(&feature.id))
    {
        for need in &feature.needs {
            match need {
                Need::Component(id) => {
                    if !usable(id) {
                        add_with_dependencies(
                            graph,
                            host,
                            id,
                            &usable,
                            &mut install,
                            &mut seen,
                            &mut unsupported,
                        );
                    }
                },
                Need::Requirement(id) => {
                    let providers = graph.providers(id);
                    if providers.iter().any(|provider| usable(&provider.id))
                        || providers.iter().any(|provider| seen.contains(&provider.id))
                    {
                        continue;
                    }
                    match providers
                        .iter()
                        .find(|provider| provider.unsupported_on(host).is_none())
                    {
                        Some(provider) => add_with_dependencies(
                            graph,
                            host,
                            &provider.id,
                            &usable,
                            &mut install,
                            &mut seen,
                            &mut unsupported,
                        ),
                        None if providers.is_empty() => unresolved.push(id.clone()),
                        None => {
                            for provider in providers {
                                let Some(reason) = provider.unsupported_on(host) else {
                                    continue;
                                };
                                if !unsupported.iter().any(|(name, _)| name == &provider.name) {
                                    unsupported.push((provider.name.clone(), reason));
                                }
                            }
                        },
                    }
                },
            }
        }
    }

    let projected = projected_observations(observed, &seen);
    let after = resolve(graph, &BTreeMap::<String, Declared>::new(), &projected);
    let left_off = graph
        .features
        .iter()
        .filter(|feature| !wanted_features.contains(&feature.id))
        .filter_map(|feature| {
            match after
                .feature(&feature.id)
                .map(|status| &status.availability)
            {
                Some(Availability::Unavailable { reason, .. }) => {
                    Some((feature.name.clone(), reason.clone()))
                },
                _ => None,
            }
        })
        .collect();

    SelectionPlan {
        install,
        unsupported,
        unresolved,
        left_off,
    }
}

/// Report the state the selected capabilities will have after the plan runs.
pub fn projected_report(
    graph: &Graph,
    observed: &BTreeMap<String, Observed>,
    plan: &SelectionPlan,
) -> Report {
    let installing = plan.install.iter().cloned().collect();
    resolve(
        graph,
        &BTreeMap::<String, Declared>::new(),
        &projected_observations(observed, &installing),
    )
}

fn add_with_dependencies(
    graph: &Graph,
    host: &Host,
    id: &str,
    usable: &dyn Fn(&str) -> bool,
    install: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    unsupported: &mut Vec<(String, String)>,
) {
    if seen.contains(id) || usable(id) {
        return;
    }
    if let Some(component) = graph.component(id) {
        if let Some(reason) = component.unsupported_on(host) {
            seen.insert(id.to_string());
            unsupported.push((component.name.clone(), reason));
            return;
        }
    }
    seen.insert(id.to_string());
    if let Some(component) = graph.component(id) {
        for dependency in &component.requires {
            add_with_dependencies(graph, host, dependency, usable, install, seen, unsupported);
        }
    }
    install.push(id.to_string());
}

fn projected_observations(
    observed: &BTreeMap<String, Observed>,
    installing: &BTreeSet<String>,
) -> BTreeMap<String, Observed> {
    let mut projected = observed.clone();
    for id in installing {
        projected.insert(id.clone(), Observed::Present("planned".to_string()));
    }
    projected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{self, Paths};

    fn host() -> Host {
        Host {
            os: "macos".to_string(),
            arch: "arm64".to_string(),
            memory_gb: 32,
        }
    }

    #[test]
    fn required_components_are_planned_without_a_feature_selection() {
        let graph = loader::load(&Paths {
            data_root: "/tmp/MagicianNotes".to_string(),
            home: "/tmp".to_string(),
        });
        let plan = plan_selection(&graph, &BTreeMap::new(), &BTreeSet::new(), &host());
        assert!(plan.install.contains(&"ollama-embedding".to_string()));
    }

    #[test]
    fn an_existing_provider_prevents_a_redundant_install() {
        let graph = loader::load(&Paths {
            data_root: "/tmp/MagicianNotes".to_string(),
            home: "/tmp".to_string(),
        });
        let observed = BTreeMap::from([
            (
                "ollama-embedding".to_string(),
                Observed::Present("ready".to_string()),
            ),
            (
                "remote-models".to_string(),
                Observed::Present("configured".to_string()),
            ),
        ]);
        let wanted = BTreeSet::from(["chat".to_string()]);
        let plan = plan_selection(&graph, &observed, &wanted, &host());
        assert!(!plan.install.contains(&"remote-models".to_string()));
    }
}
