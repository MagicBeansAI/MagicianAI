# Documentation Hub

Magician is the open-source operating system for building Personal AI. It powers
Magican, the life assistant. This page is an index: pick a lane below. The
[root README](../README.md) has the overview, and [explain.magican.ai](https://explain.magican.ai)
lets you take the system apart in 3D.

## Start Here

- [Quick Start](quickstart.md)
- [Ecosystem Map](ecosystem/README.md) — product pillars and their authoritative docs
- [Feature Map](features/README.md)
- [Architecture V2](ARCHITECTURE_V2.md)
- [Chat Mode Architecture](components/magician/chat-mode.md)
- [Consumer Channels](consumer_channels.md)
- [Deployment](DEPLOYMENT.md)
- [Code Graph Explorer and MCP](codegraph/README.md) — C4, 2D/3D exploration,
  search, impact, flows and coding-agent code intelligence
- [Testing](testing.md) — full-suite reports, failure collection and per-area lanes
- [Development workflow reference](development-workflow.md) — test entrypoints, eval
  lanes, setup targets and runtime automation notes
- [Project Versions](project-versions.md) — the `magician` version contract,
  every Rust crate version, and versioned product surfaces
- [Project-wide Dependencies](dependencies.md) — package manifests, committed
  resolution files, local inspection commands, and current lock gaps
- [Documentation Governance](process/documentation-governance.md)
- [AI Docs Hook Setup](process/ai-doc-hooks.md)

## Setting Up

- [Guided TUI installer](components/magician-setup/README.md) — capability-first setup, machine probes and current installer boundaries
- [Keys and paid services](setup/api-keys.md) — model providers, research, media
- [Channels](setup/channels.md) — WhatsApp, Telegram and email, yours or the agent's own
- [Connecting a Google account](setup/google-workspace.md) — Cloud project, OAuth, Pub/Sub
- [Building the phone apps](setup/mobile.md) — Android toolchain, Xcode, devices

## Runtime and Agents

- [LLM model catalog](components/magicllm/model-catalog.md) — providers, capabilities, workload assignments and model/thinking selection
- [Execution plane](components/magician/plane.md) — runs, work engines and the long-running harness policy
- [Eval lanes](components/magician/eval-lanes.md) — how evaluation lanes declare themselves for the `/evals` page
- [Structured decisions](components/magician/structured-decision.md) — one Decision Engine path across chat and agentic harnesses
- [HITL and Attention](components/magician/hitl-attention.md) — approvals, questions and the Attention queue
- [Learning procedures](components/magician/learning-procedures.md) — the self-improvement loop and Skill Evolution
- [Harness cycle continuation](components/magician/harness-cycle-continuation.md) — how autonomous cycles resume
- [Storage governance](components/magician/storage-governance.md) — database upkeep, budgets and maintenance
- [LLM training-data observability](components/magician/llm-training-data-observability.md)
- [Ollama logical-context chunking](components/magician/ollama-logical-context-chunking.md)

## Interface and Brand

- [Unified Work](components/unified-ui/unified-work.md) — the Work interface on web, iOS and Android
- [Decision Model dashboard](components/unified-ui/decision-models.md) — scoped quality, outcomes, latency, cost and monitoring
- [Public-site metadata](components/unified-ui/public-site-metadata.md) — marketing and explain pages, social cards and noindex previews
- [Presentation identity](components/magician/presentation-identity.md) — the single source for product names across surfaces
- [Optional Cloudflare setup](setup/cloudflare.md) and [Clarity recording](setup/clarity.md)

## Component Docs (Project Layout Mirror)

- [Component Index](components/README.md)
- [Magician](components/magician/README.md)
- [Magicutor](components/magicutor/README.md)
- [MagicLLM](components/magicllm/README.md)
- [Magic Supervisor](components/magic-supervisor/README.md)
- [Tool Runtime Core](components/tool-runtime-core/README.md)
- [MCP Client Boundary](components/magician-mcp-client/README.md)
- [Runtime Core](components/runtime-core/README.md)
- Remix
- [Unified UI](components/unified-ui/README.md)
- [Capabilities](components/capabilities/README.md)
- [OfficeCLI Tool Skills](components/magician/officecli-tools.md)
- [Authentication and workspace isolation](components/magician/auth.md)
- [Scripts](components/scripts/README.md)
- [iOS Companion (Magios)](../magios/README.md) — native Today, chat, Attention, and Tutor surfaces. Run it on your phone/simulator with **`make ios-debug-run`** (build + install + launch on a connected iPhone, else a simulator; **`make ios-debug-build`** / **`make ios-debug-deploy`** are the build-only and install-only halves). Builds contain no customer host or Cloudflare credential: connect the installed app with a short-lived iPhone QR created in Magician Settings. Run unit coverage with **`make test-ios`** and the on-demand UI smoke lane with **`make test-ios-ui`**; the blocking unit lane uses the checked-in Xcode project, never invokes XcodeGen, gives each attempt an isolated Core Foundation home/temp/cache/DerivedData environment, and enters the logged-in user bootstrap when available. It retries only the proven pre-test host abort once (and suppresses that futile retry when no Aqua bootstrap exists), caps only Xcode's post-test simulator diagnostic collector at 30 seconds, serializes report publication, and atomically writes a current-run-only dashboard to **`coverage/ios/latest.html`**. Aggregate `make test` runs use a correlation manifest to retain their immutable run-specific iOS report even if a later direct run replaces `latest.html`. Assertion/test failures are never retried, and stale result bundles are never reused. One-time host setup: **`make ios-full-setup-debug`** (mobile Cloudflare Access, UI email Access, tunnel routing, and debug build) or **`make connect-access`** (the customer-owned device Access boundary used by iOS, Android, and ESP32). `make build-all-debug` also compiles Magios via `ios-debug-build` (installable); `make check-all` runs the Rust all-target check, Unified UI Svelte/TypeScript checker, and the fast `ios-debug-check` compile gate against the generic iOS Simulator destination with no signing or installed-device dependency. The compile gate places DerivedData under `$(CARGO_TARGET_DIR)/magios-debug`, so it does not fall back to the system Xcode cache or consume code-signing services. The iOS step skips gracefully when `xcodebuild` is unavailable.


