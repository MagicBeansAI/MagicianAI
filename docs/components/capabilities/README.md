# Capabilities Docs

Landing page for capability packs (governed skills + compiled providers).

## Canonical References

- **[Pack Authoring Guide](authoring-guide.md)** — how to write a new pack.
- **[Skills authoring](../magician/skills-authoring.md)** — SKILL.md walkthrough.
- Live scoped skills: `magician_data_v3/scopes/<principal>/<workspace>/skills/`
- Source skills: [skillshub/](../../../skillshub/)
- Bot template: [skillshub/bots/bot_configs.yaml](../../../skillshub/bots/bot_configs.yaml)
- [Marimo Capability](marimo-capability.md)
- AI CLI Delegation (retired agent coding-delegate skills)
- [Runtime Config](../../../tool-runtime-config.yaml)
- [Architecture V2](../../ARCHITECTURE_V2.md)
- [Quick Start](../../quickstart.md)

## Pack format and resolution

A pack is a `SKILL.md` whose `metadata.magician.runtime_contract` uses
`schema_version: tool-runtime.skill-runtime.v1`, plus typed actions in
`runtime_actions`. Compiled providers (files, shell, duckdb, `task_state`,
`time_math`, …) ship as YAML embedded in the magician binary.

`load_pack_defs_for_scope` resolves in this order (earlier name wins):

1. `<scope>/skills/` (materialized by `make -C skillshub install-scope SCOPE=<principal>/<workspace>`; repo-root equivalent is `make skills-install-scope`)
2. Extra dirs from `tool-runtime-config.yaml` `registry.paths`
3. Embedded compiled defs

There is no `<scope>/capabilities/packs/` fallback, and
`magician_data_v3/system/` holds no pack/tool/bot templates.

Catalog for pickers and `/tools`: **`GET /api/magician/v2/skills`**. There is
no `/api/magician/v2/capabilities/packs` route.

Chat-spawned pack work creates V3 tasks. Progress renders as
`ChatMessageContent::TaskStatusUpdate` (no `PackProgress` variant). Channel
adapters may implement optional `sendFile` (`skillshub/bots/sdk`) to deliver
terminal `output_files`; telegram / telegram-self / whatsapp do, gmail and
kapso fall back to a URL text message.

Public skill names are AgentSkill kebab-case slugs. Catalog version is
`skillshub/VERSION` (**0.2.1**). Browser skill version is **0.5.4**.

## Runtime layout

- Skills: `<scope>/skills/<skill>/` with `{skill_runtime_root}` rewriting to
  that dir (`skills/path_rewrite.rs`). Per-skill env lives at
  `{skill_runtime_root}/config/.env`.
- Auth: `{scope_capability_auth_root}` = `<scope>/auth/`.
- Workdirs / home: `<scope>/workdirs/` and `workdirs/home/` (WhatsApp
  `WU_HOME` is `workdirs/home/.wu`).
- Bots: template `skillshub/bots/bot_configs.yaml`; live copy
  `<scope>/bots/bot_configs.yaml` (created if missing, never overwritten).
  Top-level `bots:` in `magician-config.yaml` is rejected. Bot source is
  `skillshub/bots/`, not `capability_templates`.
- Placeholders materialize as absolute paths. The dispatcher prepends
  `skillshub/node_modules/.bin` and `skillshub/.venv/bin` plus
  `<MAGICIAN_SKILL_DIR>/bin`.

Node is pinned at **24.20.0** (`skillshub/.nvmrc`; `package.json` `engines.node` is `24.x`).
Scope secrets are declared in each skill's `config/.env.example` and filled
by `make -C skillshub setup-env` from `skillshub/operator-config.yaml`
(gitignored runtime file). Existing non-empty `.env` values are kept.

There is no `PackHotReloadService`. Compiled providers require a rebuild;
skill YAML/SKILL.md changes apply on the next registry load.

