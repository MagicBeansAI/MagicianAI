---
name: meetings
version: 0.1.9
description: "Owner console for meeting capture: live status and transcripts, thread history with search, takeaways, upcoming calendar context, and the full capture controls through the reviewed meeting-control destination."
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
      sdk: magician-app-authoring
      version: "1.0.0"
app:
  compatibility:
    magician_contract: "1"
  distribution: system
  permissions:
    - custom_surface
  custom_surface:
    entry_points:
      - route: /console
        document: surfaces/console.html
  widgets:
    - id: active_capture
      title: Live capture
      view: sessions
      read:
        kind: view_projection
        view: sessions
        entity: capture_session
        fields:
          [session_id, mode, status, live, paused, thread_id, title, started_seconds_ago, synced_at]
      refresh_hint:
        min_interval_seconds: 10
        max_staleness_seconds: 60
      bounds:
        max_rows: 6
        max_render_bytes: 32768
      actions:
        - id: refresh
          label: Refresh capture state
          governed_action: sync_sessions
      rendering: { kind: native }
      fallback: { kind: unavailable }
      suggested_slots:
        - page: /observe
          slot: capture
          system_default: true
        - page: /
          slot: ambient
          system_default: true
      required_capabilities: [declarative_table_v1, governed_actions_v1]
    - id: recent_meetings
      title: Recent meetings
      view: threads
      read:
        kind: view_projection
        view: threads
        entity: meeting_thread
        fields: [thread_id, title, status, updated_at, synced_at]
      refresh_hint:
        min_interval_seconds: 60
        max_staleness_seconds: 900
      bounds:
        max_rows: 8
        max_render_bytes: 32768
      actions:
        - id: refresh
          label: Refresh meetings
          governed_action: sync_threads
      rendering: { kind: native }
      fallback: { kind: unavailable }
      suggested_slots:
        - page: /observe
          slot: history
          system_default: true
      required_capabilities: [declarative_table_v1, governed_actions_v1]
  indicators:
    - id: capture_state
      title: Meeting capture
      read:
        kind: view_projection
        view: sessions
        entity: capture_session
        fields: [status, live, in_scope]
      refresh_hint:
        min_interval_seconds: 10
        max_staleness_seconds: 60
      projection: { kind: state, field: status }
      bounds:
        max_text_bytes: 64
      selector:
        kind: exact_record
        filter:
          - field: live
            equals: "true"
          - field: in_scope
            equals: "true"
  navigation:
    - id: meetings_console
      title: Meetings
      route: /meetings-console
      placement: { kind: section, section: observe }
      surface:
        kind: custom_surface
        entry_point: /console
        fallback_view: sessions
  data_policy:
    defaults:
      classification_floor: sensitive
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    capture_session:
      fields:
        session_id: { type: text, required: true }
        mode: { type: enum, values: [passive, attendee], required: true }
        status: { type: text, required: true }
        live: { type: boolean, required: true }
        in_scope: { type: boolean, required: true }
        paused: { type: boolean, required: true }
        capture_mic: { type: boolean, nullable: true }
        thread_id: { type: text, nullable: true }
        title: { type: text, nullable: true }
        url: { type: text, nullable: true }
        started_seconds_ago: { type: integer, required: true }
        ended_seconds_ago: { type: integer, nullable: true }
        retained_for_seconds: { type: integer, required: true }
        synced_at: { type: timestamp, required: true }
    meeting_thread:
      fields:
        thread_id: { type: text, required: true }
        session_id: { type: text, required: true }
        title: { type: text, nullable: true }
        agent_id: { type: text, required: true }
        status: { type: enum, values: [active, archived], required: true }
        created_at: { type: timestamp, required: true }
        updated_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    transcript_line:
      fields:
        message_id: { type: text, required: true }
        thread_id: { type: text, required: true }
        session_id: { type: text, required: true }
        speaker: { type: text, nullable: true }
        line_text: { type: markdown, required: true }
        transcript: { type: boolean, required: true }
        line_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    meeting_takeaway:
      fields:
        takeaway_key: { type: text, required: true }
        thread_id: { type: text, nullable: true }
        title: { type: text, nullable: true }
        meeting_date: { type: text, nullable: true }
        summary: { type: markdown, nullable: true }
        decisions: { type: markdown, nullable: true }
        action_items: { type: markdown, nullable: true }
        updated_at: { type: text, nullable: true }
        synced_at: { type: timestamp, required: true }
    upcoming_meeting:
      fields:
        event_id: { type: text, required: true }
        title: { type: text, required: true }
        starts_at: { type: text, required: true }
        ends_at: { type: text, nullable: true }
        meet_url: { type: text, nullable: true }
        live_now: { type: boolean, required: true }
        account: { type: text, required: true }
        synced_at: { type: timestamp, required: true }
    meeting_search_hit:
      fields:
        hit_id: { type: text, required: true }
        kind: { type: enum, values: [takeaway, thread, transcript], required: true }
        thread_id: { type: text, required: true }
        session_id: { type: text, nullable: true }
        message_id: { type: text, nullable: true }
        speaker: { type: text, nullable: true }
        excerpt: { type: markdown, required: true }
        hit_at: { type: timestamp, nullable: true }
        synced_at: { type: timestamp, required: true }
    control_request:
      fields:
        request_id: { type: text, required: true }
        verb: { type: enum, values: [listen, join, pause, resume, stop], required: true }
        target_kind: { type: enum, values: [new_capture, live_session], required: true }
        target_ref: { type: text, nullable: true }
        gesture_id: { type: text, nullable: true }
        actor_ref: { type: text, nullable: true }
        note: { type: text, nullable: true }
        payload_json: { type: text, required: true }
        apply_state: { type: enum, values: [recorded], required: true }
        requested_at: { type: timestamp, required: true }
    control_receipt:
      fields:
        receipt_id: { type: text, required: true }
        decision_id: { type: text, required: true }
        verb: { type: enum, values: [listen, join, pause, resume, stop], required: true }
        outcome:
          { type: enum, values: [started, paused, resumed, stopped, owner_declined, refused], required: true }
        session_id: { type: text, nullable: true }
        thread_id: { type: text, nullable: true }
        error_code: { type: text, nullable: true }
        applied_at: { type: timestamp, required: true }
  views:
    sessions:
      entity: capture_session
      kind: table
      route: /
      columns:
        [session_id, mode, status, live, paused, thread_id, title, started_seconds_ago, synced_at]
    threads:
      entity: meeting_thread
      kind: table
      route: /threads
      columns: [thread_id, title, status, agent_id, created_at, updated_at, synced_at]
    transcript:
      entity: transcript_line
      kind: table
      route: /transcript
      columns: [message_id, thread_id, session_id, speaker, line_at, transcript, synced_at]
    takeaways:
      entity: meeting_takeaway
      kind: table
      route: /takeaways
      columns: [takeaway_key, thread_id, title, meeting_date, updated_at, synced_at]
    upcoming:
      entity: upcoming_meeting
      kind: table
      route: /upcoming
      columns: [event_id, title, starts_at, ends_at, live_now, account, synced_at]
    search:
      entity: meeting_search_hit
      kind: table
      route: /search
      columns: [hit_id, kind, thread_id, speaker, hit_at, synced_at]
    controls:
      entity: control_request
      kind: table
      route: /controls
      columns: [request_id, verb, target_kind, target_ref, gesture_id, apply_state, requested_at]
    receipts:
      entity: control_receipt
      kind: table
      route: /receipts
      columns: [receipt_id, decision_id, verb, outcome, session_id, thread_id, applied_at]
  workflows:
    sync_sessions:
      prompt: workflows/sync-sessions.md
      runner: recipe
      recipe: recipes/sync-sessions.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [capture_session]
      trigger: user
    sync_threads:
      prompt: workflows/sync-threads.md
      runner: recipe
      recipe: recipes/sync-threads.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          status: { type: text, required: false }
          text: { type: text, required: false }
          limit: { type: integer, required: false }
          after_thread_id: { type: text, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [meeting_thread]
      trigger: user
    read_transcript:
      prompt: workflows/read-transcript.md
      runner: recipe
      recipe: recipes/read-transcript.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          thread_id: { type: text, required: true }
          session_id: { type: text, required: false }
          before_message_id: { type: text, required: false }
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [transcript_line]
      trigger: user
    sync_takeaways:
      prompt: workflows/sync-takeaways.md
      runner: recipe
      recipe: recipes/sync-takeaways.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          thread_id: { type: text, required: false }
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [meeting_takeaway]
      trigger: user
    sync_upcoming:
      prompt: workflows/sync-upcoming.md
      runner: recipe
      recipe: recipes/sync-upcoming.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [upcoming_meeting]
      trigger: user
    search_meetings:
      prompt: workflows/search-meetings.md
      runner: recipe
      recipe: recipes/search-meetings.json
      uses: [meetings_data]
      input:
        type: object
        fields:
          text: { type: text, required: true }
          limit: { type: integer, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [meeting_search_hit]
      trigger: user
    listen:
      prompt: workflows/listen.md
      runner: recipe
      recipe: recipes/listen.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          url: { type: text, required: false }
          title: { type: text, required: false }
          meeting_date: { type: text, required: false }
          capture_mic: { type: boolean, required: true }
          gesture_id: { type: text, required: true }
          surface_session_id: { type: text, required: true }
          gesture_observed_at_ms: { type: integer, required: true }
          gesture_expires_at_ms: { type: integer, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [control_request]
      trigger: user
    join:
      prompt: workflows/join.md
      runner: recipe
      recipe: recipes/join.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          url: { type: text, required: true }
          title: { type: text, required: false }
          meeting_date: { type: text, required: false }
          gesture_id: { type: text, required: true }
          surface_session_id: { type: text, required: true }
          gesture_observed_at_ms: { type: integer, required: true }
          gesture_expires_at_ms: { type: integer, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [control_request]
      trigger: user
    pause:
      prompt: workflows/pause.md
      runner: recipe
      recipe: recipes/pause.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          session_id: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [control_request]
      trigger: user
    resume:
      prompt: workflows/resume.md
      runner: recipe
      recipe: recipes/resume.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          session_id: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [control_request]
      trigger: user
    stop:
      prompt: workflows/stop.md
      runner: recipe
      recipe: recipes/stop.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          session_id: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [control_request]
      trigger: user
  actions:
    sync_sessions:
      workflow: sync_sessions
      input_from: sync_sessions.input
      result_from: sync_sessions.result
    sync_threads:
      workflow: sync_threads
      input_from: sync_threads.input
      result_from: sync_threads.result
    read_transcript:
      workflow: read_transcript
      input_from: read_transcript.input
      result_from: read_transcript.result
    sync_takeaways:
      workflow: sync_takeaways
      input_from: sync_takeaways.input
      result_from: sync_takeaways.result
    sync_upcoming:
      workflow: sync_upcoming
      input_from: sync_upcoming.input
      result_from: sync_upcoming.result
    search_meetings:
      workflow: search_meetings
      input_from: search_meetings.input
      result_from: search_meetings.result
    listen:
      workflow: listen
      input_from: listen.input
      result_from: listen.result
    join:
      workflow: join
      input_from: join.input
      result_from: join.result
    pause:
      workflow: pause
      input_from: pause.input
      result_from: pause.result
    resume:
      workflow: resume
      input_from: resume.input
      result_from: resume.result
    stop:
      workflow: stop
      input_from: stop.input
      result_from: stop.result
  resources:
    # The local physical model reserves a 32K input window before dispatch.
    # Leave room for that reservation plus actual usage from earlier turns.
    per_run:
      max_tokens: 131072
      max_cost_usd: 0.50
      max_active_seconds: 600
    monthly:
      max_tokens: 900000
      max_cost_usd: 12.00
    storage:
      max_records: 20000
      max_bytes: 33554432
  dependencies:
    procedure_skills: []
    tools:
      - name: meetings_data
        version_requirement: "^1.0"
        actions:
          - active_session
          - list_threads
          - read_thread
          - read_takeaways
          - upcoming_meetings
          - search_meeting_memory
  assets: []
---
# Meetings

The meetings console as an app, additive alongside first-party `/observe`.
`/observe`'s meetings sections, the TopBar capture dot, and the fast-poll lease
are untouched: this package is a NEW consumer of unchanged rails, never a route
move. Rollback is **disable**, never uninstall.

## Read path

The six read workflows use only the closed, read-only `meetings_data` actions
declared in the dependency lock. Runtime scope is executor-owned; workflows
never accept or forward principal/workspace fields. Every call is bounded to
one page, scans keep their source `scan_truncated` truth, and an empty page is
never retried with wider filters.

`active_session` is registry-wide by design — capture that is running must
never be invisible to the operator — and deliberately narrower than the
first-party endpoint: it carries no rolling summary. Meeting *content* reaches
this package only through the scoped thread, transcript and takeaway reads.

The transcript read pages backwards from newest with an exact cursor. An
unknown `before_message_id` is refused rather than silently re-serving the
newest page, so the console's pager can never loop back to page one.

## Controls: local ledger, signed destination seam

The five control workflows are governed actions with exact inputs. They write
one `control_request` source record with a stable request id, the exact verb,
the target, the gesture that carried the owner's intent, and the canonical
payload. They do **not** start, pause or stop anything and do not fabricate a
receipt. The row keeps `actor_ref` null and stays immutable at
`apply_state: recorded`: the authenticated host stamps the actor at admission,
and mutating the head after signing would invalidate exact receipt retries.

Authoritative application is exclusively the reviewed
`magician.meeting-control` destination seam: an owner-signed,
authority-bound envelope that calls **the same** entries the first-party
`/meetings` API calls — `join_meeting_with_scope` for the attendee rail,
`start_passive_listener` for the listener rail, and the two managers' own
pause/resume/stop transitions. There is no second capture-creation path, and an
oracle pin in the destination module proves one cannot compile into being.

`control_receipt` is projected only from such a signed destination result. A
successful action run is not an application receipt.

## Intent, not just authority

`listen` and `join` are the START class. Reaching the shared join path proves
this package is *allowed* to; it does not prove a person asked. Every START
therefore carries a surface gesture — a unique act id, the frame session it
happened in, and an expiry no longer than two minutes — sealed into the
proposal, the owner's signed display and the decision id.

What that buys, exactly:

- The whole signed START is a two-minute object. The proposal pins the gesture
  inside its own lifetime and caps that lifetime, so a command cannot be sealed
  with a window that opens weeks later.
- The destination re-checks the window against its OWN clock immediately before
  it touches a manager — not the clock from when the request arrived.
- The destination consumes the signed decision id durably, so one envelope
  applies at most once.
- A START is refused outright while any capture is live on either rail, and the
  check and the start are one critical section. This path is deliberately
  stricter than the first-party API, which the operator drives directly.

What it does **not** prove: that a person was present. `surface_session_id` is
minted by this frame; no host-side registry witnesses the act. The gesture
attests recency and single-use; the owner's signature over a display that shows
the window is what carries intent. A host-minted intent ticket would close that
last gap and is recorded as owed work rather than claimed here.

A pause, resume or stop carries no gesture. Accepting one would make a
risk-reducing act read, in the owner's signed display, like an intent-bound
start.

Every accepted and every refused control lands in the shared capture-control
audit alongside the first-party handlers' own rows, tagged with this
installation, the signed decision, and the gesture. An operator asking "did an
app try to open my microphone" never has to infer the answer from an absence.

## Stopping never depends on this frame

The first-party paths stay authoritative. `/observe`, the TopBar dot and
`POST /meetings/{id}/stop` continue to work with this package disabled, absent,
or its frame closed. The console's controls are a convenience overlay, never
the only switch.

## Surface safety and bounds

`surfaces/console.js` is dependency-free and communicates only through the
reviewed eight-operation bridge. It uses `contract_capabilities` and bounded
`query_data` calls; it does not use `fetch`, XHR, WebSocket, `window.open`,
`eval`, `innerHTML`, or dynamic script. All app data is rendered with
`textContent` and DOM constructors. Each entity read is capped at 20 rows and a
bridge request times out closed.

Polling is 5 seconds while a capture is live on the Live panel and 10 seconds
otherwise, and stops entirely while the frame is hidden. It does **not** tighten
on entity-change signals: this console never calls the change-read operation,
and claiming otherwise would describe a responsiveness it does not have. The
change-signal subscription is the same deferred work as the streaming
primitive.

## Surfacing and distribution

The manifest declares two bounded native widgets, one state indicator, and the
`/meetings-console` navigation entry. Widgets read only package-owned
declarative projections; the `meetings_data` binder stays behind the explicit
governed sync workflows, so a widget button cannot gain workflow-free binder
authority.

The capture indicator is a convenience, **not** the capture-visibility
mechanism. That rests on the host-rendered TopBar dot, which is Layer-1 and
untouched by this package.

The chip names one record — the `capture_session` row that is both `live` and
`in_scope` — and says so in its selector. `capture_session` carries every
rail's sessions, including ended ones still inside their retention window and
other scopes' rows, so an unfiltered one-row read would have let store order
decide what the operator saw. `in_scope` is the half that has to be in the
filter, not just in the prose: a cross-scope row keeps its `live` and `status`,
so a `live`-only selector would have rendered another principal's recording as
this operator's own capture state, and a second scope's capture would have
blanked this one's chip by matching a second row. Two simultaneous live
captures *in this scope* still hide the chip rather than pick between them; the
TopBar dot still shows that capture is running.

Both widgets render natively or not at all. A client missing
`declarative_table_v1` hides them, because a `view` fallback would need a
second declared read and this manifest carries one closed read per widget.

These declarations do not mint system authority. Only the separately owned
host-controlled, digest-pinned boot admission can establish that provenance;
ordinary staging and SDK/VibeDev publication must reject the manifest class
claim.
