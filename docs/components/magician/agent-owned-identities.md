# Agent-Owned Identities

Presto (the `personal-assistant` agent) has its **own** external accounts,
distinct from the human owner's. This doc covers those identities, the dedicated
Google skills Presto owns, and the `self_identity` memory tier that keeps the
facts durable in the agent's context.

> **Key distinction:** an *agent-owned* identity is the assistant acting **as
> itself**. It is NOT the owner. The owner is the human the agent escalates *to*
> (see [Envoy Agent](envoy-agent.md) → `owner_identities`). Never conflate the
> two — putting an agent's own address on the owner allowlist would bypass the
> envoy trust diode.

## Presto's own identities

| Channel | Address | How Presto uses it |
|---------|---------|--------------------|
| Google Workspace | `reach.magican@gmail.com` | dedicated `presto-gmail` / `presto-calendar` / `presto-sheets` tools (hard-pinned forks of the gws skills) |
| Email (AgentMail) | `magican@agentmail.to` | `agentmail-read` / `agentmail-send` tools |
| WhatsApp (Kapso) | Presto's own number | `kapso-whatsapp-read` / `kapso-whatsapp-send` |

The runtime bot that watches the `reach.magican@gmail.com` inbox is the
`gmail-presto` managed bot (see the scope's `bot_configs.yaml`).

## The forked `presto-*` Google skills

The gws skill family is exactly **`gmail` + `calendar` + `sheets`** (the
`gmail,sheets,drive,docs,calendar` OAuth scopes are granted, but only those
three skills exist). Presto's Google identity is a set of **dedicated,
hard-pinned forks**, not a `presto` value on the shared skills:

| Owner's accounts (multi-account) | Presto's own identity (pinned) |
|----------------------------------|--------------------------------|
| `gmail` / `calendar` / `sheets` — `account: personal\|work\|business` | `presto-gmail` / `presto-calendar` / `presto-sheets` — **no `account` param** |
| held by **Vera** (`executive-assistant`) | held by **Presto** (`personal-assistant`) |

Each fork is the base skill with the `account` parameter removed and the gws
profile hardwired (base skills use `profile_selection: {mode: selectable,
default: work}`):

```yaml
# skillshub/presto-gmail/SKILL.md — auth (resolves to {scope_capability_auth_root}/gws-presto)
profile_selection:
  mode: fixed
  alias: presto
injections:
  - source: {kind: profile_auth_root, path: []}
    target: {kind: environment, name: GOOGLE_WORKSPACE_CLI_CONFIG_DIR}
  - source: {kind: profile_auth_root, path: [cloudsdk]}
    target: {kind: environment, name: CLOUDSDK_CONFIG}
```

**Why forks instead of an alias.** The shared skills default to `account: work`
(the owner's account), and only prompt guidance would stop a call from acting as
the owner — an identity-leak risk for an identity-fronting agent. A fork
**cannot** touch another account: the config dir is pinned and there is no
`account` param to get wrong. This mirrors Presto's other identity tools
(`agentmail-*`, `kapso-whatsapp-*`).

- **Presto** holds `presto-*` → always acts as `reach.magican@gmail.com`.
- **Vera** keeps the shared `gmail` / `calendar` / `sheets` with the owner's
  `personal` / `work` / `business` accounts.
- `simple-data-analyst` keeps the shared `sheets` (owner data).

**Maintenance.** The forks are copies: a gws-CLI feature change must be applied
to both the base skill and its `presto-*` fork.

**Auth.** The forks reuse the `gws-presto` OAuth profile
(`<scope>/auth/gws-presto/`), the same one the `gmail-presto` bot watches. Setup
is the one-time `gws auth login` for that profile.

## The `self_identity` memory tier

Presto's identity facts live in a dedicated runtime memory tier so they are
structured, tool-readable, and always in the agent's working context. The
persona deliberately does not repeat them.

```yaml
# personal-assistant/definition.agent.yaml → memory_tiers:
- name: self_identity
  scope: agent
  description: Magican's own durable external identities (immutable self-facts…).
  schema:
    facts:
      type: text
  render:
    format: compact_summary
    template: '{facts}'
  retention: forever
```

**Why a dedicated tier:**

- **Injected every chat turn.** `render_chat_memory_block` (`chat/service.rs`)
  sweeps all agent-scope tiers into `## AGENT MEMORY`, ranked against the turn's
  relevance query within the `memory.prompt_scope_budgets.agent` budget
  (default 24 entries / 12,000 chars; the full stage also scores the hybrid
  index). `base_tier_priority`
  gives any tier whose name contains `identity` / `profile` / `preferences` the
  top band (90), so `self_identity` always makes the cut. (`prompt_pipeline.sections`
  is the autonomous-run path, not chat.)
- **Never overwritten.** LLM consolidation only mutates tiers named as a
  `memory_consolidation` rule `target`. `self_identity` has no such rule, so its
  only writer is the deterministic `update_memory_tier`; `retention: forever`
  stops expiry.
- **Existing tiers were unfit:** `personality_profile` (persona rewrites it),
  `environment_knowledge` (excluded from chat and LLM-rewritten),
  `entities`/`insights` (retrieval-gated + LLM-upserted), `user.preferences`
  (the owner's shared prefs).

### Seeding / updating the tier

There is **no REST endpoint** that writes a tier field (`agents/{id}/memory*` is
read-only; `POST /chat/memory/preference` appends an episode, not a tier).
Durable tier writes go through `merge_into_memory_tier` (`chat/service.rs`):

1. **The agent's `update_memory_tier` tool** (in-loop) — the normal path.
2. **In-process** `merge_into_memory_tier(resolver, principal, workspace,
   agent_id, …)` — e.g. the meeting-summary writer.
3. **Direct on-disk JSON** at
   `memory/agents/<agent>/tiers/self_identity.json` (`V3MemoryTierRecord` +
   `fields.facts`). This skips the search-index dirty flag: fine for injection,
   but semantic search misses it until a reindex.

> **Re-hydration:** a new tier must be declared in the agent definition AND the
> runtime restarted, because chat enumerates tiers from the in-memory
> definition.

## Meet-bot signed-in browser profile

The Google-Meet participant bot can join as a **signed-in
`reach.magican@gmail.com`** instead of an anonymous guest, skipping the "Ask to
join" lobby and auto-admitting to invited Workspace meetings. That identity lives
in a persistent Chromium profile dir, separate from the `gws-presto` OAuth token.

- **The engine is selected by `content_acquisition.browser.engine`**, through the
  same resolver as the `browser` tool (the development config selects
  cloak-browser). Explicit selections fail clearly when their resolver is missing
  or broken. CloakBrowser's exact concurrent-session denial is the sole automatic
  switch: the join retries once with bundled Chrome for Testing. Chromium
  profiles are **version-bound**: sign in with the configured engine's binary.
- **The profile is applied via a path-valued `AGENT_BROWSER_PROFILE` env** at
  join; the pinned agent-browser treats it as Chrome's `--user-data-dir` while
  keeping DevTools-port discovery in sync. A raw `--user-data-dir` launch arg
  **breaks the CDP connect** (agent-browser reads `DevToolsActivePort` from the
  dir it manages → 60 s timeout on a blank window).

**Location.** `build_browser_joiner` (`execution/compiled_providers.rs`,
macOS-only) resolves the profile dir in order:

1. `MEET_BOT_PROFILE_DIR` env override (custom path, or a temp path to force an
   ephemeral/concurrent join);
2. **`<scope workdir root>/meet-bot-profile`** — the default with agent scope,
   from `artifact_workspace.capability_workdirs_root` (for `anonymous/default`:
   `magician_data_v3/scopes/anonymous/default/workdirs/meet-bot-profile`,
   gitignored);
3. a per-join `…/magician-meet-<uuid>` temp dir for detached / scope-blind joins.

A stable profile means **one meeting at a time** (Chrome locks a user-data-dir to
one process) — the right trade-off for a single signed-in identity. Off macOS the
bot rides an already-open browser.

**One-time sign-in** (with magician stopped, so nothing holds the lock). Launch
the **cloak Chromium binary directly** — not through agent-browser, not another
Chrome:

```bash
PROFILE="$(pwd)/magician_data_v3/scopes/anonymous/default/workdirs/meet-bot-profile"
mkdir -p "$PROFILE"
CLOAK="$(ls -d "$HOME"/.cloakbrowser/chromium-*/Chromium.app/Contents/MacOS/Chromium | tail -1)"
"$CLOAK" --user-data-dir="$PROFILE" --no-first-run https://accounts.google.com
#   → sign in to reach.magican@gmail.com BY HAND (never script the password), then quit
#     Chromium (Cmd-Q) to release the profile lock.
```

Keep the sign-in in the **Default** profile: a second profile (`Profile 1`)
triggers Chrome's profile picker and the join never reaches the Meet URL. The
join log should show `user_data_dir=…/workdirs/meet-bot-profile`, not a temp
path. If Google shows "this browser may not be secure", sign in by hand and/or
first establish account/device trust on normal Chrome.

## Related

- [Envoy Agent](envoy-agent.md) — the owner-vs-guest trust model and
  `owner_identities` (the *human* owner, never the agent itself).
- [`agent_templates/README.md`](../../../magician_data_v3/system/agent_templates/README.md)
  — agent definition format (persona, tools, memory tiers).
