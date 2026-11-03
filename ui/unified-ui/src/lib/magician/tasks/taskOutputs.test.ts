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
import { fetchTaskOutputFiles, loadOutputPreview } from './taskOutputs';
import { reportSourcePath, type PanelOutputFile } from './taskPanelModel';

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

describe('fetchTaskOutputFiles — the report source and the host path', () => {
	const jsonResponse = (body: unknown) =>
		new Response(JSON.stringify(body), {
			status: 200,
			headers: { 'Content-Type': 'application/json' }
		});

	it('marks the primary user output as the report source and carries a stated host path', async () => {
		installFetchMock([
			{
				match: () => true,
				handle: () =>
					jsonResponse({
						outputs: {
							task_id: 'task_alpha',
							primary_user_output_id: 'out_task_user_1',
							outputs: [
								{
									output_id: 'out_task_continuation_1',
									audience: 'agent',
									relative_path: 'outputs/out_task_continuation_1.json',
									media_type: 'application/json'
								},
								{
									output_id: 'out_task_user_1',
									audience: 'user',
									relative_path: 'outputs/out_task_user_1.md',
									media_type: 'text/markdown',
									absolute_path: '/Users/me/MagicianNotes/tasks/task_alpha/outputs/out_task_user_1.md'
								}
							]
						}
					})
			}
		]);

		const files = await fetchTaskOutputFiles('task_alpha', 'me', 'default');

		expect(files?.map((file) => [file.name, file.reportSource === true])).toEqual([
			['out_task_continuation_1.json', false],
			['out_task_user_1.md', true]
		]);
		expect(files?.[1].absolutePath).toBe(
			'/Users/me/MagicianNotes/tasks/task_alpha/outputs/out_task_user_1.md'
		);
		// No host path stated, none invented: the row's name stays plain text.
		expect(files?.[0]).not.toHaveProperty('absolutePath');
	});

	it('never treats a relative path as something to copy', async () => {
		installFetchMock([
			{
				match: () => true,
				handle: () =>
					jsonResponse({
						outputs: {
							outputs: [
								{ relative_path: 'outputs/a.md', absolute_path: 'outputs/a.md' }
							]
						}
					})
			}
		]);

		const [only] = (await fetchTaskOutputFiles('task_alpha', 'me', 'default')) ?? [];
		expect(only).not.toHaveProperty('absolutePath');
		expect(only).not.toHaveProperty('reportSource');
	});
});

describe('reportSourcePath — the backend\'s rule for which file the report is read from', () => {
	it('prefers the ref the primary id names, then the first user-audience ref', () => {
		const refs = [
			{ output_id: 'a', audience: 'user', relative_path: 'outputs/a.md' },
			{ output_id: 'b', audience: 'user', relative_path: 'outputs/b.md' }
		];
		expect(reportSourcePath(refs, 'b')).toBe('outputs/b.md');
		expect(reportSourcePath(refs, null)).toBe('outputs/a.md');
		expect(reportSourcePath(refs, 'missing')).toBe('outputs/a.md');
		expect(reportSourcePath([{ output_id: 'c', audience: 'agent', relative_path: 'x' }], null)).toBe(
			null
		);
	});
});

