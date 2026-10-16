import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, textResponse } from '../../test/browser';
import {
	buildInternalTaskQuery,
	cancelInternalExecution,
	deleteInternalTask,
	fetchInternalTaskDetails,
	internalTaskOutputDownloadUrl,
	listInternalTasks,
	openInternalTaskOutputFile,
	revealInternalTaskOutputFile,
	retryInternalTaskSynthesis
} from './api';

describe('internal task API', () => {
	it('serializes server pagination, sort, and trimmed filters without scope selectors', () => {
		const params = buildInternalTaskQuery({
			principal: 'alice',
			workspace: 'work',
			limit: 25,
			offset: 50,
			sort: 'updated_at',
			order: 'desc',
			agent: ' analyst ',
			status: ' failed ',
			query: ' task-42 '
		});

		expect(Object.fromEntries(params)).toEqual({
			limit: '25',
			offset: '50',
			sort: 'updated_at',
			order: 'desc',
			agent_id: 'analyst',
			status: 'failed',
			query: 'task-42'
		});
	});

	it('normalizes malformed list collections while retaining pagination defaults', async () => {
		installFetchMock([
			{
				match: '/tasks/internal?',
				handle: () => jsonResponse({ tasks: {}, pagination: { total: 3 } })
			}
		]);

		const result = await listInternalTasks({
			principal: 'alice',
			workspace: 'work',
			limit: 20,
			offset: 40,
			sort: 'created_at',
			order: 'asc'
		});

		expect(result).toEqual({
			tasks: [],
			pagination: { total: 3, limit: 20, offset: 40, has_more: false }
		});
	});

	it('encodes task identifiers for detail lookup', async () => {
		const { calls } = installFetchMock([
			{
				match: '/details',
				handle: () => jsonResponse({ task: { id: 'task/one' }, executions: [] })
			}
		]);

		await fetchInternalTaskDetails('task/one', 'alice', 'work');

		expect(calls[0]?.url).toContain('/tasks/task%2Fone/details');
		expect(calls[0]?.url).not.toContain('principal=');
		expect(calls[0]?.url).not.toContain('workspace=');
	});

	it('recurring history passes an opaque cursor without changing the authenticated scope', async () => {
		const { calls } = installFetchMock([{ match: '/details?', handle: () => jsonResponse({ task: {}, executions: [] }) }]);
		const cursor = JSON.stringify(['2026-09-11T10:00:00Z', 'exec_round_25']);
		await fetchInternalTaskDetails('task-recurring', 'alice', 'work', cursor);
		expect(calls[0]?.url).toContain(`?cursor=${encodeURIComponent(cursor)}`);
		expect(calls[0]?.url).not.toContain('principal=');
	});

	it('uses distinct destructive endpoints for delete and execution cancellation', async () => {
		const { calls } = installFetchMock([
			{ method: 'DELETE', match: '/tasks/internal/', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/executions/', handle: () => jsonResponse({}) }
		]);

		await deleteInternalTask('task-1', 'alice', 'work');
		await cancelInternalExecution('execution-1', 'alice', 'work');

		expect(calls.map((call) => call.method)).toEqual(['DELETE', 'POST']);
		expect(calls[0]?.url).toContain('/tasks/internal/task-1');
		expect(calls[1]?.url).toContain('/executions/execution-1/cancel');
	});

	it('preserves synthesis retry coalescing and accepts an empty success body', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/retry-synthesis',
				handle: () => jsonResponse({ coalesced: true })
			}
		]);
		expect(
			await retryInternalTaskSynthesis('task-1', 'execution-1', 'alice', 'work')
		).toEqual({ coalesced: true });

		installFetchMock([
			{
				method: 'POST',
				match: '/retry-synthesis',
				handle: () => textResponse('')
			}
		]);
		expect(
			await retryInternalTaskSynthesis('task-1', 'execution-1', 'alice', 'work')
		).toBeNull();
	});

	it('opens, reveals, and downloads persisted outputs through bearer-bound routes', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/outputs/open-file', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/outputs/open-folder', handle: () => jsonResponse({}) }
		]);

		await openInternalTaskOutputFile('task/1', 'reports/result.md', 'alice', 'work');
		await revealInternalTaskOutputFile('task/1', 'reports/result.md', 'alice', 'work');

		expect(calls.map((call) => JSON.parse(String(call.init?.body)))).toEqual([
			{ relative_path: 'reports/result.md' },
			{ relative_path: 'reports/result.md' }
		]);
		expect(calls[0]?.url).toContain('/tasks/task%2F1/outputs/open-file');
		expect(
			internalTaskOutputDownloadUrl('task/1', 'reports/result.md', 'alice', 'work')
		).toBe(
			'/api/magician/v3/tasks/task%2F1/outputs/reports/result.md'
		);
	});

	it('surfaces structured backend errors to the page controller', async () => {
		installFetchMock([
			{
				match: '/tasks/internal?',
				handle: () => jsonResponse({ error: 'scope denied' }, { status: 403 })
			}
		]);

		await expect(
			listInternalTasks({
				principal: 'alice',
				workspace: 'work',
				limit: 20,
				offset: 0,
				sort: 'updated_at',
				order: 'desc'
			})
		).rejects.toThrow('scope denied');
	});
});
