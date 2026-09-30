# Claude Code coding-engine contract

Phases C0–C5 of
Claude Code and Antigravity.
Claude Code is a live VibeDev coding option next to Pi, Codex, Grok, and
Antigravity when
`coding.claude.enabled` is on, exactly one reviewed `claude` binary is found,
the CLI version meets `2.1.229`, subscription auth is present, and a
background isolation receipt matches the current identity + version.

Companion: [Grok Build CLI coding contract](grok-coding-engine-contract.md).

## Frozen names

| Thing | Value |
| --- | --- |
| Rust kind | `CodingEngineKind::ClaudeCode` |
| Wire / `engine_str` | `claude_code` |
| Profile id | `claude-default` |
| Picker label | `Claude` |
| Config block | `coding.claude` |
| Kill switch | `coding.claude.enabled` (template default **false**; repo-root / live seed **true**) |
| Operator binary | `coding.claude.binary` (optional; browser/model/tool cannot set it) |
| API-key inherit | `coding.claude.use_api_key` (default **false**) |
| Native continuation | Claude `session_id` UUID on `CodingContinuationRef.native_session_id` |
| Magician session id | `claude-{uuid}` (must not leak the native UUID into proposal raw) |
| Billing V1 | `CodingBillingBasis::External` |
| Minimum CLI | `2.1.229` (`claude --version`; newer is compatible) |

Pi remains the configured and UI default. Auto stays opt-in.

Sibling: [Antigravity coding contract](agy-coding-engine-contract.md)
(`CodingEngineKind::AgyCli` / `agy_cli`).

## Launch argv (C1)

Prompt is a single `-p` argv. Magician never uses `sh -c`. Never `--bare`
(that forces API-key billing and disables OAuth/keychain). Live turns do not
pass `--no-session-persistence` (needed for `--resume`). stdin is `/dev/null`.
`cwd` is the Magician shadow workspace. Spawn requires `require_outer_fence()`.

Build:

```text
claude
  -p <prompt>
  --output-format stream-json
  --verbose
  --permission-mode bypassPermissions
  --dangerously-skip-permissions
  --strict-mcp-config
  --setting-sources <empty string>
  --disable-slash-commands
  --no-chrome
  --safe-mode
  --disallowedTools WebSearch,WebFetch,Task,CronCreate,CronDelete,CronList,PushNotification,RemoteTrigger,Monitor,SendMessage,Workflow,ToolSearch
```

Discuss (`plan_only` / `ClaudeTurnMode::Discuss`): `--permission-mode plan`
and **without** `--dangerously-skip-permissions`.

Resume: `--resume <uuid>` when
`resume_session_id` is set.

Transport is NDJSON (`type`/`subtype` events), not JSON-RPC. Headless
**must** combine `--print`/`-p`, `--output-format stream-json`, and
`--verbose`.

## Subscription-first auth

Magician's live env may contain `ANTHROPIC_API_KEY` for Magician's own LLM
calls. If the Claude child inherits it, Claude Code bills the Anthropic API
instead of the Max subscription.

Default: **never** pass `ANTHROPIC_API_KEY`, `CLAUDE_CODE_API_KEY`, or
`ANTHROPIC_AUTH_TOKEN` to the child. `apiKeySource: "none"` on the init
event means Max / OAuth, not an API key.

`coding.claude.use_api_key` default **false**. Only when true may those keys
be copied from the parent allowlist.

Child env is `env_clear` then rebuilt from `PATH`, `HOME`, `USER`, `TMPDIR`,
`LANG`, `TERM`, plus `GIT_CEILING_DIRECTORIES=scope_root`. Every `MAGICIAN_*`
variable is dropped.

Reviewed binary locations (in addition to `PATH`):

- `/opt/homebrew/bin/claude`
- `/usr/local/bin/claude`
- `~/.local/bin/claude`

## Ready rule

Ready requires all of:

1. `coding.claude.enabled`
2. exactly one resolved binary
3. cached `claude --version` ≥ `2.1.229` (background tick only; 5s)
4. **subscription auth** (default): `~/.claude.json` has a non-empty
   `oauthAccount` object. Magician's `ANTHROPIC_API_KEY` does **not** count
   unless `coding.claude.use_api_key` is true
5. a cached isolation receipt for this identity **and** version (MCP list
   present and empty, tool list present with no WebSearch / MCP-style
   names, `apiKeySource` is `none` / `oauth` / `claude.ai` when not using
   an API key)

Public snapshot JSON omits paths, home, emails, and env names.
Auth-required reason is `sign in required`. Missing lists stay
`unqualified`. Dirty lists are `incompatible`.

Refresh: `POST /api/magician/v2/coding/engines/claude_code/refresh` returns
202 `checking` and observes filesystem + caches only (never
`claude --version`, never a live `-p` probe).

## Isolation probe (C3)

The qualify worker must not bill Max every 30s. After version + auth would
otherwise be Ready it launches `qualify_launch_args()` (`-p` with
`--input-format stream-json`, isolation flags, `--no-session-persistence`,
never `--bare`) in a disposable cwd that holds a uniquely named `.mcp.json`
canary. It writes one stdin user JSONL event, drains `system/init`, then
kills the process group. Isolation attempts back off 5 minutes per
identity. Probe session ids are not journaled.

A live turn that emits `system/init` with nonempty `mcp_servers`, any
plugin other than the reviewed CLI builtins, or forbidden tools
(`WebSearch`, `WebFetch`, `Task`, …) still fails closed. The reviewed
builtins are `agents-md` and `telemetry`, accepted only as
`path: "builtin"` with `source: "<name>@builtin"`: Claude 2.1.281 reports
both in every headless init even under `--setting-sources ""` and
`--safe-mode`, and they add no tools (the tool list is checked on its own).
A new builtin fails closed until reviewed.

The Claude, Agy and Grok qualify workers log a `warn` naming the engine and
the public-safe readiness reason whenever a probe fails or does not
qualify, so a missing engine is diagnosable from the service log.

Cockpit Stop registers `CodingControlHandle::Claude` and interrupts the
NDJSON drain; Drop kills the process group. Same-engine follow-up binds
`--resume <uuid>` from the chain continuation. If `--resume` never emits
`system/init`, Magician records `continuation_lost` and starts a fresh
`-p` once.

`claude_binary` in tool args is rejected the same way as `grok_binary`.
