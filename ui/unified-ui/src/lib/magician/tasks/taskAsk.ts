/**
 * Answering the ask the panel is showing you.
 *
 * The panel renders an ask and collects a result; it makes no requests, exactly
 * as it makes none for an output file's bytes. This is the other half: the
 * surface takes the result, posts it, and hands back an outcome the panel can
 * show without claiming anything the server has not said.
 *
 * **One module rather than a copy per workspace.** `/tasks` answers asks today
 * and the chat and `/crew` mounts will; three copies of a re-ask loop is three
 * places for "the ask stays open when the post fails" to stop being true.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *Answering an ask*.
 */

import { openHitlPrompt, type HitlOpenTarget } from '$lib/attention/openHitlPrompt';
import { postHitlResponse } from '$lib/hitl/adapters';
import {
	isSensitiveRequest,
	mapPromptResultToHitlValue,
	promptFor,
	respondToHitl
} from '$lib/hitl/respondToHitl';
import type { HitlRequest } from '$lib/hitl/types';
import type {
	AttentionPromptRequest,
	AttentionPromptResult
} from '$lib/stores/attentionPromptStore';
import type { OpenHitlPromptDependencies } from '$lib/attention/openHitlPrompt';

/**
 * Where an answer to one ask has got to.
 *
 * **Keyed on the ask's id, and the panel checks it before drawing anything** —
 * the same guard an output preview carries, one level over. A reply for one ask
 * must not render under another, and on a polling surface the ask on screen can
 * change while a post is in flight.
 *
 * There is no `answered` state, and that absence is the feature: a successful
 * post clears this and the ask disappears because the *server* stopped listing
 * it, never because we posted. An ask that looks answered and was not is the
 * worst possible failure of the thing this panel is for.
 */
export interface TaskAskState {
	id: string;
	status: 'sending' | 'failed';
	/** Why it failed, in the words the backend or the resolver used. `null` while sending. */
	message: string | null;
}

export type TaskAskOutcome =
	| { ok: true }
	/** The reader dismissed the prompt. Nothing was posted; the ask stays as it was. */
	| { ok: false; cancelled: true }
	| { ok: false; cancelled?: false; message: string };

/**
 * A `HitlRequest` lifted from a target, for **rendering only**.
 *
 * A straight field lift with no contract check, deliberately: `promptFor` needs
 * a request to build a prompt from, and the strict validation — that the id
 * matches the source's canonical identifier, that the scope is populated, that
 * the source and input type are compatible — belongs where the response is
 * posted, which is `openHitlPrompt`. Splitting it that way means a target that
 * can be described but not answered shows the reader the ask and then a real
 * error, rather than being silently dropped from a panel whose whole job is to
 * say what is blocking the task.
 */
function requestFrom(target: HitlOpenTarget): HitlRequest {
	return {
		id: target.id,
		source: target.source,
		input_type: target.input_type,
		schema: target.input_schema ?? {},
		prompt: target.prompt,
		hint: target.hint ?? undefined,
		scope: target.scope ?? {},
		identifiers: target.identifiers ?? {},
		at: target.at
	};
}

/**
 * The prompt for an ask, as the shared renderer takes it.
 *
 * `id` is the caller's: `AttentionPromptRequest.id` is what `HitlPromptFields`
 * resets its state on, and a surface rendering inline owns when that should
 * happen — which is when the *ask* changes, not when the panel re-renders under
 * a poll.
 */
export function askPromptFrom(
	target: HitlOpenTarget,
	id: number
): AttentionPromptRequest {
	return { ...promptFor(requestFrom(target)), id };
}

export interface AnswerTaskAskDependencies extends OpenHitlPromptDependencies {
	/** The first post, when the reader answered in place. Injected for its own test. */
	post?: typeof postHitlResponse;
	/** Every later prompt, including a re-ask. Injected for its own test. */
	prompt?: typeof respondToHitl;
}

