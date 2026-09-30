# Agentic execution harness conformance

The execution engine is selected inside the agentic decide phase. Ordinary
tasks use `execution.harness_engine`; a plane-launched task pins the engine
from its grant. The pin overrides the process default. Chat selection is
independent.

## Test lanes

- `make test-execution-harness` — the production seam without the monolith's
  fixture build.
- `make test-execution-harness-live` — first-stage matrix: local read/write,
  tool-error recovery, and a real child delegation followed by parent
  continuation, under the built-in engine and six foreign harnesses (`pi`,
  `claude_code`, `codex`, `codex_app_server`, `grok`, `agy`;
  `scripts/eval_harness_execution.py`).
- `make test-execution-harness-eval` — the evidence grader, without services.
  See the [runner contract](../scripts/execution-harness-eval.md).
- `make test-execution-harness-adapters-live` — starts the production MCP handler
  on a temporary loopback port and drives installed CLIs against the real file
  providers: read then JSON write, the answer, governed dispatch, meter and
  canonical engine attribution. It does not replace the full task/output
  evaluation.
- `make test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=execution`
  writes the lane report under `coverage/evals/harness-conformance/execution/live/`.

Delegation has an independent gate: child lineage, target agent, successful
child tool use and published result, and a parent write after child completion.
A correct answer alone is not conformance; runtime interruptions are
inconclusive, never passes.

