# Magician Chunking

Production dependencies keep `magician/test-fixtures` disabled. This crate's
explicit `test-fixtures` feature forwards it, and development dependencies enable
it for tests, so extracting a test helper does not pull the monolith's fixture
modules into every debug runtime build.

The logical-chunking domain adapters as a satellite crate (~8k lines):

* `hierarchical_adapters` — the consolidation/archive and evidence-distill
  adapters over `artifact_v2` memory records, including the deterministic
  archive-group planner.
* `memory_adapters` — the four builtin memory operation adapters (episode
  quality, utility review, entities, environment).
* `readiness` — release-readiness diagnostics for the Phase 6/7 candidates.
* `shadow_eval` — the Phase 6 shadow-evaluation harness and fixture suite.

## Lib-side seam

`magician_v2::llm_chunking` keeps the runner, the adapter registry, and the
`CHUNK_RELEASE_CANDIDATES` vocabulary — the query-analysis LLM dispatch path
wires `LogicalChunkRunner` against the global registry in production, and
config validation crosses the release candidates at startup and hot reload.
The batch consolidator's `archive_checkpoint_groups` is restated lib-side
(the durable checkpoint boundary must not depend on the satellite).

## Boot registration

The lib's `global_chunk_adapter_registry()` initializes **empty**. After any
provider-free authoring command has exited, the bin calls
`magician_chunking::register_builtin_chunk_adapters()` before loading a live
workspace or constructing an LLM router. This makes configs that enable
chunking validate against the real builtins on both normal service boot and
live Apps CLI paths. Registration is idempotent (skips adapters already
present). Readiness tests run in this crate so registration and the registry
they assert on share one lib instance.

Readiness pins the six shipped candidates to Ollama `gemma4:12b`, matching
the repository router. The exact local endpoint,
32K physical context, 4K output reserve and chunking-policy checks still apply.
