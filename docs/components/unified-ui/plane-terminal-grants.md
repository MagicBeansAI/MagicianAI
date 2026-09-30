# Plane terminal grants panel

`TerminalGrantsPanel` (`src/lib/plane/TerminalGrantsPanel.svelte`, mounted in Settings next to
Device Pairing) manages the Magician plane's `plt_` terminal grants — the scoped, expiring
credentials a terminal harness binds to Magician's MCP door with.

- **List**: label, engine, workspace, agent, tool scope (count, or "floored catalog"), expiry,
  revoke. Expired rows strike through.
- **Mint**: label / workspace / agent identity / engine dropdown (driven by
  `LAUNCHABLE_ENGINES` — only engines this build can actually launch, so the dropdown can
  never offer a name that would silently fall back to Magician's own loop) / TTL 1–2160h /
  optional per-run ceilings (USD, wall-clock, concurrent runs) / comma-separated allowlist.
- **One-time token**: shown once at mint with any `NEVER_ON_THE_PLANE` floor-dropped tools;
  the store keeps only a hash.

The chat and background-run engine pickers live in Settings → Engines (`EnginesPanel`); the
composer's choice (`chatHarnessPreferenceStore`, reconciled with the server by
`reconcileWithRoster`) rides every chat send and voice `session.start`, and
`chat.engine.updated` switches open composers.

Data layer: `src/lib/plane/terminalGrants.ts` (`listTerminalGrants`, `mintTerminalGrant`,
`revokeTerminalGrant`) over the shared `scopedRequestHeaders` convention. Routes:
`GET/POST /api/magician/v2/plane/grants`, `DELETE /api/magician/v2/plane/grants/{id}` — see
`docs/components/magician/plane.md` for the authority model (engraved workspace, allowlist
floor, run authority with ceilings).

The dropdown lists `claude_code`, `codex`, `codex_app_server`, `grok`, and
`agy` plus the deliberate `magician` pin — each backed by a registered
engine (`LAUNCHABLE_ENGINES` in `terminalGrants.ts` mirrors the server
roster; `codex` and `codex_app_server` share the `codex` binary, so both
track its install status).

The engine dropdown is install-aware: `fetchEngineAvailability` reads
`GET /api/magician/v2/plane/engines` (server-side PATH lookup) and greys out
engines not installed on the machine, falling back to the static roster when
the endpoint is unreachable — only installed engines are offered. A live chip (`Thinking with:`) shows
the current process default engine from the same endpoint's `current` field
(`magician` renders as “Magician (own loop)”).

A second picker is the **chat mouth**. It saves this browser's chat choice and
shows each engine's `native_tool_posture`. The loop's
`execution.harness_engine` (`current`) and this browser's chat choice are
independent. Saving an uninstalled CLI is refused.
When Magician or Pi is selected, its second dropdown lists Magician API chat
profiles. Other chat harnesses show model choices. Saving the chat form writes
the browser-local choice shared with the composer, across chat sessions; each
device or browser can select its own engine and profile.

The chat composer uses this same install-aware roster for a per-turn engine
choice. It sends `harness_engine` and `harness_model` on the chat request;
clients without a per-turn choice use the server default. Pi is in
the roster for chat and run selection, but its Plane bridge does not expose a
terminal MCP grant picker because pinned Pi 0.87.1 has no native MCP client.

Model pickers: both engine selects carry a model
dropdown fed by the roster's per-harness `models` axis (probe-verified
ids; `default` = the CLI's own choice, reset on engine change, disabled
for `magician`). A second form switches the **run** engine
(`PUT /plane/engine`). When Pi is the run engine, the form also offers a
Magician profile picker populated from chat-eligible profiles, plus Pi's own
credentials/model setting. The profile is saved in `execution.pi_profile` and
restored on refresh; adaptive choices use their fast model. The Pi profile
replaces the run-model dropdown, keeping the form at two selectors on narrow
screens.
The run switch sets the engine for runs launched after it: when an external harness drives, the flow's eligible
non-local operations follow its one-shot MagicLLM profile. Operations with a
local base profile are exempt and retain their locality-aware config mapping.
The parent engine is flow-scoped, never a process value (`query_analysis/parent_engine.rs`);
a run keeps its launch-time engine pin (`plane/engine_pin.rs`). `codex_app_server` primary
turns still use App Server, while their secondary stateless operations bridge
to `op-harness-codex`.
