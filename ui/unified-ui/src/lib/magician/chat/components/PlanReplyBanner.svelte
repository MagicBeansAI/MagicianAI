<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';

	export let taskTitle: string;
	export let questionText: string;
	export let stale = false;

	const dispatch = createEventDispatcher<{ cancel: void }>();
</script>

<div class="plan-reply-banner" class:plan-reply-banner--stale={stale} role="status">
	<div class="plan-reply-banner__copy">
		<div class="plan-reply-banner__heading">
			<span>{stale ? 'Planning question changed' : 'Replying to Planner'}</span>
			<strong>{taskTitle}</strong>
		</div>
		<p>{questionText}</p>
	</div>
	<button
		type="button"
		class="plan-reply-banner__cancel"
		on:click={() => dispatch('cancel')}
		aria-label="Cancel planning reply"
		title="Cancel planning reply"
	>
		<Icon name="x" size={15} />
	</button>
</div>

<style>
	.plan-reply-banner {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.65rem 0.8rem;
		border-bottom: 1px solid color-mix(in srgb, var(--color-info, #4d9de0) 32%, var(--border-soft, #ddd));
		background: color-mix(in srgb, var(--color-info-soft, #eaf4fb) 72%, var(--bg-elevated, #fff));
		color: var(--text-primary, #2d3436);
	}

	.plan-reply-banner--stale {
		border-bottom-color: color-mix(in srgb, var(--color-warning, #d59b2d) 42%, var(--border-soft, #ddd));
		background: color-mix(in srgb, var(--color-warning-soft, #fff4d6) 76%, var(--bg-elevated, #fff));
	}

	.plan-reply-banner__copy {
		min-width: 0;
		display: grid;
		gap: 0.2rem;
	}

	.plan-reply-banner__heading {
		display: flex;
		align-items: baseline;
		gap: 0.45rem;
		min-width: 0;
		font-size: var(--text-xs);
		color: var(--text-secondary, #5f6668);
	}

	.plan-reply-banner__heading strong {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary, #2d3436);
	}

	.plan-reply-banner p {
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

	.plan-reply-banner__cancel {
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

	.plan-reply-banner__cancel:hover {
		background: color-mix(in srgb, var(--text-primary, #2d3436) 9%, transparent);
		color: var(--text-primary, #2d3436);
	}

	.plan-reply-banner__cancel:focus-visible {
		outline: 2px solid var(--color-info, #4d9de0);
		outline-offset: 1px;
	}

	@media (max-width: 520px) {
		.plan-reply-banner__heading {
			align-items: flex-start;
			flex-direction: column;
			gap: 0.1rem;
		}
	}
</style>
