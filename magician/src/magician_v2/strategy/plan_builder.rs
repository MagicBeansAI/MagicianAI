use std::collections::{HashMap, HashSet};

use super::{
    plan::{PlanEdge, PlanGraph, PlanProvenance, PlanStep},
    types::ExplorationNode,
};

/// Build a DAG-oriented plan graph from the exploration tree.
pub fn build_plan_graph(
    nodes: &HashMap<String, ExplorationNode>,
    root_id: &str,
    overall_confidence: f32,
) -> Option<PlanGraph> {
    if nodes.is_empty() {
        return None;
    }

    let mut include: HashSet<String> = HashSet::new();
    include.insert(root_id.to_string());

    // Include all terminal nodes and their ancestors
    for node in nodes.values().filter(|n| n.is_terminal) {
        let mut current = Some(node.id.clone());
        while let Some(id) = current {
            if !include.insert(id.clone()) {
                // already visited
            }
            current = nodes.get(&id).and_then(|n| n.parent_id.clone());
        }
    }

    // Ensure nodes with concrete tools are included along with ancestors and
    // dependencies
    for node in nodes.values().filter(|n| n.tool_match.is_some()) {
        let mut current = Some(node.id.clone());
        while let Some(id) = current {
            include.insert(id.clone());
            current = nodes.get(&id).and_then(|n| n.parent_id.clone());
        }
        for dep in &node.dependencies {
            include.insert(dep.clone());
        }
    }

    if include.is_empty() {
        include.insert(root_id.to_string());
    }

    // Collect nodes sorted by depth to ensure deterministic order
    let mut selected_nodes: Vec<&ExplorationNode> =
        include.iter().filter_map(|id| nodes.get(id)).collect();
    selected_nodes.sort_by_key(|node| node.depth);

    if selected_nodes.is_empty() {
        return None;
    }

    let mut steps = Vec::new();
    let mut added = HashSet::new();

    for node in selected_nodes {
        if added.contains(&node.id) {
            continue;
        }

        let mut step = node.partial_plan.clone().unwrap_or_else(|| PlanStep {
            id: node.id.clone(),
            task: node.task.clone(),
            tool: node
                .tool_match
                .as_ref()
                .and_then(|tm| tm.primary_match.as_ref())
                .map(|pm| pm.tool_name.clone()),
            parameters: node
                .available_parameters
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
            expected_outputs: Vec::new(),
            confidence: node.confidence,
            metadata: HashMap::new(),
            timeout_override_secs: None,
            ..Default::default()
        });

        step.id = node.id.clone();
        step.task = node.task.clone();
        if step.tool.is_none() {
            step.tool = node
                .tool_match
                .as_ref()
                .and_then(|tm| tm.primary_match.as_ref())
                .map(|pm| pm.tool_name.clone());
        }
        if step.parameters.is_empty() {
            step.parameters = node
                .available_parameters
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
        }
        step.confidence = node.confidence;
        step.metadata
            .entry("depth".to_string())
            .or_insert_with(|| node.depth.to_string());
        step.metadata
            .entry("priority".to_string())
            .or_insert_with(|| format!("{:.2}", node.priority));

        if let Some(parent) = node.parent_id.as_ref() {
            step.metadata
                .entry("parent".to_string())
                .or_insert_with(|| parent.clone());

            let sibling_count = nodes.get(parent).map(|p| p.children.len()).unwrap_or(0);
            if sibling_count > 1 && node.dependencies.is_empty() {
                step.metadata
                    .entry("parallel_candidate".to_string())
                    .or_insert_with(|| "true".to_string());
            }
        }

        steps.push(step);
        added.insert(node.id.clone());
    }

    // Build edges
    let mut edge_set: HashSet<(String, String, String)> = HashSet::new();
    for node in nodes.values() {
        if !include.contains(&node.id) {
            continue;
        }

        if let Some(parent) = &node.parent_id {
            if include.contains(parent) {
                edge_set.insert((
                    parent.clone(),
                    node.id.clone(),
                    "parent_dependency".to_string(),
                ));
            }
        }

        for dep in &node.dependencies {
            if include.contains(dep) {
                edge_set.insert((
                    dep.clone(),
                    node.id.clone(),
                    "explicit_dependency".to_string(),
                ));
            }
        }
    }

    let mut edges: Vec<PlanEdge> = edge_set
        .into_iter()
        .map(|(from, to, reason)| PlanEdge { from, to, reason })
        .collect();
    edges.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));

    Some(PlanGraph {
        steps,
        edges,
        unresolved_inputs: Vec::new(),
        confidence: overall_confidence,
        provenance: PlanProvenance {
            strategy: "GuidedSearch".to_string(),
            generator: Some("plan_builder".to_string()),
            notes: None,
        },
        ..Default::default()
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::strategy::types::ExplorationNode;

    fn node(id: &str, task: &str, parent: Option<&str>, depth: u32) -> ExplorationNode {
        let mut n = ExplorationNode::new(
            id.to_string(),
            task.to_string(),
            parent.map(|p| p.to_string()),
            depth,
        );
        n.partial_plan = Some(PlanStep {
            id: id.to_string(),
            task: task.to_string(),
            tool: None,
            parameters: HashMap::new(),
            expected_outputs: Vec::new(),
            confidence: 0.0,
            metadata: HashMap::new(),
            timeout_override_secs: None,
            ..Default::default()
        });
        n
    }

    #[test]
    fn build_plan_graph_preserves_parallel_branches() {
        let mut nodes: HashMap<String, ExplorationNode> = HashMap::new();

        let mut root = node("root", "Root", None, 0);
        root.is_terminal = false;
        nodes.insert(root.id.clone(), root);

        let mut child_a = node("child_a", "Fetch data", Some("root"), 1);
        child_a.is_terminal = true;
        nodes.insert(child_a.id.clone(), child_a);

        let mut child_b = node("child_b", "Collect metrics", Some("root"), 1);
        child_b.is_terminal = true;
        nodes.insert(child_b.id.clone(), child_b);

        let mut child_c = node("child_c", "Aggregate results", Some("root"), 1);
        child_c.dependencies = vec!["child_a".to_string(), "child_b".to_string()];
        child_c.is_terminal = true;
        nodes.insert(child_c.id.clone(), child_c);

        // Populate children lists for parent relationships
        nodes.get_mut("root").unwrap().children.extend([
            "child_a".to_string(),
            "child_b".to_string(),
            "child_c".to_string(),
        ]);

        let graph = build_plan_graph(&nodes, "root", 0.8).expect("plan graph");

        let edge_set: std::collections::HashSet<(String, String, String)> = graph
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone(), e.reason.clone()))
            .collect();

        assert!(edge_set.contains(&("root".into(), "child_a".into(), "parent_dependency".into())));
        assert!(edge_set.contains(&("root".into(), "child_b".into(), "parent_dependency".into())));
        assert!(edge_set.contains(&(
            "child_a".into(),
            "child_c".into(),
            "explicit_dependency".into()
        )));
        assert!(edge_set.contains(&(
            "child_b".into(),
            "child_c".into(),
            "explicit_dependency".into()
        )));

        let step_meta = graph
            .steps
            .iter()
            .find(|step| step.id == "child_a")
            .expect("child_a step")
            .metadata
            .get("parallel_candidate");
        assert_eq!(step_meta, Some(&"true".to_string()));
    }
}
