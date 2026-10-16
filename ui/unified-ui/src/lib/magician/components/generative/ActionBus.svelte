<script lang="ts">
	import { onDestroy } from 'svelte';
	import {
		v2Events,
		type V2WebSocketEvent,
		getV2EventSequence
	} from '$lib/realtime/v2-websocket';

	interface ActionBusAction {
		id: string;
		label: string;
		goal_id: string;
		disabled?: boolean;
		trigger?: string;
	}

	const RATE_LIMIT_WINDOW_MS = 5000;
	const PENDING_ACK_TIMEOUT_MS = 12000;
	const connectionStatus = v2Events.connectionStatus;

	export let actions: ActionBusAction[] = [];
	export let agentId: string = '';
	export let componentId: string = '';
	export let title: string = '';
	export let disabled: boolean = false;

	let pendingActionId: string | null = null;
	let selectedActionId: string | null = null;
	let activeCycleId: string | null = null;
	let feedbackMessage: string = '';
	let errorMessage: string | null = null;
	let rateLimitedUntil = 0;
	let nowMs = Date.now();
	let rateLimitTicker: ReturnType<typeof setInterval> | null = null;
	let pendingAckTimer: ReturnType<typeof setTimeout> | null = null;
	let lastProcessedEventSequence = 0;
	let routingSignature = '';
	let lastConnectionStatus: 'connected' | 'connecting' | 'disconnected' = 'disconnected';

	$: hasContext = agentId.trim().length > 0 && componentId.trim().length > 0;
	$: safeActions = normalizeActions(actions);
	$: rateLimitRemainingMs = Math.max(0, rateLimitedUntil - nowMs);
	$: rateLimitRemainingSeconds = Math.ceil(rateLimitRemainingMs / 1000);
	$: isRateLimited = rateLimitRemainingMs > 0;
	$: isBusy = pendingActionId !== null || activeCycleId !== null;
	$: canSubmit = hasContext && !disabled && !isBusy && !isRateLimited;

	$: if (!isRateLimited && rateLimitTicker) {
		stopRateLimitTicker();
	}

	$: {
		if ($connectionStatus !== lastConnectionStatus) {
			lastConnectionStatus = $connectionStatus;
			if ($connectionStatus === 'disconnected' && isBusy) {
				resetBusyState('Realtime disconnected before action completion. You can retry.');
			}
		}
	}

	$: {
		// Structured signature avoids delimiter-based collisions.
		const nextSignature = JSON.stringify([agentId.trim(), componentId.trim()]);
		if (nextSignature !== routingSignature) {
			routingSignature = nextSignature;
			pendingActionId = null;
			selectedActionId = null;
			activeCycleId = null;
			feedbackMessage = '';
			errorMessage = null;
			rateLimitedUntil = 0;
			nowMs = Date.now();
			// Skip historical global frames when routing context changes.
			lastProcessedEventSequence = latestEventSequence($v2Events);
			stopRateLimitTicker();
			stopPendingAckTimer();
		}
	}

	$: if ($v2Events.length > 0) {
		processIncomingEvents($v2Events);
	}

	onDestroy(() => {
		stopRateLimitTicker();
		stopPendingAckTimer();
	});

	function asTrimmedString(value: unknown): string {
		if (typeof value === 'string') return value.trim();
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value).trim();
		}
		return '';
	}

	function normalizeActions(input: ActionBusAction[]): ActionBusAction[] {
		if (!Array.isArray(input)) return [];
		const seen = new Set<string>();
		const normalized: ActionBusAction[] = [];
		for (const candidate of input as unknown[]) {
			if (candidate == null || typeof candidate !== 'object' || Array.isArray(candidate)) continue;
			const rec = candidate as Record<string, unknown>;
			const id = asTrimmedString(rec.id);
			const goalId = asTrimmedString(rec.goal_id);
			const label = asTrimmedString(rec.label || rec.id);
			const trigger = asTrimmedString(rec.trigger);
			if (!id || !goalId || !label) continue;
			if (seen.has(id)) continue;
			seen.add(id);
			normalized.push({
				id,
				goal_id: goalId,
				label,
				disabled: rec.disabled === true,
				...(trigger ? { trigger } : {})
			});
		}
		return normalized;
	}

	function processIncomingEvents(events: V2WebSocketEvent[]): void {
		const pending = events
			.map((event) => ({ event, seq: getV2EventSequence(event) }))
			.filter(({ seq }) => seq > lastProcessedEventSequence)
			.sort((left, right) => left.seq - right.seq);

		for (const { event, seq } of pending) {
			lastProcessedEventSequence = Math.max(lastProcessedEventSequence, seq);

			if (event.event_type === 'UiInteractionAck') {
				const data = event.data;
				if (!matchesInteractionRouting(data.agent_id, data.component_id)) continue;
				pendingActionId = null;
				stopPendingAckTimer();
				errorMessage = null;
				feedbackMessage = data.status === 'queued'
					? 'Action queued.'
					: 'Action accepted.';
				activeCycleId = data.cycle_id && data.cycle_id !== 'unknown'
					? data.cycle_id
					: null;
				continue;
			}

			if (event.event_type === 'UiInteractionError') {
				const data = event.data;
				if (!matchesInteractionRouting(data.agent_id, data.component_id)) continue;
				pendingActionId = null;
				activeCycleId = null;
				stopPendingAckTimer();
				feedbackMessage = '';
				errorMessage = data.error || 'Action dispatch failed';
				if (data.code === 'rate_limited') {
					nowMs = Date.now();
					rateLimitedUntil = nowMs + RATE_LIMIT_WINDOW_MS;
					startRateLimitTicker();
				}
				continue;
			}

			if (event.event_type === 'AgentCycleCompleted' && activeCycleId) {
				if (
					event.data.agent_id === agentId.trim()
					&& event.data.cycle_id === activeCycleId
				) {
					activeCycleId = null;
					selectedActionId = null;
					feedbackMessage = 'Action cycle completed.';
					errorMessage = null;
				}
			}
		}
	}

	function latestEventSequence(events: V2WebSocketEvent[]): number {
		let latest = 0;
		for (const event of events) {
			latest = Math.max(latest, getV2EventSequence(event));
		}
		return latest;
	}

	function matchesInteractionRouting(eventAgentId: string, eventComponentId: string): boolean {
		return eventAgentId === agentId.trim() && eventComponentId === componentId.trim();
	}

	function startRateLimitTicker(): void {
		if (rateLimitTicker) return;
		rateLimitTicker = setInterval(() => {
			nowMs = Date.now();
			if (nowMs >= rateLimitedUntil) {
				stopRateLimitTicker();
			}
		}, 250);
	}

	function stopRateLimitTicker(): void {
		if (!rateLimitTicker) return;
		clearInterval(rateLimitTicker);
		rateLimitTicker = null;
	}

	function startPendingAckTimer(actionId: string): void {
		stopPendingAckTimer();
		pendingAckTimer = setTimeout(() => {
			if (pendingActionId !== actionId) return;
			resetBusyState('No acknowledgement received. You can retry.');
		}, PENDING_ACK_TIMEOUT_MS);
	}

	function stopPendingAckTimer(): void {
		if (!pendingAckTimer) return;
		clearTimeout(pendingAckTimer);
		pendingAckTimer = null;
	}

	function resetBusyState(message: string): void {
		pendingActionId = null;
		selectedActionId = null;
		activeCycleId = null;
		stopPendingAckTimer();
		feedbackMessage = '';
		errorMessage = message;
	}

	function sendInteraction(action: ActionBusAction): void {
		if (!hasContext) {
			errorMessage = 'ActionBus is missing routing context (agent/component).';
			return;
		}
		if (!canSubmit || action.disabled) return;

		const sent = v2Events.send({
			type: 'ui.interaction',
			agent_id: agentId.trim(),
			component_id: componentId.trim(),
			goal_id: action.goal_id,
			trigger: action.trigger || action.id
		});

		if (!sent) {
			errorMessage = 'Realtime connection is not ready.';
			feedbackMessage = '';
			return;
		}

		pendingActionId = action.id;
		selectedActionId = action.id;
		feedbackMessage = `Triggering "${action.label}"...`;
		errorMessage = null;
		startPendingAckTimer(action.id);
	}

	function buttonBusy(actionId: string): boolean {
		if (pendingActionId === actionId) return true;
		return activeCycleId !== null && selectedActionId === actionId;
	}
