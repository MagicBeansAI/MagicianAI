/**
 * Posting the answer, and what happens when the backend does not accept it.
 *
 * The property under test throughout is **at most one post in an exchange is not
 * preceded by a fresh human answer** — the inline value the reader typed in the
 * act. Every re-ask after that goes back through the prompt, which is what
 * bounded the retired panel's loop and what bounds this one; the number the old
 * code carried (`reaskGuard < 5`) bounded nagging rather than spinning, and the
 * thing it was written against — a loop advancing unattended — is structurally
 * impossible here.
 *
 * The two collaborators are injected rather than mocked at the module boundary,
 * so the assertions are about which one was called and with what, which is the
 * whole content of this function.
 */
import { describe, expect, it, vi } from 'vitest';

import type {
	HitlOpenTarget,
	HitlRequest,
	HitlResolveOutcome,
	HitlResponseValue
} from '$lib/hitl/types';
import type { RespondToHitlOptions } from '$lib/hitl/respondToHitl';

import { answerTaskAsk } from './taskAsk';

const SCOPE = { principal: 'anonymous', workspace: 'default' };

/**
 * A clarification, which is the ask B5 is named for. Its identifiers satisfy
 * `openHitlPrompt`'s contract check — the correlation id is the target id and
 * the scope names a workflow — because a target that fails it never reaches
 * either collaborator, and this file is about what happens when it does.
 */
function clarification(overrides: Partial<HitlOpenTarget> = {}): HitlOpenTarget {
	return {
		id: 'sq_7c11',
		source: 'clarification',
		input_type: 'text',
		prompt: 'Which quarter should I report on?',
		identifiers: { correlation_id: 'sq_7c11' },
		scope: {
			principal: 'anonymous',
			workspace: 'default',
			workflow_id: 'task_alpha',
			task_id: 'task_alpha'
		},
		...overrides
	};
}

const reask: HitlResolveOutcome = {
	ok: false,
	reask: true,
	status: 200,
	message: 'That quarter is not in the export.',
	question: 'Which of Q1 or Q2?',
	previousAnswer: 'Q3'
};

/**
 * The two collaborators, typed by the signatures they stand in for. Untyped
 * `vi.fn()` gives an empty argument tuple, so `mock.calls[0][2]` reads as "no
 * such index" — and an assertion that cannot be written is an assertion nobody
 * writes.
 */
function postSpy(outcome: () => Promise<HitlResolveOutcome>) {
	return vi.fn(
		async (
			_request: HitlRequest,
			_value: HitlResponseValue,
			_headers?: HeadersInit,
			_options?: { selectedPaths?: string[] }
		) => await outcome()
	);
}

function promptSpy(outcome: () => Promise<HitlResolveOutcome>) {
	return vi.fn(
		async (
			_request: HitlRequest,
			_headers?: HeadersInit,
			_options?: RespondToHitlOptions
		) => await outcome()
	);
}

function deps(
	post: ReturnType<typeof postSpy>,
	prompt: ReturnType<typeof promptSpy>
) {
	return {
		post: post as never,
		prompt: prompt as never,
		getScope: () => ({ ...SCOPE, key: 'anonymous:default' }),
		dropPending: vi.fn(),
		dropAttention: vi.fn()
	};
}

