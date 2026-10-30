<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';

	// The composer hint shown while typed text will answer an agent's pause
	// (see `escalationReply.ts`). Cancel talks past the pause: the text then
	// goes out as a normal message. Same shape as `PlanReplyBanner`.
	export let questionText: string;
	/** The answers the ask lists, for a choice or confirmation. */
	export let choices: string[] = [];
	/** The last reply named none of the answers; it stayed in the composer. */
	export let unmatched = false;

	const dispatch = createEventDispatcher<{ cancel: void }>();
</script>

<div class="escalation-reply-banner" role="status" data-testid="escalation-reply-banner">
	<div class="escalation-reply-banner__copy">
		<span class="escalation-reply-banner__heading">Your reply will answer:</span>
		<p>{questionText}</p>
		{#if unmatched}
			<span class="escalation-reply-banner__choices escalation-reply-banner__choices--unmatched">
				Reply with: {choices.join(' · ')}, or ✕ to send as a message
			</span>
		{:else if choices.length > 0}
			<span class="escalation-reply-banner__choices">Reply with: {choices.join(' · ')}</span>
		{/if}
	</div>
	<button
		type="button"
		class="escalation-reply-banner__cancel"
		on:click={() => dispatch('cancel')}
		aria-label="Send as a normal message instead"
		title="Send as a normal message instead"
	>
		<Icon name="x" size={15} />
	</button>
</div>

<style>
	.escalation-reply-banner {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.65rem 0.8rem;
		border-bottom: 1px solid color-mix(in srgb, var(--color-warning, #d59b2d) 42%, var(--border-soft, #ddd));
		background: color-mix(in srgb, var(--color-warning-soft, #fff4d6) 76%, var(--bg-elevated, #fff));
		color: var(--text-primary, #2d3436);
	}

	.escalation-reply-banner__copy {
		min-width: 0;
		display: grid;
		gap: 0.2rem;
	}

	.escalation-reply-banner__heading,
	.escalation-reply-banner__choices {
		font-size: var(--text-xs);
		color: var(--text-secondary, #5f6668);
	}

	.escalation-reply-banner__choices--unmatched {
		color: var(--text-primary, #2d3436);
		font-weight: 600;
	}

	.escalation-reply-banner p {
		margin: 0;
		font-size: var(--text-xs);
		line-height: 1.35;
		color: var(--text-primary, #2d3436);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.escalation-reply-banner__cancel {
		flex: 0 0 auto;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.8rem;
		height: 1.8rem;
		padding: 0;
		border: 0;
		border-radius: 50%;
		background: transparent;
		color: var(--text-secondary, #5f6668);
		cursor: pointer;
	}

	.escalation-reply-banner__cancel:hover {
		background: color-mix(in srgb, var(--text-primary, #2d3436) 9%, transparent);
		color: var(--text-primary, #2d3436);
	}

	.escalation-reply-banner__cancel:focus-visible {
		outline: 2px solid var(--color-warning, #d59b2d);
		outline-offset: 1px;
	}
</style>
