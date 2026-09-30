use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde_json::Value;

use super::plan::{PlanGraph, UnresolvedInput};
use runtime_core::ToolInfo;

/// Validate a plan graph against the available tool catalog.
///
/// Returns the list of unresolved inputs (missing parameters) if validation
/// succeeds. Returns an error if the plan is structurally invalid (cycles,
/// missing tools, invalid edges).
pub fn validate_plan(
    graph: &PlanGraph,
    tool_catalog: &[ToolInfo],
    _require_atomic: bool,
) -> Result<Vec<UnresolvedInput>> {
    if graph.steps.is_empty() {
        return Ok(Vec::new());
    }

    // Index steps
    let mut steps_by_id: HashMap<&str, &super::plan::PlanStep> = HashMap::new();
    for step in &graph.steps {
        if steps_by_id.insert(&step.id, step).is_some() {
            bail!("Duplicate step identifier in plan graph: {}", step.id);
        }
    }

    // Validate edges reference existing nodes and build adjacency list
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        if !steps_by_id.contains_key(edge.from.as_str()) {
            bail!("Edge references unknown source step: {}", edge.from);
        }
        if !steps_by_id.contains_key(edge.to.as_str()) {
            bail!("Edge references unknown target step: {}", edge.to);
        }
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }

    // Cycle detection via DFS
    let mut visiting: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    for step in &graph.steps {
        if !visited.contains(step.id.as_str())
            && has_cycle(step.id.as_str(), &adjacency, &mut visiting, &mut visited)
        {
            bail!("Plan graph contains a cycle starting at step {}", step.id);
        }
    }

    // Build tool lookup
    let mut tools_by_name: HashMap<&str, &ToolInfo> = HashMap::new();
    for tool in tool_catalog {
        tools_by_name.insert(tool.name.as_str(), tool);
    }

    let mut input_holes: Vec<UnresolvedInput> = Vec::new();

    for step in &graph.steps {
        let Some(tool_name) = &step.tool else {
            continue;
        };

        let Some(tool_info) = tools_by_name.get(tool_name.as_str()) else {
            bail!("Plan references unknown tool: {}", tool_name);
        };

        for param in &tool_info.parameters {
            if param.required && !step.parameters.contains_key(&param.name) {
                input_holes.push(UnresolvedInput::from_legacy(
                    param.name.clone(),
                    Some(step.id.clone()),
                    Some(param.param_type.clone()),
                    Some(format!(
                        "Provide value for parameter '{}' of tool '{}'",
                        param.name, tool_name
                    )),
                    Some("required parameter missing".to_string()),
                ));
            }
        }

        // `composition_category` is optional and most scope-installed skills
        // omit it, so gating on `Some(..)` alone let a pack without the field
        // skip BOTH checks below silently — a missing optional field switched
        // off validation. Fall back to the tool's `categories` only when the
        // field is absent: where it IS declared it stays the sole signal, so
        // packs that already declare it (shell → `shell_operations`, files →
        // `file_operations`, read_file/grep/glob → `action`) keep their exact
        // current behaviour rather than newly tripping the rationale gate.
        let category_signals: Vec<String> = match &tool_info.composition_category {
            Some(category) => vec![category.to_lowercase()],
            None => tool_info
                .categories
                .iter()
                .map(|category| category.to_lowercase())
                .collect(),
        };

        let metadata = &step.metadata;

        if category_signals
            .iter()
            .any(|category| category.starts_with("browser"))
        {
            enforce_session_metadata(step.id.as_str(), metadata)?;
            enforce_observation_metadata(step.id.as_str(), metadata)?;
        }

        if category_signals
            .iter()
            .any(|category| category.contains("shell") || category.contains("file"))
        {
            let rationale = metadata.get("rationale").map(|s| s.trim()).unwrap_or("");
            if rationale.is_empty() {
                bail!(
                    "Atomic step '{}' using tool '{}' must include a rationale explaining the shell/file action",
                    step.id,
                    tool_name
                );
            }
        }
    }

    Ok(input_holes)
}

fn enforce_session_metadata(step_id: &str, metadata: &HashMap<String, String>) -> Result<()> {
    let Some(raw_session) = metadata.get("session") else {
        bail!("Browser step '{}' is missing session metadata", step_id);
    };

    let session: Value = serde_json::from_str(raw_session).map_err(|e| {
        anyhow::anyhow!(
            "Browser step '{}' has invalid session metadata ({}): {}",
            step_id,
            raw_session,
            e
        )
    })?;

    let has_session_id = session
        .get("session_id")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let new_session = session
        .get("new_session")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reuse_session = session
        .get("reuse_session")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !(has_session_id || new_session || reuse_session) {
        bail!(
            "Browser step '{}' must specify session metadata (session_id or new/reuse flags)",
            step_id
        );
    }

    Ok(())
}

fn enforce_observation_metadata(step_id: &str, metadata: &HashMap<String, String>) -> Result<()> {
    let Some(raw_observation) = metadata.get("observation_checkpoint") else {
        bail!(
            "Browser step '{}' must specify an observation checkpoint for verification",
            step_id
        );
    };

    let observation: Value = serde_json::from_str(raw_observation).map_err(|e| {
        anyhow::anyhow!(
            "Step '{}' has invalid observation checkpoint ({}): {}",
            step_id,
            raw_observation,
            e
        )
    })?;

    let has_expected_state = observation
        .get("expected_state")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_notes = observation
        .get("notes")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_screenshot = observation
        .get("screenshot")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    if !(has_expected_state || has_notes || has_screenshot) {
        bail!(
            "Observation checkpoint for step '{}' must include expected_state, notes, or screenshot guidance",
            step_id
        );
    }

    Ok(())
}

