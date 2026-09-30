---
name: memory-learning-review
version: 0.2.16
description: "First product-grade declarative package (platform layering plan 2.5) — the memory & learning review console over the scoped learning substrate, populated by the admitted internal_data learning-read port."
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
      - app_widgets_v1
    generated_by:
      sdk: magician_app_cli
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  distribution: system
  widgets:
    - id: learning_queue
      title: Learning review queue
      view: queue
      read:
        kind: view_projection
        view: queue
        entity: learning_candidate
        fields: [candidate_id, candidate_type, state, risk_level, updated_at]
      refresh_hint:
        min_interval_seconds: 60
        max_staleness_seconds: 300
      bounds:
        max_rows: 8
        max_render_bytes: 32768
      actions:
        - id: refresh
          label: Refresh queue
          governed_action: sync_queue
      rendering: { kind: native }
      fallback: { kind: unavailable }
      suggested_slots:
        - page: /
          slot: secondary
          system_default: true
      required_capabilities: [declarative_table_v1, governed_actions_v1]
  navigation:
    - id: learning
      title: Learning
      route: /learning
      placement: { kind: section, section: review }
      surface: { kind: view, view: queue }
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    learning_candidate:
      fields:
        candidate_id: { type: text, required: true }
        candidate_type:
          {
            type: enum,
            values:
              [
                memory_fact,
                memory_preference,
                memory_procedure,
                skill_update,
                capability_update,
                tool_schema_update,
                tool_wrapper_fix,
                agent_persona_update,
                workflow_template,
                evaluation_case,
                program_state_update,
                harness_profile_revision,
                bug_report,
                docs_update,
                other,
              ],
            required: true,
          }
        state:
          {
            type: enum,
            values:
              [
                observed,
                proposed,
                triaged,
                approved,
                implemented,
                evaluated,
                promoted,
                rejected,
                superseded,
                archived,
              ],
            required: true,
          }
        title: { type: text, required: true }
        summary: { type: markdown, required: true }
        risk_level:
          { type: enum, values: [low, medium, high, critical], required: true }
        source_agent_id: { type: text, nullable: true }
        review_required: { type: boolean, required: true }
        created_at: { type: timestamp, required: true }
        updated_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    review_decision:
      fields:
        candidate_id: { type: text, required: true }
        candidate_type: { type: text, nullable: true }
        decision:
          {
            type: enum,
            values: [approve, reject, snooze],
            required: true,
          }
        reason: { type: text, required: true }
        decided_at: { type: timestamp, required: true }
  views:
    queue:
      entity: learning_candidate
      kind: table
      route: /
      columns: [candidate_id, candidate_type, state, risk_level, updated_at]
    decisions:
      entity: review_decision
      kind: table
      route: /decisions
      columns: [candidate_id, decision, decided_at]
  workflows:
    sync_queue:
      prompt: workflows/sync-queue.md
      runner: recipe
      recipe: recipes/sync-queue.json
      uses: [internal_data]
      input:
        type: object
        fields:
          state:
            {
              type: enum,
              values:
                [
                  observed,
                  proposed,
                  triaged,
                  approved,
                  implemented,
                  evaluated,
                  promoted,
                  rejected,
                  superseded,
                  archived,
                ],
              required: false,
            }
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [learning_candidate]
      trigger: user
    approve_candidate:
      prompt: workflows/approve-candidate.md
      runner: recipe
      recipe: recipes/approve-candidate.json
      uses: []
      input:
        type: object
        fields:
          candidate_id: { type: text, required: true }
          candidate_type: { type: text, required: false }
          reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    reject_candidate:
      prompt: workflows/reject-candidate.md
      runner: recipe
      recipe: recipes/reject-candidate.json
      uses: []
      input:
        type: object
        fields:
          candidate_id: { type: text, required: true }
          candidate_type: { type: text, required: false }
          reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    snooze_candidate:
      prompt: workflows/snooze-candidate.md
      runner: recipe
      recipe: recipes/snooze-candidate.json
      uses: []
      input:
        type: object
        fields:
          candidate_id: { type: text, required: true }
          candidate_type: { type: text, required: false }
          reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
  actions:
    sync_queue:
      workflow: sync_queue
      input_from: sync_queue.input
      result_from: sync_queue.result
    approve_candidate:
      workflow: approve_candidate
      input_from: approve_candidate.input
      result_from: approve_candidate.result
    reject_candidate:
      workflow: reject_candidate
      input_from: reject_candidate.input
      result_from: reject_candidate.result
    snooze_candidate:
      workflow: snooze_candidate
      input_from: snooze_candidate.input
      result_from: snooze_candidate.result
  resources:
    # The local physical model reserves a 32K input window before dispatch.
    # Leave room for that reservation plus actual usage from earlier turns.
    per_run:
      max_tokens: 131072
      max_cost_usd: 0.50
      max_active_seconds: 600
    monthly:
      max_tokens: 400000
      max_cost_usd: 5.00
    storage:
      max_records: 5000
      max_bytes: 8388608
  dependencies:
    procedure_skills: []
    tools:
      - name: internal_data
        version_requirement: "^1.13"
        actions: [list_learning_candidates, read_learning_candidate]
  assets: []
