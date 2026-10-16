<script lang="ts">
	/**
	 * AlertFeed — the kill-feed: transient one-liners from the realtime event
	 * stream (task completed/failed, approvals appearing, agents pausing…).
	 * Newest on top, each fades after ~9s, click jumps to the citizen when the
	 * event names one. Defensive parsing: unknown event shapes are skipped.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';

	export let docked = false;

	const dispatch = createEventDispatcher<{ jump: string }>();

	interface FeedLine {
		key: number;
		icon: string;
		text: string;
		agentId: string | null;
		at: number;
	}

	let lines: FeedLine[] = [];
	let seq = 0;
	let unsub: (() => void) | null = null;
	let sweeper: ReturnType<typeof setInterval> | null = null;

	const TTL_MS = 9_000;
	const MAX_LINES = 4;

	function rec(v: unknown): Record<string, unknown> {
		return v && typeof v === 'object' ? (v as Record<string, unknown>) : {};
	}
	function str(v: unknown): string | null {
		return typeof v === 'string' && v.trim() ? v : null;
	}

	/** Map a raw websocket event to a feed line (null = not feed-worthy). */
	function toLine(raw: unknown): Omit<FeedLine, 'key' | 'at'> | null {
		const e = rec(raw);
		const type = (str(e.event_type) ?? str(e.type) ?? '').toLowerCase();
		if (!type) return null;
		const payload = { ...e, ...rec(e.payload), ...rec(e.data) };
		const agentId = str(payload.agent_id);
		const title = str(payload.title) ?? str(payload.task_title) ?? str(payload.goal) ?? '';
		const short = title.length > 44 ? `${title.slice(0, 43)}…` : title;
		const who = agentId ?? 'the crew';

		if (type.includes('task') && type.includes('complet'))
			return { icon: '✅', text: `${who} completed ${short || 'a task'}`, agentId };
		if (type.includes('task') && type.includes('fail'))
			return { icon: '💥', text: `${who} failed ${short || 'a task'}`, agentId };
		if (type.includes('approval') && (type.includes('creat') || type.includes('request')))
			return { icon: '⚠', text: `${who} awaits your approval`, agentId };
		if (type.includes('agent') && type.includes('pause'))
			return { icon: '⏸', text: `${who} paused`, agentId };
		if (type.includes('agent') && type.includes('resume'))
			return { icon: '▶', text: `${who} resumed`, agentId };
		if (type.includes('execution') && type.includes('start'))
			return { icon: '⚔', text: `${who} set out${short ? ` — ${short}` : ''}`, agentId };
		return null;
	}

	function onEvents(events: unknown[]): void {
		const fresh: FeedLine[] = [];
		for (const raw of events) {
			const line = toLine(raw);
			if (line) fresh.push({ ...line, key: ++seq, at: Date.now() });
		}
		if (fresh.length === 0) return;
		lines = [...fresh.reverse(), ...lines].slice(0, MAX_LINES);
	}

	onMount(() => {
		unsub = v2Events.subscribe((events) => onEvents(events as unknown[]));
		sweeper = setInterval(() => {
			const cutoff = Date.now() - TTL_MS;
			if (lines.some((l) => l.at < cutoff)) lines = lines.filter((l) => l.at >= cutoff);
		}, 1_000);
	});
	onDestroy(() => {
		unsub?.();
		if (sweeper) clearInterval(sweeper);
	});
</script>

{#if lines.length > 0}
	<div class="af" class:af--docked={docked} aria-live="polite" aria-label="Realm events">
		{#each lines as line (line.key)}
			<button
				class="af__line"
				disabled={!line.agentId}
				on:click={() => line.agentId && dispatch('jump', line.agentId)}
			>
				<span class="af__icon">{line.icon}</span>{line.text}
			</button>
		{/each}
	</div>
{:else if docked}
	<p class="af__empty">No live events yet.</p>
{/if}

<style>
	.af--docked {
		position: relative;
		top: auto;
		right: auto;
		z-index: 0;
		align-items: stretch;
		max-width: none;
		padding: 0.6rem;
	}
	.af--docked .af__line {
		max-width: none;
		white-space: normal;
	}
	.af {
		pointer-events: auto;
		position: absolute;
		top: 3.2rem;
		right: 0.75rem;
		z-index: 4;
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.25rem;
		max-width: 20rem;
	}
	.af__line {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.22rem 0.55rem;
		border-radius: 999px;
		border: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.3));
		background: var(--bg-card, rgba(255, 255, 255, 0.82));
		backdrop-filter: blur(6px);
		color: var(--text-primary, #222);
		font-family: var(--font-primary, system-ui);
		font-size: 0.72rem;
		cursor: pointer;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 100%;
		animation: af-in 220ms ease;
	}
	.af__line:disabled {
		cursor: default;
	}
	.af__icon {
		font-size: 0.78rem;
	}
	.af__empty {
		margin: 0;
		padding: 1rem 0.8rem;
		color: var(--text-secondary, #667085);
		font-size: 0.82rem;
	}
	@keyframes af-in {
		from {
			opacity: 0;
			transform: translateX(0.6rem);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}
</style>
