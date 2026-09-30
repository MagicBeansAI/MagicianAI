# Magican Ecosystem Map

Magican is an agent operating system surrounded by product platforms, learning
loops, capability packages, clients and developer infrastructure. This page is
the stable map of those pillars. It intentionally does not duplicate their
implementation contracts: each pillar links to the docs that own its detailed
behaviour and current limitations.

The current source tree contains 89 top-level AgentSkills packages, 31 agent
templates and seven bot packages. Treat those numbers as a source inventory,
not the definition of the ecosystem; the runtime, app platform, VibeDev, API
mining and code-intelligence layers are first-class pillars too.

For the route-and-command-level inventory, use the
[Feature Map](../features/README.md). For the eleven OS primitives and their
implementation status, use [The OS Primitives](../architecture/os-primitives.md).

## 1. Durable Agent OS

This is the substrate: scoped agent identity, typed memory, tasks and execution,
scheduling, permissions, process isolation, event history and resource
accounting. Models and reasoning harnesses can change without replacing this
durable authority boundary.

Primary docs: [Architecture V2](../ARCHITECTURE_V2.md),
[OS primitives](../architecture/os-primitives.md),
[execution](../components/magician/execution/README.md), and
[resource authority](../components/magician/resource-authority-api.md).

## 2. Models, Harnesses And Plane

Model routing chooses local, hosted or explicitly enabled CLI-subscription
profiles per operation. Chat and agentic runs can use the built-in reasoning
loop or a supported harness adapter, while an existing external harness can
retain its own loop and call governed Magican capabilities through the
authenticated Plane MCP endpoint.

Primary docs: [MagicLLM](../components/magicllm/README.md),
[routing overrides](../components/magician/llm-routing-overrides.md),
[chat engine boundary](../components/magician/chat-mode.md), and
[Plane contract](../components/magician/plane.md).

## 3. Memory, Attention And Autonomous Work

Scoped memory, episodes, evidence and the work ledger provide continuity across
turns and executions. Scheduled tasks, recurring monitors and autonomous agents
act on that context, while Today and Attention turn background findings,
follow-ups and approvals into user-visible work.

Primary docs: [memory index](../components/magician/memory-index.md),
[work ledger](../components/magician/work-ledger.md),
[autonomous agents](../components/magician/autonomous-agents.md),
[recurring monitors](../components/magician/recurring-monitors.md), and
[HITL Attention](../components/magician/hitl-attention.md).

## 4. Skillshub And Live Skill Discovery

AgentSkills package tools, procedures and personalities behind one `SKILL.md`
contract. The loader discovers skills per workspace from scoped and configured
registry roots; a higher-precedence folder owns the whole package name, and the
selected agent's exact allowlist decides which discovered skills it can use.

Primary docs: [Skillshub](../../skillshub/README.md),
[skill specification and discovery](../components/magician/skills-spec.md),
[authoring](../components/magician/skills-authoring.md), and the
[five-minute quickstart](../components/magician/skills-quickstart.md).

## 5. Learning And Skill Evolution

Completed work can yield learning candidates and reusable procedures. Feedback
and repeated successful retrieval can activate a procedure and create a skill
promotion candidate, but the candidate still passes review, validation and eval
backlogs; the system does not edit executable skill packages in place.

Primary docs: [learning procedures](../components/magician/learning-procedures.md),
[outcome learning](../components/magician/outcome-learning.md), and
[skill evolution authoring](../components/magician/skills-authoring.md).

## 6. VibeDev And Developer Workbench

VibeDev is the project-aware app-building surface and explicit `@vibedev` chat
and voice rail. Its server-owned coding profiles drive scoped builds with live
terminal visibility, diff review, previews, checks, checkpoints, targeted
edits and configured deployment hooks; Developer Mode exposes the underlying
interactive workbench for operator-controlled sessions.

Primary docs: [VibeDev rail](../components/magician/vibedev-rail.md),
[Developer Mode workbench](../components/magician/developer-mode-workbench.md),
and [coding-engine guardrails](../components/magician/coding-engine-guardrails.md).

## 7. App Platform

The App Platform defines typed packages and actions on top of the governed
runtime, with an authoring CLI, TypeScript SDK, entity storage, memory
contributions, custom surfaces and bounded action dispatch. The contract and
kernel are present; claims such as user-reachable or release-qualified remain
subject to the platform's explicit truth states, and the installable catalogue
is still roadmap work.

Primary docs: [truth baseline](../components/magician/app-platform-truth-baseline.md),
[app platform index](../components/magician-apps/README.md), and
[TypeScript SDK](../components/magician/typescript-apps-sdk.md).

## 8. Browser Automation And API Mining

Magicutor executes browser and heavyweight tool work outside the main runtime.
The mining pipeline observes eligible browser traffic, filters noise, learns
parameterised capabilities, captures scoped authentication material, validates
replay and applies per-origin policy before an API call can replace browser
work.

Primary docs: [Magicutor](../components/magicutor/README.md),
[API mining pipeline](../components/magician/api-mining-pipeline.md),
[learned-resource projections](../components/magician/api-mining-projections.md),
and [captured auth and replay](../components/magician/auth-capture-and-replay.md).

## 9. Channels, Media And Client Surfaces

Supervised bot processes connect email, Telegram and WhatsApp without placing
all credentials in one gateway process. The same runtime serves the web and
macOS experiences, native iOS and Android apps, realtime voice and meeting
rails, and thinner ESP32 and Kindle clients.

Primary docs: [consumer channels](../consumer_channels.md),
[bots](../components/bots/README.md),
[realtime media rails](../components/magician/realtime-media-rails.md), and the
[component index](../components/README.md).

## 10. Code Intelligence And Architecture Exploration

The codegraph toolchain indexes the workspace into a generated structural
graph. Its local server exposes the graph as a plane view, a human-owned C4
architecture canvas, 2D and 3D explorers, search, blast-radius and Git-impact
views, flow simulation and guided runbooks. The companion MCP server gives
coding agents graph search, architecture explanations, crate detail, endpoint
inventory, call-flow tracing, skills and Tauri inventories, dead-code
candidates, structural test coverage and graph audits.

Run `make graph-index` before relying on the artifact, `make graph-check` to
detect drift, and `make graph-serve` to open `http://localhost:8077/`.

Primary docs: [Code Graph Explorer and MCP](../codegraph/README.md) and the
[human-owned architecture model](../architecture/README.md).
