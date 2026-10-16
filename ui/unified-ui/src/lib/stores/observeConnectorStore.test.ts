import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	fetchObserveAccounts,
	fetchObserveStatus,
	saveObserveConfig
} from './observeConnectorStore';

beforeEach(() => {
	installFetchMock([{ match: () => true, handle: () => jsonResponse({}) }]);
});

describe('observeConnectorStore', () => {
	it('loads connected email and calendar accounts from the shared account endpoint', async () => {
		installFetchMock([
			{
				match: '/observe/accounts',
				handle: () =>
					jsonResponse({
						email_accounts: [
							{
								name: 'work',
								email: 'owner@example.com',
								account_type: 'gmail',
								connected: true,
								lane: 'user_assist'
							}
						],
						calendar_accounts: []
					})
			}
		]);

		const accounts = await fetchObserveAccounts();
		expect(accounts.email_accounts).toHaveLength(1);
		expect(accounts.email_accounts[0]).toMatchObject({ name: 'work', connected: true });
	});

	it('degrades malformed or failed account discovery to an empty safe state', async () => {
		installFetchMock([
			{ match: '/observe/accounts', handle: () => jsonResponse({ email_accounts: {} }) }
		]);
		expect(await fetchObserveAccounts()).toEqual({
			email_accounts: [],
			calendar_accounts: []
		});

		installFetchMock([
			{ match: '/observe/accounts', handle: () => jsonResponse({}, { status: 503 }) }
		]);
		expect(await fetchObserveAccounts()).toEqual({
			email_accounts: [],
			calendar_accounts: []
		});
	});

	it('loads producer-specific persisted consent', async () => {
		installFetchMock([
			{
				match: '/observe/calendar/status',
				handle: () =>
					jsonResponse({
						enabled: true,
						accounts: ['work'],
						frequency: 'hourly',
						time: '09:00',
						suppress_sensitive: true,
						total_synced: 12,
						last_sync_at: '2026-07-11T10:00:00Z',
						schedule_task_id: 'task-observe'
					})
			}
		]);

		expect(await fetchObserveStatus('calendar')).toMatchObject({
			enabled: true,
			accounts: ['work'],
			total_synced: 12
		});
	});

	it('saves the complete consent payload rather than a partial toggle', async () => {
		const { calls } = installFetchMock([
			{
				method: 'PUT',
				match: '/observe/email/config',
				handle: () =>
					jsonResponse({
						enabled: true,
						accounts: ['work', 'personal'],
						frequency: 'twice-daily',
						time: '08:30',
						suppress_sensitive: true,
						total_synced: 0,
						last_sync_at: null,
						schedule_task_id: 'task-observe'
					})
			}
		]);

		const update = {
			enabled: true,
			accounts: ['work', 'personal'],
			frequency: 'twice-daily',
			time: '08:30',
			suppress_sensitive: true
		};
		const result = await saveObserveConfig('email', update);

		expect(result.ok).toBe(true);
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual(update);
	});

	it('returns backend validation errors without pretending consent was saved', async () => {
		installFetchMock([
			{
				method: 'PUT',
				match: '/observe/email/config',
				handle: () => jsonResponse({ error: 'select at least one account' }, { status: 422 })
			}
		]);

		expect(
			await saveObserveConfig('email', {
				enabled: true,
				accounts: [],
				frequency: 'daily',
				time: '09:00',
				suppress_sensitive: true
			})
		).toEqual({ ok: false, error: 'select at least one account' });
	});
});
