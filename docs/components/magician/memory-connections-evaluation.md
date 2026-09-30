# Memory connections behavioural evaluation

The [feature contract](memory-connections.md) and its focused implementation tests
do not establish model usefulness. This lane records actual decisions separately.

## Live lane

`make test-memory-connections-live-eval` builds the comms example and runs the
configured `memory_connection_review` through `ConnectionRuntime`. The worker
still owns recall, the production prompt, timeout, per-scope reservation,
validation and delivery. An optional observation callback captures supplied
sources, raw replies, provider errors, latency and usage without replacing any
decision. The normal background pass installs no callback.

Cases seed synthetic canonical memories and a current memory activity in fresh
disposable scopes. This establishes the seeded recall-to-delivery path. Capture,
approval-to-recall, restarts and UI interaction are separate journey evidence;
they must not be inferred from seeded-case success.

```sh
make test-memory-connections-live-eval \
  LIVE_EVAL_CONFIG="$MAGICIAN_ROOT_DIR/magician-config.yaml"
```

The default selects 12 smoke cases with at most 12 logical review calls. For a
20-case partition repeated three times:

```sh
make test-memory-connections-live-eval \
  LIVE_EVAL_CONFIG="$MAGICIAN_ROOT_DIR/magician-config.yaml" \
  MEMORY_CONNECTIONS_EVAL_PARTITION=validation \
  MEMORY_CONNECTIONS_EVAL_REPEATS=3 \
  MEMORY_CONNECTIONS_EVAL_MAX_CALLS=60 \
  MEMORY_CONNECTIONS_EVAL_MAX_TOKENS=300000
```

Keep `CARGO_TARGET_DIR` and compiler `TMPDIR` on SSD1. Reports default under
`COVERAGE_BASE_DIR/evals/memory-connections/live`; each run is immutable and
`latest/report.html` plus `latest/report.json` contain the latest evidence. The
Makefile annotation discovers the lane on the existing Evals page.

## Cases and gates

`scripts/fixtures/memory_connections/scenarios.json` freezes 60 scenarios: 20 each
for development, validation and held-out evaluation, each balanced between ten
positive and ten negative cases. The first 12 development cases are the smoke set.
Reports retain corpus and production-prompt hashes. Held-out expectations must
not change in response to model failures.

Initial automated gates: 90% correct negative suppression and 80% positive scenario
matches, per partition and repeat. Both kinds must be present: always returning
silence cannot pass. Provider failure, invalid output or missing evidence cannot
count as correct silence. Cross-scope disclosure, unexpected memory writes and
delivery failure independently fail acceptance.

Positive matching checks retrieval of expected memories, citations to those
memories, an allowed surface and persisted delivery. This is not a human judgement
of usefulness or tone. Raw replies and expected rationale are shown for review;
owner review stays pending until the owner supplies ratings. Model confidence is
an admission input, never an evaluation grade.

## Resources and provenance

The call ceiling bounds logical worker reviews; router retries and provider
requests remain governed by production configuration and must be distinguished
from logical calls when interpreting spend. The reported-token ceiling is checked
between cases and can be exceeded by the final in-flight call. Missing provider
usage stops subsequent cases; it is not reported as zero. Costs are router
estimates when available, not independently verified invoices.

Each case records the activity, retrieved evidence, model reply, connection state,
delivered item/request, checks and timings. Runs record source hashes and process
exit status. Missing prerequisites yield inconclusive evidence.

Report checks: `PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_memory_connections_eval.py`
and `make test-memory-connections-eval-reporting`.

## Capture and response journeys

The companion `memory_capture_journey_live_eval` example persists and reloads a
synthetic owner chat, uses the configured `taste_profile_distill` operation and
production prompt, approves an evidence-grounded proposal, and reviews a later
activity without seeding the preference into structured memory. Capture and
connection usage are reported separately. It explicitly drives the capture
service; it does not certify background sweep scheduling.

`memory_connection_response_journey`, driven by
`scripts/eval-memory-response-journeys.py`, copies successful real-model fixtures
and reopens the stores in separate processes for answering and reconciliation.
The eight journeys cover acknowledge, remember, dismiss and stale evidence for
HITL, plus dismissal and changed evidence for both informational surfaces. They
check foreign-scope rejection, persistence, exact owner-only writes and repeated
reconciliation. These are service-level journeys; actual browser and device
interaction is a separate gate. The original model fixtures are preserved.

`make test-memory-connections-runtime` runs only the connection runtime tests,
including scoped profile recall and profile revisions invalidating old answers.

The examples register the same built-in chunk adapters as production before
initializing the operation router. Runtime env files and Settings routing
overrides are loaded from the supplied config's parent directory. Credentials
are not printed. Missing provider initialization fails the evaluation rather
than counting as a silent decision.

`scripts/summarize-memory-connections-eval.py --run-root <run-root>` creates the
dated study's report index and a blank owner-review template from its partition,
capture, response and test artifacts. Preserve failed scores and distinguish
strict scenario matching from subsequent fixture-quality review.
