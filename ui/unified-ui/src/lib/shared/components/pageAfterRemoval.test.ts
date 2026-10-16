import { describe, expect, it, vi } from 'vitest';

import { pageAfterRemoval } from './pageAfterRemoval';

/**
 * An offset-paged list: `rows` is the whole corpus, a page is a slice of it,
 * and a refetch of page N sees whatever the corpus holds at that moment. This
 * is `/tasks?type=internal`.
 */
function offsetList(corpus: string[], pageSize: number) {
	const reads: number[] = [];
	return {
		reads,
		refetch: async (page: number): Promise<number> => {
			reads.push(page);
			return corpus.slice((page - 1) * pageSize, page * pageSize).length;
		}
	};
}

/**
 * A client slice over a buffer that only grows when asked: `buffered` rows are
 * in hand, `server` more are reachable, and a refetch of page N pulls cursor
 * pages until N is covered. This is `/attention`, and it is the model that
 * makes the un-skippable re-read load-bearing — an empty page here is a
 * request to refill, not proof that the list ended.
 */
function growingBuffer(buffered: number, server: number, pageSize: number) {
	const reads: number[] = [];
	let inBuffer = buffered;
	let remaining = server;
	return {
		reads,
		get inBuffer() {
			return inBuffer;
		},
		refetch: async (page: number): Promise<number> => {
			reads.push(page);
			const needed = page * pageSize;
			while (inBuffer < needed && remaining > 0) {
				const pulled = Math.min(pageSize, remaining);
				inBuffer += pulled;
				remaining -= pulled;
			}
			return Math.max(0, Math.min(inBuffer, needed) - (page - 1) * pageSize);
		}
	};
}

describe('pageAfterRemoval', () => {
	it('stays on a page that still has rows, and re-reads it', async () => {
		// 45 rows over pages of 20: page 2 still holds 20 after the removal.
		const list = offsetList(Array.from({ length: 45 }, (_, i) => `row-${i}`), 20);

		expect(await pageAfterRemoval(2, list.refetch)).toBe(2);
		// The re-read is not optional: the row that left pulled a later row
		// forward onto this page, and only the server knows which one.
		expect(list.reads).toEqual([2]);
	});

	it('steps back one page when the removal emptied the final page', async () => {
		// 40 rows left over pages of 20 — page 3 held the row that just went.
		const list = offsetList(Array.from({ length: 40 }, (_, i) => `row-${i}`), 20);

		expect(await pageAfterRemoval(3, list.refetch)).toBe(2);
		expect(list.reads).toEqual([3, 2]);
	});

	it('stays on page one when it empties — there is nowhere to step back to', async () => {
		const list = offsetList([], 20);

		expect(await pageAfterRemoval(1, list.refetch)).toBe(1);
		expect(list.reads).toEqual([1]);
	});

	it('refills a non-final page from the server rather than leaving a gap', async () => {
		// The reader emptied page 3 of a client-sliced inbox, but the server
		// still has rows: the page must refill, NOT drain back toward page one.
		const buffer = growingBuffer(40, 25, 20);

		expect(await pageAfterRemoval(3, buffer.refetch)).toBe(3);
		expect(buffer.reads).toEqual([3]);
		expect(buffer.inBuffer).toBe(60);
	});

	it('steps back on a client slice only once the server has nothing left', async () => {
		// Same emptied page 3, but the cursor is exhausted: refilling is not on
		// offer, so the reader lands on the last page that has rows instead of
		// being stranded on an empty one.
		const buffer = growingBuffer(40, 0, 20);

		expect(await pageAfterRemoval(3, buffer.refetch)).toBe(2);
		expect(buffer.reads).toEqual([3, 2]);
	});

	it('does not move the reader when the re-read failed rather than came back empty', async () => {
		// The contract: a caller whose fetch can fail reports a non-zero count,
		// so a network error reads as "unchanged" and not as "this page ended".
		const refetch = vi.fn(async () => 1);

		expect(await pageAfterRemoval(4, refetch)).toBe(4);
		expect(refetch).toHaveBeenCalledTimes(1);
	});

	it('treats a nonsense page as page one', async () => {
		const refetch = vi.fn(async (_page: number) => 0);

		expect(await pageAfterRemoval(Number.NaN, refetch)).toBe(1);
		expect(await pageAfterRemoval(0, refetch)).toBe(1);
		expect(await pageAfterRemoval(-3, refetch)).toBe(1);
		expect(refetch).toHaveBeenCalledTimes(3);
		expect(refetch.mock.calls.map(([page]) => page)).toEqual([1, 1, 1]);
	});
});
