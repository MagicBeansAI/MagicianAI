<script lang="ts">
	/**
	 * InteractiveTerminalTabs — multi-session tab strip + active pane.
	 *
	 * The agent may spawn several interactive_process sessions in
	 * parallel (e.g. claude + a git rebase + a long-running cargo test).
	 * This component shows one tab per live session and renders the
	 * active one's xterm pane. Inactive tabs keep their xterm mounted
	 * so scrollback survives a switch.
	 *
	 * Phase 3 of Developer Mode. See
	 * docs/plans/2026-05-13-developer-mode-workbench.md.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { get } from 'svelte/store';
	import InteractiveTerminalPane from './InteractiveTerminalPane.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import type { LiveInteractiveSession } from '$lib/shell/interactiveSessionApi';

	const STALE_AFTER_MS = 15 * 60 * 1000;

	export let sessions: LiveInteractiveSession[] = [];
	export let activeSessionId: string | null = null;
	export let focusNonce = 0;

	const dispatch = createEventDispatcher<{
		select: { sessionId: string };
		closed: { sessionId: string };
		closeerror: { sessionId: string; message: string };
	}>();

	let closing: Set<string> = new Set();
	let nowMs = Date.now();
	let clock: ReturnType<typeof setInterval> | null = null;

	onMount(() => {
		clock = setInterval(() => {
			nowMs = Date.now();
		}, 30_000);
	});

	onDestroy(() => {
		if (clock) clearInterval(clock);
	});

	async function closeSession(sessionId: string): Promise<void> {
		if (closing.has(sessionId)) return;
		closing = new Set([...closing, sessionId]);
		const scope = get(scopeIdentityStore);
		const params = new URLSearchParams();
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}?${params.toString()}`,
				{ method: 'DELETE' }
			);
			if (!response.ok) {
				let message = `Close failed with HTTP ${response.status}`;
				try {
					const payload = (await response.json()) as { error?: string } | null;
					if (payload?.error) message = payload.error;
				} catch {
					/* Keep the status-based message. */
				}
				dispatch('closeerror', { sessionId, message });
				return;
			}
			window.dispatchEvent(
				new CustomEvent('magician:interactive-session-closed', { detail: { sessionId } })
			);
			dispatch('closed', { sessionId });
		} catch (error) {
			dispatch('closeerror', {
				sessionId,
				message: error instanceof Error ? error.message : 'Failed to close session'
			});
		} finally {
			closing.delete(sessionId);
			closing = new Set(closing);
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

	function sessionTitle(session: LiveInteractiveSession): string {
		const cwd = session.workingDir ? `cwd: ${session.workingDir}` : 'cwd: default';
		const age = `age: ${formatAge(nowMs - session.createdAtMs)}`;
		const idle = `idle: ${formatAge(nowMs - lastActivityMs(session))}`;
		const output = session.lastOutputAtMs
			? `last output: ${new Date(session.lastOutputAtMs).toLocaleTimeString()}`
			: 'last output: none';
		const input = session.lastInputAtMs
			? `last input: ${new Date(session.lastInputAtMs).toLocaleTimeString()}`
			: 'last input: none';
		return [session.program ?? 'session', session.id, cwd, age, idle, output, input].join('\n');
	}
</script>

<div class="terminal-tabs">
	{#if sessions.length > 0}
		<div class="terminal-tabs__strip" role="tablist" aria-label="Live PTY sessions">
			{#each sessions as session (session.id)}
				<div
					class="terminal-tabs__tab"
					class:terminal-tabs__tab--active={session.id === activeSessionId}
					class:terminal-tabs__tab--stale={isStale(session)}
					role="tab"
					aria-selected={session.id === activeSessionId}
					title={sessionTitle(session)}
				>
					<button
						type="button"
						class="terminal-tabs__label"
						on:click={() => dispatch('select', { sessionId: session.id })}
					>
						{session.program ?? 'session'}
						<span class="terminal-tabs__id">{session.id.slice(0, 6)}</span>
						<span class="terminal-tabs__meta">{formatAge(nowMs - session.createdAtMs)}</span>
						{#if isStale(session)}
							<span class="terminal-tabs__stale">stale</span>
						{/if}
					</button>
					<button
						type="button"
						class="terminal-tabs__close"
						title="Close session"
						aria-label="Close session"
						disabled={closing.has(session.id)}
						on:click={() => void closeSession(session.id)}
					>×</button>
				</div>
			{/each}
		</div>
	{/if}

	<div class="terminal-tabs__body">
		{#each sessions as session (session.id)}
			<div
				class="terminal-tabs__pane"
				class:terminal-tabs__pane--hidden={session.id !== activeSessionId}
				aria-hidden={session.id !== activeSessionId}
			>
				<InteractiveTerminalPane
					sessionId={session.id}
					program={session.program}
					active={session.id === activeSessionId}
					autofocus={session.id === activeSessionId}
					focusNonce={session.id === activeSessionId ? focusNonce : 0}
				/>
			</div>
		{/each}
	</div>
</div>

<style>
	.terminal-tabs {
		display: flex;
		flex-direction: column;
		flex: 1 1 auto;
		min-height: 0;
	}

	.terminal-tabs__strip {
		display: flex;
		gap: 4px;
		padding: 0;
		overflow-x: auto;
		flex-shrink: 0;
	}

	.terminal-tabs__tab {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 4px 4px 4px 10px;
		/* Inactive tab: soft surface tint over whatever the theme card
		   bg is, so it reads as "background tab" in both light and dark
		   modes. `--bg-soft` is theme-aware; previous hard-coded
		   `rgba(255,255,255,0.04)` rendered as nearly-invisible on
		   light themes. */
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.18));
		border-bottom: none;
		border-radius: 6px 6px 0 0;
		color: var(--text-muted, #888);
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 11px;
	}

	.terminal-tabs__tab--active {
		/* Active tab: match the terminal canvas so the tab visually
		   flows into the pane below. `--terminal-bg` → `--bg-card`
		   stays in sync with `buildXtermTheme()` in
		   InteractiveTerminalPane.svelte. */
		background: var(--terminal-bg, var(--bg-card, #111418));
		color: var(--terminal-fg, var(--text-primary, #e8e6e3));
		border-color: var(--border-soft, rgba(0, 0, 0, 0.18));
	}

	.terminal-tabs__tab--stale:not(.terminal-tabs__tab--active) {
		border-color: color-mix(in srgb, var(--color-warning, #d97706) 40%, transparent);
		color: var(--color-warning, #d97706);
	}

	.terminal-tabs__label {
		display: inline-flex;
		align-items: baseline;
		gap: 6px;
		background: transparent;
		border: 0;
		color: inherit;
		cursor: pointer;
		font-family: inherit;
		font-size: inherit;
		text-transform: lowercase;
		padding: 0;
	}

	.terminal-tabs__id {
		opacity: 0.55;
		font-size: 10px;
	}

	.terminal-tabs__meta {
		opacity: 0.65;
		font-size: 9.5px;
	}

	.terminal-tabs__stale {
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--color-warning, #d97706) 45%, transparent);
		color: var(--color-warning, #d97706);
		font-size: 9px;
		line-height: 1;
		padding: 2px 5px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.terminal-tabs__close {
		width: 18px;
		height: 18px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: inherit;
		background: transparent;
		border: 0;
		border-radius: 4px;
		cursor: pointer;
		font-size: 14px;
		line-height: 1;
		opacity: 0.6;
	}

	.terminal-tabs__close:hover:not(:disabled) {
		opacity: 1;
		/* `color-mix` with `currentColor` so hover bg uses the tab's
		   own text color at low alpha. Works for both light tabs
		   (dark text → dark hover) and dark tabs (light text →
		   light hover). Replaces the hard-coded
		   `rgba(255,255,255,0.08)` which was invisible on light
		   themes. */
		background: color-mix(in srgb, currentColor 12%, transparent);
	}

	.terminal-tabs__close:disabled {
		opacity: 0.3;
		cursor: not-allowed;
	}

	.terminal-tabs__body {
		flex: 1 1 auto;
		display: flex;
		min-height: 0;
		position: relative;
	}

	.terminal-tabs__pane {
		flex: 1 1 auto;
		display: flex;
		min-height: 0;
	}

	.terminal-tabs__pane--hidden {
		/* Keep mounted (scrollback preserved) but visually + a11y-wise out of band. */
		display: none;
	}
</style>
