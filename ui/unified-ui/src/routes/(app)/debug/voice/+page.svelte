<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import EventStreamCard from '$lib/realtime/EventStreamCard.svelte';
	import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
	import type { RealtimeSession } from '$lib/media/types';

	const REFRESH_MS = 8_000;

	interface SessionListEnvelope {
		sessions: RealtimeSession[];
	}

	let sessions: RealtimeSession[] = [];
	let loading = true;
	let error: string | null = null;
	let timer: ReturnType<typeof setInterval> | null = null;

	$: liveVoiceSessions = sessions.filter((session) => session.capabilities?.realtime_voice === true);
	$: traySessions = liveVoiceSessions.filter((session) => session.surface_type.startsWith('tray_'));
	$: browserSessions = liveVoiceSessions.filter((session) => session.surface_type.startsWith('web_'));
	$: staleSessions = sessions.filter((session) => Date.now() - session.last_seen_at_ms > 90_000);

	onMount(() => {
		void refresh();
		timer = setInterval(() => void refresh(), REFRESH_MS);
	});

	onDestroy(() => {
		if (timer !== null) clearInterval(timer);
	});

	async function refresh(): Promise<void> {
		loading = true;
		try {
			const response = await fetch('/api/magician/v2/media/sessions');
			if (!response.ok) {
				const text = await response.text().catch(() => '');
				throw new Error(`media sessions ${response.status}: ${text}`);
			}
			const payload = (await response.json()) as SessionListEnvelope;
			sessions = Array.isArray(payload.sessions) ? payload.sessions : [];
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Unable to load media sessions';
		} finally {
			loading = false;
		}
	}

	function formatTime(ms: number): string {
		try {
			return new Date(ms).toLocaleTimeString([], {
				hour: '2-digit',
				minute: '2-digit',
				second: '2-digit'
			});
		} catch {
			return 'unknown';
		}
	}

	function age(ms: number): string {
		const seconds = Math.max(0, Math.floor((Date.now() - ms) / 1000));
		if (seconds < 60) return `${seconds}s`;
		const minutes = Math.floor(seconds / 60);
		if (minutes < 60) return `${minutes}m`;
		const hours = Math.floor(minutes / 60);
		return `${hours}h`;
	}
</script>

<svelte:head>
	<title>Voice Debug · Magican</title>
</svelte:head>

