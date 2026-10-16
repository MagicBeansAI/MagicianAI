/**
 * The cockpit's submit path, after convergence.
 *
 * `submitCodingRun` used to assemble the whole coding-task description in the
 * browser and make four calls (create → pin the project pointer → execute →
 * roll all three back by hand). It now makes ONE, to
 * `POST /api/magician/v2/vibedev/runs`, which runs the server's
 * `VibeDevRunService::start_build` — the same entry the `@vibedev` chat rail
 * uses. Handoff plan §10: *"the cockpit and the facade both run through
 * `VibeDevRunService::start_build`; no second creation path exists."*
 *
 * So what is worth asserting here is no longer prose — the server owns that,
 * and `the_cockpit_build_description_is_byte_identical_to_the_client_assembler`
 * pins it. It is the **request**: every studio preference that used to become a
 * line of that prose has to arrive as the input behind it, or the run silently
 * loses a setting the user chose. That is the failure this file exists to catch.
 *
 * `timedFetch` is mocked the way `$lib/hitl/respondToHitl.test.ts` mocks it, so
 * the request body is inspectable without a server.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit & { timeoutMs?: number }) =>
		timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

vi.mock('$lib/stores/scopeIdentityStore', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/stores/scopeIdentityStore')>();
	actual.scopeIdentityStore.observe('user', 'ws');
	return actual;
});

import { vibeStudioStore } from '$lib/stores/vibeStudioStore';
import type { Task } from '$lib/stores/taskStore';
import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
import {
	bareTitle,
	stripRunTitlePrefix,
	submitCodingRun,
	taskAcceptsFollowUp,
	type SubmitContext
} from './submit';

function project(): VibeDevProject {
	return {
		project_id: 'proj-1',
		name: 'Landing page',
		chat_thread_id: 'vibedev',
		chat_session_id: 'vibedev-session-1',
		repo_path: 'apps/site',
		run_task_ids: [],
		created_at_ms: 10,
		updated_at_ms: 20,
		archived: false
	} as unknown as VibeDevProject;
}

function context(overrides: Partial<SubmitContext> = {}): SubmitContext {
	return {
		project: project(),
		parentTask: null,
		profile: { id: 'coding-balanced', label: 'Balanced' },
		stagedAttachments: [],
		sessionId: 'vibedev-session-1',
		...overrides
	};
}

function ok(body: Record<string, unknown>): Response {
	return {
		ok: true,
		status: 201,
		json: async () => body
	} as unknown as Response;
}

/** The parsed body of the single request `submitCodingRun` made. */
function sentBody(): Record<string, unknown> {
	expect(timedFetch).toHaveBeenCalledTimes(1);
	const [, init] = timedFetch.mock.calls[0] as [string, RequestInit];
	return JSON.parse(String(init.body)) as Record<string, unknown>;
}

beforeEach(() => {
	timedFetch.mockReset();
	vibeStudioStore.resetView();
});

