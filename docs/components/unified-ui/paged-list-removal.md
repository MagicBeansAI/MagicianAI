# Removing a row from a paged list (web client)

One policy and one helper (`src/lib/shared/components/pageAfterRemoval.ts`) for
every paged surface in `ui/unified-ui`.

## The policy

> Decrement the total locally. **Refetch the page you are on.** If it comes
> back empty and you are not on page one, **step back one page.**

The caller owns the first sentence, since each paging model keeps its total
somewhere else. `pageAfterRemoval(currentPage, refetch)` owns the other two: it
calls `refetch` at most twice (the current page, then the one before if the
first came back empty) and returns the page the reader landed on.

## Why one policy covers three paging models

| Model | Surface | What a refetch is |
| --- | --- | --- |
| Offset | Internal tasks (`?type=internal`) | Re-read the same offset |
| Replacing cursor | Monitors (`?type=monitors`), the Today resurfacing band | Re-read the cursor stored for that page |
| Client slice over a growing buffer | `/attention` | Extend the buffer until that page is covered again |

On a client slice an **empty page is not evidence that the page is empty**; it
is the request to fill it. So the re-read is never skipped because the local
buffer is empty. Otherwise `/attention`, where resolving and dismissing is the
point, would drain toward page one as the reader works it.

**A failed read is not an empty page.** A caller whose fetch can fail must
resolve `refetch` to a non-zero count on failure, so an error leaves the reader
where they are.

**One step back, not a walk.** A second empty page means rows left for another
reason; each caller's poll or clamp settles that.

## Who uses it

- **Internal tasks**: `InternalTasksWorkspace.svelte`, on delete.
- **Monitors**: `MonitorPager.removeRow(taskId)` in `monitors/pagination.ts`,
  called from the workspace's `on:deleted` (delete and pause live in the detail
  panel). `MonitorPager.reloadCurrentPage()` is the same re-read without the
  step back, for mutations that change a row, so pause, resume and save do not
  bounce the reader to page one.
- **`/attention`**: `reconcileInboxAfterRemoval()` in
  `lib/attention/AttentionWorkspace.svelte` (mounted by `/attention`), after a resolved or dismissed row
  leaves the store. See [attention-pagination](./attention-pagination.md).

Two surfaces keep their own copy, where the helper would not simplify: the
Today resurfacing band (`ResurfacingBand.svelte`, which also owns the parent's
`pagechange` dispatch) and the Today store, whose step back is a reactive clamp
over derived section counts.

## Tests

- `src/lib/shared/components/pageAfterRemoval.test.ts`: the policy against a
  fake of each model (stay and re-read, step back off an emptied final page,
  page one as the floor, refill of a client slice, step back once the cursor is
  exhausted, the failed-read rule).
- `src/lib/monitors/pagination.test.ts`: `removeRow` and `reloadCurrentPage`
  over the cursor pager.
- `src/lib/internalTasks/InternalTasksWorkspace.component.test.ts`: deleting
  the only row of the last page lands on a page that has rows.
