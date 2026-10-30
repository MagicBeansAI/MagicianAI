# Harness credentials (`execution.harness_auth`)

Claude Code, Codex, Grok, Antigravity (`agy`) and Pi keep their own sign-in.
A provider key in a CLI's environment usually wins over that sign-in and bills
the API instead, so Magician decides per CLI whether its launches get
Magician's provider key (`magician_v2::harness_auth`).

| Mode | CLI signed in | CLI not signed in, expired, or out of credits/quota |
| --- | --- | --- |
| `login_first` (default) | its own sign-in, no key | no key until the owner approves (below) |
| `login_only` | its own sign-in, no key | no key; the turn fails with the CLI's reason |
| `magician` | Magician's key | Magician's key |

**No automatic fallback** (`harness_auth::fallback`): under `login_first` a
missing, expired or rejected login, or an account out of credits/quota, fails
the turn with the CLI's reason and raises one owner decision on the shared
user-request prompts (`source: harness_auth`, `request_type:
harness_key_fallback`, answered through the canonical `/hitl/{id}/respond`
handle), e.g. "Grok: Your session has expired. Use Magician's xAI API key
instead? API charges apply." `once` retries the failed turn on the key (the
approval ends with that turn, at most 15 minutes); `always` writes
`engines.<cli>: magician` with the comment-preserving config writer; `no` (and
the 5-minute timeout) keeps the failure. A launch that already knows the login
is missing or expired asks before starting. One pending decision per scope,
CLI and failure class. Offered only when Magician holds that CLI's key
(`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`,
`GEMINI_API_KEY`/`GOOGLE_API_KEY`); never for Pi, whose route is its
Magician profile.

```yaml
execution:
  harness_auth:
    mode: login_first
    engines:          # optional per-CLI overrides
      codex: magician
```

`coding.claude.use_api_key` / `coding.agy.use_api_key` still work: `true` acts
as a `magician` override for that CLI when no override is set.

**Sign-in** is `signed_in`, `expired` or `signed_out`, read from the CLI's
own login state: `~/.claude.json` `oauthAccount`, `$CODEX_HOME|~/.codex/auth.json`
(tokens or a key), `$GROK_HOME|~/.grok/auth.json`, an Antigravity OAuth token
file, or a non-empty `$PI_CODING_AGENT_DIR|~/.pi/agent/auth.json`. A dangling
link (Grok deletes its login after a rejected refresh, leaving Magician's Grok
home link dangling), an empty or unreadable file is `signed_out`. `expired`:
every credential in the file is past its expiry with no refresh token; Grok
logged `auth.permanent_failure.set` after the file last changed (its message
is the detail); or a turn on the CLI failed authentication since then (cleared
by a successful turn). Only expiry and refresh-token presence are read, never a
credential.

**Failure reasons** (`harness_auth::failure`): a harness turn that fails for
authentication (expired session, 401/403), credits (402, insufficient
credits/quota, billing), rate or usage limits, or reachability is classified
from the CLI's output (its error events, stderr tail, launch error). The chat
reply, the run's failure, the Attention notice and the decision carry the
CLI's own reason (one line, at most 300 characters, secrets redacted) and a
remedy (`grok login`, `codex login`, `claude` `/login`, top up credits, wait
for the limit). Health notices are keyed per CLI (`harness:grok`), not per
model, so one engine failure is one notice; a Pi Magician profile is its own
service.

**Where it applies**

- Plane chat and agent-run engines: Claude Code gets `ANTHROPIC_API_KEY`,
  Codex `OPENAI_API_KEY`, Grok `XAI_API_KEY`, Antigravity the Google/Gemini
  keys and project settings only when Magician's credentials apply (the
  `magician` mode or the owner's approval). `require_cli_auth` accepts a key
  only then; otherwise it fails with the sign-in remedy.
- VibeDev coding engines and their qualification workers: the same keys are
  removed from the child environment unless Magician's credentials apply;
  readiness counts a key as sign-in only then.
- Background operations follow a harness-driven run onto its CLI
  (`op-harness-<engine>`, `parent_profile`) only when the CLI runs on its own
  sign-in, since `magicllm::providers::harness_cli` carries no keys; otherwise
  they keep Magician's route.
- Pi turns keep their Magician profile and its key either way: Pi has no
  subscription sign-in of its own unless the owner ran `pi` `/login`.
- Workbench terminals always remove provider keys
  (`ALWAYS_SCRUBBED_TERMINAL_ENV_KEYS`), whatever the mode.

**Settings**: Settings → Harness credentials (web, iOS, Android) shows each
CLI's sign-in and what it will use, with a default and per-CLI overrides; the
web panel shows e.g. "Session expired — run `grok login`", the CLI's own
reason, and that the key needs approval.
Engine pickers label each CLI from the `credentials` field (`login`,
`magician_key`, `expired`, `none`) on `GET /plane/engines` and
`GET /coding/models` engine rows, e.g. "Codex · your login".
`GET/PUT /api/magician/v2/settings/harness-auth` (owner session or the owner's
paired iOS/Android app): `{ mode, engines, status }`, each status row
`{ mode, signed_in, sign_in, sign_in_detail, remedy, last_failure: { class,
reason, remedy, at_ms } | null, uses_magician_credentials, key_fallback:
"ask" | "always" | "never", magician_key_available }`; PUT `{ "mode"?,
"engines"?: { "<cli>": <mode> | null } }` writes the live config and applies at
once.

**Replies are the final message only**: one-shot CLIs (codex, grok, agy) read
their reply only from reply events; status, progress and error events
(codex `error` "Reconnecting…", `turn.failed`, Grok `api_retry`, `is_error`
results) go to the log and the failure text, and only a failing exit appends
the CLI's last error. A clean exit with only error events is a refused turn.

**Cost**: a Pi chat turn on a Magician profile records `cost_usd` like a Pi
coding run (Pi's `get_session_stats`): the private model entry Magician
writes carries a `cost` table from Magician's pricing (`llm_pricing.json`).
