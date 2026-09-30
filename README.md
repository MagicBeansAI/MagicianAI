<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/wordmark-dark.svg">
    <img src="docs/assets/wordmark-light.svg" alt="Magician" width="292" height="68">
  </picture>
  <p>
    <b>Powering superpowers for Work, Play, and all your Side Quests</b>
  </p>
  <br/>
  <p>
    <a href="magician/Cargo.toml"><img src="https://img.shields.io/badge/Magician-v0.7.89-7C3AED.svg" alt="Magician crate version 0.7.89" /></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License" /></a>
    <a href="#architecture"><img src="https://img.shields.io/badge/rust-2021%20edition-orange.svg" alt="Rust" /></a>
    <a href="#what-it-does"><img src="https://img.shields.io/badge/runs%20on-macOS%20%7C%20iOS%20%7C%20Android%20%7C%20Linux%20soon-lightgrey.svg" alt="Platforms" /></a>
    <a href="https://discord.gg/USDChWhdj"><img src="https://img.shields.io/badge/Discord-join-5865F2.svg?logo=discord&logoColor=white" alt="Discord" /></a>
  </p>
  <p>
    <a href="docs/project-versions.md">All crate and surface versions</a>
    &nbsp;&middot;&nbsp;
    <a href="docs/dependencies.md">Project-wide dependency inventory</a>
  </p>
</div>

<br/>

<div align="center">
  <h3 style="font-size: 1.375em; margin-top: 0.4em; margin-bottom: 1.6em;">
    Transform models and agents into Personal Intelligence that compounds with you.
  </h3>
  <p>
    Open-source and model-agnostic runtime to create and engage personal assistants that remember, act across apps and devices, and build new tools to get the job done - with explicit permissions, hard budgets, and a verifiable trail.
  </p>
</div>

> Models supply cognition. Agents supply reasoning loops.
> Magician supplies the persistent, governed world those loops operate within - where every action and tool use is both an output and the next input.
>
> Scheduler, isolation, permissions, accounting, drivers, package manager, ✨ **App Platform** ✨ and other [runtime primitives](docs/architecture/os-primitives.md) an assistant needs. Your machine or cloud.
>
> Personal Intelligence is your asset - Magician is built to keep it yours.

Choose local or hosted models for individual operations in [Model Routing settings](docs/components/unified-ui/model-routing-panel.md). Our guiding principle: you decide what stays private, where you need more capability, and how speed and cost shape the work.

