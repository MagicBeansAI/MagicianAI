<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { LONG_FETCH_TIMEOUT_MS, timedFetch } from '$lib/shared/fetch';

	export let taskId: string | null = null;
	export let maxRows = 8;

	type ConnectionState = 'idle' | 'connecting' | 'live' | 'error' | 'closed';

	type PlanningActivityRow = {
		key: string;
		phase: string;
		detail: string | null;
		planId: string | null;
		timestampMs: number | null;
		arrival: number;
	};

	let rows: PlanningActivityRow[] = [];
	let controller: AbortController | null = null;
	let connectionState: ConnectionState = 'idle';
	let connectionMessage = '';
	let mounted = false;
	let lastConnectKey = '';
	let seenSerialized = new Set<string>();
	let rowSequence = 0;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

	$: if (mounted) {
		const scope = $scopeIdentityStore;
		const nextKey = `${taskId ?? ''}::${scope.principal}::${scope.workspace}`;
		if (nextKey !== lastConnectKey) {
			lastConnectKey = nextKey;
			void connect();
		}
	}

	onMount(() => {
		mounted = true;
	});

	onDestroy(() => {
		mounted = false;
		if (reconnectTimer) clearTimeout(reconnectTimer);
		void disconnect();
	});

	async function connect(): Promise<void> {
		await disconnect();
		rows = [];
		seenSerialized = new Set();
		rowSequence = 0;
		connectionMessage = '';

		const scope = $scopeIdentityStore;
		if (!browser || !taskId || !scope.principal || !scope.workspace) {
			connectionState = 'idle';
			return;
		}

		const nextController = new AbortController();
		controller = nextController;
		connectionState = 'connecting';

		const params = new URLSearchParams({
			task_id: taskId,
			event_type: 'V3PlanningProgress',
			limit: String(Math.max(maxRows * 4, 24))
		});

		try {
			const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
				headers: {
				},
				signal: nextController.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (controller !== nextController) return;
			if (!response.ok) {
				connectionState = 'error';
				connectionMessage = `${response.status} ${response.statusText}`;
				return;
			}
			if (!response.body) {
				connectionState = 'error';
				connectionMessage = 'no response body';
				return;
			}

			connectionState = 'live';
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			while (true) {
				const { done, value } = await reader.read();
				if (controller !== nextController) return;
				if (done) {
					connectionState = 'closed';
					connectionMessage = 'reconnecting';
					scheduleReconnect(nextController);
					return;
				}
				buffer += decoder.decode(value, { stream: true });
				let newlineIndex: number;
				while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, newlineIndex);
					buffer = buffer.slice(newlineIndex + 1);
					ingestLine(line);
				}
			}
		} catch (error) {
			if (controller !== nextController) return;
			if (nextController.signal.aborted || isBenignStreamAbort(error)) {
				connectionState = 'closed';
				connectionMessage = mounted ? 'reconnecting' : 'disconnected';
				if (mounted) scheduleReconnect(nextController);
				return;
			}
			connectionState = 'error';
			connectionMessage = error instanceof Error ? error.message : String(error);
			scheduleReconnect(nextController);
		}
	}

	function scheduleReconnect(expectedController: AbortController): void {
		if (!mounted || controller !== expectedController || reconnectTimer) return;
		reconnectTimer = setTimeout(() => {
			reconnectTimer = null;
			if (mounted && controller === expectedController) void connect();
		}, 750);
	}

	function isBenignStreamAbort(error: unknown): boolean {
		if (error instanceof DOMException && error.name === 'AbortError') return true;
		const message = error instanceof Error ? error.message : String(error);
		return /BodyStreamBuffer.*aborted|operation was aborted|request was aborted/i.test(message);
	}

	async function disconnect(): Promise<void> {
		if (controller) {
			controller.abort();
			controller = null;
		}
	}

	function ingestLine(serialized: string): void {
		const trimmed = serialized.trim();
		if (!trimmed || seenSerialized.has(trimmed)) return;
		seenSerialized.add(trimmed);

		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(trimmed) as Record<string, unknown>;
		} catch {
			return;
		}

		if (String(parsed.event_type ?? '') !== 'V3PlanningProgress') return;
		const data = objectRecord(parsed.data);
		if (!data) return;

		const timestampMs = extractTimestampMs(parsed, data);
		const phase = stringOrNull(data.phase) ?? 'planning';
		const detail = stringOrNull(data.detail);
		const planId = stringOrNull(data.plan_id);
		const arrival = rowSequence++;
		const key = `${planId ?? 'plan'}::${phase}::${detail ?? ''}::${timestampMs ?? arrival}`;
		if (rows.some((row) => row.key === key)) return;

		const nextRows = [
			{ key, phase, detail, planId, timestampMs, arrival },
			...rows
		].sort((left, right) => {
			const leftTime = left.timestampMs ?? left.arrival;
			const rightTime = right.timestampMs ?? right.arrival;
			return rightTime - leftTime;
		});
		rows = nextRows.slice(0, maxRows);
	}

	function objectRecord(value: unknown): Record<string, unknown> | null {
		if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
		return value as Record<string, unknown>;
	}

	function stringOrNull(value: unknown): string | null {
		if (typeof value !== 'string') return null;
		const trimmed = value.trim();
		return trimmed.length > 0 ? trimmed : null;
	}

	function extractTimestampMs(
		raw: Record<string, unknown>,
		data: Record<string, unknown>
	): number | null {
		const candidates = [
			raw.timestamp_ms,
			raw.timestamp,
			data.timestamp_ms,
			data.timestamp,
			data.started_at,
			data.finished_at
		];
		for (const candidate of candidates) {
			if (typeof candidate === 'number' && Number.isFinite(candidate) && candidate !== 0) {
				return candidate < 10_000_000_000 ? candidate * 1000 : candidate;
			}
			if (typeof candidate === 'string') {
				const parsed = Date.parse(candidate);
				if (Number.isFinite(parsed)) return parsed;
			}
		}
		return null;
	}

	function phaseLabel(phase: string): string {
		return phase
			.replace(/[._-]+/g, ' ')
			.replace(/\s+/g, ' ')
			.trim()
			.replace(/\b\w/g, (char) => char.toUpperCase());
	}

	function phaseTone(phase: string): 'active' | 'done' | 'error' {
		const normalized = phase.toLowerCase();
		if (normalized.includes('error') || normalized.includes('fail')) return 'error';
		if (
			normalized.includes('complete') ||
			normalized.includes('generated') ||
			normalized.includes('atomic_plan')
		) {
			return 'done';
		}
		return 'active';
	}

	function formatActivityTime(timestampMs: number | null): string {
		if (timestampMs === null) return 'live';
		try {
			return new Date(timestampMs).toLocaleTimeString(undefined, {
				hour: '2-digit',
				minute: '2-digit',
				second: '2-digit',
				hour12: false
			});
		} catch {
			return 'live';
		}
	}

	$: statusLabel =
		connectionState === 'live' ? 'Live' :
		connectionState === 'connecting' ? 'Connecting' :
		connectionState === 'error' ? 'Error' :
		connectionState === 'closed' ? 'Closed' :
		'Idle';
