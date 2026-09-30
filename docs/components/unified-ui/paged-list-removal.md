# Removing a row from a paged list (web client)

One policy, one helper
(`src/lib/shared/components/pageAfterRemoval.ts`), for every paged surface in
`ui/unified-ui`.

## The policy

> Decrement the total locally. **Refetch the page you are on.** If it comes
> back empty and you are not on page one, **step back one page.**

The caller owns the first sentence — a total lives somewhere different in every
paging model — and `pageAfterRemoval(currentPage, refetch)` owns the other two.
It calls `refetch` at most twice: the page the reader is on, then the page
before it if the first came back empty. It returns the page the reader landed
on.

## Why one policy covers three paging models

"The page you are on" means something in each of them, which is what makes a
single rule enough:

| Model | Surface | What a refetch is |
| --- | --- | --- |
| Offset | Internal tasks (`?type=internal`) | Re-read the same offset |
| Replacing cursor | Monitors (`?type=monitors`), the Today resurfacing band | Re-read the cursor stored for that page |
| Client slice over a growing buffer | `/attention` | Extend the buffer until that page is covered again |

The third row is what fixes the helper's shape. On a client slice an **empty
page is not evidence that the page is empty** — it is the request to go and
fill it. So the re-read is never skipped on the strength of an empty local
buffer, even though on the two server-paged models skipping it would save a
request. A surface that steps back instead of refilling drains toward page one
while the reader works it, which on `/attention` — where resolving and
dismissing *is* the point of the page — is the whole defect.

**A failed read is not an empty page.** A caller whose fetch can fail must
resolve `refetch` to a non-zero count on failure, so a network error reads as
"unchanged" rather than as "this page ended" and does not move the reader.

**One step back, not a walk.** A second empty page means rows left from
somewhere other than this removal; every caller has a poll or a standing clamp
that settles that.

## Who uses it

- **Internal tasks** — `InternalTasksWorkspace.svelte`, on delete (so deleting
  the only row of the last page cannot leave an empty page).
- **Monitors** — `MonitorPager.removeRow(taskId)` in `monitors/pagination.ts`,
  called from the workspace's `on:deleted` (delete and pause live in the
  detail panel; the replacing pager would drain like `/attention` under any
  inline row action).
  `MonitorPager.reloadCurrentPage()` is the same re-read without the step back,
  for mutations that change a row instead of removing one — it is what keeps
  pause/resume/save from bouncing the reader back to page one.
- **`/attention`** — `reconcileInboxAfterRemoval()` in
  `routes/(app)/attention/+page.svelte`, after a row the reader resolved or
  dismissed leaves the store. See [attention-pagination](./attention-pagination.md).

Two surfaces keep their own hand-written copy because adopting the helper would
not be a simplification: the Today resurfacing band
(`ResurfacingBand.svelte`, six lines that also own the parent's `pagechange`
dispatch) and the Today store, whose step back is already expressed as a
reactive clamp over derived section counts.

## Tests

- `src/lib/shared/components/pageAfterRemoval.test.ts` — the policy against a
  fake of each model: stay-and-re-read, step back off an emptied final page,
  page one as the floor, refill of a non-final client slice, step back once the
  cursor is exhausted, and the failed-read rule.
- `src/lib/monitors/pagination.test.ts` — `removeRow` and `reloadCurrentPage`
  over the cursor pager.
- `src/lib/internalTasks/InternalTasksWorkspace.component.test.ts` — deleting
  the only row of the last page lands the reader on a page that has rows.