**Powers Magican**, our AI superapp for desktop, Android, iOS and ESP32 — [next.magican.ai](https://next.magican.ai)

> [!WARNING]
> **Pre-release software.** Magician has not reached v1.0. APIs, configuration,
> data formats, CLI behaviour and user surfaces may change significantly before
> the v1.0 release. Pin versions for integrations and expect migrations between
> pre-release versions.

---

## 🚀 Quick Start

<details>
<summary><strong>Clone, install and run Magician</strong></summary>

### Install

Runs on macOS 14 or newer on Apple Silicon (Intel Macs are not supported). You need Rust, Node and `make`;
`make setup-prerequisites` installs the remaining host tools. Ollama and the
configured local models are installed for you.

Linux runs the stack through the container flow today, but the prerequisite
installer is macOS-only, so a native Linux install is still manual. A
first-class Linux path is coming.

```bash
git clone https://github.com/MagicBeansAI/MagicianAI.git
cd MagicianAI
make install
```

`make install` is the complete composed installer today. It asks for the
runtime data directory (default `~/MagicianNotes`) and execution flow, then
sets up prerequisites, the data root, local models, notes, runtime binaries or
container, meeting audio (BlackHole 16ch on macOS; Pulse tools and Xvfb on
Linux), the macOS desktop surfaces and optional Cloudflare Tunnel before
running a health sweep. Reboot once after a fresh BlackHole install.
Re-running it preserves existing config, secrets, notes and scopes.

#### Alternate: guided TUI installer

The capability-first terminal installer is available as an alternate guided
path while it converges with the composed installer:

```bash
make setup-wizard
```

It probes the machine, asks **how** to install and **what you want Magician to
do**, derives and explains the component plan, remembers prior choices, and
re-probes every step instead of trusting an exit code. The from-source macOS
path is usable now; prebuilt requires a local package through
`MAGICIAN_PACKAGE`, and the container choice is shown but not yet selectable.
The TUI does not yet invoke the core data-root/stack phase or final health
sweep itself, so finish a fresh setup with `make install` for now.

Use `make setup-wizard ARGS=--status` for a read-only inventory. See the
[guided installer](docs/components/magician-setup/README.md) for its exact
status and controls.

For CI or another non-interactive install:

```bash
MAGICIAN_INSTALL_MODE=dev MAGICIAN_INSTALL_FLOW=local \
  MAGICIAN_ROOT_DIR="$HOME/MagicianNotes" MAGICIAN_INSTALL_YES=1 \
  bash scripts/install.sh
```

The complete MODE × FLOW × RUNTIME matrix, browser modes and environment
variables are in the [full quick start](docs/quickstart.md).

### Run

Start the stack and open the UI:

```bash
make run-supervisor   # magic-supervisor :8081 manages magician :3002 + magicutor :3003
make run-ui-dev       # http://localhost:5173
```

Tell it who it is, then give it channels:

```bash
make setup-identity   # owner name, agent inbox, Cloudflare zone — never defaulted
make setup-bots       # WhatsApp, Telegram, Gmail and AgentMail channel daemons
```

### Optional capabilities

- [**Keys and paid services**](docs/setup/api-keys.md) — model providers, web
  research and media generation.
- [**Channels**](docs/setup/channels.md) — WhatsApp, Telegram and email, using
  your account or one owned by the agent.
- [**Google Workspace**](docs/setup/google-workspace.md) — OAuth, Pub/Sub and
  mail-assist setup.
- [**Mobile apps**](docs/setup/mobile.md) — Android toolchain, Xcode and device
  builds.

</details>

---

### ✨ The Magician Ecosystem

The runtime is the centre of a much larger system: **89** top-level AgentSkills
packages, **31** agent templates, **7** bot packages, and first-party web,
desktop, mobile and embedded clients. The short map is below; the
[ecosystem map](docs/ecosystem/README.md) owns the fuller boundaries, current
status and authoritative docs for every pillar.

| Pillar | What it adds |
| :--- | :--- |
| **Durable Agent OS** | Scoped identity, typed memory, task history, scheduling, permissions, isolation and a double-entry resource ledger — the persistent, governed world beneath any model or harness. |
| **Models, harnesses and Plane** | Bring local or hosted models, replace Magician's reasoning loop at chat/run boundaries, or keep an external harness and use Magician through its authenticated Plane MCP server. |
| **Memory, attention and autonomy** | Evidence, episodes, work ledgers and agent memory feed recurring work, standing monitors, autonomous agents, and the proactive Today and Attention surfaces. |
| **Skillshub and live discovery** | AgentSkills package tools, procedures and personalities. Runtime discovery is workspace-scoped, supports configured registries and whole-folder overrides, and still enforces each agent's exact allowlist. |
| **Learning and skill evolution** | Successful work becomes reusable, retrieved procedures; repeated evidence can create review- and eval-gated skill proposals. Shipped skills are never silently rewritten. |
| **VibeDev** | A project-aware coding studio and `@vibedev` rail with pluggable coding engines, live terminals, diffs, previews, checks, checkpoints and deployment hooks. |
| **App Platform** | Typed app contracts, a kernel, authoring tools, a TypeScript SDK, entity storage, memory contributions and custom surfaces. The platform contract is built; the installable catalogue remains in flight. |
| **Browser and API mining** | Governed browser automation learns task-shaped API recipes from captured traffic and reported answers. Matching tasks can replay with new inputs, scoped write grants, and guarded browser fallback. |
| **Channels, media and clients** | Supervised email, Telegram and WhatsApp bots; realtime voice and meetings; web, macOS, iOS, Android, ESP32 and Kindle surfaces. |
| **Code intelligence** | A generated workspace graph powers C4 architecture, 2D/3D explorers, impact and flow analysis, plus an MCP server that gives coding agents structural code search. |

*All eleven OS primitives, including what remains in flight: [The OS
Primitives](docs/architecture/os-primitives.md).*

---

## 🔌 Bring your own models — and your own reasoning loop

Magician keeps three things separate: the **agent** (identity, memory, permissions,
history and budget), the **model** that supplies cognition, and the **harness**
that runs the reasoning loop. You can change the latter two without replacing
the agent or surrendering its authority boundary.

### Bring your own LLM

Ten model-provider backends are implemented today: **OpenAI** (Responses and
Chat Completions), **Anthropic**, **Google Gemini**, **MiniMax**, **DeepSeek**,
**OpenRouter**, **xAI**, **Sarvam**, **Yutori N1**\*, and local **Ollama**. A model profile selects the
provider and model; routing can then choose a different profile for each
operation in **Settings → Model routing** or `llm.router.operation_mapping`.
That mapping is also the Settings catalog: each operation entry carries its
purpose and group alongside its profile selector, so adding a mapping or
profile does not require a frontend catalog change.
Planning, extraction, summarisation, memory work and other background model
calls do not have to share one provider.

Your signed-in **Claude Code**, **Codex**, **Grok**, or **agy** CLI subscription
can also serve as a stateless, text-only model provider for selected operations.
That mode is deliberately just text in/text out: it does not give the CLI tools,
an MCP connection, a plane grant or agent authority. The shipped routing table
maps **zero** operations to CLI subscriptions directly. An explicit
per-operation choice can use one, and an active external chat/run harness can
supply its matching profile to eligible non-local operations.

### Choose the harness at each stage

The built-in `magician` loop ships alongside five replaceable harness adapters:
`claude_code`, `codex`, `codex_app_server`, `grok`, and `agy`. The matching CLI
must be installed and signed in. When one drives a Magician turn, its native tools
are stripped where the CLI supports that and otherwise sandboxed; Magician-owned
actions still pass through the governed plane.

| Change point | Selector | What changes — and what does not |
| :--- | :--- | :--- |
| **Ordinary text chat** | **Settings → Terminal grants → Chat mouth** or `chat.harness_engine` | Chooses who reasons and writes the chat turn. The agent's identity, history and allowed Magician tools stay in the runtime. |
| **Agentic runs** | **Run engine** in the same panel or `execution.harness_engine` | Chooses who handles each Decide turn for normal tasks and scheduled/autonomous agent runs. Magician still owns Observe/Apply, persistence, approvals, tool dispatch and budgets. |
| **Internal and background model calls** | **Settings → Model routing** or `llm.router.operation_mapping` | Chooses a local, API or stateless CLI profile independently for each operation. When an external engine is driving, eligible operations can follow its matching CLI profile; explicit per-operation choices win, while operations with a local base profile retain their locality-aware config mapping. |
| **An existing external harness** | A durable `plt_` grant plus `/api/magician/v2/plane/mcp` | The harness keeps its own loop and uses Magician as an authenticated MCP server. It can call only granted tools, or use `run_task` to launch a real Magician execution under the grant's agent, engine and ceilings. |

“Background” therefore has two meanings: a scheduled or autonomous **agent
run** uses the run engine, while an internal worker's one-shot **model call**
uses per-operation model routing.

Specialised and disclosure-guarded paths are deliberate exceptions. Realtime
voice, meetings, App Copilot, Tutor, public-envoy chat and disclosure-guarded
turns stay on Magician's model path; protected app executions also stay on the
built-in Decide path.

### Use Magician from the harness you already have

Mint a terminal grant in **Settings → Terminal grants**, then point any
MCP-capable harness that can send bearer authentication at
`http://127.0.0.1:3002/api/magician/v2/plane/mcp` (the port Magician listens
on; the supervisor's default is 3002). The token is shown once and
stored only as a hash. Its workspace, agent identity, tool allowlist, expiry,
USD ceiling, wall-clock ceiling and concurrent-run ceiling are enforced by
Magician and can be revoked without changing the harness.

Clients that declare MCP form elicitation receive approvals and input requests
in the originating session. `request_user_input` supports free-form text,
choices, multiple selection and forms; `wait_for_run` relays questions from a
run launched by that session. See the
[terminal input and approval contract](docs/components/magician/plane.md#typed-input)
for the client requirements and supported input types.

This distinction matters: the MCP door is protocol-generic, so an existing
harness can use Magician externally. Replacing Magician's built-in chat or run
engine requires one of the five adapters implemented above. See the
[plane contract](docs/components/magician/plane.md), [chat engine boundary](docs/components/magician/chat-mode.md),
and [model-routing controls](docs/components/magician/llm-routing-overrides.md).

---

> [!IMPORTANT]
> **On the apps, honestly.** Magican's macOS app is going through Apple's
> notarisation, and its iOS and Android apps are in review. When those land they will be on the
> App Store and Google Play, and installing will be a download.
>
> Until then there is no store build. Running the phone apps means building them
> yourself. `make setup-magdroid-build` handles the Android toolchain; iOS needs
> Xcode and an Apple ID, which no script can install for you.
> [`docs/setup/mobile.md`](docs/setup/mobile.md) walks through both. The macOS
> app and the backend build from source today with no developer account at all.

---

## 🤖 Agents & Skills

### Agents as Data

> **Agent** *(noun)* — a persistent named entity with memory, obligations, and a history.

Not a prompt with tools, and not a loop that calls them; those are features of
whatever model you point at this. Agents here are defined purely in data — a
shipped one runs 500 to 760 lines of declaration: persona, wake spellings,
invocation policy, allowed and denied tools, denied tool *parameters*, trust
level, typed memory tiers, consolidation rules, circuit breaker, notification
rules, retention, and delegation targets.

This is the head of a shipped one ([`web-researcher`](magician_data_v3/system/agent_templates/agents/web-researcher/definition.agent.yaml)):

```yaml
agent_id: web-researcher
version: 3
name: Sleuth
aliases:
- sleuth
- researcher
- wr
description: "Web research and synthesis: searches diverse sources—news, technical docs, community forums, academic—to find, cross-reference, and synthesize cited answers to complex queries."
persona: |
  You are Sleuth, an accurate, economical web researcher. Find current public
  evidence, read only what is needed, and return a concise answer whose factual
  claims can be checked from its citations.
# … tools, delegation policy, memory tiers, prompt pipeline, constraints
```

Copy the folder, change the YAML, and the runtime picks it up. The schema and the memory-tier declarations are documented in the [agent templates README](magician_data_v3/system/agent_templates/README.md).

Note what is *not* structural. `llm_routing` is nullable, and thirty of the
thirty-one shipped agents name no model at all. Route one from a frontier API to
a local Ollama model and it is the same agent — same memory, same obligations,
same ledger, still answers to its name. Replace its memory tiers and it is a
different agent on the same model. **The agent is what survives a model swap.**

We rent cognition. We own identity and continuity.

### Drop-in Skills
Skills are folders with a `SKILL.md`. Validate one, then install the bundle into a workspace:

```bash
python3 skillshub/scripts/validate_skill_md.py /path/to/your/skill/
scripts/magician-skills install --scope anonymous/default
```

*The five-minute walkthrough for a third-party skill is [`skills-quickstart.md`](docs/components/magician/skills-quickstart.md).*

---

## ⚡ What it does

| Area | What you get | Read more |
| :--- | :--- | :--- |
| **Agents as data** | Persona, tools, delegation policy, memory tiers and prompt pipeline in one YAML file per agent. 31 ship. | [agent templates](magician_data_v3/system/agent_templates/README.md) |
| **Autonomous agents** | Seven agents run on their own cron with prioritised focus areas and a markdown charter. An autonomous cycle is its own code path, not a fake user turn. | [autonomous agents](docs/components/magician/autonomous-agents.md) |
| **Skills** | 89 top-level AgentSkills packages, one `SKILL.md` contract each. Runtime discovery is scope-aware, supports configured registry roots and whole-folder overrides, and filters the result through each agent's allowlist. | [skillshub](skillshub/README.md) <br> [spec and discovery](docs/components/magician/skills-spec.md) |
| **Channels** | WhatsApp, Telegram, Gmail and AgentMail, each a supervised per-platform daemon over a six-method adapter. | [bot SDK](skillshub/bots/sdk/README.md) <br> [design](docs/consumer_channels.md) |
| **Budgeted spend** | Declared costs reserve against a double-entry ledger that has to balance — ceilings, reservations, two-phase commit, and commodities beyond money (`EMAIL_SENDS`, `GITHUB_PUSHES`). | [resource authority](docs/components/magician/resource-authority-api.md) |
| **Memory** | Per-agent typed tiers — schema, scope, retention, consolidation — declared in YAML over Parquet + LanceDB. Owner memory retains revision history, reconciles duplicates and asks about uncertain conflicts. | [memory tiers](magician_data_v3/system/agent_templates/README.md#pluggable-memory-tiers-per-agent-schema) <br> [memory index](docs/components/magician/memory-index.md) <br> [memory lifecycle](docs/components/magician/memory-lifecycle.md) |
| **Event lakehouse** | Every event partitioned by principal and workspace; runs replay from canonical Parquet/JSONL. | [storage model](docs/ARCHITECTURE_V2.md#current-v3-storage-model) <br> [storage abstraction](docs/components/magician/storage-abstraction.md) |
| **Browser automation** | Drives your signed-in Chrome through an extension, out of process in `magicutor`; headed and headless fallbacks, per-thread owned tabs, session and tab tracking with live preview. | [magicutor](docs/components/magicutor/README.md) |
| **API mining** | Task Recipes compile answer-bearing browser traffic into reusable API steps. Supported matches can skip browser planning; unsafe or unsupported cases fail closed or use a guarded fallback. Live acceptance remains pending. | [pipeline](docs/components/magician/api-mining-pipeline.md) <br> acceptance |
| **VibeDev** | A project-aware coding studio and explicit chat/voice rail with pluggable coding engines, live terminals, diff review, previews, checks, checkpoints and deployment hooks. | [VibeDev rail](docs/components/magician/vibedev-rail.md) <br> [developer workbench](docs/components/magician/developer-mode-workbench.md) |
| **Generative UI** | Agents emit layout documents; 71 components render charts, metric cards, feeds and forms — not a transcript. | [unified UI](docs/components/unified-ui/README.md) |
| **Voice and realtime** | First-class on every surface, not a desktop afterthought: one set of wake-word, dictation and realtime audio rails shared by desktop, iOS and Android. The wake phrase summons the Orb and starts a Dictation, Hands-free or Live rail. | [media rails](docs/components/magician/realtime-media-rails.md) <br> [wake word](docs/components/unified-ui/wake-word-voice.md) |
| **Meetings** | An agent that joins, hears the room through a native Core-Audio bridge, and can speak back into it. Prep brief before, tracked follow-ups after. | [meetings](docs/components/magician/meetings.md) |
| **Models** | Ten native backends — OpenAI, Anthropic, Gemini, MiniMax, DeepSeek, OpenRouter, xAI, Sarvam, Yutori N1\* and local Ollama — plus four opt-in, text-only CLI-subscription routes. Routing is per operation. | [magicllm](docs/components/magicllm/README.md) <br> [routing](docs/components/magician/llm-routing-overrides.md) |
| **Learning loop** | Successful runs produce evidence-backed procedures that can become active through feedback; repeated success can create review- and eval-gated skill proposals without mutating skill files in place. | [learning procedures](docs/components/magician/learning-procedures.md) <br> [outcome learning](docs/components/magician/outcome-learning.md) |
| **Mail assist** | Gmail / Google Workspace mail triage. | [mail assist](docs/components/magician/mail-assist.md) |
| **Screen observation** | Ask what is on screen, grounded in the capture — or start a sustained watch ("tell me when the build finishes", "keep an eye on this dashboard"). | [screen](docs/components/magician/screen-capture-and-ask.md) |
| **Attention** | Today renders backend-backed lanes — For you, Worth a look, Follow-ups, Active work. Bounded background reasoning can connect relevant memories with an item and surface context or an owner clarification in HITL. | [attention lanes](docs/components/unified-ui/today-attention-lanes.md) <br> [resurfacing](docs/components/magician/resurfacing.md) <br> [memory connections](docs/components/magician/memory-connections.md) <br> behavioral evidence and limits |
| **Recurring monitors**| Standing watches that re-run on a schedule and report back. | [monitors](docs/components/magician/recurring-monitors.md) |
| **Tutor** | Blackboard-mode teaching built from data-driven primitives. | [tutor](docs/components/magician/personal-tutor.md) |
| **Notes** | Markdown notes the agent reads and writes with you. Web, iOS, and Android open the same library. | [notes provider](docs/components/magician/notes-provider.md) |
| **App Platform** | Typed contracts and a runtime kernel, authoring CLI, TypeScript SDK, entity storage, memory contributions, custom surfaces and governed action dispatch. | [truth baseline](docs/components/magician/app-platform-truth-baseline.md) <br> [apps](docs/components/magician-apps/README.md) <br> [SDK](docs/components/magician/typescript-apps-sdk.md) |
| **MCP** | Governed client over the official Rust MCP SDK, plus an authenticated Plane server for external harnesses. | [mcp client](docs/components/magician-mcp-client/README.md) <br> [plane server](docs/components/magician/plane.md) |
| **Code intelligence** | Generate and explore the indexed workspace as a C4 canvas, 2D/3D graph and searchable impact/flow model; expose the same structure to coding agents over MCP. | [Code Graph Explorer and MCP](docs/codegraph/README.md) |
| **Surfaces** | macOS tray + presence host, iOS (Magios), Android (Magdroid), an ESP32 voice terminal, a Kindle thin client. | [desktop](docs/components/desktop/README.md) <br> [iOS](docs/components/magios/README.md) <br> [Android](magdroid/README.md) <br> [ESP32](docs/components/magesp/README.md) <br> [Kindle](kindle/README.md) |

---

## 🏗️ Architecture

Don't trust a frozen diagram — regenerate it. `make graph-index` rebuilds the
workspace graph from the current tree, `make graph-check` detects stale graph
and architecture references, and `make graph-serve` opens the C4 canvas plus
the 2D, 3D and node explorers under [`docs/codegraph/`](docs/codegraph/). Point
a coding agent's MCP client at
[`docs/codegraph/mcp_server.py`](docs/codegraph/mcp_server.py) for structural
search, architecture explanations, endpoints, flows, skill inventory, dead-code
candidates, structural test coverage and graph audits.

The narrative — components, the V3 storage model, runtime flow, ports — is in [`docs/ARCHITECTURE_V2.md`](docs/ARCHITECTURE_V2.md).

**The workspace in one breath:**

* `magician` — core: orchestration, authority, execution, storage seams.
* `magician-api` — HTTP and WebSocket route owners (`/api/magician/v2`).
* `magician-bin` — process composition, boot order, workers.
* `magician-comms`, `magician-media`, `magician-learning`, `magician-surfaces`, `magician-chunking`, `magician-apps` — channel ingestion, realtime media, outcome learning, thinking maps, chunking, apps.
* `magicutor` — the out-of-process browser and tool executor.
* `magic-supervisor` — process supervision and restart policy.
* `magicllm` — model providers and routing.
* `magician-mcp-client`, `tool-runtime-core`, `runtime-core` — MCP boundary, local capability registry, shared traits.
* `ui/unified-ui` — the web UI. `desktop/` — the Tauri tray. `magios/`, `magdroid/`, `magesp/`, `kindle/` — the device clients.

---

## 📚 Documentation

* [**Docs hub**](docs/README.md) — entry point.
* [**Ecosystem map**](docs/ecosystem/README.md) — the product pillars, their
  boundaries, status and authoritative deep dives.
* [**Feature map**](docs/features/README.md) — the larger operator-facing
  inventory of runtime, learning, automation, apps, surfaces and operations.
* [**Architecture**](docs/ARCHITECTURE_V2.md) — the narrative runtime and
  storage model.
* [**Code Graph Explorer and MCP**](docs/codegraph/README.md) — run
  `make graph-index` then `make graph-serve` to explore the indexed workspace,
  C4 architecture, call flows and impact at `http://localhost:8077/`; connect
  `mcp_server.py` to give coding agents the same searchable structure.
* [**Quick start**](docs/quickstart.md) — composed and guided TUI installers,
  install matrix, runtime root, config and tunnels.
* [**Testing**](docs/testing.md) and [**Deployment**](docs/DEPLOYMENT.md).
* [**Component index**](docs/components/README.md) — one doc per crate and surface.
* [**Project versions**](docs/project-versions.md) — the `magician` version
  contract plus every Rust crate and versioned product surface.
* [**Dependencies**](docs/dependencies.md) — every package ecosystem,
  declaration manifest, committed resolution file and known lock gap.
* [**Contributing**](CONTRIBUTING.md) — conventions, checks, hooks.

---

## 🗺️ Where this is going

Magician is step one toward zero-trust, sovereign, personal AI.

* **Harness hardening:** Replaceable chat and run engines are live. Next: make chat-harness continuation durable across process restarts and append resume-executed approved actions to the run's iteration record.
* **App layer:** The app contract is built; the installable catalogue on top of it isn't yet.

---

## 🤝 Contributing

Questions, or want to see what people are building with it? [Join the Discord](https://discord.gg/USDChWhdj).

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first: `make check-all` for compilation, `make setup-hooks` once per clone for the docs-freshness gate, and `make fmt` before you push — CI checks formatting.

<details>
<summary><strong>AI Assistant Setup</strong></summary>

The repo ships ready-made configs for coding agents. `AGENTS.md` is the canonical project-rules file (Codex, Grok, ZCode, Antigravity/`agy`). Claude Code imports it from `CLAUDE.md`. Gemini CLI loads it via `.gemini/settings.json` `context.fileName`. Docs-freshness hooks live in `.claude/settings.json`, `.grok/hooks/`, `.gemini/settings.json`, `.agents/hooks.json` (`agy`), and `.zcode/config.json`. Codegraph MCP is wired in `.mcp.json`, `.codex/config.toml`, `.gemini/settings.json`, `.agents/mcp_config.json`, and `.zcode/config.json`. Claude slash commands are `.claude/commands/` (`/sync-api-docs`, `/sync-websocket-events`, `/sync-all-docs`). See [`docs/process/ai-doc-hooks.md`](docs/process/ai-doc-hooks.md).
</details>

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as below, without any additional terms or conditions.

---

## 📄 License

Licensed under either of:
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

Third-party components keep their own licenses: `magdroid/` is derived from NeuralBridge and stays under Apache-2.0 with its [NOTICE](magdroid/NOTICE); bundled font notices cover [iOS](magios/Magios/Fonts/NOTICE.txt), [Android](magdroid/android/app/src/main/assets/font_licenses_notice.txt), and [Tauri](desktop/public/fonts/NOTICE.txt), while Web font licenses remain under `ui/unified-ui/static/fonts/`. The 3D assets under `ui/unified-ui/static/fleet/` and display components under `magesp/components/` carry their own license files alongside.

\* **Yutori N1:** currently broken in the latest builds.
