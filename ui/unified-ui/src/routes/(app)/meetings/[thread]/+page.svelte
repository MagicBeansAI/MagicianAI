<script lang="ts">
	// Meeting detail — one meeting thread's transcript timeline + retention.
	// Read-only view over the thread's chat sessions (a thread can carry
	// rotated sessions; all are listed, newest first). Live controls stay on
	// the Meetings page; here: read, open-in-chat, delete transcript.
	import { onDestroy, onMount } from 'svelte';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import type { ActiveMeetingSession } from '$lib/stores/meetingsStore';
	import { putSeed, buildMeetingSeedContent } from '$lib/stores/vibeSeedStore';

	interface ThreadSession {
		id: string;
		title: string | null;
		status: string;
		updated_at: number;
		created_at?: number;
	}

	interface DisplayMessage {
		id: string;
		direction: string;
		text: string;
		created_at: number;
		source_surface?: string | null;
	}

	$: threadId = $page.params.thread ?? '';

	let sessions: ThreadSession[] = [];
	let selectedSessionId: string | null = null;
	let messages: DisplayMessage[] = [];
	let hasOlder = false;
	let live: ActiveMeetingSession | null = null;
	let loading = true;
	let loadError: string | null = null;
	let deleting = false;
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	function messageText(m: unknown): string {
		const msg = m as { content?: { text?: string } | string };
		if (typeof msg.content === 'string') return msg.content;
		return msg.content?.text ?? '';
	}

	async function loadSessions(): Promise<void> {
		const res = await fetch(
			`/api/magician/v2/chat/sessions?ui_thread_id=${encodeURIComponent(threadId)}`
		);
		if (!res.ok) throw new Error(`HTTP ${res.status}`);
		const data = await res.json();
		const list = Array.isArray(data.sessions) ? data.sessions : [];
		sessions = list
			.map((s: Record<string, unknown>) => ({
				id: String(s.id),
				title: (s.title as string | null) ?? null,
				status: String(s.status ?? ''),
				updated_at: Number(s.updated_at ?? 0),
				created_at: Number(s.created_at ?? 0)
			}))
			.sort((a: ThreadSession, b: ThreadSession) => b.updated_at - a.updated_at);
		if (!selectedSessionId || !sessions.some((s) => s.id === selectedSessionId)) {
			selectedSessionId = sessions[0]?.id ?? null;
		}
	}

	async function loadMessages(before?: string): Promise<void> {
		if (!selectedSessionId) {
			messages = [];
			hasOlder = false;
			return;
		}
		const params = new URLSearchParams({ limit: '200' });
		if (before) params.set('before', before);
		const res = await fetch(
			`/api/magician/v2/chat/sessions/${encodeURIComponent(selectedSessionId)}/messages?${params}`
		);
		if (!res.ok) throw new Error(`HTTP ${res.status}`);
		const data = await res.json();
		const batch: DisplayMessage[] = (Array.isArray(data.messages) ? data.messages : [])
			.map((m: Record<string, unknown>) => ({
				id: String(m.id),
				direction: String(m.direction ?? ''),
				text: messageText(m),
				created_at: Number(m.created_at ?? 0),
				source_surface: (m.source_surface as string | null) ?? null
			}))
			.filter((m: DisplayMessage) => m.text.trim().length > 0)
			.sort((a: DisplayMessage, b: DisplayMessage) => a.created_at - b.created_at);
		messages = before ? [...batch, ...messages] : batch;
		hasOlder = Boolean(data.has_more);
	}

	async function loadLive(): Promise<void> {
		try {
			const res = await fetch('/api/magician/v2/meetings/active');
			if (!res.ok) return;
			const data = await res.json();
			const rows: ActiveMeetingSession[] = Array.isArray(data.active) ? data.active : [];
			live = rows.find((s) => s.thread_id === threadId) ?? null;
		} catch {
			// live chip is best-effort
		}
	}

	async function refreshAll(): Promise<void> {
		try {
			await loadSessions();
			await loadMessages();
			await loadLive();
			loadError = null;
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'failed to load meeting';
		} finally {
			loading = false;
		}
	}

	async function selectSession(id: string): Promise<void> {
		selectedSessionId = id;
		try {
			await loadMessages();
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'failed to load messages';
		}
	}

	/// Retention: permanently clears every session's display messages in this
	/// thread (rotations split a meeting across sessions — partial deletes
	/// would leave a misleading half-transcript behind).
	async function deleteTranscript(): Promise<void> {
		const ok = window.confirm(
			'Delete this meeting’s entire transcript? This permanently removes the ' +
				'chat messages in this thread. Memory-tier takeaways are not affected.'
		);
		if (!ok) return;
		deleting = true;
		try {
			for (const s of sessions) {
				const res = await fetch(
					`/api/magician/v2/chat/sessions/${encodeURIComponent(s.id)}/messages`,
					{ method: 'DELETE' }
				);
				if (!res.ok) {
					const data = await res.json().catch(() => null);
					throw new Error(data?.error ?? `HTTP ${res.status}`);
				}
			}
			await refreshAll();
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'failed to delete transcript';
		} finally {
			deleting = false;
		}
	}

	function formatTime(ms: number): string {
		try {
			return new Date(ms).toLocaleTimeString(undefined, {
				hour: '2-digit',
				minute: '2-digit'
			});
		} catch {
			return '';
		}
	}

	function formatDay(ms: number): string {
		try {
			return new Date(ms).toLocaleDateString(undefined, {
				month: 'short',
				day: 'numeric',
				year: 'numeric'
			});
		} catch {
			return '';
		}
	}

	$: title = sessions[0]?.title ?? threadId;

	// M4 — turn this meeting's transcript into a seeded VibeDev build.
	function buildFromMeeting(): void {
		const turns = messages
			.filter((m) => m.text.trim().length > 0)
			.map((m) => ({ role: m.direction, text: m.text }));
		if (turns.length === 0) return;
		const ts = sessions[0]?.updated_at ?? 0;
		const date = ts > 0 ? new Date(ts).toLocaleDateString() : '';
		// Feed the agreed Decisions / Action items prose (the live capture's rolling summary)
		// into the seed, not just the raw transcript — the EM builds from what was decided.
		const content = buildMeetingSeedContent(turns, { title, date, summary: live?.latest_summary ?? null });
		const id = putSeed({
			source: 'meeting',
			label: date ? `${title} · ${date}` : title,
			sourceId: threadId,
			content,
			suggestedPrompt: 'Build what we agreed on in this meeting.'
		});
		void goto('/vibe?seed=' + id);
	}

	onMount(() => {
		void refreshAll();
		// Light follow while a capture is live; harmless when idle.
		pollTimer = setInterval(() => {
			void loadLive();
			if (live) void loadMessages();
		}, 10000);
	});
	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<div class="meeting-detail">
	<header class="detail-head">
		<button class="back-link" on:click={() => goto('/meetings')}>← Meetings</button>
		<h1>
			{title}
			{#if live}
				<span class="live-chip">
					{live.paused ? 'paused' : live.mode === 'attendee' ? 'in meeting' : 'listening'}
				</span>
			{/if}
		</h1>
		<div class="thread-id">{threadId}</div>
		<div class="head-actions">
			<button
				class="btn btn--primary"
				disabled={messages.length === 0}
				title="Start a VibeDev build seeded with this meeting's transcript"
				on:click={buildFromMeeting}
			>✦ Build what we agreed</button>
			<button class="btn" on:click={() => goto(`/t/${encodeURIComponent(threadId)}`)}>
				Open in chat
			</button>
			<button
				class="btn btn--danger"
				disabled={deleting || messages.length === 0}
				on:click={() => void deleteTranscript()}
			>{deleting ? 'Deleting…' : 'Delete transcript'}</button>
		</div>
	</header>

	{#if loadError}
		<div class="banner banner--error">{loadError}</div>
	{/if}

	{#if live?.latest_summary}
		<section class="summary-card">
			<h2>Rolling summary</h2>
			<pre class="summary-text">{live.latest_summary}</pre>
		</section>
	{/if}

	{#if sessions.length > 1}
		<div class="session-tabs">
			{#each sessions as s (s.id)}
				<button
					class="session-tab"
					class:session-tab--active={s.id === selectedSessionId}
					on:click={() => void selectSession(s.id)}
				>{formatDay(s.updated_at)} · {s.status}</button>
			{/each}
		</div>
	{/if}

	<section class="timeline">
		{#if loading}
			<p class="empty">Loading…</p>
		{:else if messages.length === 0}
			<p class="empty">No transcript in this thread.</p>
		{:else}
			{#if hasOlder}
				<button class="btn btn--small older-btn" on:click={() => void loadMessages(messages[0]?.id)}>
					Load older
				</button>
			{/if}
			<ul class="turns">
				{#each messages as m (m.id)}
					<li
						class="turn"
						class:turn--transcript={m.source_surface === 'meeting-transcript'}
						class:turn--user={m.direction === 'inbound' || m.direction === 'user'}
					>
						<span class="turn-time">{formatTime(m.created_at)}</span>
						<span class="turn-text">{m.text}</span>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
</div>

<style>
	.meeting-detail {
		width: 100%;
		max-width: 860px;
		margin: 0 auto;
		padding: 1rem 1rem 2rem;
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		color: var(--text-primary);
		font-family: var(--font-primary);
	}

	.back-link {
		background: none;
		border: none;
		padding: 0;
		color: var(--text-muted);
		font-size: 0.82rem;
		cursor: pointer;
	}

	.back-link:hover {
		color: var(--accent-primary, #c2502a);
	}

	.detail-head h1 {
		margin: 0.35rem 0 0.15rem;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.35rem;
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.live-chip {
		font-size: 0.66rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 0.12rem 0.5rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--accent-danger, #e5484d) 14%, transparent);
		color: var(--accent-danger, #e5484d);
	}

	.thread-id {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		color: var(--text-muted);
	}

	.head-actions {
		display: flex;
		gap: 0.5rem;
		margin-top: 0.6rem;
	}

	.btn {
		padding: 0.45rem 0.85rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary);
		font-size: 0.83rem;
		cursor: pointer;
	}

	.btn:disabled {
		opacity: 0.55;
		cursor: default;
	}

	.btn--danger {
		background: color-mix(in srgb, var(--accent-danger, #e5484d) 12%, transparent);
		border-color: var(--accent-danger, #e5484d);
		color: var(--accent-danger, #e5484d);
	}

	.btn--primary {
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 12%, transparent);
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
		font-weight: 600;
	}

	.btn--small {
		padding: 0.3rem 0.65rem;
		font-size: 0.76rem;
	}

	.banner {
		padding: 0.55rem 0.8rem;
		border-radius: var(--radius-md, 8px);
		font-size: 0.85rem;
	}

	.banner--error {
		background: color-mix(in srgb, var(--accent-danger, #e5484d) 12%, transparent);
		color: var(--accent-danger, #e5484d);
	}

	.summary-card {
		background: var(--bg-card, rgba(0, 0, 0, 0.03));
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.08));
		border-radius: var(--radius-lg, 12px);
		padding: 0.9rem 1rem;
	}

	.summary-card h2 {
		margin: 0 0 0.4rem;
		font-size: 0.95rem;
	}

	.summary-text {
		margin: 0;
		white-space: pre-wrap;
		font-family: inherit;
		font-size: 0.84rem;
		line-height: 1.5;
		color: var(--text-primary);
	}

	.session-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.session-tab {
		padding: 0.3rem 0.7rem;
		border-radius: 999px;
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-muted);
		font-size: 0.74rem;
		cursor: pointer;
	}

	.session-tab--active {
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
	}

	.timeline {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.older-btn {
		align-self: center;
	}

	.empty {
		color: var(--text-muted);
		font-size: 0.85rem;
		margin: 0;
	}

	.turns {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}

	.turn {
		display: flex;
		gap: 0.6rem;
		align-items: baseline;
		padding: 0.3rem 0.5rem;
		border-radius: var(--radius-md, 8px);
		font-size: 0.86rem;
		line-height: 1.45;
	}

	.turn--transcript {
		background: var(--bg-card, rgba(0, 0, 0, 0.02));
	}

	.turn--user .turn-text {
		font-weight: 600;
	}

	.turn-time {
		flex-shrink: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		color: var(--text-muted);
		min-width: 3.2rem;
	}

	.turn-text {
		white-space: pre-wrap;
		word-break: break-word;
	}
</style>
