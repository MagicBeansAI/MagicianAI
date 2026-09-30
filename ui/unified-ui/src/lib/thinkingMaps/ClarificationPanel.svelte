<!--
  ClarificationPanel — "the AI is asking" panel.

  Renders every OPEN clarification (`state === 'open'`). Each card shows the
  question, the node it's raised against, and an answer box:
    · Answer  — `resolve_clarification` with `state:'answered'` + the typed text.
    · Dismiss — `resolve_clarification` with `state:'dismissed'` + `answer: null`.

  Both go through the page's `resolve` handler, which builds the op envelope
  (idempotency_key + base_revision) and applies it, then the sharedPoll drops the
  now-resolved clarification. Dashed accent styling reads as an AI prompt.
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { Clarification, ClarificationState } from '$lib/types/thinkingMap';
	import Button from '$lib/magician/components/native/Button.svelte';

	/** Only the OPEN clarifications (filtered on the page). */
	export let clarifications: Clarification[] = [];
	/** node_id → label, for a bit of context on each card. */
	export let nodeLabels: Record<string, string> = {};
	export let busy = false;
	/**
	 * Page handler: applies a `resolve_clarification` op. Throws on failure.
	 * `answer` is the typed text (answered) or `null` (dismissed).
	 */
	export let resolve: (
		clarificationId: string,
		state: ClarificationState,
		answer: string | null
	) => Promise<void>;

	const dispatch = createEventDispatcher<{ mutated: void }>();

	/** Per-card draft answers, keyed by clarification_id. */
	let drafts: Record<string, string> = {};
	let localError: string | null = null;

	async function onAnswer(c: Clarification): Promise<void> {
		const answer = (drafts[c.clarification_id] ?? '').trim();
		if (!answer || busy) return;
		localError = null;
		try {
			await resolve(c.clarification_id, 'answered', answer);
			drafts = { ...drafts, [c.clarification_id]: '' };
			dispatch('mutated');
		} catch (err) {
			localError = err instanceof Error ? err.message : String(err);
		}
	}

	async function onDismiss(c: Clarification): Promise<void> {
		if (busy) return;
		localError = null;
		try {
			await resolve(c.clarification_id, 'dismissed', null);
			dispatch('mutated');
		} catch (err) {
			localError = err instanceof Error ? err.message : String(err);
		}
	}
</script>

{#if clarifications.length > 0}
	<section class="cl" aria-label="Clarifications from the AI">
		<header class="cl__head">
			<span class="cl__mark" aria-hidden="true">✦</span>
			<h2>The AI is asking</h2>
			<span class="cl__count">{clarifications.length}</span>
		</header>

		{#each clarifications as c (c.clarification_id)}
			<article class="cl__card">
				<p class="cl__question">{c.question}</p>
				{#if nodeLabels[c.node_id]}
					<p class="cl__about">about “{nodeLabels[c.node_id]}”</p>
				{/if}
				<div class="cl__answer-row">
					<input
						class="cl__input"
						type="text"
						placeholder="Answer…"
						bind:value={drafts[c.clarification_id]}
						on:keydown={(e) => {
							if (e.key === 'Enter') onAnswer(c);
						}}
						disabled={busy}
						aria-label={`Answer: ${c.question}`}
					/>
					<Button
						variant="primary"
						size="sm"
						label="Answer"
						interactive={!busy && (drafts[c.clarification_id] ?? '').trim().length > 0}
						on:click={() => onAnswer(c)}
					/>
					<Button
						variant="outline"
						size="sm"
						label="Dismiss"
						interactive={!busy}
						on:click={() => onDismiss(c)}
					/>
				</div>
			</article>
		{/each}

		{#if localError}
			<div class="cl__error" role="alert">{localError}</div>
		{/if}
	</section>
{/if}

<style>
	.cl {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		padding: 0.85rem;
		border: 1px dashed color-mix(in srgb, var(--accent-primary, currentColor) 48%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary, transparent) 6%, var(--bg-card));
		box-shadow: var(--shadow-sm);
		animation: cl-in var(--transition-base, 0.25s) var(--ease-settle, ease) both;
	}

	@keyframes cl-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.cl {
			animation: none;
		}
	}

	.cl__head {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}

	.cl__mark {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.35rem;
		height: 1.35rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 16%, transparent);
		color: var(--accent-primary);
		font-size: 0.78rem;
	}

	.cl__head h2 {
		margin: 0;
		font-size: 0.78rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary);
	}

	.cl__count {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.3rem;
		height: 1.3rem;
		padding: 0 0.35rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 16%, transparent);
		font-size: 0.7rem;
		font-weight: 700;
		color: var(--accent-primary);
	}

	.cl__card {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.6rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
	}

	.cl__question {
		margin: 0;
		font-size: 0.85rem;
		font-weight: 600;
		line-height: 1.4;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.cl__about {
		margin: 0;
		font-size: 0.72rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.cl__answer-row {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
		align-items: center;
	}

	.cl__input {
		flex: 1;
		min-width: 8rem;
		padding: 0.45rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.82rem;
		transition: border-color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.cl__input::placeholder {
		color: var(--text-faint);
	}

	.cl__input:focus {
		background: var(--bg-card);
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 45%, var(--border-soft));
	}

	.cl__input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.cl__error {
		padding: 0.4rem 0.6rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.78rem;
	}
</style>
