<script lang="ts">
	import { goto } from '$app/navigation';

	export let active: 'talk' | 'code' = 'talk';
	export let draftText: string = '';

	async function switchTo(target: 'talk' | 'code') {
		if (target === active) return;
		const query = (draftText || '').trim();
		if (target === 'code') {
			const targetUrl = query ? `/vibe?q=${encodeURIComponent(query)}` : '/vibe';
			await goto(targetUrl);
		} else {
			const targetUrl = query ? `/chat?q=${encodeURIComponent(query)}` : '/chat';
			await goto(targetUrl);
		}
	}
</script>

<div class="talk-code-switch" role="radiogroup" aria-label="Talk or Code mode">
	<button
		type="button"
		class="switch-btn"
		class:active={active === 'talk'}
		class:icon-only={active === 'talk'}
		role="radio"
		aria-checked={active === 'talk'}
		aria-label="Talk mode"
		title="Talk — chat and conversational assistant (/chat)"
		on:click={() => switchTo('talk')}
	>
		<svg class="switch-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
			<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
		</svg>
		{#if active !== 'talk'}<span>Talk</span>{/if}
	</button>
	<button
		type="button"
		class="switch-btn"
		class:active={active === 'code'}
		class:icon-only={active === 'code'}
		role="radio"
		aria-checked={active === 'code'}
		aria-label="Code mode"
		title="Code — VibeDev living coding studio (/vibe)"
		on:click={() => switchTo('code')}
	>
		<svg class="switch-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
			<polyline points="16 18 22 12 16 6"/>
			<polyline points="8 6 2 12 8 18"/>
		</svg>
		{#if active !== 'code'}<span>Code</span>{/if}
	</button>
</div>

<style>
	.talk-code-switch {
		display: inline-flex;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border-radius: 5px;
		padding: 1.5px;
		gap: 0;
		flex-shrink: 0;
		user-select: none;
		vertical-align: middle;
	}

	.switch-btn {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		padding: 2px 7px;
		font-family: var(--font-mono);
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 3.5px;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
	}

	.switch-btn:hover:not(.active) {
		color: var(--text-primary);
	}

	.switch-btn.icon-only {
		padding: 2px 5px;
	}

	.switch-btn.active {
		background: var(--accent-secondary-soft, var(--accent-primary-soft));
		color: color-mix(in srgb, var(--accent-secondary, var(--accent-primary)) 82%, var(--text-primary));
		font-weight: 700;
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-secondary, var(--accent-primary)) 45%, transparent);
	}

	.switch-btn.active:hover {
		background: color-mix(in srgb, var(--accent-secondary, var(--accent-primary)) 24%, transparent);
	}

	.switch-btn:active {
		transform: scale(0.96);
	}

	.switch-icon {
		flex-shrink: 0;
		width: 10px;
		height: 10px;
	}
</style>
