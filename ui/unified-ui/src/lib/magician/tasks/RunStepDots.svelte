<script lang="ts">
	/**
	 * The collapsed Run card's glance: one small dot per step of the grouped feed
	 * (`timelineDotsView`), coloured by how it ended.
	 *
	 * One `role="img"` with the summary sentence as its name — the dots restate
	 * that sentence for the eye, so each dot is `aria-hidden` and nothing here is
	 * a live region (the verdict owns the panel's one). `live` is whether the
	 * task can still move: a `running` dot breathes only then, so a settled run
	 * never pulses.
	 */
	import type { TimelineDotsView } from './taskTimeline';

	export let view: TimelineDotsView;
	export let live = false;
</script>

<div
	class="run-dots"
	class:run-dots--live={live}
	role="img"
	aria-label={view.summary}
	title={view.summary}
	data-testid="run-dots"
>
	{#if view.hidden > 0}
		<span class="run-dots__more" aria-hidden="true">+{view.hidden}</span>
	{/if}
	{#each view.dots as dot (dot.key)}
		<span class="run-dots__dot" data-status={dot.status} title={dot.label} aria-hidden="true"></span>
	{/each}
</div>

<style>
	.run-dots {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem;
		min-width: 0;
		line-height: 1;
	}

	.run-dots__more {
		flex: none;
		margin-right: 0.125rem;
		font-size: 0.68rem;
		font-variant-numeric: tabular-nums;
		color: var(--text-secondary);
	}

	.run-dots__dot {
		--run-dot: var(--status-completed);

		flex: none;
		width: 0.4375rem;
		height: 0.4375rem;
		border-radius: 50%;
		background: var(--run-dot);
	}

	.run-dots__dot[data-status='failed'] {
		--run-dot: var(--status-failed);
	}

	.run-dots__dot[data-status='running'] {
		--run-dot: var(--status-running);
	}

	.run-dots__dot[data-status='waiting'] {
		--run-dot: var(--status-paused);
	}

	/* Not recorded: a hollow ring rather than a colour that claims an outcome. */
	.run-dots__dot[data-status='unknown'] {
		background: transparent;
		box-shadow: inset 0 0 0 1px var(--text-secondary);
	}

	.run-dots--live .run-dots__dot[data-status='running'] {
		animation: run-dots-breathe calc(var(--dur-slow) * 4) ease-in-out infinite;
	}

	@keyframes run-dots-breathe {
		0%,
		100% {
			transform: scale(1);
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--run-dot) 0%, transparent);
		}
		50% {
			transform: scale(1.25);
			box-shadow: 0 0 0 3px color-mix(in srgb, var(--run-dot) 28%, transparent);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.run-dots--live .run-dots__dot[data-status='running'] {
			animation: none;
		}
	}
</style>
