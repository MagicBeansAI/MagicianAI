---
name: research_outline
version: 1.0.0
description: Normalize a bounded research outline without network, file, host, or side-effect authority.
metadata:
  magician:
    skill_type: tool
    expose:
      apps: true
      agents: true
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires: { bins: [research-outline] }
      runtime:
        protocol: cli
        command_prefix: []
        limits:
          stdout_bytes: 4096
          stderr_bytes: 1024
    runtime_catalog:
      categories: [merge]
      composition_category: utility_operations
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        normalize:
          description: Normalize one bounded research outline.
          fixed_args: [normalize]
          parameters:
            objective:
              type: string
              description: Research objective.
              required: true
              max_length: 4096
            questions:
              type: string_array
              description: Bounded research questions.
              required: true
              min_items: 1
              max_items: 16
              max_item_bytes: 1024
          mappings:
            - type: flag
              flag: --objective
              parameter: objective
            - type: repeated_flag
              flag: --question
              parameter: questions
---
Normalize the supplied outline and emit one bounded JSON result. This fixture
is a separately reviewed ToolSkill candidate, not code inside the app bundle.