## Skill-bundled binaries

Vendored binaries live in the skill's `bin/` (e.g. `metabase-pp-cli`,
`pdftotext`, `marimo`). `install-scope` symlinks them into
`<scope>/skills/<skill>/bin/`. Browser builds the host-native driver via
`make setup-agent-browser` (`AGENT_BROWSER_CARGO_TARGET_DIR` overrides the
cache).

## Google Workspace Packs

Shipped as governed skills, not pack YAML templates:

- [calendar](../../../skillshub/calendar/SKILL.md), [gmail](../../../skillshub/gmail/SKILL.md), [sheets](../../../skillshub/sheets/SKILL.md)

All three run `gws` with an `account` profile parameter (`work` default;
`personal` / `business`). OAuth state is
`{scope_capability_auth_root}/gws-<account>/` with a real
`client_secret.json` per account. `make -C skillshub setup-gws-accounts`
propagates the repo-root client secret. Auth scopes:
`gmail,sheets,drive,docs,calendar`. `GOOGLE_WORKSPACE_CLI_CONFIG_DIR` points
at the profile dir. Gmail watchers use `GOOGLE_WORKSPACE_PROJECT_ID` or
derive it from `client_secret.json`. Managed profiles can set
`GWS_PROFILE_LABEL`, `GWS_EXPECTED_EMAIL`, `GWS_REQUIRE_MANAGED_AUTH=true`
so Bot Control owns re-auth and does not delete the auth directory.

## Image Generation Packs

[image-generation](../../../skillshub/image-generation/) — Gemini Nano Banana
2 Lite / 2 / Pro via the `nanobanana2` binary
(`skillshub/image-generation/bin/nanobanana2`). Auth is
`NANOBANANA2_API_KEY` as a `secret_ref` from the vault, injected as env.
Needs `python3`, `google-genai`, Pillow. Prefer explicit `quality_tier`;
`auto` is `balanced`; `model` is a hard override.

## Video Generation Packs

[video-generation-via-veo](../../../skillshub/video-generation-via-veo/) —
Veo 3.1 Fast / Standard via the `veo31` binary. Auth: `VEO31_API_KEY`
(`secret_ref`), with `GEMINI_API_KEY` as `at_least_one` fallback. Needs
`python3` and `google-genai`. `make setup-capability-tools` installs the
shared Gemini Python deps used by both image and video skills. Output is a
local `.mp4`. No Vertex/ADC/GCS setup.

## Browser Pack Contract

Shipped [browser](../../../skillshub/browser/SKILL.md) version **0.5.4**,
driven by the pinned `agent-browser` CLI (snapshot refs, click/fill/press,
coordinate `mouse`, `eval`, CDP file-chooser helpers). Session mode is
chosen on the first call (`cdp` / `headed` / `headless`). Lightpanda is a
headless DOM-first engine preference; CloakBrowser or bundled Chrome is the
full-fidelity path. See the SKILL.md body for the verification contract.

## Core Utility Packs

Tools tagged `core_utility` stay in filtered catalogs unless the agent
excludes them. Shipped compiled helper: `time_math` (embedded pack def) —
local/UTC RFC3339, Unix, Apple-epoch, SQL half-open ranges. Call it before
date filters on sources such as iMessage.

## iMessage Pack

Compiled read-only SQLite provider for `~/Library/Messages/chat.db`. Needs
macOS Full Disk Access. Uses bundled `rusqlite`. Prefer direct table names
(`message`, `chat`, `handle`); `time_math` for `message.date` Apple-epoch
bounds.

## Metabase Pack