</script>

<div class="muij-actionbus" aria-live="polite">
    {#if title.trim().length > 0}
        <div class="muij-actionbus-title">{title}</div>
    {/if}

    {#if safeActions.length === 0}
        <div class="muij-actionbus-empty">No actions configured.</div>
    {:else}
        <div class="muij-actionbus-grid">
            {#each safeActions as action (action.id)}
                <button
                    type="button"
                    class="muij-actionbus-button"
                    disabled={!canSubmit || !!action.disabled}
                    on:click={() => sendInteraction(action)}
                >
                    {#if buttonBusy(action.id)}
                        <span class="muij-actionbus-spinner" aria-hidden="true"></span>
                    {/if}
                    <span class="muij-actionbus-label">{action.label}</span>
                </button>
            {/each}
        </div>
    {/if}

    {#if isRateLimited}
        <div class="muij-actionbus-meta">
            Rate limited. Try again in {rateLimitRemainingSeconds}s.
        </div>
    {:else if errorMessage}
        <div class="muij-actionbus-error">{errorMessage}</div>
    {:else if feedbackMessage}
        <div class="muij-actionbus-meta">{feedbackMessage}</div>
    {/if}
</div>

<style>
    .muij-actionbus {
        display: flex;
        flex-direction: column;
        gap: var(--space-sm);
    }

    .muij-actionbus-title {
        font-family: var(--font-primary);
        font-size: 0.75rem;
        font-weight: 600;
        color: var(--text-secondary);
        letter-spacing: 0.01em;
    }

    .muij-actionbus-grid {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-xs);
    }

    .muij-actionbus-button {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        min-height: 32px;
        padding: 6px 12px;
        border-radius: var(--radius-md);
        border: 1px solid var(--border-soft);
        background: var(--bg-card);
        color: var(--text-body);
        font-family: var(--font-primary);
        font-size: 0.75rem;
        font-weight: 500;
        cursor: pointer;
        transition: background 0.2s ease, border-color 0.2s ease, color 0.2s ease;
    }

    .muij-actionbus-button:hover:not(:disabled) {
        background: var(--bg-soft);
        border-color: var(--accent-primary);
    }

    .muij-actionbus-button:disabled {
        opacity: 0.6;
        cursor: default;
    }

    .muij-actionbus-spinner {
        width: 12px;
        height: 12px;
        border: 2px solid var(--border-soft);
        border-top-color: var(--accent-primary);
        border-radius: 999px;
        animation: muij-actionbus-spin 0.8s linear infinite;
        flex: 0 0 auto;
    }

    .muij-actionbus-label {
        overflow-wrap: anywhere;
    }

    .muij-actionbus-meta {
        font-family: var(--font-primary);
        font-size: 0.6875rem;
        color: var(--text-secondary);
        overflow-wrap: anywhere;
    }

    .muij-actionbus-error {
        font-family: var(--font-primary);
        font-size: 0.6875rem;
        color: var(--accent-danger, #b91c1c);
        overflow-wrap: anywhere;
    }

    .muij-actionbus-empty {
        font-family: var(--font-primary);
        font-size: 0.6875rem;
        color: var(--text-secondary);
        font-style: italic;
    }

    @keyframes muij-actionbus-spin {
        from { transform: rotate(0deg); }
        to { transform: rotate(360deg); }
    }
</style>
