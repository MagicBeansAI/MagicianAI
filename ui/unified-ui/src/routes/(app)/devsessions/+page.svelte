<script lang="ts">
	import { goto } from '$app/navigation';
	import { onDestroy, onMount } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { threadStore } from '$lib/stores/threadStore';
	import {
		normalizeInteractiveSessionSummary,
		scopedInteractiveSessionParams,
		type InteractiveSessionList,
		type LiveInteractiveSession
	} from '$lib/shell/interactiveSessionApi';

	const STALE_AFTER_MS = 15 * 60 * 1000;

	let sessions: LiveInteractiveSession[] = [];
	let isLoading = true;
	let errorMessage: string | null = null;
	let closingSessionIds = new Set<string>();
	let nowMs = Date.now();
	let clock: ReturnType<typeof setInterval> | null = null;
	let refreshTimer: ReturnType<typeof setInterval> | null = null;
	let unsubscribeEvents: (() => void) | null = null;
	let unsubscribeSessionClosed: (() => void) | null = null;

	onMount(() => {
		threadStore.start();
		clock = setInterval(() => {
			nowMs = Date.now();
		}, 30_000);
		refreshTimer = setInterval(() => {
			void refreshSessions(false);
		}, 30_000);
		unsubscribeEvents = v2Events.subscribe((events) => {
			for (const event of events) {
				if (event.event_type !== 'InteractivePtyChunk') continue;
				const data = event.data;
				if (!data?.session_id) continue;
				upsertSessionFromChunk(
					data.session_id,
					data.program ?? null,
					data.ui_thread_id ?? null,
					data.timestamp_ms ?? Date.now(),
					typeof data.offset_end === 'number' ? data.offset_end : null
				);
			}
		});
		const handleClosed = (event: Event) => {
			const sessionId = (event as CustomEvent<{ sessionId?: string }>).detail?.sessionId;
			if (sessionId) {
				sessions = sessions.filter((session) => session.id !== sessionId);
			}
		};
		window.addEventListener('magician:interactive-session-closed', handleClosed);
		unsubscribeSessionClosed = () => {
			window.removeEventListener('magician:interactive-session-closed', handleClosed);
		};
		void refreshSessions();
	});

	onDestroy(() => {
		threadStore.stop();
		if (clock) clearInterval(clock);
		if (refreshTimer) clearInterval(refreshTimer);
		unsubscribeEvents?.();
		unsubscribeSessionClosed?.();
	});

	async function refreshSessions(showLoading = true): Promise<void> {
		if (showLoading) {
			isLoading = true;
			errorMessage = null;
		}
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions?${scopedInteractiveSessionParams().toString()}`
			);
			const payload = (await response.json().catch(() => null)) as InteractiveSessionList | null;
			if (!response.ok || !payload) {
				throw new Error((payload as { error?: string } | null)?.error || `server returned ${response.status}`);
			}
			sessions = sortSessions(
				(payload.sessions ?? []).map((session) =>
					normalizeInteractiveSessionSummary(session, nowMs)
				)
			);
		} catch (error) {
			errorMessage = error instanceof Error ? error.message : 'Failed to load sessions';
		} finally {
			if (showLoading) isLoading = false;
		}
	}

	function sortSessions(items: LiveInteractiveSession[]): LiveInteractiveSession[] {
		return [...items].sort((a, b) => {
			const threadCmp = (a.uiThreadId ?? '').localeCompare(b.uiThreadId ?? '');
			if (threadCmp !== 0) return threadCmp;
			return b.createdAtMs - a.createdAtMs;
		});
	}

	function upsertSessionFromChunk(
		sessionId: string,
		program: string | null,
		uiThreadId: string | null,
		timestampMs: number,
		offsetEnd: number | null
	): void {
		const existing = sessions.find((session) => session.id === sessionId);
		if (existing) {
			existing.program = existing.program ?? program;
			existing.uiThreadId = existing.uiThreadId ?? uiThreadId;
			existing.lastOutputAtMs = timestampMs;
			existing.lastSeenMs = timestampMs;
			if (offsetEnd != null) existing.replayEndOffset = offsetEnd;
			sessions = sortSessions(sessions);
			return;
		}
		sessions = sortSessions([
			...sessions,
			{
				id: sessionId,
				program,
				uiThreadId,
				workingDir: null,
				createdAtMs: timestampMs,
				lastOutputAtMs: timestampMs,
				lastInputAtMs: null,
				replayStartOffset: 0,
				replayEndOffset: offsetEnd ?? 0,
				alive: true,
				exitCode: null,
				lastSeenMs: timestampMs
			}
		]);
	}

	async function openSession(session: LiveInteractiveSession): Promise<void> {
		const threadId = session.uiThreadId?.trim();
		if (!threadId) {
			showError(
				'Session is not thread-attached',
				'This PTY was started without a UI thread, so it cannot be reopened in Developer Mode. You can still close it from here.'
			);
			return;
		}
		await threadStore.updateThread(threadId, { display_mode: 'dev' });
		await goto(
			`/t/${encodeURIComponent(threadId)}?dev_session=${encodeURIComponent(session.id)}`
		);
	}

	async function closeSession(event: MouseEvent, sessionId: string): Promise<void> {
		event.stopPropagation();
		if (closingSessionIds.has(sessionId)) return;
		closingSessionIds = new Set([...closingSessionIds, sessionId]);
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}?${scopedInteractiveSessionParams().toString()}`,
				{ method: 'DELETE' }
			);
			if (!response.ok) {
				throw new Error(`server returned ${response.status}`);
			}
			sessions = sessions.filter((session) => session.id !== sessionId);
			window.dispatchEvent(
				new CustomEvent('magician:interactive-session-closed', { detail: { sessionId } })
			);
			showSuccess('Session closed.');
		} catch (error) {
			showError('Failed to close session', error instanceof Error ? error.message : 'Unknown error');
		} finally {
			closingSessionIds.delete(sessionId);
			closingSessionIds = new Set(closingSessionIds);
		}
	}

	function lastActivityMs(session: LiveInteractiveSession): number {
		return Math.max(
			session.lastInputAtMs ?? 0,
			session.lastOutputAtMs ?? 0,
			session.createdAtMs ?? 0
		);
	}

	function isStale(session: LiveInteractiveSession): boolean {
		return nowMs - lastActivityMs(session) > STALE_AFTER_MS;
	}

	function formatAge(ms: number): string {
		if (!Number.isFinite(ms) || ms <= 0) return 'now';
		const minutes = Math.floor(ms / 60_000);
		if (minutes < 1) return '<1m';
		if (minutes < 60) return `${minutes}m`;
		const hours = Math.floor(minutes / 60);
		if (hours < 24) return `${hours}h ${minutes % 60}m`;
		const days = Math.floor(hours / 24);
		return `${days}d ${hours % 24}h`;
	}

	function formatTime(ms: number | null): string {
		if (!ms) return 'none';
		return new Date(ms).toLocaleString();
	}

	function formatBytes(bytes: number): string {
		if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
		const units = ['B', 'KB', 'MB'];
		let value = bytes;
		let unit = 0;
		while (value >= 1024 && unit < units.length - 1) {
			value /= 1024;
			unit += 1;
		}
		return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
	}
