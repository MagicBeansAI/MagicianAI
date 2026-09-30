<script lang="ts">
	/**
	 * Stuck banner — surfaces a no-progress run as a visible, steerable moment
	 * (plan §4.4 "Stuck"). When the live run repeats the same tool call ≥3× the
	 * cockpit shows this instead of letting it hang silently. "Redirect" prefills
	 * the composer with a steer hint; "Dismiss" hides it until the signature
	 * changes. (Interactive mid-turn steer to the warm Pi session is the U8
	 * control-plane follow-on; redirecting via a follow-up is the buildable path.)
	 */
	import { createEventDispatcher } from 'svelte';

	export let toolName = '';
	export let args = '';

	const dispatch = createEventDispatcher<{ redirect: void; dismiss: void }>();

	function shortArgs(value: string): string {
		const compact = value.replace(/\s+/g, ' ').trim();
		return compact.length > 80 ? `${compact.slice(0, 80)}…` : compact;
	}
</script>

<div class="stuck" role="status" aria-live="polite">
	<span class="stuck__pulse" aria-hidden="true"></span>
	<div class="stuck__body">
		<div class="stuck__title">The coding agent seems stuck</div>
		<p class="stuck__detail">
			It has tried <code>{toolName || 'the same step'}</code> several times{args ? ` (${shortArgs(args)})` : ''}.
			Want to redirect it?
		</p>
	</div>
	<div class="stuck__actions">
		<button type="button" class="stuck__btn" on:click={() => dispatch('dismiss')}>Dismiss</button>
		<button type="button" class="stuck__btn stuck__btn--primary" on:click={() => dispatch('redirect')}>Redirect…</button>
	</div>
</div>

<style>
	.stuck {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		margin: 0 var(--space-md, 1rem) 0;
		border: 1px solid color-mix(in srgb, var(--vibe-warning) 45%, var(--vibe-border));
		border-radius: var(--radius-md, 18px);
		background: color-mix(in srgb, var(--vibe-warning) 9%, var(--vibe-surface));
		padding: 0.65rem 0.85rem;
		animation: stuck-in 0.24s var(--ease-settle, ease);
	}
	.stuck__pulse {
		flex-shrink: 0;
		width: 0.6rem;
		height: 0.6rem;
		border-radius: 999px;
		background: var(--vibe-warning);
		animation: stuck-pulse 1.3s ease-in-out infinite;
	}
	.stuck__body {
		flex: 1;
		min-width: 0;
	}
	.stuck__title {
		font-family: var(--font-display, inherit);
		font-weight: 600;
		font-size: 0.86rem;
		color: var(--vibe-text);
	}
	.stuck__detail {
		margin: 0.15rem 0 0;
		font-size: 0.8rem;
		line-height: 1.4;
		color: var(--vibe-text-muted);
		overflow-wrap: anywhere;
	}
	.stuck__detail code {
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		color: var(--vibe-text);
	}
	.stuck__actions {
		display: inline-flex;
		gap: 0.4rem;
		flex-shrink: 0;
	}
	.stuck__btn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 600;
		padding: 0.32rem 0.7rem;
		cursor: pointer;
	}
	.stuck__btn--primary {
		border-color: color-mix(in srgb, var(--vibe-warning) 60%, transparent);
		background: var(--vibe-warning);
		color: var(--button-primary-color, #fff);
	}
	.stuck__btn:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	@keyframes stuck-in {
		from {
			opacity: 0;
			transform: translateY(-4px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
	@keyframes stuck-pulse {
		0%,
		100% {
			opacity: 0.4;
		}
		50% {
			opacity: 1;
		}
	}
	@media (max-width: 720px) {
		.stuck {
			flex-direction: column;
			align-items: stretch;
		}
		.stuck__actions .stuck__btn {
			flex: 1;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.stuck,
		.stuck__pulse {
			animation: none;
		}
	}
</style>
