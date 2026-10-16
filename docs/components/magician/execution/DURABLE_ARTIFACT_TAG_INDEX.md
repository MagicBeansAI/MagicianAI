# Durable Artifact Tag Index

## Purpose

Cross-namespace artifact discovery by task, agent, and execution, so the
runtime can answer "what did this task produce?" without knowing whether
outputs live under `execution_artifacts`, `task_state`, `surfaces`, or another
durable namespace.

## Core Model

- `DurableFrontmatter` is the source of truth for durable artifact provenance.
- `DurableArtifactStore` keeps a shared in-memory `TagIndex` across store clones.
- Tags are derived at runtime and never written back as separate metadata:
  namespace (for example `execution_artifacts`, `task_state`, `surfaces`),
  `task:<id>` from `source_task_id`, `agent:<id>` from `source_agent_id`,
  `execution:<id>` from `source_execution_id`.

## Provenance Sources

Durable writers populate structured provenance:

- `task_state_provider` writes task, execution, and agent provenance from
  injected hidden execution params.
- `materialize_dashboardable_artifacts()` (beside `AutoSurfacePublisher`) writes
  execution artifacts with task, execution, cycle, and agent provenance.
  `AutoSurfacePublisher` publishes V3 surface records, not durable manifests.
- The executor's durable-file post-write hook stamps that provenance onto
  direct file writes, then reindexes.

Durable files with frontmatter participate in tag lookup even when they are not
lifecycle-registered.

## Query Surface

- `DurableArtifactStore::list_by_tags()` intersects tags. Production code does
  not call it.
- Chat `list_artifacts` requires `task_id` and lists from the scoped V3
  artifact store (`FilesystemExecutionArtifactIndexStore::list_artifacts`).
  Optional `execution_id` falls back to the task's active or latest root
  execution. No tag parameter.
- `read(namespace, name)` fetches full durable content once a path is known.

Production discovery is the V3 artifact store; chat tools do not use the tag
index.

## Index Lifecycle

Startup walks namespace directories recursively and builds the index from disk.
`write()` inserts or replaces; `append()` reindexes after the frontmatter
timestamp changes; `delete()` drops the artifact from every tag bucket;
`reindex_artifact()` refreshes after out-of-band frontmatter mutation
(executor provenance stamping). Tags are derived, so there is no migration
layer. Restart rebuilds from disk.

## Current Boundaries

- Plain `list()` is still the older filesystem enumeration path. It is not
  fully index-backed, and unfiltered listing remains non-recursive.
- Tag query ordering is not guaranteed; callers that need "latest" sort by
  artifact timestamp after lookup.
- Tags are provenance-derived only. User-authored semantic tags are out of
  scope.