</script>

<svelte:head>
	<title>Developer Sessions - Magican</title>
</svelte:head>

<main class="devsessions">
	<header class="devsessions__header">
		<div>
			<p class="devsessions__eyebrow">Developer Mode</p>
			<h1>Live PTY Sessions</h1>
		</div>
		<button type="button" class="devsessions__refresh" disabled={isLoading} on:click={() => void refreshSessions()}>
			{isLoading ? 'Refreshing' : 'Refresh'}
		</button>
	</header>

	{#if errorMessage}
		<div class="devsessions__error">{errorMessage}</div>
	{/if}

	{#if isLoading && sessions.length === 0}
		<div class="devsessions__empty">Loading live sessions...</div>
	{:else if sessions.length === 0}
		<div class="devsessions__empty">No live PTY sessions.</div>
	{:else}
		<section class="devsessions__grid" aria-label="Live PTY sessions">
			{#each sessions as session (session.id)}
				<div class="devsessions__tile">
					<button
						type="button"
						class="devsessions__card"
						class:devsessions__card--stale={isStale(session)}
						disabled={!session.uiThreadId}
						title={session.uiThreadId ? 'Open in Dev thread' : 'Unthreaded sessions cannot be reopened yet'}
						on:click={() => void openSession(session)}
					>
						<div class="devsessions__card-head">
							<span class="devsessions__program">{session.program ?? 'session'}</span>
							<span class="devsessions__status" class:devsessions__status--stale={isStale(session)}>
								{isStale(session) ? 'stale' : 'live'}
							</span>
						</div>
						<div class="devsessions__thread">
							<span>{session.uiThreadId ? `#${session.uiThreadId}` : 'unthreaded'}</span>
							<span>{session.id.slice(0, 8)}</span>
						</div>
						<div class="devsessions__cwd" title={session.workingDir ?? 'default cwd'}>
							{session.workingDir ?? 'default cwd'}
						</div>
						<div class="devsessions__stats">
							<div>
								<span>Running</span>
								<strong>{formatAge(nowMs - session.createdAtMs)}</strong>
							</div>
							<div>
								<span>Idle</span>
								<strong>{formatAge(nowMs - lastActivityMs(session))}</strong>
							</div>
							<div>
								<span>Replay</span>
								<strong>{formatBytes(session.replayEndOffset - session.replayStartOffset)}</strong>
							</div>
						</div>
						<div class="devsessions__activity">
							<span>Last output: {formatTime(session.lastOutputAtMs)}</span>
							<span>Last input: {formatTime(session.lastInputAtMs)}</span>
						</div>
						<div class="devsessions__actions">
							<span>{session.uiThreadId ? 'Open in Dev thread' : 'Close from this page'}</span>
						</div>
					</button>
					<button
						type="button"
						class="devsessions__close"
						disabled={closingSessionIds.has(session.id)}
						on:click={(event) => void closeSession(event, session.id)}
					>
						Close
					</button>
				</div>
			{/each}
		</section>
	{/if}
</main>

<style>
	.devsessions {
		width: min(1320px, calc(100% - 48px));
		margin: 0 auto;
		padding: 34px 0 64px;
		color: var(--text-primary, #1a1a1a);
	}

	.devsessions__header {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 18px;
		margin-bottom: 22px;
	}

	.devsessions__eyebrow {
		margin: 0 0 5px;
		font: 700 11px var(--font-primary);
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--accent-primary, #c2502a);
	}

	.devsessions h1 {
		margin: 0;
		font: 700 28px/1.1 var(--font-primary);
		letter-spacing: 0;
	}

	.devsessions__refresh,
	.devsessions__close {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 7px;
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
		font: 650 12px var(--font-primary);
		cursor: pointer;
	}

	.devsessions__refresh {
		height: 32px;
		padding: 0 13px;
	}

	.devsessions__refresh:hover:not(:disabled),
	.devsessions__close:hover:not(:disabled) {
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
	}

	.devsessions__refresh:disabled,
	.devsessions__close:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.devsessions__error,
	.devsessions__empty {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		background: var(--bg-elevated, #fff);
		color: var(--text-muted, #777);
		font: 13px var(--font-primary);
		padding: 18px;
	}

	.devsessions__error {
		margin-bottom: 16px;
		color: var(--danger, #b42318);
	}

	.devsessions__grid {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 14px;
	}

	.devsessions__tile {
		display: grid;
		gap: 8px;
		min-width: 0;
	}

	.devsessions__card {
		appearance: none;
		display: grid;
		gap: 12px;
		width: 100%;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		background: var(--bg-elevated, #fff);
		box-shadow: var(--shadow-sm, 0 8px 24px -18px rgba(0, 0, 0, 0.3));
		color: inherit;
		font: inherit;
		text-align: left;
		padding: 14px;
		cursor: pointer;
		min-width: 0;
	}

	.devsessions__card:hover {
		border-color: color-mix(in srgb, var(--accent-primary, #c2502a) 45%, transparent);
		box-shadow: var(--shadow-md, 0 16px 36px -24px rgba(0, 0, 0, 0.35));
	}

	.devsessions__card:disabled {
		cursor: default;
		opacity: 0.72;
	}

	.devsessions__card:disabled:hover {
		border-color: var(--border-soft, rgba(0, 0, 0, 0.1));
		box-shadow: var(--shadow-sm, 0 8px 24px -18px rgba(0, 0, 0, 0.3));
	}

	.devsessions__card--stale {
		border-color: color-mix(in srgb, var(--warning, #d97706) 38%, var(--border-soft, rgba(0, 0, 0, 0.1)));
	}

	.devsessions__card-head,
	.devsessions__thread,
	.devsessions__actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		min-width: 0;
	}

	.devsessions__program {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font: 700 15px var(--font-mono, ui-monospace, monospace);
	}

	.devsessions__status {
		border: 1px solid color-mix(in srgb, var(--success, #15803d) 35%, transparent);
		border-radius: 999px;
		color: var(--success, #15803d);
		font: 700 10px/1 var(--font-primary);
		letter-spacing: 0.05em;
		text-transform: uppercase;
		padding: 4px 7px;
	}

	.devsessions__status--stale {
		border-color: color-mix(in srgb, var(--warning, #d97706) 45%, transparent);
		color: var(--warning, #d97706);
	}

	.devsessions__thread {
		color: var(--text-muted, #777);
		font: 11px var(--font-mono, ui-monospace, monospace);
	}

	.devsessions__cwd {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-secondary, #444);
		font: 11px var(--font-mono, ui-monospace, monospace);
	}

	.devsessions__stats {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 8px;
	}

	.devsessions__stats div {
		display: grid;
		gap: 3px;
		border-radius: 7px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.045));
		padding: 8px;
	}

	.devsessions__stats span,
	.devsessions__activity {
		color: var(--text-muted, #777);
		font: 10.5px var(--font-primary);
	}

	.devsessions__stats strong {
		font: 700 12px var(--font-mono, ui-monospace, monospace);
		color: var(--text-primary, #1a1a1a);
	}

	.devsessions__activity {
		display: grid;
		gap: 4px;
		min-width: 0;
	}

	.devsessions__activity span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.devsessions__actions {
		color: var(--accent-primary, #c2502a);
		font: 650 12px var(--font-primary);
	}

	.devsessions__close {
		height: 26px;
		padding: 0 9px;
		justify-self: end;
	}

	@media (max-width: 1100px) {
		.devsessions__grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	@media (max-width: 720px) {
		.devsessions {
			width: calc(100% - 24px);
			padding-top: 22px;
		}

		.devsessions__header {
			align-items: stretch;
			flex-direction: column;
		}

		.devsessions__grid {
			grid-template-columns: 1fr;
		}
	}
</style>