Single [metabase](../../../skillshub/metabase/) skill. Inner actions
(`database_list`, `card_run`, `dataset_query`, `find_search`, plus
`doctor` / `raw` / `help`) invoke `{skill_runtime_root}/bin/metabase-pp-cli`
with `--json --no-input --no-color --yes`. Printing Press **v4.0.3**
generates the CLI from `skillshub/metabase/scripts/spec.json`. Setup:
`make setup-printing-press`, `make regen-metabase-cli`,
`make -C skillshub setup-metabase-cli`. Spec refresh:
`make refresh-metabase-spec` then regen. Auth: `METABASE_BASE_URL` and
`METABASE_API_KEY` via `make -C skillshub setup-env` into
`<scope>/skills/metabase/config/.env`. Envelope:
`{"meta":{"source":"live"},"results":…}`. Design:
metabase CLI design.

`simple-data-analyst` uses `metabase` with `chat_inline: auto`.

Compiled inner-loop packs (`duckdb`, `imessage`, `read_trace`,
`internal_data`) dispatch primitives through
`execution/primitive_dispatch/compiled_provider.rs`. `internal_data` is
fail-closed on `__principal` / `__workspace`. Typed LLM observability
actions and `/analytics/llm/*` share `LlmAnalyticsReadService`.

Ask-mode media uses the direct `image-generation`,
`video-generation-via-veo`, `gif-search-via-klipy`, and
`meme-generation-via-imgflip` skills.

## Delegation Providers

Compiled providers for work outside the Magician repo:

- **`delegation_shell`** — expands `allowed_working_dirs` with the step's
  `working_dir`; injects `MAGICIAN_ROOT` when outside the repo.
- **`delegation_files`** — expands `allowed_roots` with the nearest existing
  ancestor of the target path.

Both call `validate_delegation_path()`: path must exist, be a directory,
be under `$HOME`, and not a sensitive system path. Sandbox is cloned per
execution.

## AI CLI Delegation (Architecture)

See ai-cli-delegation.md.
`skillshub/{claude,codex,agy,opencode}` agent tools are retired. Agent
coding uses `run_coding_task`. Operators still launch those CLIs from
Developer Mode. Do not confuse with `websearch-via-claude` /
`deep-research-with-claude`. See
[cli-delegate-ecosystem.md](../magician/cli-delegate-ecosystem.md).

## Web Research Packs

Ordinary path: `web-researcher` discovers with `content_search` and reads
with `content_read`. The controller chooses Exa, Tavily, DuckDuckGo,
source-native adapters, cache, and browser handoffs — those transports are
not a second model-authored ladder.

Situation-awareness may fan out to granted Reddit, HN, GitHub, Product
Hunt, arXiv, Polymarket, and YouTube tools before `catchup_merge`.
Exhaustive reports may use `deep-research-with-openai`. Neither is a
fallback for ordinary search. Direct `websearch`, Exa, Tavily, MiniMax,
OpenAI quick-synthesis, `htmltotext`, legacy `whatsgoingon2`, OSINT,
scraping-playbook, and presentation skills stay outside the default Sleuth
grant.

- Optional `websearch-via-claude` / `deep-research-with-claude` use current
  Anthropic aliases and are denied on the default agent.
- `deep-research-with-openai` always enables `web_search`.
- `websearch-via-openai` uses Responses API `filters.allowed_domains`
  (≤100 bare hosts).
- `gif-search-via-klipy` and `meme-generation-via-imgflip` are governed
  static-secret adapters (Imgflip is two-secret).
- `news-search-via-tavily` and `semantic-websearch-via-exa` follow current
  provider search-type guidance (`neural` → `auto`).
- `htmltotext` is the local extraction stage; HTTPS uses the OS trust
  store (`truststore` or `curl` fallback on issuer errors).
- `find-details-by-username` wraps Maigret `0.6.1` with `--ai` off.
- `unpublish_dashboard` (compiled, deferred) unpublishes a Desk surface by
  `surface_id`.

## Native macOS Dialogs

No standalone Magician capability for native file dialogs. Browser
uploads/downloads use the browser pack's CDP file-chooser commands.
Host-level desktop automation belongs in the macOS host bridge, not the
agent tool catalog.