<main class="voice-debug">
	<header class="page-head">
		<div>
			<p class="eyebrow">Debug</p>
			<h1>Voice sessions</h1>
			<p class="lede">Inspect active media sessions, desktop voice-note attempts, and live PTT bridge events.</p>
		</div>
		<button type="button" class="refresh" on:click={() => void refresh()} disabled={loading}>
			{loading ? 'Refreshing…' : 'Refresh'}
		</button>
	</header>

	<section class="metric-row" aria-label="Voice media summary">
		<div class="metric">
			<span class="metric-value">{liveVoiceSessions.length}</span>
			<span class="metric-label">voice sessions</span>
		</div>
		<div class="metric">
			<span class="metric-value">{traySessions.length}</span>
			<span class="metric-label">tray live PTT</span>
		</div>
		<div class="metric">
			<span class="metric-value">{browserSessions.length}</span>
			<span class="metric-label">browser calls</span>
		</div>
		<div class="metric" class:warn={staleSessions.length > 0}>
			<span class="metric-value">{staleSessions.length}</span>
			<span class="metric-label">stale heartbeat</span>
		</div>
	</section>

	{#if error}
		<section class="error-card" role="alert">{error}</section>
	{/if}

	<section class="panel">
		<div class="panel-head">
			<h2>Active media sessions</h2>
			<p>{sessions.length === 0 ? 'No active sessions in this scope.' : `${sessions.length} active session${sessions.length === 1 ? '' : 's'}`}</p>
		</div>
		<div class="session-grid">
			{#each sessions as session (session.session_id)}
				<article class="session-card">
					<div class="session-top">
						<div>
							<h3>{session.display_label ?? session.surface_type}</h3>
							<p>{session.session_id}</p>
						</div>
						<span class="status status-{session.status}">{session.status}</span>
					</div>
					<div class="session-meta">
						<span>{session.surface_type}</span>
						<span>{session.transport}</span>
						<span>thread #{session.thread_id ?? 'none'}</span>
						<span>created {formatTime(session.created_at_ms)}</span>
						<span>seen {age(session.last_seen_at_ms)} ago</span>
					</div>
					<div class="caps">
						{#if session.capabilities.realtime_voice}<span>realtime</span>{/if}
						{#if session.capabilities.mic}<span>mic</span>{/if}
						{#if session.capabilities.provider_tts}<span>provider tts</span>{/if}
						{#if session.capabilities.text_bubble}<span>bubble</span>{/if}
						{#if session.permissions.raw_media_persistence === 'granted'}<span>raw retained</span>{/if}
					</div>
				</article>
			{/each}
		</div>
	</section>

	<section class="event-grid">
		<EventStreamCard
			title="Voice-note attempts"
			density="compact"
			maxHeight="420px"
			defaultCategories={['media']}
			defaultEventType="media.voice_note"
		/>
		<EventStreamCard
			title="Live-call bridge and turn events"
			density="compact"
			maxHeight="420px"
			defaultCategories={['media']}
			defaultEventType="media.voice"
		/>
	</section>
</main>

<style>
	.voice-debug {
		box-sizing: border-box;
		width: 100%;
		max-width: 1320px;
		min-width: 0;
		margin: 0 auto;
		padding: 24px 0 40px;
		color: var(--text-primary);
	}

	.page-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		margin-bottom: 18px;
	}

	.eyebrow {
		margin: 0 0 4px;
		font-size: 11px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--accent, #7c3aed);
	}

	h1,
	h2,
	h3,
	p {
		margin: 0;
	}

	h1 {
		font-size: 28px;
		line-height: 1.1;
	}

	.lede {
		margin-top: 8px;
		color: var(--text-secondary, var(--text-muted));
		font-size: 14px;
	}

	.refresh {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		background: var(--surface-elevated, var(--surface));
		color: var(--text-primary);
		border-radius: 8px;
		padding: 8px 12px;
		font-weight: 650;
		cursor: pointer;
	}

	.metric-row {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 10px;
		margin-bottom: 14px;
	}

	.metric,
	.panel,
	.error-card {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		background: color-mix(in srgb, var(--surface-elevated, var(--surface)) 92%, transparent);
		border-radius: 8px;
	}

	.metric {
		padding: 12px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.metric.warn {
		border-color: color-mix(in srgb, var(--warning, #f59e0b) 50%, var(--border-soft));
	}

	.metric-value {
		font-size: 24px;
		font-weight: 750;
	}

	.metric-label {
		font-size: 12px;
		color: var(--text-secondary, var(--text-muted));
	}

	.error-card {
		padding: 10px 12px;
		margin-bottom: 14px;
		color: var(--error, #ef4444);
	}

	.panel {
		padding: 14px;
		margin-bottom: 14px;
	}

	.panel-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
		margin-bottom: 12px;
	}

	.panel-head h2 {
		font-size: 16px;
	}

	.panel-head p {
		font-size: 12px;
		color: var(--text-secondary, var(--text-muted));
	}

	.session-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
		gap: 10px;
	}

	.session-card {
		box-sizing: border-box;
		min-width: 0;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		padding: 12px;
		background: var(--surface, transparent);
	}

	.session-top {
		display: grid;
		grid-template-columns: minmax(0, 1fr) max-content;
		align-items: flex-start;
		gap: 10px;
		min-width: 0;
		margin-bottom: 8px;
	}

	.session-top > div {
		min-width: 0;
	}

	.session-top h3 {
		font-size: 14px;
		overflow-wrap: anywhere;
	}

	.session-top p {
		margin-top: 3px;
		font-size: 11px;
		color: var(--text-tertiary, var(--text-muted));
		word-break: break-all;
	}

	.status {
		width: max-content;
		align-self: flex-start;
		justify-self: end;
		font-size: 11px;
		font-weight: 700;
		border-radius: 999px;
		padding: 2px 7px;
		background: color-mix(in srgb, var(--accent, #7c3aed) 14%, transparent);
		color: var(--accent, #7c3aed);
		white-space: nowrap;
	}

	.status-disconnected,
	.status-revoked {
		background: color-mix(in srgb, var(--error, #ef4444) 14%, transparent);
		color: var(--error, #ef4444);
	}

	.status-paused {
		background: color-mix(in srgb, var(--warning, #f59e0b) 16%, transparent);
		color: var(--warning, #f59e0b);
	}

	.session-meta,
	.caps {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		font-size: 11.5px;
		color: var(--text-secondary, var(--text-muted));
	}

	.caps {
		margin-top: 10px;
	}

	.caps span {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 999px;
		padding: 2px 7px;
		color: var(--text-primary);
	}

	.event-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 14px;
	}

	.event-grid :global(.event-stream-card) {
		min-width: 0;
	}

	@media (max-width: 900px) {
		.metric-row,
		.event-grid {
			grid-template-columns: 1fr;
		}

		.page-head {
			flex-direction: column;
		}
	}

	@media (max-width: 520px) {
		.voice-debug {
			padding-inline: 12px;
		}

		.panel {
			padding: 12px;
		}
	}
</style>