</script>

<div class="planning-activity" aria-live="polite">
	<div class="planning-activity__meta">
		<span class="planning-activity__state state-{connectionState}">
			<span class="planning-activity__dot" aria-hidden="true"></span>
			{statusLabel}
		</span>
		<span class="planning-activity__count">{rows.length} {rows.length === 1 ? 'event' : 'events'}</span>
		{#if connectionMessage}
			<span class="planning-activity__message">{connectionMessage}</span>
		{/if}
	</div>

	{#if rows.length === 0}
		<div class="planning-activity__empty">
			{#if connectionState === 'connecting'}
				Connecting to planning activity.
			{:else if connectionState === 'error'}
				Could not load planning activity.
			{:else}
				Waiting for planning progress.
			{/if}
		</div>
	{:else}
		<ol class="planning-activity__list">
			{#each rows as row (row.key)}
				{@const tone = phaseTone(row.phase)}
				<li class="planning-activity__row tone-{tone}">
					<span class="planning-activity__marker" aria-hidden="true"></span>
					<div class="planning-activity__copy">
						<div class="planning-activity__row-header">
							<strong>{phaseLabel(row.phase)}</strong>
							<time>{formatActivityTime(row.timestampMs)}</time>
						</div>
						{#if row.detail}
							<p>{row.detail}</p>
						{/if}
					</div>
				</li>
			{/each}
		</ol>
	{/if}
</div>

<style>
	.planning-activity {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		min-width: 0;
	}

	.planning-activity__meta {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.45rem 0.65rem;
		min-width: 0;
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.planning-activity__state {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-mono);
	}

	.planning-activity__dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--text-muted);
	}

	.state-live .planning-activity__dot {
		background: var(--accent-primary);
		animation: planning-activity-pulse 1.4s ease-in-out infinite;
	}

	.state-error .planning-activity__dot {
		background: var(--color-error);
	}

	.planning-activity__count,
	.planning-activity__message {
		font-family: var(--font-mono);
	}

	.planning-activity__message {
		color: var(--color-error);
	}

	.planning-activity__empty {
		padding: 0.85rem;
		border: 1px dashed var(--border-soft);
		border-radius: 8px;
		color: var(--text-muted);
		font-size: 0.82rem;
		text-align: center;
		background: color-mix(in srgb, var(--bg-soft, #f6f1e8) 60%, transparent);
	}

	.planning-activity__list {
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		margin: 0;
		padding: 0;
		max-height: 15rem;
		overflow-y: auto;
	}

	.planning-activity__row {
		display: grid;
		grid-template-columns: 0.65rem minmax(0, 1fr);
		gap: 0.55rem;
		padding: 0.55rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
	}

	.planning-activity__marker {
		width: 0.55rem;
		height: 0.55rem;
		margin-top: 0.28rem;
		border-radius: 999px;
		background: var(--accent-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 16%, transparent);
	}

	.tone-done .planning-activity__marker {
		background: var(--color-success, #2e9d68);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--color-success, #2e9d68) 16%, transparent);
	}

	.tone-error .planning-activity__marker {
		background: var(--color-error);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--color-error) 16%, transparent);
	}

	.planning-activity__copy {
		min-width: 0;
	}

	.planning-activity__row-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
	}

	.planning-activity__row-header strong {
		font-size: 0.86rem;
		color: var(--text-primary);
	}

	.planning-activity__row-header time {
		flex: 0 0 auto;
		font-family: var(--font-mono);
		font-size: 0.7rem;
		color: var(--text-muted);
	}

	.planning-activity__copy p {
		margin: 0.22rem 0 0;
		font-size: 0.8rem;
		line-height: 1.35;
		color: var(--text-secondary, var(--text-muted));
	}

	@keyframes planning-activity-pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.4;
		}
	}
</style>
