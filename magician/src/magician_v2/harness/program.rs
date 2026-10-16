use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;

use crate::magician_v2::agents::{resolve_focus_area_for_goal_id, AgentDefinition};
use crate::magician_v2::artifact_v2::io::write_bytes_durably;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const DEFAULT_PROGRAM_DOC: &str = "program.md";
const PROGRAM_RUNTIME_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct LoadedProgram {
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramRuntimeScope {
    pub principal: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramRuntimeProgramRef {
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state_relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramStateUpdateProvenance {
    pub updated_at: DateTime<Utc>,
    pub actor: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub patch: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramRuntimeState {
    pub schema_version: u32,
    pub scope: ProgramRuntimeScope,
    pub program: ProgramRuntimeProgramRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_loops: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_action_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_conditions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_learning_candidates: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_meta_harness_verdict: Option<Value>,
    /// Boundary C: the continuation this lane's most recent cycle settled on,
    /// with the budget that justified it. Bookkeeping, like its neighbours —
    /// it records what a cycle decided, and grants no authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cycle_decision: Option<Value>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
    pub update_reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update_history: Vec<ProgramStateUpdateProvenance>,
}

#[derive(Debug, Clone)]
pub struct ProgramLoader {
    workspace: ArtifactV2Workspace,
}

impl ProgramLoader {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    pub async fn load_for_goal(
        &self,
        principal: &str,
        workspace: &str,
        definition: &AgentDefinition,
        goal_id: Option<&str>,
    ) -> Result<Option<LoadedProgram>, String> {
        let focus_area =
            goal_id.and_then(|goal_id| resolve_focus_area_for_goal_id(definition, goal_id));
        self.load_for_focus_area(principal, workspace, definition, focus_area)
            .await
    }

    pub async fn load_for_focus_area(
        &self,
        principal: &str,
        workspace: &str,
        definition: &AgentDefinition,
        focus_area: Option<&crate::magician_v2::agents::types::FocusArea>,
    ) -> Result<Option<LoadedProgram>, String> {
        let programs_root = self.workspace.program_specs_root(principal, workspace);

        let (relative_path, section) = if let Some(path) = focus_area
            .and_then(|focus_area| focus_area.program.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            (normalize_program_path(path)?, None)
        } else {
            (
                PathBuf::from(DEFAULT_PROGRAM_DOC),
                definition
                    .harness
                    .as_ref()
                    .and_then(|config| config.program_section.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            )
        };

        let absolute_path = programs_root.join(&relative_path);
        if !fs::try_exists(&absolute_path).await.map_err(|error| {
            format!(
                "failed to check program document `{}`: {error}",
                absolute_path.display()
            )
        })? {
            return Ok(None);
        }

        let raw = fs::read_to_string(&absolute_path).await.map_err(|error| {
            format!(
                "failed to read program document `{}`: {error}",
                absolute_path.display()
            )
        })?;

        let content = if let Some(section_name) = section.as_deref() {
            extract_markdown_section(&raw, section_name).ok_or_else(|| {
                format!(
                    "section `{section_name}` was not found in program document `{}`",
                    relative_path.display()
                )
            })?
        } else {
            raw.trim().to_string()
        };

        if content.is_empty() {
            return Ok(None);
        }

        Ok(Some(LoadedProgram {
            relative_path: relative_path.display().to_string(),
            section,
            title: first_heading(&raw),
            content,
        }))
    }

    pub async fn load_by_reference(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
        section: Option<&str>,
    ) -> Result<Option<LoadedProgram>, String> {
        let relative_path = normalize_program_path(relative_path)?;
        let absolute_path = self
            .workspace
            .program_specs_root(principal, workspace)
            .join(&relative_path);
        if !fs::try_exists(&absolute_path).await.map_err(|error| {
            format!(
                "failed to check program document `{}`: {error}",
                absolute_path.display()
            )
        })? {
            return Ok(None);
        }
        let raw = fs::read_to_string(&absolute_path).await.map_err(|error| {
            format!(
                "failed to read program document `{}`: {error}",
                absolute_path.display()
            )
        })?;
        let normalized_section = section
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let content = if let Some(section_name) = normalized_section.as_deref() {
            extract_markdown_section(&raw, section_name).ok_or_else(|| {
                format!(
                    "section `{section_name}` was not found in program document `{}`",
                    relative_path.display()
                )
            })?
        } else {
            raw.trim().to_string()
        };
        if content.is_empty() {
            return Ok(None);
        }
        Ok(Some(LoadedProgram {
            relative_path: relative_path.display().to_string(),
            section: normalized_section,
            title: first_heading(&raw),
            content,
        }))
    }

    pub fn runtime_state_relative_path(
        &self,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
    ) -> String {
        format!(
            "programs/state/{}",
            runtime_state_file_name(loaded, goal_id)
        )
    }

    pub fn runtime_state_path(
        &self,
        principal: &str,
        workspace: &str,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
    ) -> PathBuf {
        self.workspace.program_runtime_state_path(
            principal,
            workspace,
            &runtime_state_file_name(loaded, goal_id),
        )
    }

    pub async fn load_runtime_state(
        &self,
        principal: &str,
        workspace: &str,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
    ) -> Result<Option<ProgramRuntimeState>, String> {
        let path = self.runtime_state_path(principal, workspace, loaded, goal_id);
        if !fs::try_exists(&path).await.map_err(|error| {
            format!(
                "failed to check program runtime state `{}`: {error}",
                path.display()
            )
        })? {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path).await.map_err(|error| {
            format!(
                "failed to read program runtime state `{}`: {error}",
                path.display()
            )
        })?;
        let mut state: ProgramRuntimeState = serde_json::from_str(&raw).map_err(|error| {
            format!(
                "failed to parse program runtime state `{}`: {error}",
                path.display()
            )
        })?;
        state.schema_version = PROGRAM_RUNTIME_STATE_SCHEMA_VERSION;
        state.scope = ProgramRuntimeScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        };
        state.program.relative_path = loaded.relative_path.clone();
        state.program.section = loaded.section.clone();
        state.program.title = loaded.title.clone();
        state.program.state_relative_path = self.runtime_state_relative_path(loaded, goal_id);
        if state.goal_id.is_none() {
            state.goal_id = goal_id.map(str::to_string);
        }
        Ok(Some(state))
    }

    pub async fn load_or_create_runtime_state(
        &self,
        principal: &str,
        workspace: &str,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
    ) -> Result<ProgramRuntimeState, String> {
        if let Some(state) = self
            .load_runtime_state(principal, workspace, loaded, goal_id)
            .await?
        {
            return Ok(state);
        }
        let state = ProgramRuntimeState::new(
            principal,
            workspace,
            loaded,
            goal_id,
            self.runtime_state_relative_path(loaded, goal_id),
            "program_loader",
            "initialized runtime state for program spec",
            Value::Object(Default::default()),
        );
        self.write_runtime_state(principal, workspace, loaded, goal_id, &state)
            .await?;
        Ok(state)
    }

    pub async fn write_runtime_state(
        &self,
        principal: &str,
        workspace: &str,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
        state: &ProgramRuntimeState,
    ) -> Result<(), String> {
        let path = self.runtime_state_path(principal, workspace, loaded, goal_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(|error| {
                format!(
                    "failed to create program runtime state directory `{}`: {error}",
                    parent.display()
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(state)
            .map_err(|error| format!("failed to serialize program runtime state: {error}"))?;
        // Durable publish rather than an in-place `fs::write`: the harness
        // rewrites this file on every loop iteration, and a torn one fails
        // `load_runtime_state`'s parse — which is the whole program's memory of
        // where it got to.
        let body = format!("{raw}\n");
        write_bytes_durably(&path, body.as_bytes())
            .await
            .map_err(|error| {
                format!(
                    "failed to write program runtime state `{}`: {error}",
                    path.display()
                )
            })
    }
}

impl ProgramRuntimeState {
    pub fn new(
        principal: &str,
        workspace: &str,
        loaded: &LoadedProgram,
        goal_id: Option<&str>,
        state_relative_path: String,
        actor: impl Into<String>,
        reason: impl Into<String>,
        patch: Value,
    ) -> Self {
        let now = Utc::now();
        let actor = actor.into();
        let reason = reason.into();
        Self {
            schema_version: PROGRAM_RUNTIME_STATE_SCHEMA_VERSION,
            scope: ProgramRuntimeScope {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
            },
            program: ProgramRuntimeProgramRef {
                relative_path: loaded.relative_path.clone(),
                section: loaded.section.clone(),
                title: loaded.title.clone(),
                state_relative_path,
            },
            goal_id: goal_id.map(str::to_string),
            current_phase: None,
            current_step: None,
            open_loops: Vec::new(),
            last_run_summary: None,
            next_action_hints: Vec::new(),
            blocked: None,
            stop_conditions: Vec::new(),
            recent_learning_candidates: Vec::new(),
            last_meta_harness_verdict: None,
            last_cycle_decision: None,
            updated_at: now,
            updated_by: actor.clone(),
            update_reason: reason.clone(),
            update_history: vec![ProgramStateUpdateProvenance {
                updated_at: now,
                actor,
                reason,
                candidate_id: None,
                task_id: None,
                execution_id: None,
                patch,
            }],
        }
    }

    pub fn apply_update(
        &mut self,
        patch: Value,
        actor: impl Into<String>,
        reason: impl Into<String>,
        candidate_id: Option<String>,
        task_id: Option<String>,
        execution_id: Option<String>,
    ) -> Result<(), String> {
        let Value::Object(object) = &patch else {
            return Err("program runtime state patch must be a JSON object".to_string());
        };

        for (key, value) in object {
            match key.as_str() {
                "current_phase" => self.current_phase = optional_string_value(value, key)?,
                "current_step" => self.current_step = optional_string_value(value, key)?,
                "open_loops" => self.open_loops = array_value(value, key)?,
                "last_run_summary" => self.last_run_summary = optional_json_value(value),
                "next_action_hints" => self.next_action_hints = string_array_value(value, key)?,
                "blocked" | "blocked_status" | "escalation_status" => {
                    self.blocked = optional_json_value(value);
                },
                "stop_conditions" | "stop_condition_status" => {
                    self.stop_conditions = array_value(value, key)?;
                },
                "recent_learning_candidates" | "learning_candidates" => {
                    self.recent_learning_candidates = array_value(value, key)?;
                },
                "last_meta_harness_verdict" | "meta_harness_verdict" => {
                    self.last_meta_harness_verdict = optional_json_value(value);
                },
                "last_cycle_decision" => self.last_cycle_decision = optional_json_value(value),
                "goal_id" => self.goal_id = optional_string_value(value, key)?,
                "program" | "scope" | "schema_version" | "updated_at" | "updated_by"
                | "update_reason" | "update_history" => {
                    return Err(format!(
                        "program runtime state patch cannot modify immutable field `{key}`"
                    ));
                },
                _ => {
                    return Err(format!(
                        "program runtime state patch contains unsupported field `{key}`"
                    ));
                },
            }
        }

        let now = Utc::now();
        let actor = actor.into();
        let reason = reason.into();
        self.updated_at = now;
        self.updated_by = actor.clone();
        self.update_reason = reason.clone();
        self.update_history.push(ProgramStateUpdateProvenance {
            updated_at: now,
            actor,
            reason,
            candidate_id,
            task_id,
            execution_id,
            patch,
        });
        if self.update_history.len() > 50 {
            let drop_count = self.update_history.len() - 50;
            self.update_history.drain(0..drop_count);
        }
        Ok(())
    }
}

fn normalize_program_path(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw.trim());
    if path.as_os_str().is_empty() {
        return Err("program document path must not be empty".to_string());
    }
    if path.is_absolute() {
        return Err(format!(
            "program document path `{}` must be relative",
            path.display()
        ));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {},
            _ => {
                return Err(format!(
                    "program document path `{}` contains unsupported traversal components",
                    path.display()
                ))
            },
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err("program document path must not resolve to the programs root".to_string());
    }

    Ok(normalized)
}

fn runtime_state_file_name(loaded: &LoadedProgram, goal_id: Option<&str>) -> String {
    let raw = format!(
        "{}::{}::{}",
        loaded.relative_path,
        loaded.section.as_deref().unwrap_or_default(),
        goal_id.unwrap_or_default()
    );
    let hash_hex = blake3::hash(raw.as_bytes()).to_hex().to_string();
    let stem = Path::new(&loaded.relative_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(slug_fragment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "program".to_string());
    let section = loaded
        .section
        .as_deref()
        .map(slug_fragment)
        .filter(|value| !value.is_empty());
    let goal = goal_id.map(slug_fragment).filter(|value| !value.is_empty());
    let mut prefix = stem;
    if let Some(section) = section {
        prefix.push('-');
        prefix.push_str(&section);
    }
    if let Some(goal) = goal {
        prefix.push('-');
        prefix.push_str(&goal);
    }
    if prefix.len() > 72 {
        prefix.truncate(72);
        prefix = prefix.trim_end_matches('-').to_string();
    }
    format!("{}-{}.json", prefix, &hash_hex[..12])
}

fn slug_fragment(value: &str) -> String {
    let mut output = String::new();
    let mut last_was_dash = false;
    for ch in value.trim().chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '-' | '_' | '.' | ' ') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = next else {
            continue;
        };
        if ch == '-' {
            if !last_was_dash && !output.is_empty() {
                output.push(ch);
                last_was_dash = true;
            }
        } else {
            output.push(ch);
            last_was_dash = false;
        }
    }
    output.trim_matches('-').to_string()
}

fn optional_string_value(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.trim().to_string()).filter(|text| !text.is_empty())),
        _ => Err(format!(
            "program runtime state field `{key}` must be a string or null"
        )),
    }
}

fn optional_json_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        other => Some(other.clone()),
    }
}

fn array_value(value: &Value, key: &str) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        _ => Err(format!(
            "program runtime state field `{key}` must be an array"
        )),
    }
}

