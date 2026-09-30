---
name: harness-sre-reliability
version: 0.1.0
description: First-party agents-as-data dogfood that wraps the existing harness-sre agent template as its workflow runner (platform layering plan 2.2).
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
    required_features:
      - typed_entities_v1
      - declarative_views_v1
      - governed_actions_v1
      - immutable_dependencies_v1
      - owner_data_plane_v1
      - durable_action_runs_v1
    generated_by:
      sdk: magician_app_cli
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    reliability_audit:
      fields:
        summary: { type: text, required: true }
        outcome: { type: enum, values: [healthy, repaired, escalated], required: true }
        audited_at: { type: timestamp, required: true }
  views:
    audits:
      entity: reliability_audit
      kind: list
      route: /
  workflows:
    run_audit:
      prompt: workflows/run-audit.md
      runner: auto
      agent: harness-sre
      uses: []
      input:
        type: object
        fields:
          focus: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [reliability_audit]
      may_mutate: [reliability_audit]
      trigger: user
  actions:
    run_audit:
      workflow: run_audit
      input_from: run_audit.input
      result_from: run_audit.result
  resources:
    per_run:
      max_tokens: 2000
      max_cost_usd: 0.50
      max_active_seconds: 300
    monthly:
      max_tokens: 20000
      max_cost_usd: 5.00
    storage:
      max_records: 1000
      max_bytes: 1048576
  dependencies:
    procedure_skills: []
    tools: []
  assets: []
---
# Harness SRE Reliability

This package is the plan-2.2 packaging dogfood for agents-as-data. It ships no
logic of its own: the `run_audit` workflow names the existing `harness-sre`
agent template as its runner, and installation review resolves, eligibility
checks, and digest-seals that definition from the live agent definition store.
The agent template and its program doc stay where they live; this package only
declares the smallest governed surface around them. The scheduled autonomous
loop continues to run the same definition whether or not this package is
installed, enabled, or uninstalled.
