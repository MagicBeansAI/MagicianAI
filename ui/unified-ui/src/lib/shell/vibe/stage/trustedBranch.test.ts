import { beforeEach, describe, expect, it, vi } from 'vitest';

const timedFetch = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/fetch', () => ({ timedFetch }));

import {
	TrustedBranchError,
	defaultPullRequestBody,
	defaultPullRequestTitle,
	fetchTrustedBranch,
	formatBytes,
	mergeTrustedBranch,
	rewindTrustedBranch,
	type TrustedBranchSummary
} from './trustedBranch';
import { chainRootTaskId } from '$lib/stores/vibeHitlStore';
import type { Task } from '$lib/stores/taskStore';

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

const summary: TrustedBranchSummary = {
	root_task_id: 'task_root',
	branch: 'vibedev/app-1234abcd',
	base_branch: 'main',
	base_sha: 'aaaaaaaaaa',
	head_sha: 'bbbbbbbbbb',
	commits: [
		{ sha: 'bbbbbbbbbb', subject: 'vibedev: end of turn (task_b)', author: 'Owner', committed_at: 2 },
		{ sha: 'cccccccccc', subject: 'Add dark mode toggle', author: 'Owner', committed_at: 1 }
	],
	files: [],
	behind_base: 0,
	dirty: false,
	merged: null,
	pull_request_url: null
};

describe('trusted branch client', () => {
	beforeEach(() => timedFetch.mockReset());

	it('reads a chain that is not trusted as null, not an error', async () => {
		timedFetch.mockResolvedValue(json({ error: 'vibedev_trusted_run_not_found' }, 404));
		await expect(fetchTrustedBranch('task_x')).resolves.toBeNull();
		expect(timedFetch.mock.calls[0][0]).toBe('/api/magician/v2/vibedev/runs/task_x/branch');
	});

	it("surfaces the server's refusal message", async () => {
		timedFetch.mockResolvedValue(
			json({ error: 'vibedev_trusted_action_refused', message: '`vibedev/x` conflicts with `main`' }, 400)
		);
		const error = await mergeTrustedBranch('task_root').catch((e) => e);
		expect(error).toBeInstanceOf(TrustedBranchError);
		expect(error.message).toContain('conflicts with `main`');
		expect(error.status).toBe(400);
	});

	it('posts the rewind target', async () => {
		timedFetch.mockResolvedValue(json({ head_sha: 'cccccccccc' }));
		await rewindTrustedBranch('task_root', 'cccccccccc');
		const [url, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/magician/v2/vibedev/runs/task_root/rewind');
		expect(JSON.parse(String(init.body))).toEqual({ commit: 'cccccccccc' });
	});

	it('titles a PR after the first real commit and lists commits oldest first', () => {
		expect(defaultPullRequestTitle(summary)).toBe('Add dark mode toggle');
		expect(defaultPullRequestBody(summary)).toBe(
			'Commits:\n\n- Add dark mode toggle\n- vibedev: end of turn (task_b)\n'
		);
		expect(defaultPullRequestTitle({ ...summary, commits: [] })).toBe('app-1234abcd');
	});
});

describe('chainRootTaskId', () => {
	const task = (id: string, parent?: string) =>
		({ id, description: parent ? `Parent task: ${parent}\n` : 'root' }) as unknown as Task;

	it('walks Parent task links to the root', () => {
		const tasks = [task('a'), task('b', 'a'), task('c', 'b')];
		expect(chainRootTaskId(tasks[2], tasks)).toBe('a');
		expect(chainRootTaskId(tasks[0], tasks)).toBe('a');
		expect(chainRootTaskId(null, tasks)).toBeNull();
	});

	it('stops at a parent it has not loaded', () => {
		expect(chainRootTaskId(task('c', 'missing'), [])).toBe('missing');
	});
});

describe('formatBytes', () => {
	it('uses decimal units with one decimal under ten', () => {
		expect(formatBytes(512)).toBe('512 B');
		expect(formatBytes(1_234_000_000)).toBe('1.2 GB');
		expect(formatBytes(31_000_000_000)).toBe('31 GB');
		expect(formatBytes(null)).toBe('—');
	});
});