fn has_cycle(
    node: &str,
    adjacency: &HashMap<String, Vec<String>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> bool {
    if visiting.contains(node) {
        return true;
    }
    if visited.contains(node) {
        return false;
    }

    visiting.insert(node.to_string());

    if let Some(neighbors) = adjacency.get(node) {
        for next in neighbors {
            if has_cycle(next, adjacency, visiting, visited) {
                return true;
            }
        }
    }

    visiting.remove(node);
    visited.insert(node.to_string());
    false
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use runtime_core::{ParameterDefinition, ToolInfo};

    fn tool_info(name: &str, required_param: &str) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: format!("{} description", name),
            category: "test".to_string(),
            categories: vec!["test".to_string()], // Already populated correctly
            parameters: vec![ParameterDefinition {
                name: required_param.to_string(),
                param_type: "string".to_string(),
                required: true,
                description: "Required value".to_string(),
                validation_rules: vec![],
                default_value: None,
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: Some("test".to_string()),
            providing_agent_id: None,
        }
    }

    #[test]
    fn validator_returns_input_hole_for_missing_parameter() {
        let plan = PlanGraph {
            steps: vec![super::super::plan::PlanStep {
                id: "step-1".to_string(),
                task: "Do something".to_string(),
                tool: Some("test_tool".to_string()),
                parameters: HashMap::new(),
                expected_outputs: Vec::new(),
                confidence: 0.9,
                metadata: HashMap::new(),
                timeout_override_secs: None,
                ..Default::default()
            }],
            edges: Vec::new(),
            unresolved_inputs: Vec::new(),
            confidence: 0.9,
            provenance: super::super::plan::PlanProvenance::default(),
            ..Default::default()
        };

        let result = validate_plan(&plan, &[tool_info("test_tool", "target")], true)
            .expect("validation should succeed");

        assert_eq!(result.len(), 1);
        let input = &result[0];
        assert_eq!(input.parameter, "target");
        assert!(input.prompt.contains("Provide value for parameter"));
    }

    #[test]
    fn validator_rejects_unknown_tool() {
        let plan = PlanGraph {
            steps: vec![super::super::plan::PlanStep {
                id: "step-1".to_string(),
                task: "Do something".to_string(),
                tool: Some("missing_tool".to_string()),
                parameters: HashMap::new(),
                expected_outputs: Vec::new(),
                confidence: 0.9,
                metadata: HashMap::new(),
                timeout_override_secs: None,
                ..Default::default()
            }],
            edges: Vec::new(),
            unresolved_inputs: Vec::new(),
            confidence: 0.9,
            provenance: super::super::plan::PlanProvenance::default(),
            ..Default::default()
        };

        let err = validate_plan(&plan, &[], true).expect_err("validation should fail");
        assert!(err.to_string().contains("unknown tool"));
    }

    #[test]
    fn validator_requires_session_for_browser_steps() {
        let plan = PlanGraph {
            steps: vec![super::super::plan::PlanStep {
                id: "step-1".to_string(),
                task: "Open dashboard".to_string(),
                tool: Some("browser_tool".to_string()),
                parameters: HashMap::new(),
                expected_outputs: Vec::new(),
                confidence: 0.9,
                metadata: HashMap::new(),
                timeout_override_secs: None,
                ..Default::default()
            }],
            edges: Vec::new(),
            unresolved_inputs: Vec::new(),
            confidence: 0.9,
            provenance: super::super::plan::PlanProvenance::default(),
            ..Default::default()
        };

        let tool = ToolInfo {
            name: "browser_tool".to_string(),
            description: "Browser step".to_string(),
            category: "browser".to_string(),
            categories: vec!["browser".to_string()],
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: Some("browser_navigation".to_string()),
            providing_agent_id: None,
        };

        let err = validate_plan(&plan, &[tool], true).expect_err("validation should fail");
        assert!(err
            .to_string()
            .contains("Browser step 'step-1' is missing session metadata"));
    }

    #[test]
    fn validator_requires_rationale_for_shell_steps() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "session".to_string(),
            "{\"session_id\":\"shell\"}".to_string(),
        );
        metadata.insert(
            "observation_checkpoint".to_string(),
            "{\"expected_state\":\"Command output visible\"}".to_string(),
        );

        let plan = PlanGraph {
            steps: vec![super::super::plan::PlanStep {
                id: "step-2".to_string(),
                task: "Execute script".to_string(),
                tool: Some("shell_tool".to_string()),
                parameters: HashMap::new(),
                expected_outputs: Vec::new(),
                confidence: 0.9,
                metadata,
                timeout_override_secs: None,
                ..Default::default()
            }],
            edges: Vec::new(),
            unresolved_inputs: Vec::new(),
            confidence: 0.9,
            provenance: super::super::plan::PlanProvenance::default(),
            ..Default::default()
        };

        let tool = ToolInfo {
            name: "shell_tool".to_string(),
            description: "Shell step".to_string(),
            category: "shell".to_string(),
            categories: vec!["shell".to_string()],
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: Some("shell_operations".to_string()),
            providing_agent_id: None,
        };

        let err = validate_plan(&plan, &[tool], true).expect_err("validation should fail");
        assert!(err
            .to_string()
            .contains("must include a rationale explaining the shell/file action"));
    }
}
