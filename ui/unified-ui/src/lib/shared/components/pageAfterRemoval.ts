/**
 * Where a paged list lands after one of its rows is removed.
 *
 * Every paged surface here answers the same question after a delete, a dismiss
 * or a resolve: the row is gone, so what should the reader be looking at now?
 * There is one answer, and it is the same one for all three of the paging
 * models in this app:
 *
 * > Decrement the total locally. **Refetch the page you are on.** If it comes
 * > back empty and you are not on page one, **step back one page.**
 *
 * The caller owns the first sentence — a total lives somewhere different in
 * every model — and this owns the other two.
 *
 * "The page you are on" means something in each model, which is what makes one
 * policy enough:
 *
 * - **Offset** (internal tasks) — re-read the same offset. Deleting the only
 *   row of the last page returns nothing, and the step back is what stops the
 *   reader staring at an empty table under a pager still counting that page.
 * - **Replacing cursor** (monitors, the Today resurfacing band) — re-read the
 *   cursor stored for that page. Those cursors are already kept so that Prev
 *   and First are single fetches, so the step back costs no more than any
 *   other move.
 * - **Client slice over a growing buffer** (`/attention`) — a "refetch" is
 *   *extend the buffer until this page is covered again*. This is the model
 *   that decides the shape below: the re-read is never skipped on the strength
 *   of an empty local page, because here an empty page is not evidence that the
 *   page is empty — it is the request to go and fill it. A surface that steps
 *   back instead drains toward page one while the reader works it, which is the
 *   defect this exists to prevent.
 *
 * `refetch` re-reads one 1-based page and resolves to the number of rows that
 * page now holds. It is called at most twice: the page the reader is on, and
 * then the page before it if the first came back empty. **A failed read is not
 * an empty page** — a caller whose fetch can fail should resolve to a non-zero
 * count on failure so a network error does not move the reader.
 *
 * One step back, not a walk: a second empty page is a different event (rows
 * left from somewhere other than this removal), and every caller has either a
 * poll or a standing clamp that settles it.
 */
export async function pageAfterRemoval(
	currentPage: number,
	refetch: (page: number) => Promise<number>
): Promise<number> {
	const page = Number.isFinite(currentPage) ? Math.max(1, Math.floor(currentPage)) : 1;
	const rows = await refetch(page);
	// Page one is the floor. An empty page one is an empty list, which is a
	// state the reader is entitled to see — there is nowhere to step back to.
	if (rows > 0 || page === 1) return page;
	const previous = page - 1;
	await refetch(previous);
	return previous;
}
