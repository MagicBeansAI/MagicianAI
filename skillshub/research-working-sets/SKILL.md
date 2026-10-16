---
name: research-working-sets
version: 0.1.3
description: Procedure skill — preserve durable, server-captured research evidence across delegation and long-running synthesis. Use when web research must be resumed, audited, or handed from a web researcher to another worker such as VC research. Covers working-set references, scoped search/read, citation discipline, and failure handling. It owns no executable tool and never accepts model-supplied evidence for storage.
metadata:
  magician:
    skill_type: procedure
---

# Durable Research Evidence Handoff

Use this procedure when research needs to survive beyond the current answer:

- a worker delegates web research to another agent;
- another worker must verify or extend an earlier result;
- a long-running investigation needs an auditable evidence trail; or
- a final recommendation depends on exact source text rather than a prior
  worker's paraphrase.

Do not activate it for a self-contained lookup that ends in the same turn and
needs no later evidence access. `web-researcher` still captures eligible pages
automatically; this procedure decides how to preserve and consume the returned
reference.

## Core contract

`content_read` and the `web_fetch` compatibility facade share one
server-managed evidence writer. They capture a working set only after the
retrieval controller returns a claim-eligible `ContentDocument`. A model cannot create a working set from
prompt text, a pasted quote, a search snippet, or a reconstructed page body.
Only `public` documents may become durable evidence. Private or restricted
documents remain in the retrieval response but are never written to a working
set.

Working sets are immutable, scope-bound, and bounded operationally: snapshots
expire after seven days, and each principal/workspace retains at most 64 sets
or 256 MiB of source content. The server removes expired snapshots and evicts
the oldest retained snapshots before accepting a new capture. Do not rely on a
working-set reference as permanent storage.

The server bounds the pre-publication staging write to 1.5 seconds. Storage
failure reports `unavailable` without changing the source read, while a
`created` reference is returned only after immutable publication completes.
The server plans retention before publication but removes older snapshots only
after the new immutable snapshot is published under a per-scope lock; transient
storage errors therefore preserve existing evidence. Interrupted
staging directories are reclaimed after five minutes on a later capture.

The capture appears at the top level of a `content_read` result. For
`web_fetch`, it appears under `retrieval.working_set` so the compatibility
fields remain unchanged:

```json
{
  "working_set": {
    "status": "created",
    "working_set_id": "ws-opaque-id",
    "title": "Web research evidence",
    "source_count": 3,
    "chunk_count": 8
  }
}
```

`working_set_id` is opaque. Preserve it exactly. It is valid only inside the
same principal and workspace; it is neither a URL nor a credential and does
not authorize cross-scope access.

## Web-research workflow

1. Discover with `content_search` and read selected pages with `content_read`.
   Search snippets choose pages but are never factual evidence.
2. Inspect the normal read result first: URL, redirect status, title, and
   supporting text must actually support the intended claim.
3. For every `working_set.status: created`, retain the exact
   `working_set_id` with the task state or delegation result. Include a short
   description of which claims or sources it covers.
4. Finish the answer normally. The working-set reference supplements the
   answer; it does not replace inline citations or permit a claim unsupported
   by the opened page.

Capture is automatic for the active agents listed in the trusted deployment
setting `content_acquisition.working_sets.auto_capture_agents`; the shipped
default is `web-researcher`. Other callers may request a named snapshot with
`working_set_title`, but must never fabricate the document body or hash.

## Downstream-worker workflow

Use a received working-set reference before re-fetching a page merely to
recover context.

1. Call `working_set_search` with the exact `working_set_id` and a narrow
   evidence phrase. It returns source title, canonical URL, source ID, chunk
   index, hash, score, and a bounded excerpt.
2. Call `working_set_read` with that exact `working_set_id`, `source_id`, and
   `chunk_index` when the complete cited chunk is needed for synthesis.
3. Base the claim only on the returned chunk. Keep the source title and URL in
   the final inline citation.
4. If no chunk supports a material claim, mark the claim unverified or ask the
   web researcher for a focused additional read. Do not convert a prior
   worker's unsupported summary into evidence.

For VC research, the web-researcher delegation should return the working-set
reference alongside its findings. Search the handoff first; only delegate a
new web read for a real evidence gap, freshness need, or contradiction.

## Capture outcomes

- `created`: preserve and use the reference.
- `empty`: the read yielded no claim-eligible document. Do not invent an ID or
  treat discovery-only text as evidence.
- `unavailable`: retrieval itself succeeded but durable storage was
  unavailable. Use the successful read result for the current task, report an
  audit limitation only when material, and do not re-fetch solely to retry
  storage.
- no `working_set` field: the caller was not eligible for automatic capture
  and did not opt in. Continue existing behavior; do not assume a snapshot
  exists.

## Boundaries and anti-patterns

- Never call a source-native fetcher, shell, or browser just to recreate a
  working set. The controller owns acquisition and capture.
- Never quote a `working_set_search` excerpt as though it were the complete
  source when the claim requires surrounding context. Read the cited chunk.
- Never mutate, append to, or reuse a working-set ID for new evidence. Each
  working set is immutable; request another controller read when evidence
  changes.
- Never hand a reference across principal or workspace boundaries. The store
  fails closed on that access.
- Never expose raw internal storage errors to the user. `unavailable` is a
  bounded storage condition, not evidence that a source was wrong.