describe('answerTaskAsk', () => {
	it('posts what the reader typed in the act, without opening a prompt at all', async () => {
		const post = postSpy(async () => ({ ok: true }) as HitlResolveOutcome);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(
			clarification(),
			{ kind: 'text', value: 'Q3' },
			SCOPE,
			deps(post, prompt)
		);

		expect(outcome).toEqual({ ok: true });
		expect(post).toHaveBeenCalledTimes(1);
		expect(post.mock.calls[0]?.[1]).toEqual({ type: 'text', value: 'Q3' });
		// Answering in place means answering in place: no modal was opened.
		expect(prompt).not.toHaveBeenCalled();
	});

	it('opens the prompt when the ask is one the panel hands off', async () => {
		const post = postSpy(async () => ({ ok: true }) as HitlResolveOutcome);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(clarification(), null, SCOPE, deps(post, prompt));

		expect(outcome).toEqual({ ok: true });
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(post).not.toHaveBeenCalled();
	});

	it('re-asks through the prompt, so nothing after the first post runs unattended', async () => {
		// The backend accepts the answer and asks for a revision. The old panel
		// bounded this with a counter; what actually bounded it was that every
		// iteration blocked on a human, and that is what is preserved.
		const post = postSpy(async () => reask);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(
			clarification(),
			{ kind: 'text', value: 'Q3' },
			SCOPE,
			deps(post, prompt)
		);

		expect(outcome).toEqual({ ok: true });
		// One unattended post, and exactly one.
		expect(post).toHaveBeenCalledTimes(1);
		expect(prompt).toHaveBeenCalledTimes(1);
		// Re-opened with the revised question and seeded with what was already
		// typed, so the reader is revising rather than starting again.
		expect(prompt.mock.calls[0]?.[0].prompt).toBe('Which of Q1 or Q2?');
		expect(prompt.mock.calls[0]?.[2]).toEqual({ defaultValue: 'Q3' });
	});

	it('stops the moment the reader dismisses, and reports nothing as an error', async () => {
		// A dismissal is the reader's own gesture. Nothing was posted and nothing
		// changed; an error line here would tell them their own choice failed.
		const post = postSpy(async () => reask);
		const prompt = promptSpy(async () => ({ ok: false, cancelled: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(
			clarification(),
			{ kind: 'text', value: 'Q3' },
			SCOPE,
			deps(post, prompt)
		);

		expect(outcome).toEqual({ ok: false, cancelled: true });
		expect(prompt).toHaveBeenCalledTimes(1);
	});

	it('never reports success the server did not give', async () => {
		const post = postSpy(
			async () => ({ ok: false, status: 500, message: 'Pause state not found' }) as HitlResolveOutcome
		);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(
			clarification(),
			{ kind: 'text', value: 'Q3' },
			SCOPE,
			deps(post, prompt)
		);

		expect(outcome).toEqual({ ok: false, message: 'Pause state not found' });
		// And it did not quietly fall back to the modal, which would have made a
		// failed post look like a prompt the reader had simply not finished.
		expect(prompt).not.toHaveBeenCalled();
	});

	it('claims nothing either way when the scope moved under an answer in flight', async () => {
		let key = 'anonymous:default';
		const post = postSpy(async () => {
			key = 'other:default';
			return { ok: true } as HitlResolveOutcome;
		});
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		const outcome = await answerTaskAsk(clarification(), { kind: 'text', value: 'Q3' }, SCOPE, {
			post: post as never,
			prompt: prompt as never,
			getScope: () => ({ principal: key.split(':')[0], workspace: 'default', key }),
			dropPending: vi.fn(),
			dropAttention: vi.fn()
		});

		// The post went out and its outcome can no longer be trusted, so neither
		// "sent" nor "not sent" is a claim to make. The sentence says exactly that.
		expect(outcome.ok).toBe(false);
		expect(outcome).toMatchObject({
			message: expect.stringContaining('reopen the task to check')
		});
	});

	it('answers a scopeless target in the reader’s scope, and never overwrites one that names its own', async () => {
		const post = postSpy(async () => ({ ok: true }) as HitlResolveOutcome);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		// A target this client synthesized from a task row carries no principal —
		// the only scope it can be answered in is the reader's.
		const synthesized = clarification({
			id: 'plan_31f',
			source: 'plan_approval',
			input_type: 'confirmation',
			prompt: 'Approve this plan so it can run?',
			identifiers: { correlation_id: 'plan_31f' },
			scope: { workflow_id: 'task_alpha', task_id: 'task_alpha' }
		});
		expect(
			(await answerTaskAsk(synthesized, { kind: 'choice', choiceId: 'confirm' }, SCOPE, deps(post, prompt))).ok
		).toBe(true);
		expect(post.mock.calls[0]?.[0].scope.principal).toBe('anonymous');

		// One that names a different owner keeps it, so the mismatch guard fires
		// rather than the answer being silently re-addressed.
		const elsewhere = clarification({
			scope: {
				principal: 'someone-else',
				workspace: 'default',
				workflow_id: 'task_alpha',
				task_id: 'task_alpha'
			}
		});
		const outcome = await answerTaskAsk(
			elsewhere,
			{ kind: 'text', value: 'Q3' },
			SCOPE,
			deps(post, prompt)
		);
		expect(outcome.ok).toBe(false);
		expect(outcome).toMatchObject({ message: expect.stringContaining('someone-else') });
		expect(post).toHaveBeenCalledTimes(1);
	});

	it('refuses an answer whose shape the input type cannot post', async () => {
		const post = postSpy(async () => ({ ok: true }) as HitlResolveOutcome);
		const prompt = promptSpy(async () => ({ ok: true }) as HitlResolveOutcome);

		// A `multi_choice` result under a `text` ask. Loud rather than quiet: it is
		// the failure the `Record<HitlInputType, …>` guards exist to make
		// impossible, and posting *something* would be worse than saying so.
		const outcome = await answerTaskAsk(
			clarification(),
			{ kind: 'multi_choice', choiceIds: ['a'] },
			SCOPE,
			deps(post, prompt)
		);

		expect(outcome.ok).toBe(false);
		expect(post).not.toHaveBeenCalled();
		expect(prompt).not.toHaveBeenCalled();
	});
});
