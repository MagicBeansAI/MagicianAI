import { afterEach, describe, expect, it, vi } from 'vitest';

import type { HitlRequest, HitlResolveOutcome } from '$lib/hitl/types';
import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	activateAttentionRow,
	chooseAttentionRowOption,
	createAttentionItemController,
	type AttentionScopeSnapshot
} from './controller';
import { attentionQuickChoices, type AttentionDisplayRow } from './model';

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

describe('chooseAttentionRowOption', () => {
	afterEach(() => vi.unstubAllGlobals());

	// A delegated run's `need_user_input(input_type="choice")` pause.
	function choiceRow(): AttentionDisplayRow {
		return row({
			request: request({
				id: 'pause-1',
				input_type: 'choice',
				prompt: 'The notes are drafted. What should I do?',
				schema: {
					options: [
						{ id: 'approve_notes', label: 'Approve notes' },
						{ id: 'approve_all', label: 'Approve everything' },
						{ id: 'change', label: 'Change…' }
					]
				},
				identifiers: { pause_state_id: 'pause-1', correlation_id: 'pause-1' }
			}),
			correlation_id: 'pause-1',
			alias_ids: []
		});
	}

	it('offers the options, with Change… opening the prompt', () => {
		expect(attentionQuickChoices(choiceRow())).toEqual([
			{ id: 'approve_notes', label: 'Approve notes', opensPrompt: false },
			{ id: 'approve_all', label: 'Approve everything', opensPrompt: false },
			{ id: 'change', label: 'Change…', opensPrompt: true }
		]);
		expect(attentionQuickChoices(row())).toEqual([]);
	});

	it('offers no one-click answer when the ask carries details the row cannot show', () => {
		const detailed = { ...choiceRow(), hint: '## Goal tree\n- Raise a round' };
		expect(attentionQuickChoices(detailed)).toEqual([]);
		const oneLine = { ...choiceRow(), hint: 'Notes go under Workspaces/Magican/Goals' };
		expect(attentionQuickChoices(oneLine)).toHaveLength(3);
	});

	it('posts the option to /hitl/{cid}/respond without the prompt and drops the row', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: /\/hitl\/[^/]+\/respond$/,
				handle: () => jsonResponse({ resumed: true })
			}
		]);
		const prompt = vi.fn();
		const dropAttention = vi.fn();
		const result = await chooseAttentionRowOption(choiceRow(), 'approve_all', {
			getScope: () => scope,
			respond: prompt,
			dropPending: vi.fn(),
			dropAttention
		});

		expect(result.status).toBe('resolved');
		expect(prompt).not.toHaveBeenCalled();
		expect(calls).toHaveLength(1);
		expect(calls[0].url).toBe('/api/magician/v2/hitl/pause-1/respond');
		expect(JSON.parse(String(calls[0].init?.body))).toEqual({
			source: 'agentic',
			input_type: 'choice',
			value: { type: 'choice', selected_id: 'approve_all' },
			channel: 'web',
			execution_id: 'execution-1'
		});
		expect(dropAttention).toHaveBeenCalledWith('pause-1');
	});

	it('offers no buttons for an escalation-source pause', () => {
		// max-iterations / cannot-proceed / loop-detected: budget and their own endpoint.
		const escalation = choiceRow();
		escalation.request = { ...escalation.request!, source: 'escalation' };
		expect(attentionQuickChoices({ ...escalation, source: 'escalation' })).toEqual([]);
	});

	it('offers a repeated option id once', () => {
		const duplicated = choiceRow();
		duplicated.request = {
			...duplicated.request!,
			schema: {
				options: [
					{ id: 'approve_notes', label: 'Approve notes' },
					{ id: 'approve_notes', label: 'Approve notes again' }
				]
			}
		};
		expect(attentionQuickChoices(duplicated).map((choice) => choice.id)).toEqual(['approve_notes']);
	});

	it('offers no buttons for a confirmation outside the allow-list', () => {
		const outside = [
			// An agentic tool-action approval: no destructive flag at all.
			{ action_type: 'shell' },
			// The producer's explicit destructive: false does not excuse an action.
			{ destructive: false, action_type: 'pack' },
			// A protected app's ask.
			{ destructive: false, protected: true },
			// Not flagged: unknown, so not plain.
			{}
		];
		for (const schema of outside) {
			const confirmation = row({ request: request({ input_type: 'confirmation', schema }) });
			expect(attentionQuickChoices(confirmation)).toEqual([]);
		}
	});

	it('posts confirm as a confirmation for a plain confirmation row', async () => {
		const postResponse = vi.fn().mockResolvedValue({ ok: true });
		const confirmation = row({
			request: request({ input_type: 'confirmation', schema: { destructive: false } })
		});
		expect(attentionQuickChoices(confirmation)).toEqual([
			{ id: 'confirm', label: 'Approve', opensPrompt: false },
			{ id: 'deny', label: 'Deny', opensPrompt: false }
		]);

		await chooseAttentionRowOption(confirmation, 'confirm', {
			getScope: () => scope,
			postResponse,
			dropPending: vi.fn(),
			dropAttention: vi.fn()
		});
		expect(postResponse).toHaveBeenCalledWith(
			confirmation.request,
			{ type: 'confirmation', confirmed: true },
			expect.anything()
		);
	});

	it('keeps the row and reports the failure', async () => {
		const dropAttention = vi.fn();
		const controller = createAttentionItemController({
			getScope: () => scope,
			postResponse: vi.fn().mockResolvedValue({ ok: false, status: 500, message: 'resume failed' }),
			dropPending: vi.fn(),
			dropAttention
		});
		let latest = null as ReturnType<typeof stateSnapshot> | null;
		const unsubscribe = controller.state.subscribe((state) => {
			latest = stateSnapshot(state);
		});

		const result = await controller.choose(choiceRow(), 'approve_notes');

		expect(result.status).toBe('error');
		expect(latest?.error).toBe('resume failed');
		expect(latest?.choosingKey).toBeNull();
		expect(dropAttention).not.toHaveBeenCalled();
		unsubscribe();
	});
});

function stateSnapshot(state: {
	notice: string | null;
	error: string | null;
	skillEvolutionActionKey: string | null;
	choosingKey?: string | null;
}) {
	return { ...state };
}
