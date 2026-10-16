---
name: research-planner
version: 0.2.0
description: Plan bounded research, retain normalized source records, and recover reviewed runs through the public Apps contract.
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
      - contribution_ports_v1
    generated_by:
      sdk: magician-app-authoring
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: candidate_allowed
      external_egress: denied
  entities:
    research_topic:
      fields:
        title: { type: text, required: true }
        query: { type: text, required: true }
        status: { type: enum, values: [draft, active, complete], required: true }
        priority: { type: integer, required: true }
        updated_at: { type: timestamp, required: true }
    research_source:
      fields:
        topic_id: { type: reference, entity: research_topic, required: true }
        title: { type: text, required: true }
        url: { type: text, required: true }
        summary: { type: markdown, required: true }
        status: { type: enum, values: [captured, reviewed, rejected], required: true }
        captured_at: { type: timestamp, required: true }
    research_plan:
      fields:
        topic_id: { type: reference, entity: research_topic, required: true }
        body: { type: markdown, required: true }
        status: { type: enum, values: [draft, approved], required: true }
        reviewed_at: { type: timestamp, required: false, nullable: true }
  views:
    topics:
      entity: research_topic
      kind: table
      route: /
      columns: [title, status, priority, updated_at]
      components:
        - component: section
          id: topic_overview
          children:
            - component: detail
              id: current_topic
              fields: [title, query, status, priority, updated_at]
            - component: form
              id: create_topic
              fields: [title, query, status, priority, updated_at]
        - component: list
          id: topic_list
          fields: [title, status, priority]
        - component: table
          id: topic_table
          columns: [title, status, priority, updated_at]
    sources:
      entity: research_source
      kind: list
      route: /sources
    plans:
      entity: research_plan
      kind: list
      route: /plans
  workflows:
    build_plan:
      prompt: workflows/build-plan.md
      runner: auto
      uses:
        - research_outline
        - time_math
        - research-planner-worker
        - browser__snapshot
        - browser__navigate
        - browser__scroll
        - browser__click
      procedures: ["skill:plan-brief"]
      input:
        type: object
        fields:
          topic_id: { type: reference, entity: research_topic, required: true }
          query: { type: text, required: true }
          start_date: { type: text, required: true }
          end_date: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [research_plan]
        output_schema:
          type: object
          fields:
            plan_id: { type: text, required: true }
            status: { type: enum, values: [draft, approved], required: true }
      contribution_ports:
        plan_memory:
          source:
            kind: mutation_backed_entity_projection
            entity: research_plan
            selected_fields: [topic_id, body, status]
          destination: memory
          purposes: [research_continuity]
          audiences: ["user:owner"]
          evidence_classes: [hypothesis]
          frequency: { max_proposals: 4, window_seconds: 3600 }
          maximum_retention_seconds: 604800
      may_mutate: [research_plan]
      trigger: user
    recipe_query_topics:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/query.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - kind: record
              fields:
                entity: { value_type: 1, required: true }
                limit: { value_type: 2, required: true }
                select: { value_type: 3, required: true }
            - { kind: text, max_bytes: 64 }
            - { kind: integer }
            - { kind: array, items: 4, min_items: 1, max_items: 16 }
            - { kind: text, max_bytes: 128 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
    recipe_get_topic:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/get.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - kind: record
              fields:
                entity: { value_type: 1, required: true }
                limit: { value_type: 2, required: true }
                select: { value_type: 3, required: true }
            - { kind: text, max_bytes: 64 }
            - { kind: integer }
            - { kind: array, items: 4, min_items: 1, max_items: 16 }
            - { kind: text, max_bytes: 128 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
    recipe_map_value:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/map.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - kind: record
              fields:
                value: { value_type: 1, required: true }
            - { kind: text, max_bytes: 4096 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
    recipe_sequence_value:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/sequence.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - kind: record
              fields:
                value: { value_type: 1, required: true }
            - { kind: text, max_bytes: 4096 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
    recipe_parallel_value:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/parallel.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - kind: record
              fields:
                value: { value_type: 1, required: true }
            - { kind: text, max_bytes: 4096 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
    recipe_switch_value:
      prompt: workflows/recipe.md
      runner: recipe
      recipe: recipes/switch.json
      input:
        type: object
        fields: {}
        value_schema:
          version: v1
          root: 0
          handling_floor: { classification: personal, model_processing: local_only }
          nodes:
            - { kind: tagged_union, discriminator: status, variants: { active: 1, draft: 3 } }
            - kind: record
              fields:
                value: { value_type: 2, required: true }
            - { kind: text, max_bytes: 4096 }
            - kind: record
              fields:
                value: { value_type: 4, required: true }
            - { kind: text, max_bytes: 4096 }
      result: { kind: typed_value, entities: [] }
      may_mutate: []
      trigger: user
  actions:
    build_plan:
      workflow: build_plan
      input_from: build_plan.input
      result_from: build_plan.result
    recipe_query_topics:
      workflow: recipe_query_topics
      input_from: recipe_query_topics.input
      result_from: recipe_query_topics.result
    recipe_get_topic:
      workflow: recipe_get_topic
      input_from: recipe_get_topic.input
      result_from: recipe_get_topic.result
    recipe_map_value:
      workflow: recipe_map_value
      input_from: recipe_map_value.input
      result_from: recipe_map_value.result
    recipe_sequence_value:
      workflow: recipe_sequence_value
      input_from: recipe_sequence_value.input
      result_from: recipe_sequence_value.result
    recipe_parallel_value:
      workflow: recipe_parallel_value
      input_from: recipe_parallel_value.input
      result_from: recipe_parallel_value.result
    recipe_switch_value:
      workflow: recipe_switch_value
      input_from: recipe_switch_value.input
      result_from: recipe_switch_value.result
  resources:
    per_run:
      max_tokens: 6000
      max_cost_usd: 1.50
      max_active_seconds: 180
    monthly:
      max_tokens: 120000
      max_cost_usd: 30.00
    storage:
      max_records: 5000
      max_bytes: 16777216
  dependencies:
    procedure_skills:
      - skill: "skill:plan-brief"
        version_requirement: "^1"
        vendored_path: vendor/skills/plan-brief/SKILL.md
    tools:
      - name: research_outline
        actions: [normalize]
        version_requirement: "^1"
      - name: time_math
        actions: [date_range]
        version_requirement: "^1"
      - name: research-planner-worker
        actions: [agent_as_tool]
        version_requirement: "^1"
      - name: browser__snapshot
        actions: [snapshot]
        interactive:
          schema: magician.app-interactive-capability-request.v1
          owner: browser
          allowed_origins: [about:blank, "https://example.com"]
          target_profile_class: installation_ephemeral_headless
          target_selectors: {}
          action_classes: [observe]
          background: direct_owner
          capture: structured_evidence_only
          transfer: denied
          resources:
            max_sessions: 1
            max_steps: 8
            max_duration_seconds: 180
            max_evidence_bytes: 589824
            max_evidence_nodes: 1024
            max_pixels: 0
            max_artifact_bytes: 0
            max_output_bytes: 524288
          expiry_session:
            grant_lifetime_seconds: 86400
            max_session_seconds: 180
            session: run_bound
        version_requirement: "^1"
      - name: browser__navigate
        actions: [navigate]
        interactive:
          schema: magician.app-interactive-capability-request.v1
          owner: browser
          allowed_origins: [about:blank, "https://example.com"]
          target_profile_class: installation_ephemeral_headless
          target_selectors: {}
          action_classes: [navigate_or_launch]
          background: direct_owner
          capture: structured_evidence_only
          transfer: denied
          resources:
            max_sessions: 1
            max_steps: 8
            max_duration_seconds: 180
            max_evidence_bytes: 65536
            max_evidence_nodes: 16
            max_pixels: 0
            max_artifact_bytes: 0
            max_output_bytes: 32768
          expiry_session:
            grant_lifetime_seconds: 86400
            max_session_seconds: 180
            session: run_bound
        version_requirement: "^1"
      - name: browser__scroll
        actions: [scroll]
        interactive:
          schema: magician.app-interactive-capability-request.v1
          owner: browser
          allowed_origins: [about:blank, "https://example.com"]
          target_profile_class: installation_ephemeral_headless
          target_selectors: {}
          action_classes: [interact]
          background: direct_owner
          capture: structured_evidence_only
          transfer: denied
          resources:
            max_sessions: 1
            max_steps: 8
            max_duration_seconds: 180
            max_evidence_bytes: 65536
            max_evidence_nodes: 16
            max_pixels: 0
            max_artifact_bytes: 0
            max_output_bytes: 32768
          expiry_session:
            grant_lifetime_seconds: 86400
            max_session_seconds: 180
            session: run_bound
        version_requirement: "^1"
      - name: browser__click
        actions: [click]
        interactive:
          schema: magician.app-interactive-capability-request.v1
          owner: browser
          allowed_origins: [about:blank, "https://example.com"]
          target_profile_class: installation_ephemeral_headless
          target_selectors: {}
          action_classes: [outward_commit]
          background: direct_owner
          capture: structured_evidence_only
          transfer: denied
          resources:
            max_sessions: 1
            max_steps: 8
            max_duration_seconds: 180
            max_evidence_bytes: 65536
            max_evidence_nodes: 16
            max_pixels: 0
            max_artifact_bytes: 0
            max_output_bytes: 32768
          expiry_session:
            grant_lifetime_seconds: 86400
            max_session_seconds: 180
            session: run_bound
        version_requirement: "^1"
  assets: []
---
# Research Planner

This package is intentionally declarative. Runtime behavior is owned by the
reviewed dependencies and the supported Apps data/action contract.
