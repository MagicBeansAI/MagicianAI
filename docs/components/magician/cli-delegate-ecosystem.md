# CLI Delegate Ecosystem

Companion doc to [Developer Mode Workbench](developer-mode-workbench.md).

Magician agents do not spawn `claude` / `codex` / `agy` / `opencode` as tools.
Those `skillshub/` packs and the coding-agent PTY controller are gone. Agent
coding goes through `run_coding_task` (Pi, Codex app-server, Grok ACP) with a
Magician shadow and `diff_approval`. Operators launch those CLIs from Developer
Mode through the HTTP/UI PTY lane (`interactive_process.operator_cli_programs`).
There is no agent-facing `interactive_process` catalog tool.

The same governed CLI-owned-session baseline also serves non-coding native
tools such as Higgsfield. Higgsfield is a batch CLI, not a coding delegate: its
`SKILL.md` calls the reviewed installed native executable, and manifest-owned
argv rules deny `auth` and limit `workspace` to `list`/`status`. Magician
contains no Higgsfield dispatch branch.

## Agent vs operator

| Who | Path |
| --- | --- |
| Magician engineering agents | `run_coding_task` (VibeDev coding engines) |
| Operator in Developer Mode | HTTP PTY sessions starting `claude` / `codex` / `agy` / `opencode` |
| Retired | `skillshub/{claude,codex,agy,opencode}` agent tools |

## Operator HTTP/UI PTY lane

PTY implementation lives in `magician-pty` (`start_session`,
`registry_for_scope`, `program_concurrency_limit`,
`count_program_sessions_globally`).
`magician/src/magician_v2/execution/interactive_process.rs` is a re-export
shim. The sole production caller of `start_session` is the HTTP API in
`magician-api/src/interactive_process_api.rs`:

- `POST /api/magician/v2/interactive-sessions` — start a scoped PTY
- `POST /api/magician/v2/interactive-sessions/{id}/stdin` — base64 keystrokes
- `GET /api/magician/v2/interactive-sessions` — list live sessions
- `GET /api/magician/v2/interactive-sessions/cli-runtimes` — server-owned catalog
- `GET /api/magician/v2/interactive-sessions/{id}/buffer` — non-draining replay
- `GET /api/magician/v2/interactive-sessions/{id}/diff` — session git diff
- `DELETE /api/magician/v2/interactive-sessions/{id}` — close

Endpoints are scope-checked. Sessions are keyed per `(principal, workspace)`.
PTY chunks broadcast as `RuntimeTransportEvent::InteractivePtyChunk` into
`ui/unified-ui/src/lib/shell/InteractiveTerminalPane.svelte`.

## Operator CLI catalog and concurrency cap

```yaml
interactive_process:
  operator_cli_programs: [claude, codex, agy, opencode]
  max_concurrent_per_program: {claude: 1, codex: 1, agy: 1, opencode: 1}
```

`operator_cli_programs` is the server-owned Workbench catalog, read through
`GET /api/magician/v2/interactive-sessions/cli-runtimes`. Pi is not listed: it
is the internal `run_coding_task` adapter, not an operator CLI.

Each of those four programs is capped at **one live PTY session process-wide**
(across all principals/workspaces). `start_session` enforces the cap before
spawn. Programs without a map entry have no cap. Operators raise the cap in
`magician-config.yaml` once the upstream supports concurrent sessions. Caps
exist because concurrent sessions share one subscription, race plan-mode
confirmations, and fight local file locks.

## Auth handoff

Governed children get the CLI-owned environment baseline: `HOME` is available
so the installed program can read its own login/config; the ambient host
environment is not inherited. Claude and Codex may receive a declared optional
API-key binding after authorization. First-run browser/device login remains an
operator prerequisite.

### TLS trust for governed children

The portable baseline carries `SSL_CERT_FILE` after `env_clear`. The value is
resolved from a fixed candidate list, never from the parent environment.
Absence degrades to the interpreter default rather than failing launch. OS-owned
stores (`/etc/ssl/cert.pem`, `/etc/ssl/certs/ca-certificates.crt`,
`/etc/pki/tls/certs/ca-bundle.crt`, `/etc/ssl/ca-bundle.pem`) are tried before
package-manager prefixes (`/opt/homebrew/etc/ca-certificates/cert.pem`,
`/usr/local/etc/openssl@3/cert.pem`). Add new candidates to the group that
matches who owns the path.

### Memory ceilings for governed children

A skill may declare `runtime.limits.memory_bytes`; the runtime will not run a
child under a bound it cannot hold. `memory_limit_enforcement()` reports how:
`setrlimit(RLIMIT_AS)` where that resource is real, or a Darwin parent-side
watchdog on the process group's physical footprint. Elsewhere construction
refuses. A breach is `process_memory_exceeded`, not a timeout. Only
`document-to-markdown` declares a ceiling today. See
`docs/components/tool-runtime-core/README.md`.

## Where the integration lives

| Concern | Path |
|---|---|
| Agent coding | `run_coding_task` + [pi](pi-coding-engine-contract.md) / [grok](grok-coding-engine-contract.md) / [claude](claude-coding-engine-contract.md) / [agy](agy-coding-engine-contract.md) |
| Operator CLIs | HTTP PTY + `interactive_process.operator_cli_programs` |
| PTY implementation | `magician-pty/src/session.rs` |
| Re-export shim | `magician/src/magician_v2/execution/interactive_process.rs` |
| Concurrency cap | `start_session` + `program_concurrency_limit` / `count_program_sessions_globally` |
| HTTP surface | `magician-api/src/interactive_process_api.rs` |
| Configuration | `magician-config.yaml` → `interactive_process.max_concurrent_per_program` |
| Session registry | `InteractiveSessionRegistry` via `registry_for_scope` |
| UI pane | `ui/unified-ui/src/lib/shell/InteractiveTerminalPane.svelte` |

A live PTY holds the process-wide cap slot; remaining children are reaped on
executor drop. `display_mode: dev` mounts the
[workbench](developer-mode-workbench.md) (`ContextPill` or `cmd-shift-D`).
API mining ([`api-mining-pipeline.md`](api-mining-pipeline.md)) can capture
CLI HTTP traces so later visits replay without spawning the CLI.

## Related

- [Developer Mode Workbench](developer-mode-workbench.md)
- [API Mining Pipeline](api-mining-pipeline.md)
- [API Mining projections](api-mining-projections.md)
- Plan archive: `docs/archive/plans/2026-05-13-developer-mode-workbench.md`
