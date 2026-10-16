# File / artifact context carry-through across steps

## What this solves

A file produced by tool N exists on disk, but by the time the LLM picks tool
N+1 its path may have fallen out of context (compaction, summarisation,
history truncation). In chat, turn 2 says "send that image over WhatsApp" and
the model has lost the path from turn 1; in agentic execution, step 2 needs
step 1's screenshot and the model picks the wrong path or asks the user to
re-paste it.

## The fix

Each surface renders a small "Recent files" block into its system or decision
prompt **on every turn**, regenerated from a persistent index that compaction
never touches. The prompt always carries the absolute path of every recent
file produced in this session or execution, plus user attachments.

```
## RECENT FILES IN THIS EXECUTION                         ← agentic
Files produced by prior tool calls in this execution (and any seed-time
inputs). When the next step needs one of these files as input, reuse the
absolute `path:` shown below verbatim as the relevant file argument of the
next tool — do NOT ask the user to re-paste a path you already have, and do
NOT manually copy it through `update_runtime_ledger`. …

- summary.md [text/markdown; tool_output; 2418 bytes] path: /…/summary.md
…
```

The chat block is headed `## Recent files in this session`, covers tool
outputs and user attachments, and has the same instruction without the
ledger clause. Entry lines differ per surface:

- chat: `- <label> [<attachment | tool_output:<tool>>; <mime>; <bytes> bytes] path: <abs_path>`
- agentic: `- <name> [<content_type>[; <artifact_type>][; <bytes> bytes]] path: <abs_path>`
  (type and size appear only when known; entries are deduped by path)

Absolute paths let the model pass them verbatim into the next tool's `path` /
`file` / `media` argument (or an operator paste them into a terminal).

## Two surfaces, same contract

### Chat — `recent_session_files_block`

`magician/src/magician_v2/chat/service.rs::render_recent_session_files_block`

- Source of truth: per-session JSON index `…/scopes/{principal}/{workspace}/ui/chat_sessions/{session_id}/file_index.json` (`chat_session_file_index_path`; `ChatSessionFileIndex` / `ChatSessionFileRecord` in `chat/models.rs`).
- Populated with `ToolOutput` records (bytes written to `chat_session_outputs_dir` and registered against the session) and `Attachment` records (user uploads, plus files written by the `screenshot_preview`, `capture_reference` and `media_edit` handlers). The plan flow renders its attachments from the same records.
- Cap: `RECENT_FILES_LIMIT = 12`, newest `created_at` first.
- Prompt variable `recent_session_files_block`, introduced in `data/magician_v2/prompts/chat_outer_loop_system_v0.0.1.json` and carried by the pinned v0.0.7.

### Agentic execution — `execution_files_section`

`magician/src/magician_v2/execution/agentic/decision.rs::build_execution_files_section`

- Source of truth: `ExecutionHistory.seeded_artifacts` + `ExecutionHistory.artifacts` (typed `Artifact` records). Compaction touches message history, not the artifact list, so the index survives after the producing tool's message is evicted.
- Only artifacts with a non-empty `materialized_path` (in-memory `data`-only artifacts cannot be passed by path).
- Cap: `EXECUTION_FILES_LIMIT = 16` (multi-step automations accumulate more files than chat).
- Prompt variable `execution_files_section`, introduced in `data/magician_v2/prompts/agentic_decision_v1.1.0.json` and carried by the pinned v1.3.11.

## Design rules

- **Pushed, not pulled.** A list-files tool would cost a round trip for a
  fixed list; relying on the model to record paths (the retired inner-loop
  `update_runtime_ledger` tool, still named in the agentic instruction) failed
  because models forgot; a system reminder in the transcript gets compacted
  away exactly when it is needed.
- **Domain-agnostic.** The instruction says "the relevant file argument of the
  next tool" and never names a flow, so it does not bias the model toward one
  (for example sending versus summarising). Routing stays with the model and
  the user's request.
- **Bounded, newest first.** Over the cap, newer entries win. The block is
  omitted entirely (no orphan header) until a materialised file exists.

No unit test calls either builder directly; the caps and ordering are
backed only by the code above.

## See also

- [`chat-mode.md`](./chat-mode.md) — chat outer-loop system-prompt assembly.
- [`execution/AGENTIC_EXECUTION_DESIGN.md`](./execution/AGENTIC_EXECUTION_DESIGN.md) — agentic decision-prompt construction.
- `magician/src/magician_v2/artifact_v2/events.rs` — `ArtifactV2EventType::ArtifactCreated` is the durable disk record that makes a file visible to retention; the "Recent files" blocks read the in-memory mirror.
