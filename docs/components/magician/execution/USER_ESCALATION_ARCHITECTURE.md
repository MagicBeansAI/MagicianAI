# User Escalation Architecture

## Purpose

User escalation converts blocked or budget-exhausted agentic runs into a
pause-and-resume HITL instead of a terminal failure, so the user can unblock
or continue the run.

## Config

`execution.on_failure` (`OnFailureMode`) is threaded into `AgenticContext`:

- `ask_user` (default): pause and ask
- `fail`: keep terminal failure for unattended or CI-style runs

Ask-user is not an interview for generic give-up. A `Decision::Failed` /
`CannotProceed` without an auth or permission wall completes as terminal (or
partial success / yield-back to the previous owner). Protected-app failures
and auth/permission yield blockers still escalate.

## Runtime Behavior

Escalation maps onto the existing waiting-for-user pause flow
(`AgenticOutcome::WaitingForUser`, confirmation, max-iterations, and
resumable budget pauses). Chat consumes canonical
`HitlRequested { source: "agentic" }`, plus `AgenticMaxIterationsReached`
for the continue offer. The pause payload carries the prompt and an
`escalation_trigger` identifier.

Malformed LLM responses synthesize a terminal `Decision::Failed` (the
invalid-response path). That does **not** record a failed iteration and
continue: the run yields back to the owner, converts to partial success,
escalates under `ask_user` for a protected app, or ends `CannotProceed`.

Loop detection first applies loop pressure (advisory context, then continue).
Hard iteration and time budgets remain the emergency stop.

## User Actions

`EscalationListener` authors the chat buttons. Resume uses `input_type`, not
`escalation_type`.

| Trigger | Chat options |
|---|---|
| `cannot_proceed`, `loop_detected` | Provide Guidance, Mark Done |
| `tool_authorization` | Allow Once, Allow for This Run, Deny |
| `sandbox_override` | Allow Once, Deny |
| confirmation (or `action_type` present) | Approve, Deny |
| other `external_action` | Mark Done, Provide Guidance |
| `max_iterations` | Keep Trying, Stop |

Auth/permission yield HITL uses trigger strings `reauth` / `permission` and
the external-action option set. Guidance text is injected into the next
iteration. Mark Done / Done resumes with `external_action_completed`.

Keep Trying is a chat option only for `max_iterations`. It posts
`POST /api/magician/v2/executions/{id}/execution/agentic-continue`. The
Magicutor overlay still shows Done / Keep Trying / Cancel on any pause
with `escalation_trigger` set.

## Budget and iteration limits

`MaxIterationsReached` and resumable `BudgetExhausted` (`pause_state: Some`)
are the continue-or-cancel path: a fresh budget from the checkpoint via
`agentic-continue`. A `BudgetExhausted` with `pause_state: None` stays a
terminal failure. `is_continuable_pause` also accepts escalation-tagged and
manual pauses.

## Delivery Surfaces

- Chat HITL cards (`EscalationListener` → unified-ui `ChatPanel`)
- Magicutor extension overlay
- Magios `EscalationCard`
