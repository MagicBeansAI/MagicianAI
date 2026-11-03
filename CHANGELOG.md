# Changelog

Product-level release log. One entry per published release, newest first,
written for someone reading the repository rather than for the team that
wrote the code.

Per-crate detail lives in each crate's own `CHANGELOG.md` (`magician/`,
`magicutor/`, `magicllm/`, `ui/unified-ui/`, and the rest), which keep their
five most recent entries; anything older is in `docs/archive/changelogs/`.
This file keeps its five most recent entries too; older ones are in
`docs/archive/changelogs/product.md`.

Entries accumulate under `[Unreleased]` between releases and are promoted to a
dated, version-stamped heading when a release is published.

## [Unreleased]

## [0.7.151] - 2026-11-02

- **Approvals inside delegated work:** When an agent you delegated to hits an approval gate, you now get the approval card and the work waits for you, instead of being cancelled within a second and retried in a loop.
- **God mode reaches coding work:** God mode set in a chat now carries into the coding runs it starts. Writes to Magician's own repo still ask you every time, allow-once only.
- **Chats survive a restart:** A chat turn cut off by a restart no longer looks like it is still running, and Stop clears it. A task the chat started posts its result back to the chat after a restart, and no new work starts while Magician is shutting down.
- **Windows tools:** Bare tool names such as `git` and `rg` are found on Windows.
- **HTML slide decks:** Muse and the deck maker can build animated single-file HTML presentations with the new Frontend Slides procedure.

## [0.7.150] - 2026-11-02

- **Tasks pick up after you answer:** A task that hands work to another agent right after you answer its question now resumes when that agent finishes, instead of failing hours later.
- **Why a tool failed:** The task panel on web, iOS and Android shows the reason a tool call failed, two lines with Show more.
- **Long installs and Swift in tasks:** A shell command that runs out of time says how to give it longer or run it in the background, and a task's shell can now build Swift and clang modules.
- **Chats with big images:** Images are scaled to fit before they are sent, so a chat holding large screenshots is no longer refused as too big.
- **Delivered files and threads:** Files an agent produces reach you with the answer, threads get readable links that survive a rename, and tapping a thread in History lists its sessions.
- **Agent tools:** grep and glob use Magician's own ripgrep, and web page reads work again for every agent.
- **Decks as PDF, and Impeccable for designers:** Agents can export a PowerPoint deck to PDF, one page per slide, and Muse, the deck maker and the frontend engineers can use the Impeccable design procedure.
- **Desktop voice keeps to itself:** Saying the wake word starts a new chat in a Desktop thread of its own instead of joining the chat you have open in #general.

## [0.7.149] - 2026-10-31

- **Security:** zip 9 (fixes RUSTSEC-2026-0367/0368/0369) in the server and in the desktop updater (now Tauri 2.12), and actix-http 3.20 on zstd 0.14.3 (RUSTSEC-2026-0391, -0398, -0403). zip 6 via duckdb, zip 8.6 via anydoc and zstd 0.13 via arrow and lance stay until upstream releases move.
- **Teams of agents finish:** An agent that hands work to other agents, who hand work on again, now picks up when they finish; a CEO-led launch task used to stop there and fail. A cancellation the runtime forces says why instead of naming the user.
- **Team routines follow your team:** Start team sequence on web, iOS and Android runs each harness agent's current focus areas instead of five routines retired in October, and the server refuses a retired routine. Pausing routines mid-cycle no longer raises an anomaly for autofix.
- **Commands and chats on mobile:** A partly typed command (`/eng`) opens on iOS and Android as it does on the web, while a message that only starts with a command name is sent. iOS shows a spinner, then Try again, while a conversation opens.

## [0.7.148] - 2026-10-30

- **Chats that last:** Long chats fold older turns into a running summary and can search their full history. `/engine`, `/run-engine` and `/voice-mode` pick engine, model and thinking on web, iOS and Android. Archiving or deleting a thread changes every session on it, and new chats on iOS open on the welcome screen.
- **Today:** Reading Room cards get an Act menu (reply, open, start a follow-up task with an instruction, snooze, details) on web, iOS and Android. Cards show timestamps and clearer sources, swipe decks page and count reliably, and each section loads on its own, progressively on mobile.
- **Goals and programs:** Magican sets up private goal workspaces itself, one folder per main goal. A workspace is open only to its listed members and a private goal only to its owner, whatever route an agent takes. The Goals screen lists done and dropped goals only under Closed, and a goal can link a Playbook. Program documents are editable in Work → Programs, with every save restorable, and agents propose revisions to shared documents that show their reason and changed lines until you approve.
- **Agents and runs:** Answer an agent's question by typing in chat; cancelled questions leave Needs you. Steps appear while they run, long runs keep and can reach what they read, and answers reflect everything a paused run did. A run always ends with an answer and says what blocked it. Agents propose an independent review for work that leaves the house or is hard to undo, read-only folders stay unchangeable, and deleted agents stay deleted.
- **Memory:** Reviews now take effect, finish quickly after a restart and no longer time out. Repeated and differently spelled memories are stored once, agents recall short summaries instead of raw instructions, and the owner's memory file is bounded by archiving rather than deleting (observe mode until switched on).
- **Coding and browser:** Coding CLIs (Claude Code, Codex, Grok, Antigravity, Pi 1.3.0) use your own sign-in first and ask before using Magician's API key. VibeDev works in your original folder with exact-diff approval and needs no git. Browser runs name their tab group after the task and use agent-browser 0.41, and deep research runs for up to 35 minutes.
- **Commerce and inspection:** Installation-owned commerce MCP profiles, Scenes checkout, owner identity claims, automatic bot webhook setup and evidence-preserving answers. Magican delegates runtime checks to Atlas, and God mode HTTP and file searches offer scoped approvals.
- **Models and speed:** Claude Haiku 5.5 and GPT-6.1 Terra join the pickers, which follow what each provider offers now. Whole-call tool constraints no longer break Anthropic chats, `/llm` analytics load faster, attention asks land in the lane of what they need, and every Attention item carries the exact call to answer it.
- **Build:** Rust is pinned to 1.95.0 for every target.

