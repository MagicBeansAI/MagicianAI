# Changelog

All notable changes to the Magician project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-10-15 — 0.7.109

- VibeDev engines run inside Magician's box (`coding.box_harness`, default on); the box also opens the turn's build caches and owner-allowed folders.
- Super god mode (debug, `execution.super_god_mode`): an approved harness launch starts with no restrictions and stops when it ends; set it in Settings or with the VibeDev ⚡ toggle.
- God mode does not apply in VibeDev, Discuss included; "This session" covers a whole build or agent run.
- Fixed: file-lock waits no longer fill the blocking pool and hang chats, tasks and VibeDev; god-mode and engineer runs no longer overflow the stack.
- Fixed: Claude Code accepts any CLI-bundled plugin; Grok runs in Magician's own Grok home and its MCP gateway passes only when contained; the box keeps the keychain readable for Claude Code and Antigravity.
- Claude Code's per-server MCP timeout matches the plane's 24-hour tool timeout.
- Fixed: a settings write (such as toggling god mode) while runs were active no longer overflows the stack; the capability snapshot refreshes on the scheduler root.
- Fixed: a Claude Code turn that ends on an error result keeps Claude's own words, and a safety refusal says resending it will be refused again.
- Prices live in `llm_pricing.json`, not code; TTS/STT calls are metered (local engines at $0) and Grok voice bills its minutes; rates rechecked against vendor pages.
- Fixed: deleting a task whose run already ended no longer fails with `task_active` when the task still pointed at that run or listed it as awaiting synthesis.

### 2026-10-15 — 0.7.108

- A coding run or crew lane survives a Magician restart mid-run: it resumes within about a minute instead of stalling for 15 minutes or being cancelled.
- "Magician at work": one ranked summary of coding runs, crews and Work tasks for the phones and the notch, with Follow for up to three items.
- Acknowledge-only Attention items expire after two hours; failures leave Attention after two hours and are deleted after a week.
- The model is told the delegation depth budgets the runtime enforces.

### 2026-10-15 — 0.7.107

- Tracked sends and calendar effects feed Work follow-through; review authority survives restart; failed-tool and owner-reply evidence is kept for final verification.

### 2026-10-12 — 0.7.106

- Long-running harnesses get 24-hour defaults, durable slices and renewable budget pauses with cumulative grants; owned Work supports typed completion/input controls and durable replies.
- **Security:** the shell sandbox is OS-enforced (`execution.shell_sandbox.os_enforce`, on in the seed config): shell commands outside coding runs run under `sandbox-exec` (macOS) or `bwrap` (Linux), can write only to working, temp and cache directories, cannot read credentials, keychains, `.env` files or the vault, and cannot launch coding-agent CLIs. Previously a `cd` elsewhere was unrestricted.
- **Security:** network policy (`execution.network_policy`, `public` by default) blocks loopback, private, link-local and metadata addresses for the shell (via an egress proxy) and the `http` tool, redirects included; `allow_private` lists exceptions.
- God mode (`execution.god_mode`): trusted agents get unrestricted shell, files and network, set globally in Settings or per chat. Anything the normal sandbox would refuse asks the owner (once, this session or always, at a chosen scope); commands that cannot be fully parsed can only be allowed once. Changing the setting forgets all approvals. VibeDev is unaffected.
- Idle workspace databases are closed after 60 s and reopened on demand, with in-flight work protected; no data is deleted. DuckDB limits apply on every open.
- Fixed: harnesses see each Magician tool in the restricted form Magician actually runs, so plans no longer fail schema checks; `submit_plan` validates arguments up front.
- **Removed:** the unused harness planner-mode transport.
- **Changed:** harness engines (Claude Code, Codex and others) run their normal turn in agent runs and chat, with every Magician tool call checked fail-closed by the Decision Engine; the propose-only chat loop and agent planner sessions are removed. Harnesses may hand over a multi-step plan with `submit_plan`.
- Agent runs on the Magician engine fall back to the agent's own model and real tools when no step is picked; the Decision Engine validates each call and returns rejections with a reason.
- Plans stick: later steps run under Decision Engine validation without re-planning until a step fails, a steer arrives or instructions change. Logs record who decided each step.
- Attention learning storage is more compact and deduplicated; retention runs on a timer in small batches with incremental vacuum.
- **Breaking (storage, one-way):** data written by this build is not readable by builds before 0.7.98; do not downgrade past it.
- New maintenance script queues leftover unregistered workspaces for purge on next boot (data roots upgraded to 0.7.97+).

### 2026-10-12 — 0.7.105

- App-authoring catalogs cache validation by exact source content while still picking up edits and removals immediately.
- Archived chats, threads, tasks and VibeDev runs can be listed on request; VibeDev runs gain paging and search.
- Notes index writes are serialized per index.
- Native text routes accept `tool_choice: auto`.
- Lock files for overlong data filenames use a hashed name.
- Native app workflow admission runs with bounded stack headroom.
- Learning package 0.2.17 binds updated internal-data primitives; older archive locks refuse the changed binding.

### 2026-10-12 — 0.7.104

- Notes settings and messages say "notes folder" instead of SilverBullet (stored IDs unchanged).

---

Older entries: `docs/archive/changelogs/magician.md`
