# Plane terminal grants panel

`TerminalGrantsPanel` (`src/lib/plane/TerminalGrantsPanel.svelte`, in Settings
next to Device Pairing) manages the Magician plane's `plt_` terminal grants: the
scoped, expiring credentials a terminal harness uses to bind to Magician's MCP
door.

- **List**: label, engine, workspace, agent, tool scope (count, or "floored
  catalog"), expiry, revoke. Expired rows strike through.
- **Mint**: label / workspace / agent identity / engine dropdown / TTL 1–2160h /
  optional per-run ceilings (USD, wall-clock, concurrent runs) /
  comma-separated allowlist.
- **One-time token**: shown once at mint with any `NEVER_ON_THE_PLANE`
  floor-dropped tools; the store keeps only a hash.

Data layer: `src/lib/plane/terminalGrants.ts` (`listTerminalGrants`,
`mintTerminalGrant`, `revokeTerminalGrant`) over the shared
`scopedRequestHeaders` convention. Routes:
`GET/POST /api/magician/v2/plane/grants`,
`DELETE /api/magician/v2/plane/grants/{id}`. See
`docs/components/magician/plane.md` for the authority model (engraved
workspace, allowlist floor, run authority with ceilings).

## Engines

The mint dropdown lists `claude_code`, `codex`, `codex_app_server`, `grok` and
`agy` plus the deliberate `magician` pin. `LAUNCHABLE_ENGINES` in
`terminalGrants.ts` mirrors the server roster, so it never offers a name that
would silently fall back to Magician's own loop; `codex` and
`codex_app_server` share the `codex` binary and its install status. Pi is in
the roster for chat and run selection, but its Plane bridge has no terminal MCP
grant picker, because pinned Pi 0.87.1 has no native MCP client.

The dropdowns are install-aware: `fetchEngineAvailability` reads
`GET /api/magician/v2/plane/engines` (server-side PATH lookup) and greys out
engines not installed, falling back to the static roster when the endpoint is
unreachable. A `Thinking with:` chip shows the process default from the
endpoint's `current` field (`magician` reads “Magician (own loop)”).

The chat and background-run engine pickers live in Settings → Engines
(`EnginesPanel`):

- **Chat mouth.** Saves this browser's chat choice (shared with the composer
  across sessions; each device or browser picks its own) and shows each
  engine's `native_tool_posture`. It is independent of the loop's
  `execution.harness_engine` (`current`). Saving an uninstalled CLI is refused.
  With Magician or Pi selected, the second dropdown lists Magician API chat
  profiles; other harnesses show model choices. The composer's choice
  (`chatHarnessPreferenceStore`, reconciled by `reconcileWithRoster`) rides
  every chat send and voice `session.start` as `harness_engine` and
  `harness_model`; clients without one use the server default.
  `chat.engine.updated` switches open composers.
- **Run engine** (`PUT /plane/engine`). Applies to runs launched afterwards. A
  model dropdown is fed by the roster's per-harness `models` axis
  (probe-verified ids; `default` = the CLI's own choice, reset on engine change,
  disabled for `magician`). With Pi as the run engine, a Magician profile
  picker (chat-eligible profiles, saved in `execution.pi_profile`) replaces the
  model dropdown, plus Pi's own credentials/model setting; adaptive choices use
  their fast model.

When an external harness drives, the flow's eligible non-local operations
follow its one-shot MagicLLM profile; operations with a local base profile keep
their locality-aware mapping. The parent engine is flow-scoped, never a process
value (`query_analysis/parent_engine.rs`), and a run keeps its launch-time
engine pin (`plane/engine_pin.rs`). `codex_app_server` primary turns use App
Server, while their secondary stateless operations bridge to
`op-harness-codex`.

## Preference checks

`PreferenceChecksPanel.svelte` (`preferenceChecks.ts`) sits under Decision
routing in the same card.

- **Two states, shown apart**: the mode in force for this owner and the saved
  configuration's dry-run verdict (`GET /preference-alignment/status`). A
  rejected save shows its stable `code` and the `property` to fix, for example
  `alignment_enforce_route_required` with
  `llm.router.operation_mapping.action_alignment_judge`, and says the mode in
  force stays until a valid save loads.
- **Check policy**: "Recommended", "Just mention it", "Offer a choice" or
  "Always ask first" (`PUT /preference-alignment/policy`). The save carries the
  revision on screen; a stale save gets the newer policy back and shows it.
  Per-preference refinements are kept. An unreadable policy is saved over with
  `replace_unreadable`.
- **Notes from recent checks**: open notices
  (`GET /preference-alignment/notices?surface=web`), each a nudge on an action
  Magician went ahead with or a hold that expired unanswered. "Save this
  wording" posts the notice's token, bound to the exact wording shown; if the
  preference changed since, it reports that nothing was saved. "Dismiss" closes
  the notice.
