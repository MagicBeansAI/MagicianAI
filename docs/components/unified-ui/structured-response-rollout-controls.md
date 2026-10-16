# Structured Response Rollout Controls

This document captures the Unified UI contract for structured response rendering.
Generic migrated messages render a validated server-produced sidecar when its
complete canonical output fits the V1 bounds. When it cannot, the sidecar is
omitted and the canonical renderer remains authoritative; no data is truncated
to force a structured card. The legacy adapter and percentage rollout routes no
longer control production behavior.

## Scope

Applicable kinds:

- `tool_call_executed`
- `rich_tool_result`
- `attachment`
- `text`
- `escalation_resolved`
- `task_status_update`
- `task_status_update_live`
- `escalation` (active) is a deliberate no-rollout path: it remains on the
  specialized HITL card/native pause-elevation path while this migration tracks
  card-safe kinds only.

## Runtime contract

- `text`, `tool_call_executed`, `rich_tool_result`, `attachment`,
  `task_status_update`, and `escalation_resolved` use `presentation` only after
  schema, serialized 64 KiB envelope, and canonical-plain-text validation
  succeeds. The server accepts a supplied sidecar only when it exactly matches
  its deterministic canonical projection, including visible blocks and actions.
- The renderer compares against the semantic content projection, never the
  display-only prose used by legacy chat rows. This keeps tool summaries, rich
  result summaries, filenames, task status, and resolved-escalation summaries
  consistent with the backend, iOS, and bots.
- `open_artifact` resolves logical session-output and task-output targets. Task
  output paths stay scoped through the existing task-output URL builder.
- Active escalation and live task-tail status remain specialized components.
  They own pause authority and incremental run semantics, which are not generic
  presentation data.

## Migration disposition

Structured responses are fully migrated; no server runtime flag or cohort
assignment can disable production of a valid sidecar or its transport to a
capable realtime client. Historical `sr_*` query and local-storage keys have
no supported operational meaning. Missing, invalid, or over-bound
presentations fall back atomically to canonical content as data protection,
not as a rollout mechanism.
