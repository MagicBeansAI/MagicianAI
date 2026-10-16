<script lang="ts">
	/**
	 * The questions this conversation is waiting on, above the composer, with
	 * every answer option in place (choices, confirm/deny, forms, plan approval
	 * with Review / Approve / Approve & Run).
	 *
	 * It answers through `answerTaskAsk`, the task panel's path: the answer is
	 * posted against the ask's canonical id and its aliases are dropped from the
	 * pending and Attention stores, so every other surface clears. A question
	 * answered elsewhere leaves this dock the same way, because the host derives
	 * `targets` from the pending store.
	 *
	 * Secrets and grants (password, code, tool/sandbox authorization, diff review)
	 * open the focused prompt instead: they belong in the surface opened to type
	 * or decide them, as in the task panel.
	 */
	import { get } from 'svelte/store';
	import HitlPromptFields from '$lib/hitl/HitlPromptFields.svelte';
	import { openHitlPrompt } from '$lib/attention/openHitlPrompt';
	import type { HitlInputType, HitlOpenTarget } from '$lib/hitl/types';
	import { askPromptFrom, answerTaskAsk, type TaskAskState } from '$lib/magician/tasks/taskAsk';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		PROMPT_HAS_OWN_ACTIONS,
		type AttentionPromptResult
	} from '$lib/stores/attentionPromptStore';
	import { showError } from '$lib/shared/stores/notifications';

	export let targets: HitlOpenTarget[] = [];
	/** Open the plan graph for a plan approval's Review. */
	export let onReviewPlan: ((taskId: string) => void) | undefined = undefined;
	export let disabled = false;

	const HANDOFF: Partial<Record<HitlInputType, string>> = {
		password: 'Enter it securely →',
		otp: 'Enter it securely →',
		tool_authorization: 'Review and decide →',
		sandbox_override: 'Review and decide →',
		diff_approval: 'Review the changes →'
	};

	let activeId: string | null = null;
	let fields: HitlPromptFields | null = null;
	let canSubmit = false;
	let state: TaskAskState | null = null;
	let promptSeq = 0;

	// The active question survives a new arrival; it moves on only when it leaves.
	$: if (!targets.some((target) => target.id === activeId)) activeId = targets[0]?.id ?? null;
	$: index = targets.findIndex((target) => target.id === activeId);
	$: active = index >= 0 ? targets[index] : null;
	// Rebuilt only when the active question changes: `targets` is re-derived on
	// every store update, and a fresh prompt would reset a half-typed answer.
	let prompt: ReturnType<typeof askPromptFrom> | null = null;
	let promptFor: string | null = null;
	$: if ((active?.id ?? null) !== promptFor) {
		promptFor = active?.id ?? null;
		prompt = active ? askPromptFrom(active, ++promptSeq) : null;
	}
	$: handoff = active
		? (HANDOFF[active.input_type] ??
			(active.input_schema?.sensitive ? HANDOFF.password : undefined))
		: undefined;
	$: progress = state !== null && active !== null && state.id === active.id ? state : null;
	$: busy = progress?.status === 'sending';

	function step(delta: number): void {
		if (targets.length === 0) return;
		const next = (Math.max(index, 0) + delta + targets.length) % targets.length;
		activeId = targets[next].id;
	}

	async function answer(event: CustomEvent<AttentionPromptResult>): Promise<void> {
		if (!active || busy) return;
		const target = active;
		state = { id: target.id, status: 'sending', message: null };
		const outcome = await answerTaskAsk(target, event.detail, get(scopeIdentityStore));
		if (outcome.ok || outcome.cancelled) {
			state = null;
			return;
		}
		state = { id: target.id, status: 'failed', message: outcome.message };
		showError(outcome.message);
	}

	async function openFocused(): Promise<void> {
		if (!active) return;
		const result = await openHitlPrompt(active);
		if (result.status === 'error' && result.error) showError(result.error);
	}

	function reviewPlan(event: CustomEvent<{ taskId: string }>): void {
		onReviewPlan?.(event.detail.taskId);
	}
</script>

{#if active && prompt}
	<section class="composer-ask" aria-label="Waiting on you" data-testid="composer-ask-dock">
		<header class="composer-ask__head">
			<span class="composer-ask__badge">Waiting on you</span>
			{#if active.delegated_agent_id}
				<span class="composer-ask__from">from {active.delegated_agent_id}</span>
			{/if}
			{#if targets.length > 1}
				<span class="composer-ask__switch">
					<button type="button" class="mg-chip-action" aria-label="Previous question" on:click={() => step(-1)}>‹</button>
					<span class="composer-ask__count">{index + 1} of {targets.length}</span>
					<button type="button" class="mg-chip-action" aria-label="Next question" on:click={() => step(1)}>›</button>
				</span>
			{/if}
		</header>
		<p class="composer-ask__prompt">{active.prompt}</p>
		{#if handoff}
			<button type="button" class="mg-chip-action composer-ask__handoff" {disabled} on:click={openFocused}>
				{handoff}
			</button>
		{:else}
			{#key active.id}
				<HitlPromptFields
					bind:this={fields}
					bind:canSubmit
					request={prompt}
					busy={busy || disabled}
					on:answer={answer}
					on:reviewPlan={reviewPlan}
				/>
			{/key}
			{#if !PROMPT_HAS_OWN_ACTIONS[prompt.kind]}
				<!-- A choice or a field has no decision buttons of its own; this
				     submits it, gated on the fields' own `canSubmit`. -->
				<button
					type="button"
					class="mg-chip-action composer-ask__submit"
					disabled={!canSubmit || busy || disabled}
					on:click={() => fields?.submit()}
				>
					{busy ? 'Sending…' : 'Answer'}
				</button>
			{/if}
		{/if}
		{#if progress?.status === 'failed'}
			<p class="composer-ask__error" role="alert">{progress.message}</p>
		{/if}
	</section>
{/if}

<style>
	.composer-ask {
		display: grid;
		gap: 0.5rem;
		margin: 0 auto 0.5rem;
		width: min(var(--chat-col, 760px), calc(100% - 48px));
		padding: 0.7rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-warning) 45%, var(--border-default));
		border-radius: var(--radius-composer);
		background: var(--bg-card);
		box-shadow: var(--shadow-float);
	}

	.composer-ask__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
	}

	.composer-ask__badge {
		padding: 0.05rem 0.45rem;
		border-radius: var(--radius-chip);
		background: color-mix(in srgb, var(--color-warning) 30%, transparent);
		color: var(--text-primary);
		font-size: 0.7rem;
		font-weight: 600;
	}

	.composer-ask__from {
		color: var(--text-secondary);
		font-size: 0.75rem;
	}

	.composer-ask__switch {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		margin-inline-start: auto;
	}

	.composer-ask__count {
		color: var(--text-secondary);
		font-size: 0.72rem;
	}

	.composer-ask__prompt {
		margin: 0;
		color: var(--text-primary);
		font-size: 0.9rem;
		line-height: 1.45;
		overflow-wrap: anywhere;
	}

	.composer-ask__handoff {
		justify-self: start;
	}

	.composer-ask__submit {
		justify-self: end;
	}

	.composer-ask__error {
		margin: 0;
		color: var(--color-error);
		font-size: 0.78rem;
	}
</style>