## [0.7.109] - 2026-10-16

- **Work and coding runs:** Follow live work from the desktop notch and phones; runs and paused approvals survive restarts. Cost, time and attempt limits pause for an owner-approved allowance; tracked sends, calendar effects and replies feed follow-through.
- **Safer execution:** OS-enforced shell and network policies, scoped approvals and sandboxed coding engines. Fully specified tool packs run directly, and stalled turns stop with a reason.
- **Less resource growth:** Idle workspace databases close after 60 seconds, Attention expires old items and stores rows compactly, and deleted workspaces stay deleted. Task deletion recovers from invalidated Feed databases; the live-work indicator excludes idle ready tasks.
- **Updated interface:** Browse archived chats, tasks and coding runs; use the Talk / Code composer, notes folder and command palette. HTML outputs render in a sandboxed frame, and media editing supports silent video.
- **Setup and builds:** macOS source installer, optional private Cloudflare and Clarity configuration, portable mr-boxington build caching and leaner DuckDB C++ builds.
- **Usage accounting:** Faster lifetime cost totals, file-based model prices and metered speech calls.
- **Storage compatibility:** New Attention rows require Magician 0.7.98 or later. Do not downgrade below that version after upgrading.

## [0.7.95] - 2026-10-04

- Bounded Relentless task improvement with review, sealed references, paging, capture policies and guidance evaluation; fixes to verification, media probing, accounting, task state and supervisor shutdown; refreshed thinking maps and branding. (Development notes.)

## [0.7.90] - 2026-09-30

- **Sarvam speech.** Sarvam Bulbul v3 voices and Saaras v4 dictation for Hindi, Tamil, Bengali and other Indian languages, using `SARVAM_API_KEY`.
- **The autonomous harness says why it did not run.** Dropped cycles record a reason, a missing program document is reported, and the global pause can no longer be bypassed at startup.
- **Delegation survives restarts.** A delegated agent that cannot resume fails with a reason so its parent carries on.
- **No phantom workspaces.** Stray writes are quarantined instead of creating new workspaces.
- Development versions: Magician `0.7.90`, Magician API `0.3.32`, Magdroid `0.6.0` (code `26`), Magios `0.4.0` (build `203`).

## [0.7.89] - 2026-09-30

First public release.

- **One runtime, not a stack of glued-together services.** Agents, skills, memory, channels, tool execution and UI run together on your own machine: 89 skills, 31 agents, 6 chat channels, iOS and Android companions, and generative UI.
- **Every agent is a YAML file.** Persona, tools, delegation and memory tiers are data, not code.
- **Zero-trust by construction.** Every tool call carries a capability token, every spend hits a double-entry ledger, and over-budget calls are refused.
- **Models are replaceable per operation.** Ten providers, including local Ollama, chosen per operation in one line of config.
- **Nothing shares a process.** Chat channels and browser automation run as supervised separate processes.
- **Decisions are cheap and local where they can be.** A separate decision engine answers typed questions with small models, on the Apple Silicon GPU where available.
- **Runs are replayable.** Analytics events are also written to Parquet per scope and day.

Install with `make install`, which stands up the whole stack and ends with a
health sweep. The README has the mode and flow matrix; per-crate detail is in
each crate's own `CHANGELOG.md`.

### Also in this release

- Memory decisions run on the decision engine with reviewed thresholds and editable local and cloud models.
- Sarvam for Indic-language chat; GPT-6 Sol profiles run on GPT-6.1 Sol.
- Fixed a desktop tray memory leak (Desktop `0.3.18`).

#### 2026-09-28 — App keys go only where you allow

- **Security:** an app tool's key reaches only the hosts the tool declares or you pick at install; "any site" is a separate explicit choice. Skills can no longer read tool settings in your home folder.
- Development versions: Magician `0.7.88`, Apps `0.2.7`, Unified UI `0.1.31`.

#### 2026-09-28 — Keys can be locked to several sites, or all sites

- MagicVault scopes a released key to a set of sites, or all sites when the key allows it (magicvault-core `0.1.6`).
- Development version: Magician `0.7.87`.

#### 2026-09-28 — Outside edits refresh the notes index

- Editing a Markdown file in the notes folder refreshes search once the folder has been still for about a second.
- Development version: Magician `0.7.86`.

#### 2026-09-28 — Notes stay in Magician

- **Removed:** web, iOS and Android open notes in the app; the old notebook site, web view and notes server are gone. Notes are still Markdown files.

#### 2026-09-28 — Today reads like a morning newspaper on web, iPhone and Android

- iPhone and Android Today match web's Morning Edition: masthead, realtime wire, lead story, operations carousel, Reading Room and briefings.
- Fixed: web Today no longer reports failed actions as successes or shows placeholder activity.
- Development versions: Unified UI `0.1.29`, Magdroid `0.5.0` (code `25`), Magios `0.3.0` (build `202`).