---
# Memory & Learning Review Console

## Surfacing and distribution

The manifest classifies this package as `system` and declares a bounded native
table widget plus the first-party `/learning` navigation entry. Its widget read
is only a closed projection of the package-owned queue view; `internal_data`
still runs solely inside the explicit `sync_queue` governed workflow. That
refresh workflow accepts the exact empty object, so the widget button does not
invent row bindings or workflow-free tool authority.

The class and surfacing declarations are review material. System authority can
come only from the separately owned host-controlled, digest-pinned boot
admission; ordinary staging and SDK/VibeDev publication must reject the
manifest claim.

This package is the plan-2.5 first product-grade declarative package: the
memory & learning review console as entities, declarative views and governed
actions over the scoped learning substrate. It ships no bespoke UI and no
logic of its own.

## Population path (read side, real)

The `sync_queue` workflow is the queue's population path. It declares
`internal_data` in `dependencies.tools` narrowed to exactly the two reviewed
learning read actions — `list_learning_candidates` and
`read_learning_candidate` — which installation review snapshots into
`capability:internal_data` lock evidence. Those two actions are the first
admitted host-read port for Apps (see
`docs/components/magician/app-tool-bind.md`): the executor owns the runtime
scope (`__principal`/`__workspace`), the parameter surface is closed
fail-closed, and the pack's broader diagnostics vocabulary stays
agent-only. The workflow projects one bounded page of real learning
candidates into this package's own entity store; nothing writes the learning
substrate.

## Review actions (decision ledger real; apply path via the reviewed port)

`approve_candidate`, `reject_candidate` and `snooze_candidate` are governed
actions that record the owner's decision in the durable `review_decision`
ledger this package owns, keyed by the core candidate id. The recorded
decision is designed to be the exact source head a
`magician.learning-decision` contribution proposes: the plan-2.5 apply-path
port (`magician-app-contract` contribution family + the core destination
owner in `magician_v2/learning_decision_contribution.rs`) carries an
approve, reject or snooze decision to the real
`LearningStore::transition_candidate` behind an owner-signed,
destination-head-bound decision envelope — the same transition the
first-party `/learning/candidates/{id}/transition` API serves, never a
second authority. Approve maps to `approved`, reject to `rejected`, snooze
records its deferral in the core decision log without changing state;
`promoted` is unreachable from the port (the promotion bridges stay
first-party), replays of the same signed decision are idempotent, and
terminal candidates fail closed. This package's manifest does not declare
`contribution_ports` yet, so that feed is designed-for rather than wired:
declaring the port is a future manifest revision, and until then these
actions only write their own ledger. Proposal staging from the
workflow terminal into the destination queue rides the same
destination-consumer follow-up as the attention port's staging side; the
signed-apply seam and its receipts are landed and tested.


## Rollback

Once trusted system-package admission owns this class, rollback is persistent
disable, never uninstall. Disabling revokes the package grant and hides its
route/widget while the learning substrate and existing first-party consoles
continue unchanged. Before that boot owner exists, the `system` claim is
deliberately inadmissible through ordinary staging/publication.
