<script lang="ts">
	/**
	 * Settings → Browser notifications: web push for asks (HITL questions)
	 * while this tab is closed or in the background. Docs:
	 * docs/components/unified-ui/web-push.md.
	 */
	import { onMount } from 'svelte';
	import {
		disableWebPush,
		enableWebPush,
		fetchWebPushConfig,
		readWebPushState,
		type WebPushConfig,
		type WebPushState
	} from '$lib/webPush/client';

	let config = $state<WebPushConfig | null>(null);
	let pushState = $state<WebPushState | 'loading'>('loading');
	let busy = $state(false);
	let error = $state<string | null>(null);

	const STATE_LABEL: Record<WebPushState | 'loading', string> = {
		loading: 'Checking…',
		unsupported: 'Not supported',
		'desktop-app': 'Handled by the desktop app',
		'server-disabled': 'Off on this server',
		blocked: 'Blocked',
		off: 'Off',
		on: 'On'
	};

	onMount(() => {
		void refresh();
	});

	async function refresh(): Promise<void> {
		error = null;
		try {
			config = await fetchWebPushConfig();
		} catch (caught) {
			config = null;
			error = caught instanceof Error ? caught.message : 'Web push settings are unavailable';
		}
		pushState = await readWebPushState(config).catch(() => 'off' as const);
	}

	async function enable(): Promise<void> {
		if (!config) return;
		busy = true;
		error = null;
		try {
			pushState = await enableWebPush(config);
		} catch (caught) {
			error = caught instanceof Error ? caught.message : 'Could not turn on browser notifications';
			pushState = await readWebPushState(config).catch(() => 'off' as const);
		} finally {
			busy = false;
		}
	}

	async function disable(): Promise<void> {
		busy = true;
		error = null;
		try {
			pushState = await disableWebPush();
		} catch (caught) {
			error = caught instanceof Error ? caught.message : 'Could not turn off browser notifications';
		} finally {
			busy = false;
		}
	}
</script>

<section class="card" aria-labelledby="browser-notifications-title" data-testid="browser-notifications-panel">
	<div class="header">
		<div class="title-row">
			<div>
				<p class="overline">Browser notifications</p>
				<h2 id="browser-notifications-title">Questions while this tab is closed</h2>
			</div>
			<span class="chip" class:on={pushState === 'on'} data-testid="browser-notifications-state">
				{STATE_LABEL[pushState]}
			</span>
		</div>
		<p>
			When Magician asks you something and no tab is open, this browser shows a notification. A
			simple yes/no or two-option question can be answered from the notification; anything else
			opens the question here. The notification is fetched by this browser after a content-free
			push — no question text passes through the push service.
		</p>
	</div>

	{#if pushState === 'unsupported'}
		<p class="meta">This browser cannot receive web push here (it needs HTTPS or localhost, and on iPhone the app added to the Home Screen).</p>
	{:else if pushState === 'desktop-app'}
		<p class="meta">The desktop app shows questions in its own notch; browser notifications are not used here.</p>
	{:else if pushState === 'server-disabled'}
		<p class="meta">Web push is not configured on this Magician server (no VAPID key), so there is nothing to turn on.</p>
	{:else if pushState === 'blocked'}
		<p class="meta">Notifications are blocked for this site. Allow them in the browser's site settings, then come back.</p>
	{/if}

	{#if error}
		<p class="alert" role="alert">{error}</p>
	{/if}

	<div class="actions">
		{#if pushState === 'on'}
			<button type="button" disabled={busy} onclick={() => void disable()}>
				{busy ? 'Turning off…' : 'Turn off'}
			</button>
		{:else if pushState === 'off'}
			<button class="primary" type="button" disabled={busy || !config} onclick={() => void enable()}>
				{busy ? 'Turning on…' : 'Turn on browser notifications'}
			</button>
		{/if}
	</div>
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem;
	}
	.header {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.title-row {
		align-items: flex-start;
		display: flex;
		gap: 0.75rem;
		justify-content: space-between;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		margin: 0;
		text-transform: uppercase;
	}
	h2,
	p {
		margin: 0;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
	}
	.meta {
		color: var(--text-secondary);
		font-size: 0.85rem;
	}
	.chip {
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		color: var(--text-secondary);
		font-size: 0.75rem;
		padding: 0.15rem 0.6rem;
		white-space: nowrap;
	}
	.chip.on {
		border-color: var(--accent);
		color: var(--text-primary);
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.actions:empty {
		display: none;
	}
	button {
		background: var(--bg-secondary);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		padding: 0.45rem 0.85rem;
	}
	button.primary {
		background: var(--accent-fill);
		border-color: var(--accent);
		color: var(--text-on-accent, #fff);
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.alert {
		background: var(--bg-secondary);
		border-left: 3px solid var(--danger, #c0392b);
		padding: 0.5rem 0.75rem;
	}
</style>
