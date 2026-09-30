<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
	import type { RealtimeSession } from '$lib/media/types';

	const REFRESH_MS = 10_000;

	interface SessionListEnvelope {
		sessions: RealtimeSession[];
	}

	let sessions: RealtimeSession[] = [];
	let error: string | null = null;
	let timer: ReturnType<typeof setInterval> | null = null;

	$: liveTraySessions = sessions.filter((session) =>
		session.status === 'connected'
		&& session.capabilities?.realtime_voice === true
		&& (
			session.surface_type === 'tray_macos'
			|| session.surface_type === 'tray_windows'
			|| session.surface_type === 'tray_linux'
			|| (session.display_label ?? '').toLowerCase().includes('live ptt')
		)
	);
	$: primary = liveTraySessions[0] ?? null;
	$: ageLabel = primary ? formatAge(Date.now() - primary.created_at_ms) : '';
	$: lastSeenLabel = primary ? formatAge(Date.now() - primary.last_seen_at_ms) : '';

	onMount(() => {
		void refresh();
		timer = setInterval(() => void refresh(), REFRESH_MS);
	});

	onDestroy(() => {
		if (timer !== null) clearInterval(timer);
	});

	async function refresh(): Promise<void> {
		try {
			const response = await fetch('/api/magician/v2/media/sessions');
			if (!response.ok) {
				throw new Error(`media sessions ${response.status}`);
			}
			const payload = (await response.json()) as SessionListEnvelope;
			sessions = Array.isArray(payload.sessions) ? payload.sessions : [];
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Unable to read voice sessions';
		}
	}

	function formatAge(ms: number): string {
		const secs = Math.max(0, Math.floor(ms / 1000));
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		if (mins < 60) return `${mins}m`;
		const hours = Math.floor(mins / 60);
		return `${hours}h`;
	}
</script>

{#if primary}
	<section class="tray-live-banner" aria-label="Desktop live voice session">
		<div class="pulse" aria-hidden="true"></div>
		<div class="copy">
			<div class="title">
				<span>Desktop live voice is active</span>
				<span class="count">{liveTraySessions.length > 1 ? `${liveTraySessions.length} sessions` : 'tray'}</span>
			</div>
			<div class="meta">
				<span>{primary.display_label ?? 'Live PTT'}</span>
				<span>thread #{primary.thread_id ?? 'general'}</span>
				<span>age {ageLabel}</span>
				<span>seen {lastSeenLabel} ago</span>
			</div>
		</div>
		<a class="debug-link" href="/debug/voice">Debug</a>
	</section>
{:else if error}
	<section class="tray-live-banner tray-live-banner--error" aria-label="Desktop live voice status">
		<div class="pulse" aria-hidden="true"></div>
		<div class="copy">
			<div class="title">Voice session status unavailable</div>
			<div class="meta">{error}</div>
		</div>
	</section>
{/if}

<style>
	.tray-live-banner {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		padding: 8px 10px;
		margin-bottom: 6px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		color: var(--text-primary);
		background:
			linear-gradient(90deg, color-mix(in srgb, var(--accent, #7c3aed) 12%, transparent), transparent 65%);
	}

	.tray-live-banner--error {
		background:
			linear-gradient(90deg, color-mix(in srgb, var(--error, #ef4444) 12%, transparent), transparent 65%);
	}

	.pulse {
		width: 9px;
		height: 9px;
		border-radius: 999px;
		background: var(--accent, #7c3aed);
		box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent, #7c3aed) 18%, transparent);
		flex: 0 0 auto;
	}

	.tray-live-banner--error .pulse {
		background: var(--error, #ef4444);
		box-shadow: 0 0 0 5px color-mix(in srgb, var(--error, #ef4444) 18%, transparent);
	}

	.copy {
		min-width: 0;
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.title {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 12.5px;
		font-weight: 650;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.count {
		font-size: 11px;
		font-weight: 600;
		color: var(--text-secondary, var(--text-muted));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 999px;
		padding: 1px 6px;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		font-size: 11.5px;
		color: var(--text-secondary, var(--text-muted));
	}

	.debug-link {
		flex: 0 0 auto;
		font-size: 11.5px;
		font-weight: 650;
		color: var(--accent, #7c3aed);
		text-decoration: none;
	}
</style>
