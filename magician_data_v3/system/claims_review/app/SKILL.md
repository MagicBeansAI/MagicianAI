---
name: claims-review
version: 0.2.11
description: "Owner review console for pending transcript claims, commitments, correction context, and signed decision receipts over the governed evidence seams."
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
      - route: /review
        document: surfaces/review.html
  widgets:
    - id: pending_claims
      title: Pending claims
      view: pending_claims
      read:
        kind: view_projection
        view: pending_claims
        entity: claim_summary
        fields:
          [claim_id, status, speaker_name, audience_kind, audience_id, expected_revision, created_at]
      refresh_hint:
        min_interval_seconds: 30
        max_staleness_seconds: 180
      bounds:
        max_rows: 8
        max_render_bytes: 32768
      actions:
        - id: refresh
          label: Refresh claims
          governed_action: sync_claims
      rendering: { kind: native }
      fallback: { kind: unavailable }
      suggested_slots:
        - page: /observe
          slot: reviews
          system_default: true
      required_capabilities: [declarative_table_v1, governed_actions_v1]
  navigation:
    - id: claims_review
      title: Claims review
      route: /claims-review
      placement: { kind: section, section: review }
      surface:
        kind: custom_surface
        entry_point: /review
        fallback_view: pending_claims
  data_policy:
    defaults:
      classification_floor: sensitive
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    claim_sync_page:
      fields:
        page_id: { type: text, required: true }
        after_claim_id: { type: text, nullable: true }
        next_cursor: { type: text, nullable: true }
        claim_ids_json: { type: text, required: true }
        synced_at: { type: timestamp, required: true }
    claim_summary:
      fields:
        claim_id: { type: text, required: true }
        status: { type: text, required: true }
        claim_text: { type: markdown, required: true }
        speaker_name: { type: text, required: true }
        audience_kind: { type: text, nullable: true }
        audience_id: { type: text, nullable: true }
        context_excerpt: { type: markdown, nullable: true }
        expected_revision: { type: integer, required: true }
        created_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    claim_detail:
      fields:
        claim_id: { type: text, required: true }
        transcript_id: { type: text, required: true }
        utterance_id: { type: text, required: true }
        speaker_id: { type: text, required: true }
        speaker_name: { type: text, required: true }
        claim_text: { type: markdown, required: true }
        prior_context: { type: markdown, nullable: true }
        following_context: { type: markdown, nullable: true }
        audience_kind: { type: text, nullable: true }
        audience_id: { type: text, nullable: true }
        extractor_id: { type: text, nullable: true }
        expected_revision: { type: integer, required: true }
        created_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    commitment:
      fields:
        term_id: { type: text, required: true }
        status: { type: text, required: true }
        direction: { type: text, required: true }
        term_text: { type: markdown, required: true }
        audience_kind: { type: text, required: true }
        audience_id: { type: text, required: true }
        named_person: { type: text, nullable: true }
        source_claim_id: { type: text, nullable: true }
        expected_revision: { type: integer, required: true }
        created_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    evidence_record:
      fields:
        record_id: { type: text, required: true }
        state: { type: text, required: true }
        summary: { type: markdown, required: true }
        created_at: { type: timestamp, required: true }
        synced_at: { type: timestamp, required: true }
    evidence_entity:
      fields:
        entity_key: { type: text, required: true }
        entity_kind: { type: text, required: true }
        label: { type: text, required: true }
        synced_at: { type: timestamp, required: true }
    review_decision:
      fields:
        request_id: { type: text, required: true }
        decision_id: { type: text, nullable: true }
        target_kind:
          { type: enum, values: [claim, commitment], required: true }
        target_id: { type: text, required: true }
        decision:
          {
            type: enum,
            values:
              [confirm_claim, reject_claim, record_commitment, confirm_commitment],
            required: true,
          }
        expected_revision: { type: integer, required: true }
        actor_ref: { type: text, nullable: true }
        reason: { type: text, nullable: true }
        payload_json: { type: text, required: true }
        apply_state:
          { type: enum, values: [recorded], required: true }
        decided_at: { type: timestamp, required: true }
    review_receipt:
      fields:
        receipt_id: { type: text, required: true }
        decision_id: { type: text, required: true }
        target_kind:
          { type: enum, values: [claim, commitment, ingest], required: true }
        target_id: { type: text, required: true }
        outcome:
          { type: enum, values: [applied, idempotent, refused], required: true }
        actor_ref: { type: text, required: true }
        destination_revision: { type: integer, required: true }
        error_code: { type: text, nullable: true }
        applied_at: { type: timestamp, required: true }
    ingest_request:
      fields:
        ingest_id: { type: text, required: true }
        transcript_text: { type: markdown, required: true }
        speaker_mapping_json: { type: text, required: true }
        audience_kind: { type: text, required: true }
        audience_id: { type: text, required: true }
        outwardness_reason: { type: text, required: true }
        actor_ref: { type: text, nullable: true }
        apply_state:
          { type: enum, values: [recorded, applied, refused], required: true }
        act_ref: { type: text, nullable: true }
        submitted_at: { type: timestamp, required: true }
  views:
    sync_status:
      entity: claim_sync_page
      kind: table
      route: /sync
      columns: [page_id, after_claim_id, next_cursor, synced_at]
    pending_claims:
      entity: claim_summary
      kind: table
      route: /
      columns:
        [claim_id, status, speaker_name, audience_kind, audience_id, expected_revision, created_at]
    claim_context:
      entity: claim_detail
      kind: list
      route: /claims
    commitments:
      entity: commitment
      kind: table
      route: /commitments
      columns:
        [term_id, status, direction, audience_kind, audience_id, named_person, expected_revision]
    evidence_history:
      entity: evidence_record
      kind: list
      route: /evidence
    entity_context:
      entity: evidence_entity
      kind: table
      route: /entities
      columns: [entity_key, entity_kind, label, synced_at]
    decisions:
      entity: review_decision
      kind: table
      route: /decisions
      columns:
        [request_id, target_kind, target_id, decision, expected_revision, apply_state, decided_at]
    receipts:
      entity: review_receipt
      kind: table
      route: /receipts
      columns:
        [receipt_id, decision_id, target_kind, target_id, outcome, actor_ref, destination_revision, applied_at]
    ingests:
      entity: ingest_request
      kind: table
      route: /ingests
      columns: [ingest_id, audience_kind, audience_id, actor_ref, apply_state, submitted_at]
  workflows:
    sync_claims:
      prompt: workflows/sync-claims.md
      runner: recipe
      recipe: recipes/sync-claims.json
      uses: [evidence_data]
      input:
        type: object
        fields:
          status: { type: text, required: false }
          audience_kind: { type: text, required: false }
          audience_id: { type: text, required: false }
          text: { type: text, required: false }
          limit: { type: integer, required: false }
          after_claim_id: { type: text, required: false }
          claim_id: { type: text, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [claim_summary, claim_detail, claim_sync_page]
      trigger: user
    sync_context:
      prompt: workflows/sync-context.md
      runner: recipe
      recipe: recipes/sync-context.json
      uses: [evidence_data]
      input:
        type: object
        fields:
          agent_id: { type: text, required: true }
          audience_kind: { type: text, required: true }
          audience_id: { type: text, required: true }
          commitment_status: { type: text, required: false }
          limit: { type: integer, required: false }
          after_term_id: { type: text, required: false }
          after_record_id: { type: text, required: false }
          after_entity_key: { type: text, required: false }
      result:
        kind: typed_value
        entities: []
      may_mutate: [commitment, evidence_record, evidence_entity]
      trigger: user
    confirm_claim:
      prompt: workflows/confirm-claim.md
      runner: recipe
      recipe: recipes/confirm-claim.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          claim_id: { type: text, required: true }
          expected_revision: { type: integer, required: true }
          reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    reject_claim:
      prompt: workflows/reject-claim.md
      runner: recipe
      recipe: recipes/reject-claim.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          claim_id: { type: text, required: true }
          expected_revision: { type: integer, required: true }
          reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    record_commitment:
      prompt: workflows/record-commitment.md
      runner: recipe
      recipe: recipes/record-commitment.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          claim_id: { type: text, required: true }
          expected_revision: { type: integer, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    confirm_commitment:
      prompt: workflows/confirm-commitment.md
      runner: recipe
      recipe: recipes/confirm-commitment.json
      uses: []
      input:
        type: object
        fields:
          request_id: { type: text, required: true }
          commitment_id: { type: text, required: true }
          audience_kind: { type: text, required: true }
          audience_id: { type: text, required: true }
          expected_revision: { type: integer, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [review_decision]
      trigger: user
    stage_ingest:
      prompt: workflows/stage-ingest.md
      runner: recipe
      recipe: recipes/stage-ingest.json
      uses: []
      input:
        type: object
        fields:
          ingest_id: { type: text, required: true }
          transcript_text: { type: markdown, required: true }
          speaker_mapping_json: { type: text, required: true }
          audience_kind: { type: text, required: true }
          audience_id: { type: text, required: true }
          outwardness_reason: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      may_mutate: [ingest_request]
      trigger: user
  actions:
    sync_claims:
      workflow: sync_claims
      input_from: sync_claims.input
      result_from: sync_claims.result
    sync_context:
      workflow: sync_context
      input_from: sync_context.input
      result_from: sync_context.result
    confirm_claim:
      workflow: confirm_claim
      input_from: confirm_claim.input
      result_from: confirm_claim.result
    reject_claim:
      workflow: reject_claim
      input_from: reject_claim.input
      result_from: reject_claim.result
    record_commitment:
      workflow: record_commitment
      input_from: record_commitment.input
      result_from: record_commitment.result
    confirm_commitment:
      workflow: confirm_commitment
      input_from: confirm_commitment.input
      result_from: confirm_commitment.result
    stage_ingest:
      workflow: stage_ingest
      input_from: stage_ingest.input
      result_from: stage_ingest.result
  resources:
    # The local physical model reserves a 32K input window before dispatch.
    # Leave room for that reservation plus actual usage from earlier turns.
    per_run:
      max_tokens: 131072
      max_cost_usd: 0.50
      max_active_seconds: 600
    monthly:
      max_tokens: 600000
      max_cost_usd: 8.00
    storage:
      max_records: 10000
      max_bytes: 16777216
  dependencies:
    procedure_skills: []
    tools:
      - name: evidence_data
        version_requirement: "^1.0"
        actions:
          - list_pending_claims
          - read_claim
          - list_commitments
          - list_evidence_records
          - list_entities
  assets: []
---
# Claims & Commitments Review

This system-class package exposes the two outward-assertion gates without
creating a second evidence authority. The rich `/review` console is a reviewed
custom surface for web and iOS. The manifest's ordinary declarative views are
the companion surface for every client, including Android.

## Surfacing and distribution

The manifest declares a bounded native pending-claims widget plus the
first-party `/claims-review` navigation entry. The widget reads only the
package-owned declarative queue projection; the `evidence_data` binder remains
behind the explicit `sync_claims` governed workflow. That refresh accepts the
exact empty object, so the widget button does not infer claim input or gain
workflow-free binder authority. The rich console retains `pending_claims` as
its declarative fallback on clients that do not host the reviewed custom
surface.

These declarations do not mint system authority. Only the separately owned
host-controlled, digest-pinned boot admission can establish that provenance;
ordinary staging and SDK/VibeDev publication must reject the manifest class
claim.

## Read path

`sync_claims` and `sync_context` use only the five closed, read-only
`evidence_data` actions declared in the dependency lock. Runtime scope is
executor-owned; workflows never accept or forward principal/workspace fields.
Every call is bounded to one page, substring scans keep their source
`scan_truncated` truth, and an empty page is not retried with wider filters.
The package stores projections for display; the pending-claims register,
commitment register, correction history, and entity source remain host-owned.
The host read seam returns only the fields those projections declare. Raw
evidence provenance, observed actions, artifacts, facets, entity/person keys,
scores, sensitivity labels, producer metadata, and extensible metadata never
enter this package; deleted evidence tombstones remain absent.

## Decisions: local ledger, signed destination seam

The four decision workflows are governed actions with exact inputs. They write
one `review_decision` source record with a stable request id, expected
destination revision, optional review reason, and exact payload.
They do not directly mutate a claim or commitment and do not fabricate an
apply receipt.

`request_id` is action idempotency, not the canonical signed `decision_id`.
The source row keeps `decision_id` and `actor_ref` null and remains immutable at
`apply_state: recorded`: mutating the head after signing would invalidate exact
receipt retries. A trusted host derives the proposal actor from authenticated
`actor_ref`; frame text is never accepted as actor authority. The actor and
canonical decision id are projected only in `review_receipt`. Authoritative application is exclusively
the reviewed `magician.claims-decision` destination seam: an owner-signed,
destination-head-bound envelope calls the same transition functions as the
first-party claims API and returns a durable receipt. A receipt projector may
write the resulting `review_receipt`; that entity is not proof until such a
signed destination receipt exists. Replays of one signed decision are
idempotent, stale revisions are refused, extractor self-confirm is refused,
and only a named person can confirm a commitment.

Destination admission reopens the exact live source head and fail-closes unless
its `request_id`/dedupe key, verb, target and audience, expected revision,
reason, null host-stamped fields, `recorded` state, and canonical `payload_json`
all equal the signed proposal. The source digest therefore cannot be borrowed
from an unrelated decision row.

No current consumer automatically converts a package `review_decision` record
into that signed envelope. Until a separately reviewed consumer exists, the
ledger remains `recorded` and neither it nor a successful action run is an
application receipt.

This manifest deliberately declares no workflow `contribution_ports`.
The current contribution-port manifest vocabulary describes hypothesis-style
proposal feeds; it is not authority for this decision family and must never be
used as a substitute for `magician.claims-decision`.

## Structured ingest and act-always

`stage_ingest` records a request, not an authoritative ingest. Its
`speaker_mapping_json` must be a closed JSON document containing explicit
speaker keys mapped to named people and an ordered utterance list that assigns
every utterance to one of those keys. Raw pasted prose is never parsed to guess
who spoke. The caller also supplies the exact audience and a human explanation
of why the transcript is outward-facing.

The host applies a staged request only through the canonical
`TranscriptIngestion` entry. That entry is **act-always**: every accepted ingest
creates its durable act row even when extraction yields zero claims. The
package must not infer success from claim count; only the host receipt/act ref
can move an ingest projection from `recorded` to `applied`.

The custom frame launches only the minimal direct request: action name,
idempotency key, and exact reviewed input. It deterministically derives the
request id from the action and immutable intent fields and uses that same value
as the launch idempotency key, so reload/retry does not create a timestamp-
shaped second intent. It cannot provide mutable schema, grant, policy, scope,
or provenance fields; the authenticated host resolves those immediately before
admission. No frame-supplied field can redirect the run into another
installation or scope.

## Surface safety and bounds

`surfaces/review.js` is dependency-free and communicates only through the
reviewed eight-operation bridge. It uses `contract_capabilities` and bounded
`query_data` calls; it does not use `fetch`, XHR, WebSocket, `window.open`,
`eval`, `innerHTML`, or dynamic script. All app data is rendered with
`textContent` and DOM constructors. One session has a 24-message client budget,
each entity read is capped at 20 rows, and a bridge request times out closed.
Keyboard triage (`J` confirm, `K` reject) launches the governed source-ledger
action only after an explicit row selection and review reason. It never
applies an authoritative claim decision by itself.
Claim summaries remain a bounded 20-row page, while transcript-bearing claim
detail is queried only for the selected claim with an equality predicate and a
one-row limit. Other surface reads select only fields they actually render or
need for a reviewed action.

## Rollback

Once trusted system-package admission owns this class, rollback is persistent
disable, never uninstall. Its local projections, request ledgers, route, and
widget disappear from reach while every host register and first-party claims
API continues unchanged. Before that boot owner exists, the `system` claim is
deliberately inadmissible through ordinary staging/publication.