fn string_array_value(value: &Value, key: &str) -> Result<Vec<String>, String> {
    let Value::Array(items) = value else {
        return Err(format!(
            "program runtime state field `{key}` must be an array of strings"
        ));
    };
    let mut output = Vec::new();
    for item in items {
        let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) else {
            return Err(format!(
                "program runtime state field `{key}` must contain only non-empty strings"
            ));
        };
        output.push(text.to_string());
    }
    Ok(output)
}

pub fn extract_markdown_section(markdown: &str, requested: &str) -> Option<String> {
    let target = requested.trim();
    if target.is_empty() {
        return None;
    }

    let mut in_section = false;
    let mut target_level = 0usize;
    let mut lines = Vec::new();

    for line in markdown.lines() {
        if let Some((level, title)) = parse_heading(line) {
            if in_section && level <= target_level {
                break;
            }
            if title.eq_ignore_ascii_case(target) {
                in_section = true;
                target_level = level;
                continue;
            }
        }

        if in_section {
            lines.push(line);
        }
    }

    trim_lines(lines)
}

fn first_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| parse_heading(line).map(|(_, title)| title.to_string()))
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 {
        return None;
    }
    let title = trimmed[level..].trim();
    if title.is_empty() {
        None
    } else {
        Some((level, title))
    }
}

