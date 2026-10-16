# Agentic execution harness evaluation

`eval-harness-conformance-live.py --lane execution` calls
`eval_harness_execution.py`. `make test-execution-harness-eval` (or `--self-test`)
validates grading and runner contracts without contacting services or providers.
`make test-execution-harness-live` uses its own report directory, preserving
concurrent chat evaluation evidence.

Authentication uses `MAGICIAN_BEARER_TOKEN`, or configured
`MAGICIAN_EVAL_USERNAME` / `MAGICIAN_EVAL_PASSWORD`. Login selects the default
workspace; an existing bearer retains its bound workspace. Reports exclude
credentials and grant values. Both HTTP 200 and 201 are valid login responses;
login errors report the status without copying the authentication response.

The file and delegation cases create a nonce task and a bounded, launch-pinned
grant without changing global engine settings. `direct_web_research` temporarily
switches the run engine and restores the prior setting in `finally`. Stage one
defaults to all seven engines:
Magician, Pi, Claude Code, Codex CLI, Codex App Server, Grok and Antigravity.
The same five cases run for each engine, giving a 35-cell matrix per repeat.
For Pi, configure `execution.pi_profile` with a chat-eligible Magician profile
or configure Pi's own credentials and default model before a live run. The
execution route also needs the normal runtime keychain signer; starting
Magician with `MAGICIAN_SKIP_KEYCHAIN=1` cannot exercise this lane.

| Case | Required evidence |
| --- | --- |
| `local_tools` | Governed read of the exact generated file; published hidden second-line marker and four-line count. |
| `local_write` | Governed read and write of the exact fixture paths, plus actual JSON containing the marker and numeric count. |
| `tool_recovery` | An observed governed missing-file error, followed by a successful read of the real file and the correct final answer. |
| `delegate_tools` | A real completed child belonging to this parent/task and the requested agent; the child's governed file read and published answer; a governed parent JSON write after the child completes, with the correct contents. |
| `direct_web_research` | The web-researcher live evaluation's grounded answer, citations, and selected-engine journal evidence. |

The delegation fixture explicitly requests delegation to the existing
`simple-data-analyst` agent, a seeded personal-assistant delegation target.
Override with `--execution-delegate-agent AGENT_ID` if the workspace uses a
different permitted worker with file access. The runner creates no agents and
changes no delegation grants. It asks the selected engine to delegate; it does
not force delegation through a task-start API parameter. Native CLI subagents,
unrelated tasks, a queued child, or the parent doing the read cannot satisfy
the case. The child's engine is recorded in its journal when available; this
case checks the selected **parent** engine's ability to delegate and resume,
not child-engine inheritance.

The marker is absent from the prompt, title and path. Passing also requires
full completion, a published answer, governed calls, parent harness
attribution, complete nonnegative/nonzero foreign-harness token usage, and no
advertised-tool provider failures. Model prose alone never establishes a
tool call or delegation. Missing evidence fails the relevant gate.

Reports contain JSON, HTML and per-case fixture evidence; tasks and fixtures
are kept, and nothing in the data root is deleted. The runner cancels only its
own active runs on timeout and revokes its own grants in `finally`. Cleanup
failures fail the case, except runtime outages (including transport loss
during MCP initialization or launch), which stay inconclusive with cleanup
errors recorded; an explicit HTTP refusal is still a failure. Descendant
cleanup follows only the nonce task's lineage, and a child still active after
its parent ends fails the case and is cancelled. A runtime unavailable before
launch yields one inconclusive entry per requested engine/case/repeat without
creating a task; missing harness installations are inconclusive too. Reports
count pass/fail/inconclusive and never call a partly unavailable matrix
successful.

`make test-execution-harness-adapters-live` runs the installed CLIs against a
temporary HTTP server using the production MCP handlers and file providers.
Each harness must read an unpredictable marker and write the checked JSON.
It isolates adapter and execution-seam failures from shared-runtime restarts.
`HARNESS_CONFORMANCE_ENGINES` selects engines for both live targets. Adapter
reports default to
`$(COVERAGE_BASE_DIR)/evals/harness-conformance/execution/adapters/latest`
(reachable through the repo-root `coverage/` symlink).
Set `HARNESS_EXECUTION_ADAPTER_REPORT_DIR` to retain a separate repeat report.

When concurrent edits keep invalidating Cargo's cache, copy the executable
printed by `make test-execution-harness` to a stable path on the build volume
and pass it as `HARNESS_EXECUTION_TEST_BINARY` to
`make run-execution-harness-adapters-live`; rebuild before qualifying newer
source.

```sh
make test-execution-harness-live \
  HARNESS_CONFORMANCE_RUNS=2 \
  HARNESS_CONFORMANCE_LIVE_EVAL_ARGS='--turn-timeout-secs 900'

# Narrow a follow-up run to the baseline and one replacement.
make test-execution-harness-live \
  HARNESS_CONFORMANCE_ENGINES=magician,codex \
  HARNESS_CONFORMANCE_LIVE_EVAL_ARGS='--case delegate_tools --execution-delegate-agent simple-data-analyst'

# Exercise Pi's governed file-read path with its configured run profile.
make test-execution-harness-live \
  HARNESS_CONFORMANCE_ENGINES=pi \
  HARNESS_CONFORMANCE_LIVE_EVAL_ARGS='--case local_tools'
```

Provider-free tests cover the fixture's positive matrix shapes and reject wrong
lineage/agent/path, borrowed events, queued or partial children, parent reads,
premature parent writes, unrelated errors, wrong engines, and missing/invalid
usage. They validate the oracle, not installed-harness behavior. Stage one
does not qualify approval round trips, cancellation/resume, server restart
recovery, parallel children, or long-running loop equivalence.