The lane's `direct_web_research` case runs `scripts/eval-web-researcher-live.py
--case <fixture>` (default `direct_openai_sarvam_pricing`) and takes that eval's
gates as its `effect`. It cannot be launch-pinned (the eval creates its task
through the ordinary V3 path), so the lane switches the run engine
(`PUT /plane/engine`) around the subprocess and restores engine and model in
`finally`, retrying within a bounded window if the service restarted. Under a
harness the eval's router-profile gates are not evidence (a harness makes no
`agentic_decision` call; the settle proof stands in) and its `tools.*` gates read
an analytics registry a plane call never writes, so the lane re-derives those
requirements from the journal's governed hands.

## Execution contract

**Grants and catalog**

- Run grants carry the execution's tool index, offered catalog, approval rules,
  tool denials and launch attenuation. Static hot names do not authorize
  unavailable capabilities; `tool_search` discovers within this ceiling.
- Harnesses that cannot refresh their tool list receive their permitted indexed
  tools at startup; later discovery neither grants authority nor unloads tools.
- A registry build publishes only compiled packs it can serve. Unbound compiled
  packs are withheld from the catalog *after* late binding completes (the scope
  snapshot before building its tool index; the boot after its in-place
  bindings, also retiring the names from the local tool surface); withholding at
  registry-build time would remove the definitions the boot binds from. A switched
  engine follows the catalog as written, so an advertised unbound pack fails as
  `No provider registered`.
- A run grant's catalog is also filtered by the run's effective registry
  (narrowed to the owner's tool scope), since dispatch resolves there. An empty
  registry means unknown dispatchability, not denial.
- `delegate_to_agent` has no tool-index entry, so a run whose executors carry a
  delegation dispatcher admits it explicitly (as with `tool_search`).

**Delegation on the plane**

- `delegate_to_agent` is a plane-owned verb whose only argument is the targets.
  It spawns nothing: it captures the targets on the grant and ends the turn with
  a delegation stop (as a gated action ends it with an approval stop). The turn
  engine reads the stop reason back from the grant after every settle, because a
  one-shot CLI never observes the stop signal. The settled turn becomes the
  loop's `Decision::DelegateToAgent`, and the native handler validates, spawns,
  parks `WaitingForChildren`, wakes on the V3 lifecycle and resumes.
- The plane schema does not offer `required_capability` or `expected_artifacts`
  (a harness has no roster of packs or artifact contract); a call sending them is
  refused with the reason. Everything the child must report belongs in `context`.
- A parent reconciles only once every child in `active_delegation_group` is
  terminal (`delegated_children_are_settled`), and defers while a completed
  child with no registered output still has a finalizer pending; the child's
  output callback re-enters the resume, and a failed finalizer clears the wait.
- The resume summary carries each child's primary text and promoted media
  (`collect_child_delegation_deliverable`); a failed read degrades to the path
  line. The resumed parent's outcome is persisted from a job on the execution
  runtime (fresh stack): the terminal-settlement chain nearly fills a 2 MiB
  worker stack.
- A delegation successor and a refinement pass each run on a new segment under a
  fresh stateless control generation, registered and published before the exact
  resume and cleared after; the successor seed hands over the prior segment's
  speculative-phase terminal fence (`rebind_terminal_fence_to_successor`), never
  control or manual-pause fences.
- Parent cancellation reaches the grant's child token; revoking a turn does not
  cancel its parent. Grants are revoked on startup failure and turn completion.

**Answers and completion**

- A nonempty settled answer enters the synthetic-completion evidence gate as
  `Artifact::task_deliverable`, published byte-preserving. Refused, cancelled,
  exhausted and empty replies cannot claim completion; a summary-only yield is
  no progress.
- The `yield` summary is the answer the reader receives: it must state every
  value, identifier, quotation and count the goal asked for, verbatim.
- The finalizer composes statement then deliverable when the deliverable is
  textual and fits the bounded preview; a large or binary deliverable is
  published exactly as accepted. It also appends every other small textual note
  the execution filed under a verified digest (in filing order) that the
  published text lacks; notes failing any check are left out.
- `publishes_verified_bytes_unchanged` is the one predicate the execution output,
  task-agent and task-user projections use to decide whether to digest-check the
  file or read composed text.
- The synthesis in-flight marker is registered in the same commit that flips a
  root execution to `completed` (only on the transition), and terminal reducers
  and startup recovery clear it — so "terminal and nothing pending" always means
  the output is readable.

**Budget, telemetry, journal**

- Reported tokens charge the hard execution meter. A settle without usage
  exhausts an active hard budget — except a turn Magician itself stopped (approval
  or delegation), which is not charged. The rig's `token_usage` gate exempts
  `Delegate` / `NeedsApproval` settles.
- `execution.progress` with `kind: harness_turn_settled` identifies engine,
  iteration, stop reason and token counts, without prompt/answer content.
- A pack action's journal row names what it did:
  `files(action="read", path="/abs/path")`. Only `action`, `path`, `source`,
  `destination` are rendered (JSON-quoted, length-bounded) — parameters can carry
  credentials, so the allowlist is the boundary. Conformance gates read this
  field.
- A failed governed call journals `tool.failed` with the same
  `(action_type, target)` identity, `success: false` and a bounded `error`, on
  both failure arms (provider error, executor-classified failure).

**Pause integrity and recovery**

- A pause envelope's authorization hash is sealed over the body as read back
  from disk; a mismatch names the field that moved. The failed-tool ledger is a
  `BTreeSet` because the hash canonicalises keys but not array order.
- The interrupted-execution sweep (every 30 s) treats a runnable row this process
  updated within the last two minutes as owned, so a freshly admitted child
  without controls is not driven twice. Rows untouched since before process start
  are recovered by the startup sweep; rows a panicked owner abandoned, after the
  grace.
- A checkpoint that fails hydration names the disagreeing component.

**Process environment and CLI argv**

- `magicllm`'s one-shot harness provider clears the environment and passes only
  `PATH`, `HOME`, `USER`, `TMPDIR`, `LANG` (matching `apply_env_allowlist`):
  ambient provider keys would bill the wrong account, and a parent coding
  harness's session markers (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`,
  `CODEX_COMPANION_SESSION_ID`) make nested launches refuse or hang. Key-based
  auth is explicit configuration.
- For grok and agy the prompt follows `-p` immediately; a failed CLI's stderr
  tail is part of the error.
- Prompts include task identity, prior work and recent history; prompts and
  operator steering are sanitized before reaching the harness.

**Build note**

`magician-bin/tests/execution_harness_contract.rs` includes
`plane/turn_engine.rs`, `plane/usage.rs` and `coding_engine/codex_usage.rs` via
`#[path]` and resolves `crate::magician_v2` through the public `magician` crate,
so everything they reference must be `pub` (e.g.
`build_delegation_results_section`). Prove changes with
`cargo test -p magician-bin --no-run --test execution_harness_contract`.

Codex App Server usage is read from `tokenUsage.total` / `last` of
`thread/tokenUsage/updated`, excluding prior conversation spend; incomplete usage
objects are rejected.