fn trim_lines(lines: Vec<&str>) -> Option<String> {
    let mut start = 0usize;
    let mut end = lines.len();

    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    if start >= end {
        None
    } else {
        Some(lines[start..end].join("\n"))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;
    use crate::magician_v2::agents::AgentDefinition;

    fn owner_definition(program_section: Option<&str>, program: Option<&str>) -> AgentDefinition {
        let program_line = program
            .map(|value| format!("      program: \"{value}\"\n"))
            .unwrap_or_default();
        let harness_block = program_section
            .map(|value| format!("harness:\n  program_section: \"{value}\"\n"))
            .unwrap_or_else(|| "harness: {}\n".to_string());
        AgentDefinition::from_yaml_str(&format!(
            r#"
agent_id: "ceo"
name: "CEO"
persona: "CEO"
kind: personal
principal: "owner"
workspace: "default"
is_primary: true
autonomous_config:
  schedule: "0 7 * * *"
  focus_areas:
    - name: "Morning Briefing"
      description: "Review the team"
      priority: medium
{program_line}{harness_block}"#
        ))
        .expect("owner definition")
    }

    #[test]
    fn extract_markdown_section_preserves_nested_headings() {
        let markdown = r#"
# Program

## Engineering
Ship the harness.

### KPIs
- Merge rate

## Marketing
Grow demand.
"#;

        let section = extract_markdown_section(markdown, "Engineering").expect("section");
        assert!(section.contains("Ship the harness."));
        assert!(section.contains("### KPIs"));
        assert!(!section.contains("## Marketing"));
    }

    #[tokio::test]
    async fn loader_uses_default_program_with_section() {
        let tempdir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let programs_root = workspace.program_specs_root("owner", "default");
        fs::create_dir_all(&programs_root)
            .await
            .expect("programs root");
        fs::write(
            programs_root.join("program.md"),
            "# Company\n\n## Engineering\nShip harness.\n\n## Marketing\nWrite docs.\n",
        )
        .await
        .expect("program write");

        let loader = ProgramLoader::new(workspace);
        let loaded = loader
            .load_for_goal(
                "owner",
                "default",
                &owner_definition(Some("Engineering"), None),
                Some("harness:ceo:morning-briefing"),
            )
            .await
            .expect("load")
            .expect("program");

        assert_eq!(loaded.relative_path, "program.md");
        assert_eq!(loaded.section.as_deref(), Some("Engineering"));
        assert_eq!(loaded.content, "Ship harness.");
    }

    #[tokio::test]
    async fn loader_uses_focus_area_override_document() {
        let tempdir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let programs_root = workspace.program_specs_root("owner", "default");
        fs::create_dir_all(&programs_root)
            .await
            .expect("programs root");
        fs::write(
            programs_root.join("daily_ops.md"),
            "# Daily Ops\n\nWatch overnight runs.\n",
        )
        .await
        .expect("program write");

        let loader = ProgramLoader::new(workspace);
        let loaded = loader
            .load_for_goal(
                "owner",
                "default",
                &owner_definition(Some("Engineering"), Some("daily_ops.md")),
                Some("harness:ceo:morning-briefing"),
            )
            .await
            .expect("load")
            .expect("program");

        assert_eq!(loaded.relative_path, "daily_ops.md");
        assert!(loaded.section.is_none());
        assert!(loaded.content.contains("Watch overnight runs."));
    }

    #[tokio::test]
    async fn runtime_state_is_created_and_reloaded_separately_from_program_spec() {
        let tempdir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let programs_root = workspace.program_specs_root("owner", "default");
        fs::create_dir_all(&programs_root)
            .await
            .expect("programs root");
        fs::write(
            programs_root.join("program.md"),
            "# Program\n\n## Engineering\nRun it.\n",
        )
        .await
        .expect("program write");

        let loader = ProgramLoader::new(workspace.clone());
        let loaded = loader
            .load_by_reference("owner", "default", "program.md", Some("Engineering"))
            .await
            .expect("load")
            .expect("program");
        let mut state = loader
            .load_or_create_runtime_state("owner", "default", &loaded, Some("goal-1"))
            .await
            .expect("state");
        state
            .apply_update(
                serde_json::json!({
                    "current_phase": "triage",
                    "current_step": "review latest blocked tasks",
                    "next_action_hints": ["read program metrics"],
                    "stop_conditions": [{"id": "owner_pause", "status": "open"}]
                }),
                "test",
                "record current step",
                Some("lc_1".to_string()),
                Some("task_1".to_string()),
                Some("exec_1".to_string()),
            )
            .expect("apply");
        loader
            .write_runtime_state("owner", "default", &loaded, Some("goal-1"), &state)
            .await
            .expect("write");

        let reloaded = loader
            .load_runtime_state("owner", "default", &loaded, Some("goal-1"))
            .await
            .expect("reload")
            .expect("state");
        assert_eq!(reloaded.program.relative_path, "program.md");
        assert_eq!(reloaded.program.section.as_deref(), Some("Engineering"));
        assert_eq!(reloaded.current_phase.as_deref(), Some("triage"));
        assert_eq!(reloaded.next_action_hints, vec!["read program metrics"]);
        assert_eq!(
            reloaded.program.state_relative_path,
            loader.runtime_state_relative_path(&loaded, Some("goal-1"))
        );
        assert!(workspace
            .scope_root("owner", "default")
            .join(&reloaded.program.state_relative_path)
            .exists());
    }

    #[tokio::test]
    async fn runtime_state_write_publishes_atomically_and_leaves_no_staging_file() {
        let tempdir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let programs_root = workspace.program_specs_root("owner", "default");
        fs::create_dir_all(&programs_root)
            .await
            .expect("programs root");
        fs::write(programs_root.join("program.md"), "# Program\n\nRun it.\n")
            .await
            .expect("program write");

        let loader = ProgramLoader::new(workspace.clone());
        let loaded = loader
            .load_by_reference("owner", "default", "program.md", None)
            .await
            .expect("load")
            .expect("program");
        loader
            .load_or_create_runtime_state("owner", "default", &loaded, None)
            .await
            .expect("state");

        // A reader only ever sees the published file: no staging sibling is
        // left in the state directory, and what is there parses.
        let state_dir = workspace.program_runtime_states_dir("owner", "default");
        let mut names = Vec::new();
        let mut entries = fs::read_dir(&state_dir).await.expect("state dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(
            names,
            vec![runtime_state_file_name(&loaded, None)],
            "the durable write must publish exactly one file, with no staging sibling"
        );
        let raw = fs::read_to_string(state_dir.join(&names[0]))
            .await
            .expect("state body");
        serde_json::from_str::<ProgramRuntimeState>(&raw).expect("published state must parse");
    }

    #[tokio::test]
    async fn program_specs_are_scoped_alongside_runtime_state() {
        let tempdir = TempDir::new().expect("tempdir");
        let space_root = tempdir.path().join("MagicianNotes");
        let workspace = ArtifactV2Workspace::with_silverbullet_space_provider(&space_root)
            .expect("silverbullet workspace");

        // Specs are now SCOPED — the specs root IS the scope's `programs/` dir
        // (same root as the runtime state), not the old flat visible `Programs/`.
        let programs = workspace.program_specs_root("owner", "default");
        assert_eq!(programs, workspace.programs_root("owner", "default"));
        fs::create_dir_all(&programs).await.expect("programs root");
        fs::write(
            programs.join("daily_ops.md"),
            "# Daily Ops\n\nUse the scoped operating doc.\n",
        )
        .await
        .expect("scoped program write");

        let loader = ProgramLoader::new(workspace.clone());
        let loaded = loader
            .load_for_goal(
                "owner",
                "default",
                &owner_definition(Some("Engineering"), Some("daily_ops.md")),
                Some("harness:ceo:morning-briefing"),
            )
            .await
            .expect("load")
            .expect("scoped program");

        assert_eq!(loaded.relative_path, "daily_ops.md");
        assert!(loaded.content.contains("scoped operating doc"));

        // A named program that doesn't exist resolves to None — no accidental
        // match (the loader reads a specific spec, never scans/guesses).
        let missing = loader
            .load_by_reference("owner", "default", "nonexistent.md", Some("Engineering"))
            .await
            .expect("load missing");
        assert!(missing.is_none());

        // Runtime state coexists under the `state/` SUBDIR of the same scoped
        // root — `.md` specs and `.json` state never collide.
        let state = loader
            .load_or_create_runtime_state("owner", "default", &loaded, Some("goal-1"))
            .await
            .expect("runtime state");
        assert_eq!(state.program.relative_path, "daily_ops.md");
        let state_dir = workspace.program_runtime_states_dir("owner", "default");
        assert_eq!(state_dir, programs.join("state"));
        assert!(state_dir
            .join(runtime_state_file_name(&loaded, Some("goal-1")))
            .exists());
        // Spec and state coexist in the one scoped programs dir.
        assert!(programs.join("daily_ops.md").exists());
    }
}
