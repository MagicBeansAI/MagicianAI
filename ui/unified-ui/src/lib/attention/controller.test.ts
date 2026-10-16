import { describe, expect, it, vi } from 'vitest';

import type { HitlRequest, HitlResolveOutcome } from '$lib/hitl/types';
import {
	activateAttentionRow,
	createAttentionItemController,
	type AttentionScopeSnapshot
} from './controller';
import type { AttentionDisplayRow } from './model';

function request(overrides: Partial<HitlRequest> = {}): HitlRequest {
	return {
		id: 'pause-1',
		source: 'agentic',
		input_type: 'text',
		schema: {},
		prompt: 'Original question',
		scope: { execution_id: 'execution-1' },
		identifiers: {
			pause_state_id: 'pause-1',
			approval_id: 'approval-1',
			correlation_id: 'correlation-1',
			request_id: 'request-1'
		},
		...overrides
	};
}

function row(overrides: Partial<AttentionDisplayRow> = {}): AttentionDisplayRow {
	const hitl = request();
	return {
		key: 'pause-1',
		source: hitl.source,
		prompt: hitl.prompt,
		scope: hitl.scope,
		at: 1,
		correlation_id: 'correlation-1',
		alias_ids: ['approval-1', 'request-1'],
		origin: 'bus',
		request: hitl,
		pause_state_id: 'pause-1',
		feed_item_id: 'v3:attention:item-1',
		...overrides
	};
}

const scope: AttentionScopeSnapshot = {
	principal: 'anonymous',
	workspace: 'default',
	key: 'anonymous:default'
};

describe('activateAttentionRow', () => {
	it('drops every alias from both pending stores after canonical success', async () => {
		const dropPending = vi.fn();
		const dropAttention = vi.fn();
		const result = await activateAttentionRow(row(), {
			getScope: () => scope,
			respond: vi.fn().mockResolvedValue({ ok: true }),
			dropPending,
			dropAttention
		});

		expect(result.status).toBe('resolved');
		const aliases = [
			'pause-1',
			'correlation-1',
			'approval-1',
			'request-1',
			'v3:attention:item-1'
		];
		for (const alias of aliases) {
			expect(dropPending).toHaveBeenCalledWith(alias);
			expect(dropAttention).toHaveBeenCalledWith(alias);
		}
	});

	it('does not dismiss or drop a row when the singleton prompt is cancelled', async () => {
		const dismissFailed = vi.fn();
		const dropPending = vi.fn();
		const result = await activateAttentionRow(row(), {
			getScope: () => scope,
			respond: vi.fn().mockResolvedValue({ ok: false, cancelled: true }),
			dismissFailed,
			dropPending,
			dropAttention: vi.fn()
		});

		expect(result.status).toBe('cancelled');
		expect(dismissFailed).not.toHaveBeenCalled();
		expect(dropPending).not.toHaveBeenCalled();
	});

	it('reopens a reask_required response with the clarified prompt', async () => {
		const outcomes: HitlResolveOutcome[] = [
			{
				ok: false,
				reask: true,
				status: 200,
				message: 'Revise',
				question: 'Clarified question',
				hint: 'More detail',
				previousAnswer: 'First answer'
			},
			{ ok: true }
		];
		const respond = vi.fn().mockImplementation(async () => outcomes.shift()!);

		const result = await activateAttentionRow(row(), {
			getScope: () => scope,
			respond,
			dropPending: vi.fn(),
			dropAttention: vi.fn()
		});

		expect(result.status).toBe('resolved');
		expect(respond).toHaveBeenCalledTimes(2);
		expect((respond.mock.calls[1][0] as HitlRequest).prompt).toBe('Clarified question');
		expect((respond.mock.calls[1][0] as HitlRequest).hint).toBe('More detail');
		expect(respond.mock.calls[1][2]).toEqual({ defaultValue: 'First answer' });
	});

	it.each([
		['text', { multiline: true }],
		['guidance', {}],
		['file_path', { multiple: true }]
	] as const)('preserves a previous %s answer as the reask default', async (inputType, schema) => {
		const respond = vi
			.fn()
			.mockResolvedValueOnce({
				ok: false,
				reask: true,
				status: 200,
				message: 'Revise',
				previousAnswer: 'Previous answer'
			})
			.mockResolvedValueOnce({ ok: true });

		await activateAttentionRow(
			row({ request: request({ input_type: inputType, schema }) }),
			{
				getScope: () => scope,
				respond,
				dropPending: vi.fn(),
				dropAttention: vi.fn()
			}
		);

		expect(respond.mock.calls[1][2]).toEqual({ defaultValue: 'Previous answer' });
	});

	it('never carries a previous password into the reask prompt', async () => {
		const respond = vi
			.fn()
			.mockResolvedValueOnce({
				ok: false,
				reask: true,
				status: 200,
				message: 'Revise',
				previousAnswer: 'secret'
			})
			.mockResolvedValueOnce({ ok: false, cancelled: true });

		await activateAttentionRow(row({ request: request({ input_type: 'password' }) }), {
			getScope: () => scope,
			respond
		});

		expect(respond.mock.calls[1][2]).toBeUndefined();
	});

	it('ignores a hydration response after the active scope changes', async () => {
		let activeScope = scope;
		let resolveHydration!: (value: HitlRequest | null) => void;
		const hydration = new Promise<HitlRequest | null>((resolve) => {
			resolveHydration = resolve;
		});
		const respond = vi.fn();
		const activation = activateAttentionRow(
			row({ request: null, execution_id: 'execution-1' }),
			{
				getScope: () => activeScope,
				hydrateRequest: () => hydration,
				respond
			}
		);
		activeScope = { principal: 'other', workspace: 'next', key: 'other:next' };
		resolveHydration(request());

		expect((await activation).status).toBe('stale');
		expect(respond).not.toHaveBeenCalled();
	});

	it('passes the raw FeedItem.id to durable failed dismissal', async () => {
		const dismissFailed = vi.fn();
		const result = await activateAttentionRow(row({ failed: true }), {
			getScope: () => scope,
			dismissFailed
		});

		expect(result.status).toBe('dismissed');
		expect(dismissFailed).toHaveBeenCalledWith('pause-1', 'v3:attention:item-1');
	});
});

describe('createAttentionItemController scope guards', () => {
	it('does not apply a special-action result after scope changes', async () => {
		let activeScope = scope;
		let resolveFetch!: (response: Response) => void;
		const pendingFetch = new Promise<Response>((resolve) => {
			resolveFetch = resolve;
		});
		const refreshAttention = vi.fn().mockResolvedValue(undefined);
		const controller = createAttentionItemController({
			getScope: () => activeScope,
			fetch: vi.fn().mockReturnValue(pendingFetch),
			refreshAttention
		});
		let latest = null as ReturnType<typeof stateSnapshot> | null;
		const unsubscribe = controller.state.subscribe((state) => {
			latest = stateSnapshot(state);
		});
		const action = controller.runSkillEvolutionGate(
			row({
				skillEvolution: {
					gate: 'proposal_review',
					action: 'approve',
					actionEnabled: true,
					candidateId: 'candidate-1'
				}
			})
		);
		activeScope = { principal: 'other', workspace: 'next', key: 'other:next' };
		resolveFetch(new Response('{}', { status: 200 }));
		await action;

		expect(latest?.notice).toBeNull();
		expect(latest?.error).toBeNull();
		expect(refreshAttention).not.toHaveBeenCalled();
		unsubscribe();
	});
});

function stateSnapshot(state: {
	notice: string | null;
	error: string | null;
	skillEvolutionActionKey: string | null;
}) {
	return { ...state };
}
