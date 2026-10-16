import { beforeEach, describe, expect, it, vi } from 'vitest';

const requestAttentionInput = vi.fn();
vi.mock('$lib/stores/attentionPromptStore', async (importOriginal) => {
	const original = await importOriginal<typeof import('$lib/stores/attentionPromptStore')>();
	return {
		...original,
		requestAttentionInput: (request: unknown) => requestAttentionInput(request)
	};
});

const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit) => timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

import {
	CLARIFICATION_RESPONDER_REQUEST_PREFIX,
	buildPersistedChatHitlTarget,
	chatHitlContinuationIsCurrent,
	loadSeedSessionOrActive
} from './ChatPanel.svelte';
import { hitlRequestFromOpenTarget } from '$lib/attention/openHitlPrompt';
import { resolveHitlCall } from '$lib/hitl/adapters';
import { respondToHitl } from '$lib/hitl/respondToHitl';

function successfulResponse(): Response {
	return new Response(JSON.stringify({ accepted: true }), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});
}

function persistedTarget(overrides: Record<string, unknown> = {}) {
	return buildPersistedChatHitlTarget({
		executionId: 'exec-1',
		pauseStateId: 'question-1',
		escalationType: 'clarification',
		requestId: `${CLARIFICATION_RESPONDER_REQUEST_PREFIX}task-1`,
		inputType: 'text',
		question: 'Which account?',
		options: [{ id: 'respond', label: 'Respond', requires_input: true }],
		principal: 'owner',
		workspace: 'default',
		...overrides
	});
}

describe('ChatPanel persisted HITL migration', () => {
	beforeEach(() => {
		requestAttentionInput.mockReset();
		timedFetch.mockReset();
		timedFetch.mockResolvedValue(successfulResponse());
	});

	it('rebuilds a V3 clarification with task responder and durable plan execution after reload', () => {
		const target = persistedTarget({ executionId: 'planexec-7' });
		expect(target).not.toBeNull();
		const request = target && hitlRequestFromOpenTarget(target);

		expect(request?.scope).toEqual(
			expect.objectContaining({
				workflow_id: 'task-1',
				task_id: 'task-1',
				execution_id: 'planexec-7'
			})
		);
		const call = request && resolveHitlCall(request, { type: 'text', value: 'billing' });
		expect(call?.body).toEqual(
			expect.objectContaining({
				source: 'clarification',
				task_id: 'task-1',
				execution_id: 'planexec-7'
			})
		);
	});

	it('keeps a legacy AskLoop workflow as both responder and execution identity', () => {
		const target = persistedTarget({
			executionId: 'workflow-legacy',
			requestId: `${CLARIFICATION_RESPONDER_REQUEST_PREFIX}workflow-legacy`
		});
		const request = target && hitlRequestFromOpenTarget(target);

		expect(request?.scope).toEqual(
			expect.objectContaining({
				workflow_id: 'workflow-legacy',
				execution_id: 'workflow-legacy'
			})
		);
		const call = request && resolveHitlCall(request, { type: 'text', value: 'legacy answer' });
		expect(call?.body).toEqual(
			expect.objectContaining({
				task_id: 'workflow-legacy',
				execution_id: 'workflow-legacy'
			})
		);
	});

	it('sends canonical selected_ids for persisted multi-choice cards', async () => {
		const target = persistedTarget({
			inputType: 'multi_choice',
			options: [
				{ id: 'staging', label: 'Staging' },
				{ id: 'prod', label: 'Production' }
			]
		});
		const request = target && hitlRequestFromOpenTarget(target);
		expect(request).not.toBeNull();
		requestAttentionInput.mockResolvedValue({
			kind: 'multi_choice',
			choiceIds: ['staging', 'prod']
		});

		await respondToHitl(request!);

		const body = JSON.parse(String(timedFetch.mock.calls[0]?.[1]?.body));
		expect(body.value).toEqual({
			type: 'multi_choice',
			selected_ids: ['staging', 'prod']
		});
	});

	it('sends canonical paths for persisted file-path cards', async () => {
		const target = persistedTarget({ inputType: 'file_path', question: 'Which files?' });
		const request = target && hitlRequestFromOpenTarget(target);
		expect(request).not.toBeNull();
		requestAttentionInput.mockResolvedValue({
			kind: 'text',
			value: 'src/a.ts, src/b.ts'
		});

		await respondToHitl(request!);

		const body = JSON.parse(String(timedFetch.mock.calls[0]?.[1]?.body));
		expect(body.value).toEqual({
			type: 'file_path',
			paths: ['src/a.ts', 'src/b.ts']
		});
	});

	it('invalidates continuations when the principal/workspace scope changes', () => {
		expect(chatHitlContinuationIsCurrent('owner:default', 'owner:default', 4, 4)).toBe(true);
		expect(chatHitlContinuationIsCurrent('owner:default', 'other:default', 4, 4)).toBe(false);
		expect(chatHitlContinuationIsCurrent('owner:default', 'owner:default', 4, 5)).toBe(false);
	});

	it('opens the exact session that owns a server-staged screen attachment', async () => {
		const openSession = vi.fn().mockResolvedValue({ ui_thread_id: 'screens' });
		const loadActiveSession = vi.fn().mockResolvedValue({ ui_thread_id: 'screens' });

		const selected = await loadSeedSessionOrActive('screens', 'capture-session', {
			openSession,
			loadActiveSession
		});

		expect(selected).toEqual({ ui_thread_id: 'screens' });
		expect(openSession).toHaveBeenCalledWith('capture-session');
		expect(loadActiveSession).not.toHaveBeenCalled();
	});

	it('falls back to the active thread session when a staged seed is stale', async () => {
		const openSession = vi.fn().mockResolvedValue({ ui_thread_id: 'general' });
		const loadActiveSession = vi.fn().mockResolvedValue({ ui_thread_id: 'screens' });

		await loadSeedSessionOrActive('screens', 'stale-session', {
			openSession,
			loadActiveSession
		});

		expect(loadActiveSession).toHaveBeenCalledWith('screens');
	});
});
