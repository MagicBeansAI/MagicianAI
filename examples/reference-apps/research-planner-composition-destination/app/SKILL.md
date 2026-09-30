---
name: research-planner-composition-destination
version: 0.1.0
description: Accept one bounded research-plan reference through reviewed Apps composition.
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
    required_features:
      - governed_actions_v1
      - durable_action_runs_v1
    generated_by:
      sdk: magician-app-authoring
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: denied
      memory_promotion: denied
      external_egress: denied
  entities:
    accepted_plan:
      fields:
        source_plan_id: { type: text, required: true }
        accepted: { type: boolean, required: true }
  views:
    accepted_plans:
      entity: accepted_plan
      kind: list
      route: /
  workflows:
    accept_plan:
      prompt: workflows/accept-plan.md
      runner: auto
      input:
        type: object
        fields:
          source_plan_id: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
        output_schema:
          type: object
          fields:
            forward_plan_id: { type: text, required: true }
            accepted: { type: boolean, required: true }
      may_mutate: []
      trigger: user
  actions:
    accept_plan:
      workflow: accept_plan
      input_from: accept_plan.input
      result_from: accept_plan.result
  resources:
    per_run:
      max_tokens: 512
      max_cost_usd: 0.10
      max_active_seconds: 30
    monthly:
      max_tokens: 10000
      max_cost_usd: 2.00
    storage:
      max_records: 1
      max_bytes: 1024
  dependencies:
    procedure_skills: []
    tools: []
  assets: []
---
# Composition destination

This package exposes one exact typed action for reviewed result composition.