describe('submitCodingRun', () => {
	it('starts a run in ONE request, and returns what the server said about it', async () => {
		timedFetch.mockResolvedValue(
			ok({ task_id: 'task_abc', execution_id: 'exec_1', is_follow_up: false, scheduled: false })
		);

		const result = await submitCodingRun('fix the footer spacing', context());

		expect(result).toEqual({ taskId: 'task_abc', isFollowUp: false, scheduled: false });
		const [url, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toContain('/api/magician/v2/vibedev/runs');
		expect(init.method).toBe('POST');
		// Scope is exclusively a bearer claim, never a URL selector.
		expect(url).not.toContain('principal=');
		expect(url).not.toContain('workspace=');
	});

	it('sends every studio preference as an INPUT, never as prose', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_abc' }));
		vibeStudioStore.setAutoApply(true);
		vibeStudioStore.setCostBudget(2.5);
		vibeStudioStore.setVisualSelfCorrect(false);

		await submitCodingRun('fix the footer spacing', context({ projectIsVisual: false }));

		const body = sentBody();
		expect(body.mode).toBe('build');
		expect(body.auto_apply).toBe(true);
		expect(body.cost_budget_usd).toBe(2.5);
		expect(body.visual_self_correct).toBe(false);
		expect(body.project_is_visual).toBe(false);
		expect(body.coding_profile_id).toBe('coding-balanced');
		expect(body.coding_choice).toEqual({ kind: 'profile', profile_id: 'coding-balanced' });
		// The whole point: nothing that looks like an assembled description.
		expect(JSON.stringify(body)).not.toContain('run_coding_task repo_path:');
		expect(JSON.stringify(body)).not.toContain('VIBEDEV_USER_PROMPT');
	});

	it('sends Auto as a kind and omits a fake profile id', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_auto' }));

		await submitCodingRun('fix the footer spacing', context({ profile: { id: 'auto', label: 'Auto' } }));

		const body = sentBody();
		expect(body.coding_choice).toEqual({ kind: 'auto' });
		expect(body.coding_profile_id).toBeUndefined();
	});

	it('carries the parent, the threading gesture and the @task chips', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_child', is_follow_up: true }));
		const parentTask = { id: 'task_parent', status: 'completed' } as unknown as Task;

		const result = await submitCodingRun(
			'and the header',
			context({ parentTask, threaded: true, referenceTaskIds: ['task_chip'] })
		);

		const body = sentBody();
		expect(body.parent_task_id).toBe('task_parent');
		expect(body.threaded).toBe(true);
		// Only the chips: the SERVER adds the parent continuation reference,
		// because only it knows whether the parent is a clean completed run.
		expect(body.reference_task_ids).toEqual(['task_chip']);
		expect(result.isFollowUp).toBe(true);
	});

	it('does not thread a run with no parent, whatever the caller asked for', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_abc' }));

		await submitCodingRun('fix the footer', context({ threaded: true }));

		expect(sentBody().threaded).toBe(false);
	});

	it('mints a fresh submission id per submit, so two asks are two runs', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_abc' }));
		await submitCodingRun('fix the footer', context());
		const first = sentBody().client_run_id;
		timedFetch.mockReset();
		timedFetch.mockResolvedValue(ok({ task_id: 'task_def' }));
		await submitCodingRun('fix the footer', context());
		const second = sentBody().client_run_id;

		expect(typeof first).toBe('string');
		expect(first).not.toEqual(second);
	});

	it('sends the attachment manifest the coding tool reads back', async () => {
		timedFetch.mockResolvedValue(ok({ task_id: 'task_abc' }));

		await submitCodingRun(
			'match the mock',
			context({
				stagedAttachments: [
					{
						attachment_id: 'att-1',
						filename: 'hero.png',
						label: 'Hero mock',
						mime_type: 'image/png',
						size: 2048
					} as unknown as SubmitContext['stagedAttachments'][number]
				]
			})
		);

		expect(sentBody().attachments).toEqual([
			{
				attachment_id: 'att-1',
				filename: 'hero.png',
				label: 'Hero mock',
				mime_type: 'image/png',
				size: 2048
			}
		]);
		expect(sentBody().attachment_session_id).toBe('vibedev-session-1');
	});

	it('reports the SERVER’s message when a run cannot start', async () => {
		timedFetch.mockResolvedValue({
			ok: false,
			status: 500,
			json: async () => ({ error: 'start_vibedev_run_failed', message: 'the executor is down' })
		} as unknown as Response);

		await expect(submitCodingRun('fix the footer', context())).rejects.toThrow(
			'the executor is down'
		);
	});

	it('refuses before the request when there is no project', async () => {
		await expect(
			submitCodingRun('fix the footer', context({ project: null as unknown as VibeDevProject }))
		).rejects.toThrow('Could not prepare a VibeDev project');
		expect(timedFetch).not.toHaveBeenCalled();
	});
});

describe('the title helpers the cockpit still owns', () => {
	it('mirrors the server run title it has to strip back off', () => {
		expect(bareTitle('  fix the   footer\nand the header  ')).toBe('fix the footer');
		expect(stripRunTitlePrefix('VibeDev · fix the footer')).toBe('fix the footer');
		expect(stripRunTitlePrefix('VibeDev follow-up · fix the footer')).toBe('fix the footer');
	});

	it('only offers a follow-up on a settled run', () => {
		const run = (over: Partial<Task>) => ({ status: 'completed', ...over }) as Task;
		expect(taskAcceptsFollowUp(run({}))).toBe(true);
		expect(taskAcceptsFollowUp(run({ synthesisPending: true }))).toBe(false);
		expect(taskAcceptsFollowUp(run({ status: 'running' }))).toBe(false);
	});
});
