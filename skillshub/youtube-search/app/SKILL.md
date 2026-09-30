---
name: youtube-search-coverage
version: 0.1.0
description: First-party skill-pack publication dogfood that declares the existing youtube-search skillshub pack as a ToolSkill dependency (platform layering plan 2.4).
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
      classification_floor: ordinary
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    video_coverage:
      fields:
        topic: { type: text, required: true }
        best_video: { type: text, required: true }
        captured_at: { type: timestamp, required: true }
  views:
    coverage:
      entity: video_coverage
      kind: list
      route: /
  workflows:
    find_videos:
      prompt: workflows/find-videos.md
      runner: auto
      uses: [youtube-search]
      input:
        type: object
        fields:
          topic: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [video_coverage]
      may_mutate: [video_coverage]
      trigger: user
  actions:
    find_videos:
      workflow: find_videos
      input_from: find_videos.input
      result_from: find_videos.result
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
    tools:
      - name: youtube-search
        version_requirement: "^0.3"
  assets: []
---
# YouTube Search Coverage

This package is the plan-2.4 publication dogfood for skillshub packs. It ships
no logic of its own: the `find_videos` workflow declares the existing
`youtube-search` skillshub pack in `dependencies.tools`, and the authoring lock
snapshots that pack's exact reviewed `SKILL.md` bytes — eligibility, typed
action schemas and implementation-plan digests included — into
`capability:youtube-search` evidence. The pack stays where it lives in
`skillshub/youtube-search/`; this package only declares the smallest governed
surface around it. Agents keep calling the skill through the ordinary skill
catalog whether or not this package is installed, enabled, or uninstalled.
