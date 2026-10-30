<script lang="ts">
	/**
	 * Every open question, above the composer, with every answer option in
	 * place (choices, confirm/deny, forms, plan approval with Review / Approve /
	 * Approve & Run): the reader answers wherever they are. This conversation's
	 * questions come first; the others say where they came from.
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
	import { onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
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
	/** Ids of the targets that belong to this conversation; the rest are labelled with their origin. `null`: all of them. */
	export let here: ReadonlySet<string> | null = null;
	export let originChatTitles: Record<string, string> = {};
	export let originTaskTitles: Record<string, string> = {};
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

	/** The shown ask; bound by chat so typed composer text answers this one. */
	export let activeId: string | null = null;
	let fields: HitlPromptFields | null = null;
	let canSubmit = false;
	let states = new Map<string, TaskAskState>();
	let disposed = false;
	onDestroy(() => { disposed = true; });
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
	$: godGrant = active?.source === 'user_request' && prompt?.kind === 'authorization';
	// A question from another chat or run says where it came from.
	$: foreign = !!active && here !== null && !here.has(active.id);
	$: originSource = godGrant || foreign ? active : null;
	$: originContext = originSource && typeof originSource.input_schema?.context === 'object' ? originSource.input_schema.context : null;
	$: originChatId = typeof originContext?.chat_session_id === 'string' ? originContext.chat_session_id : '';
	$: originTaskId = originSource?.scope?.task_id ?? '';
	$: originSession = typeof originContext?.session === 'string' ? originContext.session : `chat:${originChatId}`;
	$: originAgent = typeof originContext?.agent_id === 'string' ? originContext.agent_id : (originSource?.scope?.agent_id ?? '');
	$: originChat = originChatTitles[originChatId] || (originChatId
		? `Chat ${originChatId.slice(0, 8)}`
		: originTaskId
			? (originTaskTitles[originTaskId] ? `task “${originTaskTitles[originTaskId]}”` : `task ${originTaskId.slice(0, 13)}`)
			: 'background run');
	$: originLabel = originSession.startsWith('run:') ? `Run ${originSession.slice(4, 12)} · ${originChat}` : originChat;
	$: handoff = active && !godGrant
		? (HANDOFF[active.input_type] ??
			(active.input_schema?.sensitive ? HANDOFF.password : undefined))
		: undefined;
	$: progress = active ? states.get(active.id) ?? null : null;
	$: busy = progress?.status === 'sending';

	function step(delta: number): void {
		if (targets.length === 0) return;
		const next = (Math.max(index, 0) + delta + targets.length) % targets.length;
		activeId = targets[next].id;
	}

	async function answer(event: CustomEvent<AttentionPromptResult>): Promise<void> {
		if (!active || states.get(active.id)?.status === 'sending') return;
		const target = active;
		setState(target.id, { id: target.id, status: 'sending', message: null });
		let outcome: Awaited<ReturnType<typeof answerTaskAsk>>;
		try { outcome = await answerTaskAsk(target, event.detail, get(scopeIdentityStore)); }
		catch (error) { outcome = { ok: false, message: error instanceof Error ? error.message : 'Could not send this answer.' }; }
		if (disposed) return;
		if (outcome.ok || outcome.cancelled) {
			setState(target.id, null);
			return;
		}
		setState(target.id, { id: target.id, status: 'failed', message: outcome.message });
		showError(outcome.message);
	}

	async function openFocused(): Promise<void> {
		if (!active || states.get(active.id)?.status === 'sending') return;
		const target = active;
		setState(target.id, { id: target.id, status: 'sending', message: null });
		try {
			const result = await openHitlPrompt(target);
			if (!disposed && result.status === 'error' && result.error) showError(result.error);
		} catch (error) {
			if (!disposed) showError(error instanceof Error ? error.message : 'Could not open this request.');
		} finally {
			if (!disposed) setState(target.id, null);
		}
	}

	function setState(id: string, state: TaskAskState | null): void {
		const next = new Map(states);
		if (state) next.set(id, state); else next.delete(id);
		states = next;
	}

	function reviewPlan(event: CustomEvent<{ taskId: string }>): void {
		if (onReviewPlan) onReviewPlan(event.detail.taskId);
		else void goto(`/work?selected=${encodeURIComponent(event.detail.taskId)}&plan=open`)
			.catch(() => showError('Could not open the plan.'));
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
		<!-- svelte-ignore a11y_no_noninteractive_tabindex (Keyboard users need to scroll the full request.) -->
		<div class="composer-ask__context" role="region" aria-label="Request context" tabindex="0">
		{#if godGrant}
            <p class="composer-ask__from">Requested in {originLabel}{originAgent ? ` · ${originAgent}` : ''}. “This session” refers to that originating chat or run.</p>
		{:else if foreign}
			<p class="composer-ask__from" data-testid="composer-ask-origin">From {originLabel}{originAgent ? ` · ${originAgent}` : ''}</p>
        {/if}
		<p class="composer-ask__prompt">{active.prompt}</p>
		</div>
		{#if handoff}
			<button type="button" class="mg-chip-action composer-ask__handoff" disabled={disabled || busy} on:click={openFocused}>
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
		display: flex;
		flex-direction: column;
		max-height: min(60dvh, 640px);
		min-height: 0;
		box-sizing: border-box;
		overflow: auto;
		--hitl-review-max-height: min(24dvh, 280px);
		gap: 0.5rem;
		margin: 0 auto 0.5rem;
		width: min(var(--chat-col, 760px), calc(100% - 48px));
		padding: 0.7rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-warning) 45%, var(--border-default));
		border-radius: var(--radius-composer);
		background: var(--bg-card);
		box-shadow: var(--shadow-float);
	}
	.composer-ask > :global(.hitl-fields) { flex: 0 0 auto; min-height: 0; }
	.composer-ask__context { min-height: 0; max-height: 6rem; overflow: auto; flex: 0 0 auto; }

	.composer-ask__head {
		flex: 0 0 auto;
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