/**
 * Post one answer, and see it through whatever the backend says next.
 *
 * `result` is what the reader produced in place, or `null` when the ask is one
 * the panel hands to the focused prompt — in which case the modal collects the
 * answer and this posts it.
 *
 * **The loop is `openHitlPrompt`'s, and this adds none of its own.** The old
 * panel wrote ~110 lines of re-ask handling bounded by `reaskGuard < 5`,
 * commented "so a misbehaving backend can't spin forever". The bound that
 * actually did that work was never the number: every iteration of that loop
 * awaited `requestAttentionInput`, so it could not advance without a fresh human
 * answer, and a dismissal ended it. The number bounded *nagging*, not spinning.
 *
 * What bounds this one is the same property, kept rather than re-implemented:
 * `openHitlPrompt` re-opens the modal on every `reask`, seeded with the revised
 * question and the previous answer, so no iteration after the first can run
 * unattended; a `cancelled` outcome ends it; and its `getScope().key === scope.key`
 * condition ends it if the reader switches scope mid-answer, which the counter
 * never covered. The inline value is substituted for the *first* modal and
 * nothing else, so at most one post in the whole exchange is not preceded by a
 * fresh human answer.
 *
 * **Nothing here reports success the server did not.** `resolved` is the only
 * outcome that answers `ok`, and the surface's response to it is to re-read the
 * task rather than to hide the ask locally.
 */
export async function answerTaskAsk(
	target: HitlOpenTarget,
	result: AttentionPromptResult | null,
	scope: { principal: string; workspace: string },
	dependencies: AnswerTaskAskDependencies = {},
	options: { cancel?: boolean } = {}
): Promise<TaskAskOutcome> {
	const { post = postHitlResponse, prompt = respondToHitl, ...openDependencies } = dependencies;

	// **Filled only where absent.** A target that names a scope keeps it, so
	// `openHitlPrompt`'s "this belongs to another principal" guard still fires;
	// one that names none — a pause record written without them, or an ask this
	// client synthesized from a task row — is answered in the only scope this
	// surface can post from, which is the reader's.
	const scoped: HitlOpenTarget = {
		...target,
		scope: {
			...target.scope,
			principal: target.scope?.principal ?? scope.principal,
			workspace: target.scope?.workspace ?? scope.workspace
		}
	};

	const inlineRequest = requestFrom(scoped);
	// Giving up on a secret ask in place — an expired code, a reader who wants a
	// fresh one — is an explicit cancel, not a request to open the modal: the
	// pending operation must retire so a new challenge can be raised, the same
	// way `respondToHitl` treats dismissing the modal. Only a secret ask is
	// cancelled this way; for any other ask the gesture is the handoff it
	// always was.
	const inlineValue =
		result === null
			? options.cancel && isSensitiveRequest(inlineRequest)
				? ({ type: 'aborted', reason: 'fresh_code_requested' } as const)
				: null
			: mapPromptResultToHitlValue(inlineRequest, result);
	if (result !== null && inlineValue === null) {
		// The renderer produced a shape this input type cannot post. Reporting it
		// rather than falling back to the modal: a control that answered something
		// other than what was clicked is the failure the whole `Record` guard
		// exists to make impossible, and it should be loud if it ever happens.
		return { ok: false, message: 'That answer does not fit this question.' };
	}

	let usedInline = false;
	const outcome = await openHitlPrompt(scoped, {
		...openDependencies,
		respond: async (request, headers, options) => {
			if (inlineValue !== null && !usedInline) {
				usedInline = true;
				const selectedPaths = result?.kind === 'choice' ? result.selectedPaths : undefined;
				return await post(request, inlineValue, headers, { selectedPaths });
			}
			return await prompt(request, headers, options);
		}
	});

	if (outcome.status === 'resolved') return { ok: true };
	if (outcome.status === 'cancelled') return { ok: false, cancelled: true };
	if (outcome.status === 'stale') {
		// The post went out and the scope changed before its outcome could be
		// trusted, so neither "sent" nor "not sent" is a claim we can make. The
		// sentence says exactly that rather than picking one.
		return {
			ok: false,
			message: 'Your scope changed while this was being answered — reopen the task to check.'
		};
	}
	return { ok: false, message: outcome.error };
}