describe('fetchTaskOutputFiles — deliverables, the other outputs, and the sidecars', () => {
	const jsonResponse = (body: unknown) =>
		new Response(JSON.stringify(body), {
			status: 200,
			headers: { 'Content-Type': 'application/json' }
		});
	const serve = (body: unknown) =>
		installFetchMock([{ match: () => true, handle: () => jsonResponse(body) }]);

	it('reads the new shape: the deliverable mark, the host path and the sidecar files', async () => {
		serve({
			outputs: {
				task_id: 'task_alpha',
				default_task_agent_output_id: null,
				primary_user_output_id: 'out_task_user_1',
				outputs: [
					{
						output_id: 'out_task_user_1',
						audience: 'user',
						role: 'primary_task_user',
						relative_path: 'outputs/x.html',
						media_type: 'text/html',
						deliverable: true,
						absolute_path: '/srv/tasks/task_alpha/outputs/x.html'
					},
					{
						output_id: 'out_task_continuation_1',
						audience: 'agent',
						role: 'continuation_context',
						relative_path: 'outputs/out_task_continuation_1.json',
						media_type: 'application/json',
						deliverable: false,
						absolute_path: '/srv/tasks/task_alpha/outputs/out_task_continuation_1.json'
					}
				],
				internal_files: [
					{
						kind: 'task_summary',
						file_name: 'task_summary.json',
						absolute_path: '/srv/tasks/task_alpha/outputs/task_summary.json'
					}
				]
			}
		});

		const files = (await fetchTaskOutputFiles('task_alpha', 'me', 'default')) ?? [];

		expect(
			files.map((file) => [file.name, file.deliverable !== false, file.absolutePath ?? null])
		).toEqual([
			['x.html', true, '/srv/tasks/task_alpha/outputs/x.html'],
			['out_task_continuation_1.json', false, '/srv/tasks/task_alpha/outputs/out_task_continuation_1.json'],
			['task_summary.json', false, '/srv/tasks/task_alpha/outputs/task_summary.json']
		]);
		expect(files[0].reportSource).toBe(true);
		// The sidecar is served by the same route, so it keeps the same chips.
		expect(files[2].url).toBe(`${URL_BASE}/outputs/task_summary.json`);
	});

	it('derives the mark on the old shape, so the continuation and the legacy agent copy drop out', async () => {
		serve({
			outputs: {
				task_id: 'task_alpha',
				default_task_agent_output_id: 'out_task_agent_1',
				primary_user_output_id: 'out_task_user_1',
				outputs: [
					{
						output_id: 'out_task_user_1',
						audience: 'user',
						role: 'primary_task_user',
						relative_path: 'outputs/out_task_user_1.md',
						media_type: 'text/markdown'
					},
					{
						output_id: 'out_task_agent_1',
						audience: 'agent',
						role: 'primary_task_agent',
						relative_path: 'outputs/out_task_agent_1.md',
						media_type: 'text/markdown'
					},
					{
						output_id: 'out_task_continuation_1',
						audience: 'agent',
						role: 'continuation_context',
						relative_path: 'outputs/out_task_continuation_1.json',
						media_type: 'application/json'
					},
					{
						output_id: 'out_media_1',
						audience: 'user',
						role: 'user_media',
						relative_path: 'outputs/chart.png',
						media_type: 'image/png'
					}
				]
			}
		});

		const files = (await fetchTaskOutputFiles('task_alpha', 'me', 'default')) ?? [];

		// The summary drops behind the file the run delivered, and reads Summary.
		expect(files.filter((file) => file.deliverable !== false).map((file) => file.name)).toEqual([
			'chart.png',
			'out_task_user_1.md'
		]);
		expect(files.find((file) => file.name === 'out_task_user_1.md')?.label).toBe('Summary');
		expect(files.filter((file) => file.deliverable === false).map((file) => file.name)).toEqual([
			'out_task_agent_1.md',
			'out_task_continuation_1.json'
		]);
	});

	it('leads with the primary deliverable, then the other delivered files, then the summary', async () => {
		serve({
			outputs: {
				task_id: 'task_alpha',
				primary_user_output_id: 'out_task_user_task_alpha_exec_1',
				primary_deliverable_output_id: 'deliv_report',
				outputs: [
					{
						output_id: 'out_task_user_task_alpha_exec_1',
						audience: 'user',
						role: 'primary_task_user',
						relative_path: 'outputs/out_task_user_task_alpha_exec_1.md',
						media_type: 'text/markdown',
						deliverable: true
					},
					{
						output_id: 'out_task_continuation_task_alpha_exec_1',
						audience: 'agent',
						role: 'continuation_context',
						relative_path: 'outputs/out_task_continuation_task_alpha_exec_1.json',
						deliverable: false
					},
					{
						output_id: 'deliv_notes',
						audience: 'user',
						role: 'user_attachment',
						relative_path: 'outputs/delivered/exec_1/notes.md',
						media_type: 'text/markdown',
						deliverable: true,
						download_url: '/api/magician/v3/tasks/task_alpha/outputs/delivered/exec_1/notes.md'
					},
					{
						output_id: 'deliv_report',
						audience: 'user',
						role: 'user_attachment',
						relative_path: 'outputs/delivered/exec_1/Q3 report.pdf',
						media_type: 'application/pdf',
						size_bytes: 2048,
						deliverable: true,
						primary: true,
						download_url: '/api/magician/v3/tasks/task_alpha/outputs/delivered/exec_1/Q3%20report.pdf'
					}
				]
			}
		});

		const files = (await fetchTaskOutputFiles('task_alpha', 'me', 'default')) ?? [];

		expect(
			files.map((file) => [file.name, file.label ?? null, file.primary === true, file.deliverable !== false])
		).toEqual([
			['Q3 report.pdf', null, true, true],
			// The continuation keeps its slot: only deliverables move.
			['out_task_continuation_task_alpha_exec_1.json', null, false, false],
			['notes.md', null, false, true],
			['out_task_user_task_alpha_exec_1.md', 'Summary', false, true]
		]);
		expect(files[0].sizeBytes).toBe(2048);
		expect(files[3].reportSource).toBe(true);
	});

	it('takes the primary from the top-level id when a row does not say so', async () => {
		serve({
			outputs: {
				primary_deliverable_output_id: 'b',
				outputs: [
					{ output_id: 'a', audience: 'user', role: 'user_attachment', relative_path: 'outputs/delivered/e/a.csv' },
					{ output_id: 'b', audience: 'user', role: 'user_attachment', relative_path: 'outputs/delivered/e/b.csv' }
				]
			}
		});

		const files = (await fetchTaskOutputFiles('task_alpha', 'me', 'default')) ?? [];
		expect(files.map((file) => [file.name, file.primary === true])).toEqual([
			['b.csv', true],
			['a.csv', false]
		]);
	});
});
