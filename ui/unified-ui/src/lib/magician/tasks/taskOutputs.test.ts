/**
 * The joint, not the two sides of it.
 *
 * `taskFilePreview.test.ts` proves the classification and `UnifiedTaskPanel`'s
 * suite proves the rendering; what neither can see is the piece between them —
 * whether a file the panel offers no preview for is ever fetched, whether the
 * ceiling is enforced against a size the record only *claimed*, and whether a
 * body larger than its own record still stops. Every one of those is a seam
 * where both halves can be right and the result still wrong.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock } from '../../../test/browser';
import { PREVIEW_MAX_BYTES } from './taskFilePreview';
import { loadOutputPreview } from './taskOutputs';
import type { PanelOutputFile } from './taskPanelModel';

afterEach(() => {
	vi.unstubAllGlobals();
});

const URL_BASE = '/api/magician/v3/tasks/task_alpha/outputs';

/**
 * One output row. Every default is the *previewable* answer, so a case that
 * fails does so because of the one field it overrode and not because the
 * fixture was already unreadable.
 */
function file(overrides: Partial<PanelOutputFile> = {}): PanelOutputFile {
	return {
		name: 'report.md',
		kind: 'document',
		path: 'q3/report.md',
		mediaType: 'text/markdown',
		sizeBytes: 4_096,
		url: `${URL_BASE}/q3/report.md`,
		...overrides
	};
}

const textResponse = (body: string) => new Response(body, { status: 200 });

/** Every request that reached the network, so "no request" is provable. */
function serving(body: string) {
	return installFetchMock([{ match: () => true, handle: () => textResponse(body) }]);
}

describe('loadOutputPreview — what is fetched, and what is refused before it is', () => {
	it('returns the bytes, keyed to the row that asked', async () => {
		const { calls } = serving('# Revenue\n\nNorth rose 12%.\n');

		const preview = await loadOutputPreview(2, file());

		expect(preview).toEqual({
			index: 2,
			status: 'ready',
			text: '# Revenue\n\nNorth rose 12%.\n',
			detail: null
		});
		// The row's own address, not one rebuilt here: a second minter is a second
		// answer to where a file lives.
		expect(calls.map((call) => call.url)).toEqual([`${URL_BASE}/q3/report.md`]);
	});

	it('refuses a kind it cannot draw without spending a request', async () => {
		const { calls } = serving('%PDF-1.7');

		const preview = await loadOutputPreview(0, file({ name: 'r.pdf', path: 'r.pdf', mediaType: 'application/pdf' }));

		expect(preview.status).toBe('failed');
		expect(preview.text).toBeNull();
		expect(preview.detail).toBeTruthy();
		// The point of the assertion: a loader that fetched first and classified
		// afterwards would pull a hundred megabytes of archive to decide it had
		// nothing to show.
		expect(calls).toHaveLength(0);
	});

	it('refuses a row with no address, rather than requesting one built from nothing', async () => {
		const { calls } = serving('anything');

		const preview = await loadOutputPreview(1, file({ url: null }));

		expect(preview.status).toBe('failed');
		expect(preview.index).toBe(1);
		expect(calls).toHaveLength(0);
	});

	it('stops a file the record already says is too large, before it is read', async () => {
		const { calls } = serving('x');

		const preview = await loadOutputPreview(0, file({ sizeBytes: 4 * 1024 * 1024 }));

		expect(preview.status).toBe('too-large');
		expect(preview.text).toBeNull();
		expect(preview.detail).toContain('4.0 MB');
		expect(calls).toHaveLength(0);
	});

	/**
	 * The half a declared-size check cannot cover, and the reason there are two.
	 * `size_bytes` is optional on the outputs row and is written at synthesis
	 * time, so a file appended to since — a log — arrives larger than its record.
	 */
	it('stops a body that arrived larger than its record claimed', async () => {
		const { calls } = serving('y'.repeat(PREVIEW_MAX_BYTES + 1));

		const preview = await loadOutputPreview(0, file({ path: 'run.log', mediaType: null, sizeBytes: null }));

		expect(preview.status).toBe('too-large');
		expect(preview.text).toBeNull();
		expect(calls).toHaveLength(1);
	});

	it('counts bytes rather than characters, so a multi-byte file is measured honestly', async () => {
		// Just under the ceiling in UTF-16 code units and three times over it in
		// bytes. A check on `text.length` passes this and puts 750 KB of text
		// into a drawer.
		serving('€'.repeat(PREVIEW_MAX_BYTES - 10));

		const preview = await loadOutputPreview(0, file({ sizeBytes: null }));

		expect(preview.status).toBe('too-large');
		// 768 KB, not the 256 KB a code-unit count would have measured — the
		// number in the sentence is the one that proves which was counted.
		expect(preview.detail).toContain('768 KB');
	});

	it('says so on a non-2xx, and carries the status', async () => {
		installFetchMock([
			{ match: () => true, handle: () => new Response('nope', { status: 404 }) }
		]);

		const preview = await loadOutputPreview(0, file());

		expect(preview.status).toBe('failed');
		expect(preview.text).toBeNull();
		expect(preview.detail).toContain('404');
	});

	it('says so when the request throws, rather than answering with an empty file', async () => {
		installFetchMock([
			{
				match: () => true,
				handle: () => {
					throw new Error('Network is unreachable');
				}
			}
		]);

		const preview = await loadOutputPreview(3, file());

		expect(preview).toEqual({
			index: 3,
			status: 'failed',
			text: null,
			detail: 'Network is unreachable'
		});
	});

	it('reports an empty file as empty rather than as a failure', async () => {
		serving('');

		const preview = await loadOutputPreview(0, file({ sizeBytes: 0 }));

		// `''` with `ready` is what lets the panel say `This file is empty`; a
		// `failed` here would blame the network for a fact about the task.
		expect(preview.status).toBe('ready');
		expect(preview.text).toBe('');
	});
});
