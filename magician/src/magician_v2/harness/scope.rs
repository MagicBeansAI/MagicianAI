use std::collections::HashSet;

use serde::Serialize;

use crate::magician_v2::agents::types::AgentKind;
use crate::magician_v2::agents::{
    disabled_agent_hierarchy, resolve_effective_delegation_target_ids,
    resolve_focus_area_for_goal_id, AgentDefinition, AgentDefinitionStore, DefinitionRecord,
};

#[derive(Debug, Clone, Serialize)]
pub struct HarnessScopeTarget {
    pub agent_id: String,
    pub name: String,
    pub kind: AgentKind,
    pub description: String,
    pub harness_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct HarnessScope {
    owner_agent_id: String,
    goal_id: Option<String>,
    focus_area_name: Option<String>,
    targets: Vec<DefinitionRecord>,
}

impl HarnessScope {
    pub async fn resolve(
        definition_store: &AgentDefinitionStore,
        owner: &AgentDefinition,
        goal_id: Option<&str>,
    ) -> Result<Self, String> {
        let records = definition_store
            .list_definitions()
            .await
            .map_err(|error| format!("failed to list harness scope definitions: {error}"))?;
        let disabled = disabled_agent_hierarchy(records.iter().map(|record| &record.definition));
        let allowed_ids = resolve_effective_delegation_target_ids(
            owner,
            records.iter().map(|record| &record.definition),
            &disabled,
        )
        .into_iter()
        .collect::<HashSet<_>>();
        let mut targets = records
            .into_iter()
            .filter(|record| allowed_ids.contains(&record.definition.agent_id))
            .collect::<Vec<_>>();

        let focus_area_name = goal_id.and_then(|goal_id| {
            resolve_focus_area_for_goal_id(owner, goal_id).map(|f| f.name.clone())
        });

        if let Some(goal_id) = goal_id {
            if let Some(focus_area) = resolve_focus_area_for_goal_id(owner, goal_id) {
                if let Some(scope) = focus_area
                    .scope
                    .as_ref()
                    .filter(|entries| !entries.is_empty())
                {
                    if !scope.iter().any(|entry| entry == "*") {
                        let allowed = scope
                            .iter()
                            .map(|entry| entry.trim())
                            .filter(|entry| !entry.is_empty())
                            .collect::<HashSet<_>>();
                        targets
                            .retain(|record| allowed.contains(record.definition.agent_id.as_str()));
                    }
                }
            }
        }

        targets.sort_by(|left, right| left.definition.agent_id.cmp(&right.definition.agent_id));

        Ok(Self {
            owner_agent_id: owner.agent_id.clone(),
            goal_id: goal_id.map(str::to_string),
            focus_area_name,
            targets,
        })
    }

    pub fn owner_agent_id(&self) -> &str {
        &self.owner_agent_id
    }

    pub fn goal_id(&self) -> Option<&str> {
        self.goal_id.as_deref()
    }

    pub fn focus_area_name(&self) -> Option<&str> {
        self.focus_area_name.as_deref()
    }

    pub fn contains(&self, agent_id: &str) -> bool {
        self.targets
            .iter()
            .any(|record| record.definition.agent_id == agent_id)
    }

    pub fn require_contains(&self, agent_id: &str) -> Result<(), String> {
        if self.contains(agent_id) {
            Ok(())
        } else {
            Err(format!(
                "agent `{agent_id}` is outside the active harness scope for `{}`",
                self.owner_agent_id
            ))
        }
    }

    pub fn target_ids(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|record| record.definition.agent_id.clone())
            .collect()
    }

    pub fn target_records(&self) -> &[DefinitionRecord] {
        &self.targets
    }

    pub fn targets_summary(&self) -> Vec<HarnessScopeTarget> {
        self.targets
            .iter()
            .map(|record| HarnessScopeTarget {
                agent_id: record.definition.agent_id.clone(),
                name: record.definition.name.clone(),
                kind: record.definition.kind.clone(),
                description: record.definition.description.clone(),
                harness_enabled: record.definition.harness.is_some(),
            })
            .collect()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::agents::AgentDefinitionStore;

    fn worker(agent_id: &str) -> crate::magician_v2::agents::AgentDefinition {
        crate::magician_v2::agents::AgentDefinition::from_yaml_str(&format!(
            r#"
agent_id: "{agent_id}"
name: "{agent_id}"
kind: worker
persona: "Worker"
tools:
  - "files"
"#
        ))
        .expect("worker definition")
    }

    fn owner_with_focus(scope: Option<Vec<&str>>) -> crate::magician_v2::agents::AgentDefinition {
        let scope_block = scope
            .map(|entries| {
                let rendered = entries
                    .into_iter()
                    .map(|entry| format!("        - \"{entry}\""))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("      scope:\n{rendered}\n")
            })
            .unwrap_or_default();
        crate::magician_v2::agents::AgentDefinition::from_yaml_str(&format!(
            r#"
agent_id: "ceo"
name: "CEO"
persona: "CEO"
kind: personal
principal: "owner"
workspace: "default"
is_primary: true
delegation_targets:
  - "*"
autonomous_config:
  schedule: "0 7 * * *"
  focus_areas:
    - name: "Morning Briefing"
      description: "Review the team"
      priority: medium
{scope_block}harness: {{}}
"#
        ))
        .expect("owner definition")
    }

    #[tokio::test]
    async fn wildcard_scope_excludes_owner_and_system_agents() {
        let tempdir = TempDir::new().expect("tempdir");
        let store =
            AgentDefinitionStore::with_workspace_root(tempdir.path()).for_scope("owner", "default");
        store
            .create_definition(owner_with_focus(None))
            .await
            .expect("create owner");
        store
            .create_definition(worker("backend"))
            .await
            .expect("create backend");
        store
            .create_definition(worker("frontend"))
            .await
            .expect("create frontend");
        let mut loom = worker("brainstorm-facilitator");
        loom.invocation_policy = crate::magician_v2::agents::AgentInvocationPolicy {
            discoverability: crate::magician_v2::agents::AgentDiscoverability::SurfaceOnly,
            delegation: crate::magician_v2::agents::AgentDelegationPolicy::None,
            allowed_direct_surfaces: vec![
                crate::magician_v2::agents::InvocationSurface::ThinkingMap,
            ],
        };
        store
            .create_definition(loom)
            .await
            .expect("create surface-only Loom");

        let owner = store
            .get_definition("ceo")
            .await
            .expect("get owner")
            .expect("owner");
        let scope = HarnessScope::resolve(&store, &owner.definition, None)
            .await
            .expect("scope");

        assert_eq!(
            scope.target_ids(),
            vec!["backend".to_string(), "frontend".to_string()]
        );
    }

    #[tokio::test]
    async fn focus_scope_intersects_with_resolved_targets() {
        let tempdir = TempDir::new().expect("tempdir");
        let store =
            AgentDefinitionStore::with_workspace_root(tempdir.path()).for_scope("owner", "default");
        store
            .create_definition(owner_with_focus(Some(vec!["backend"])))
            .await
            .expect("create owner");
        store
            .create_definition(worker("backend"))
            .await
            .expect("create backend");
        store
            .create_definition(worker("frontend"))
            .await
            .expect("create frontend");

        let owner = store
            .get_definition("ceo")
            .await
            .expect("get owner")
            .expect("owner");
        let scope = HarnessScope::resolve(
            &store,
            &owner.definition,
            Some("harness:ceo:morning-briefing"),
        )
        .await
        .expect("scope");

        assert_eq!(scope.target_ids(), vec!["backend".to_string()]);
    }
}
