---
name: brainstorm-canvas
version: 0.2.15
description: "First custom-surface reference package (platform layering plan 4, Brainstorm packaging) — the thinking-map canvas over the scoped host-read binder, populated by the admitted thinking_maps_data read port and rendered by a scripted surfaces/ member."
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
      - custom_surfaces_v1
      - app_widgets_v1
    generated_by:
      sdk: magician_app_cli
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  distribution: system
  permissions:
    - custom_surface
  custom_surface:
    entry_points:
      - route: /canvas
        document: surfaces/canvas.html
  widgets:
    - id: brainstorm_canvas
      title: Brainstorm canvas
      view: library
      read:
        kind: view_projection
        view: library
        entity: thinking_map_summary
        fields: [map_id, title, lifecycle, latest_revision, updated_at]
      refresh_hint:
        min_interval_seconds: 60
        max_staleness_seconds: 300
      bounds:
        max_rows: 8
        max_render_bytes: 32768
      actions:
        - id: refresh
          label: Refresh maps
          governed_action: sync_maps
      rendering:
        kind: mini_frame
        entry_point: /canvas
      fallback:
        kind: view
        view: library
      suggested_slots:
        - page: /
          slot: primary
          system_default: true
      required_capabilities:
        [declarative_table_v1, governed_actions_v1, mini_frame_v1]
  navigation:
    - id: brainstorm
      title: Brainstorm
      route: /brainstorm
      placement: { kind: section, section: create }
      surface:
        kind: custom_surface
        entry_point: /canvas
        fallback_view: library
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    thinking_map_summary:
      fields:
        map_id: { type: text, required: true }
        title: { type: text, required: true }
        lifecycle:
          {
            type: enum,
            values: [active, paused, archived, deleted],
            required: true,
          }
        latest_revision: { type: integer, required: true }
        updated_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    thinking_map_snapshot:
      fields:
        map_id: { type: text, required: true }
        title: { type: text, required: true }
        lifecycle:
          {
            type: enum,
            values: [active, paused, archived, deleted],
            required: true,
          }
        revision: { type: integer, required: true }
        snapshot: { type: text, required: true }
        synced_at: { type: timestamp, required: true }
  views:
    library:
      entity: thinking_map_summary
      kind: table
      route: /
      columns: [map_id, title, lifecycle, latest_revision, updated_at]
  workflows:
    sync_maps:
      prompt: workflows/sync-maps.md
      runner: recipe
      recipe: recipes/sync-maps.json
      uses: [thinking_maps_data]
      input:
        type: object
        fields:
          lifecycle:
            {
              type: enum,
              values: [active, paused, archived, deleted],
              required: false,
            }
          limit: { type: integer, required: false }
          map_id: { type: text, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [thinking_map_summary, thinking_map_snapshot]
      trigger: user
  actions:
    sync_maps:
      workflow: sync_maps
      input_from: sync_maps.input
      result_from: sync_maps.result
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
      max_records: 2000
      max_bytes: 8388608
  dependencies:
    procedure_skills: []
    tools:
      - name: thinking_maps_data
        version_requirement: "^1.0"
        actions: [list_maps, read_map]
  assets: []
---
# Brainstorm Canvas

This package is the plan-4 Brainstorm custom-surface reference consumer: the
first real package to exercise the whole `custom_surfaces_v1` stack — manifest
declaration, owner review with the executable-member inventory, the scripted
host kernel, and the eight-operation bridge — fed by the thinking-map
host-read binder (`thinking_maps_data`, the Phase 4 re-open condition). It
replaces nothing: the existing Brainstorm canvases stay where they are, and
this package is additive.

## Surfacing and distribution

The manifest classifies this package as `system` and declares one bounded
Brainstorm widget plus the first-party `/brainstorm` navigation entry. The
mini-frame is never the only renderer: `library` is its declarative table
fallback for clients that do not admit `mini_frame_v1`. The widget read is a
closed projection of the package-owned summary entity; it does not execute the
host binder directly. `sync_maps` is an explicit governed refresh action and
accepts an empty input object, so the widget button needs no invented row or
ambient input binding.

These declarations are review material. This package becomes system authority
only through the separately owned host-controlled, digest-pinned boot admission;
ordinary staging and SDK/VibeDev publication must reject the manifest's class
claim.

## Population path (read side, real)

The `sync_maps` workflow is the canvas's population path, the plan-2.5
sync_queue pattern over the thinking-map substrate. It declares
`thinking_maps_data` in `dependencies.tools` narrowed to exactly the two
reviewed read actions — `list_maps` and `read_map` — which installation
review snapshots into `capability:thinking_maps_data` lock evidence. The
executor owns the runtime scope (`__principal`/`__workspace`), the parameter
surface is closed fail-closed, and map mutation stays on the first-party API.
The workflow projects one bounded page of map summaries into this package's
own entity store; when its input carries a `map_id` it also reads that one
map and projects its full snapshot (the complete typed map document,
serialized verbatim as JSON text). Nothing writes the map substrate.

## The custom surface (the canvas)

`surfaces/canvas.html` is the package's one declared scripted entry point,
hosted in the sandboxed frame under the kernel CSP (`default-src 'none'`,
`script-src 'self'`, `connect-src 'none'`) with the bridge as its only
authority channel. Its script member `surfaces/canvas.js` (a separate
`.js` file because the CSP forbids inline script) reads the synced entities
through the bridge's `query_data` operation — the map library sidebar from
`thinking_map_summary`, the currently-synced board from
`thinking_map_snapshot` — and renders the map as an SVG graph: positioned
labeled nodes, edges as lines, tap-to-select with a detail panel, scrolling
over the full board. No network fetches, no external libraries, no editing;
the map mutation port stays a separate re-open condition.

The V1 kernel keeps frame authority deliberately minimal. A frame may launch
`sync_maps` with only the action name, an idempotency key, and typed input; the
authenticated host resolves installation-bound action/schema/grant revisions,
scope, policy, and provenance immediately before admission. Full
`AppActionInvocation` remains accepted only as a compatibility shape. The
script-delivery gap the first version shipped under is also closed:
- **Script delivery (resolved).** A relative `<script src="canvas.js">`
  resolves under the entry document's digest-keyed address; the kernel's
  session-scoped sibling resolution now admits that request for a live
  session whose entry document still hashes to the address's digest,
  serving the script's own manifest-verified bundle bytes under the
  kernel CSP (`no-store`, because the address names the entry document,
  not the script). The entry document's static boot notice describes the
  boot sequence and is removed by the script once the bridge is up; if
  the script is refused, the notice is the whole surface.

The owner's review will flag two patterns in the executable inventory's
static scan: `parent.postMessage` — the bridge itself, the frame's only
channel — and the `http://` of the SVG XMLNS namespace constant. Both are
review material, not findings against the package; the scan is an aid and
the isolation kernel is the boundary.

## Rollback

Once trusted system-package admission owns this class, rollback is persistent
disable, never uninstall. Disabling revokes the package grant and hides its
route/widget without changing the thinking-map substrate or its existing
first-party consoles. Before that boot owner exists, the `system` claim is
deliberately inadmissible through ordinary staging/publication. The operator
switch (`app_platform.custom_surfaces_v1.enabled`, default off) independently
gates the scripted half process-wide.
