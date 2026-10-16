<script lang="ts">
	/**
	 * FleetWorld — the living-world hero of the Fleet Civilization (/square).
	 *
	 * Hosts the Three.js FleetEngine (isometric Civilization world: terrain,
	 * roads, guild buildings, citizens walking A* paths) and the Jarvis HUD
	 * overlay (HTML, app-themed, progressive disclosure — see the design doc's
	 * "Jarvis HUD" section). This component maps AgentSummary → CitizenVM/GuildVM
	 * view-models; the engine knows nothing about stores.
	 */
	import { createEventDispatcher, onMount, onDestroy } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { loadAgents, type AgentSummary } from '$lib/stores/agentStore';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { FleetEngine } from './engine/engine';
	import { agentTarget, type CitizenVM, type GuildVM, type GodHandDrop } from './engine/types';
	import { showError } from '$lib/shared/stores/notifications';
	import {
		fetchCrewHealthCached,
		healthByAgent,
		type AgentHealthProjection
	} from '$lib/magician/crew/health';
	import {
		citizenVibeOf,
		guildIdOf,
		guildNameOf,
		isEnvoyOf,
		isCeoOf,
		boardReviewGoalIdOf,
		displayNameOf,
		normalizeProgram,
		titleOf
	} from './derive';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { groupNeedsByAgent } from './attentionGlue';
	import { fetchHandoffEdges } from './fleetHandoffs';
	import {
		fetchFleetState,
		fleetStateCurrentWork,
		fleetStateHandoffEdges,
		fleetStateProgramIds,
		fleetStateQuests,
		type FleetStateSnapshot
	} from './fleetState';
	import { fetchLocalWeather, getObserverCoords } from './fleetWeather';
	import FleetHud from './hud/FleetHud.svelte';
	import './fleet-theme.css';
	import './game-chrome.css';

	export let agents: AgentSummary[] = [];
	/** Render-pause: true when the hero scrolls off-view / immerse closed. */
	export let paused = false;
	/** Selection owned by the page dock — the engine reports changes up. */
	export let selectedTarget: string | null = null;

	export function focusAgent(id: string): void {
		if (!engine) return;
		engine.select(agentTarget(id));
		engine.focus(agentTarget(id));
	}

	const dispatch = createEventDispatcher<{ select: { target: string | null } }>();

	let hostEl: HTMLElement;
	let canvasEl: HTMLCanvasElement;
	let engine: FleetEngine | null = null;
	let hoverTarget: string | null = null;
	let cameraMoved = false;
	/** A God-Hand drop awaiting its steer message (composer in the HUD). */
	let godHandDrop: { citizen: CitizenVM; guild: GuildVM } | null = null;

	function onGodHand(drop: GodHandDrop): void {
		if (!drop.guildId) return; // dropped on open ground — just put them back
		const citizen = citizens.find((c) => c.id === drop.citizenId);
		const guild = guilds.find((g) => g.id === drop.guildId);
		if (!citizen || !guild) return;
		if (!citizen.executionId) {
			showError(`${citizen.name} has no live run to steer — assign work first.`);
			return;
		}
		godHandDrop = { citizen, guild };
	}

	/** Canonical overall health + rolling seven-day operations. */
	let healthMap: Map<string, AgentHealthProjection> | null = null;
	let healthTimer: ReturnType<typeof setInterval> | null = null;
	async function refreshHealth(): Promise<void> {
		const next = healthByAgent(await fetchCrewHealthCached());
		if (next) healthMap = next;
	}

	/** Canonical scoped game snapshot. A slower cadence avoids rebuilding
	 * execution trees aggressively; realtime events trigger handoff fallbacks. */
	let fleetState: FleetStateSnapshot | null = null;
	let fleetStateTimer: ReturnType<typeof setInterval> | null = null;
	async function refreshFleetState(): Promise<void> {
		const next = await fetchFleetState();
		if (!next) return;
		fleetState = next;
		if (next.availability.handoffs.status !== 'unavailable') {
			engine?.setHandoffs(fleetStateHandoffEdges(next));
		}
	}

	/** Live delegation edges → hand-off beams in the world. The 20s poll is
	 * the backstop; task/execution websocket events kick an immediate
	 * (debounced) refresh so beams appear within ~2s of a real delegation. */
	let handoffTimer: ReturnType<typeof setInterval> | null = null;
	let handoffKick: ReturnType<typeof setTimeout> | null = null;
	let unsubEvents: (() => void) | null = null;
	async function refreshHandoffs(): Promise<void> {
		if (fleetState && fleetState.availability.handoffs.status !== 'unavailable') {
			engine?.setHandoffs(fleetStateHandoffEdges(fleetState));
			return;
		}
		const edges = await fetchHandoffEdges();
		if (edges) engine?.setHandoffs(edges);
	}
	function onRealtimeEvents(events: unknown[]): void {
		const relevant = events.some((e) => {
			const rec = e as { event_type?: unknown; type?: unknown };
			return /task|execution|delegat/i.test(String(rec?.event_type ?? rec?.type ?? ''));
		});
		if (!relevant || handoffKick) return;
		handoffKick = setTimeout(() => {
			handoffKick = null;
			void refreshHandoffs();
		}, 1_500);
	}

	let agentTimer: ReturnType<typeof setInterval> | null = null;
	let unsubHealth: (() => void) | null = null;

	/** Real local weather -> clouds/rain/snow (best-effort; clear otherwise). */
	let weatherTimer: ReturnType<typeof setInterval> | null = null;
	let lastWeather: import('./engine/environment').WeatherState | null = null;
	async function refreshWeather(): Promise<void> {
		const weather = await fetchLocalWeather();
		if (weather) {
			lastWeather = weather;
			engine?.setWeather(weather);
		}
	}

	/** Needs-you items from the attention funnel, grouped per citizen — the
	 * [!] bubble opens straight onto these, actionable in the Citizen panel. */
	$: needsByAgent = groupNeedsByAgent($attentionStore);

	/** Backend liveness (the realtime socket): when it drops, statuses are
	 * stale — the crew goes off duty (relaxed wandering) under a red badge. */
	let backendDown = false;
	/** Slow tick so resting (staleness-based) re-derives without churn. */
	let nowTick = Date.now();
	let tickTimer: ReturnType<typeof setInterval> | null = null;
	/** Idle this long with no activity -> napping at the bench. */
	const RESTING_AFTER_MS = 30 * 60_000;
	$: fleetCitizenById = new Map(
		(fleetState?.citizens ?? []).map((citizen) => [citizen.citizen_id, citizen])
	);
	$: authoritativeQuests =
		fleetState && fleetState.availability.quests.status !== 'unavailable'
			? fleetStateQuests(fleetState)
			: null;
	$: taskBlockedIds = new Set(
		(authoritativeQuests ?? [])
			.filter(
				(quest) =>
					quest.isBlocked
					|| quest.pendingQuestions.length > 0
					|| quest.state === 'awaiting_orders'
					|| quest.state === 'blocked'
			)
			.map((quest) => quest.id)
	);

	$: citizens = agents.map((a): CitizenVM => {
		const projected = fleetCitizenById.get(a.agent_id);
		const currentWork = fleetStateCurrentWork(projected, taskBlockedIds);
		const guildIds = fleetStateProgramIds(projected);
		const primaryGuildId = guildIds[0] ?? guildIdOf(a);
		let vibe = citizenVibeOf(
			a,
			currentWork,
			(needsByAgent.get(a.agent_id)?.length ?? 0) > 0
		);
		// Backend down: claimed statuses can't be trusted — everyone off duty.
		if (backendDown && vibe !== 'offline') vibe = 'idle';
		const health = healthMap?.get(a.agent_id);
		const lastActive = Math.max(health?.overall.inputs.last_activity_at_ms ?? 0, a.updated_at || 0);
		const resting =
			!backendDown && vibe === 'idle' && lastActive > 0 && nowTick - lastActive > RESTING_AFTER_MS;
		return {
			resting,
			id: a.agent_id,
			name: projected?.display_name || displayNameOf(a),
			role: projected?.description || a.description || a.agent_id,
			title: titleOf(a),
			trustLevel: a.trust_level ?? null,
			bornAt: a.created_at ?? null,
			quests: (a.autonomous_config?.focus_areas ?? []).map((f) => ({
				name: f.name,
				program: f.program?.trim() || null,
				schedule: f.schedule ?? null,
				priority: f.priority ?? null
			})),
			vibe,
			guildId: primaryGuildId,
			guildIds: guildIds.length > 0 ? guildIds : [primaryGuildId],
			currentWork,
			executionId: currentWork.find((work) => work.executionId)?.executionId ?? a.current_execution_id,
			pendingApprovals: a.pending_approvals ?? 0,
			lastGoal: currentWork[0]?.title ?? a.last_goal,
			lastOutcome: a.last_outcome,
			updatedAt: a.updated_at,
			health: health?.overall.score ?? null,
			healthCoverage: health?.overall.coverage.ratio ?? null,
			healthAverage7d: health?.rolling_7d.score_average ?? null,
			healthDelta7d: health?.rolling_7d.score_delta ?? null,
			healthSampleDays7d: health?.rolling_7d.sample_days ?? 0,
			spendUsd7d: health?.rolling_7d.spend_usd ?? null,
			successRate7d: health?.rolling_7d.success_rate ?? null,
			calls7d: health?.rolling_7d.calls ?? null,
			isPrimary: a.is_primary ?? false,
			isEnvoy: isEnvoyOf(a),
			isCeo: isCeoOf(a),
			boardReviewGoalId: boardReviewGoalIdOf(a),
			tools: a.tools ?? [],
			delegationTargets: a.delegation_targets ?? []
		};
	});

	$: guilds = ((): GuildVM[] => {
		const byId = new Map<string, GuildVM>();
		if (fleetState?.availability.guilds.status !== 'unavailable') {
			for (const guild of fleetState?.guilds ?? []) {
				const id = normalizeProgram(guild.program_id);
				byId.set(id, {
					id,
					name: guild.title,
					memberIds: citizens.filter((citizen) => citizen.guildIds.includes(id)).map((citizen) => citizen.id),
					missionsMarkdown: guild.missions_markdown,
					activeQuestCount: 0,
					blockedQuestCount: 0,
					deliveredQuestCount: 0
				});
			}
		}
		for (const c of citizens) {
			const g = byId.get(c.guildId);
			if (g) {
				if (!g.memberIds.includes(c.id)) g.memberIds.push(c.id);
			}
			else byId.set(c.guildId, {
				id: c.guildId,
				name: guildNameOf(c.guildId),
				memberIds: [c.id],
				activeQuestCount: 0,
				blockedQuestCount: 0,
				deliveredQuestCount: 0
			});
		}
		for (const guild of byId.values()) {
			const quests = (authoritativeQuests ?? []).filter(
				(quest) => quest.agentId && guild.memberIds.includes(quest.agentId)
			);
			guild.activeQuestCount = quests.filter((quest) =>
				['active', 'planning', 'delivering'].includes(quest.state)
			).length;
			guild.blockedQuestCount = quests.filter((quest) =>
				['blocked', 'awaiting_orders', 'failed'].includes(quest.state)
			).length;
			guild.deliveredQuestCount = quests.filter((quest) => quest.state === 'succeeded').length;
		}
		return Array.from(byId.values()).sort((a, b) => a.id.localeCompare(b.id));
	})();

	$: engine?.setData(citizens, guilds);
	$: engine?.setPaused(paused);
	$: if (engine) engine.select(selectedTarget);

	onMount(() => {
		engine = new FleetEngine(hostEl, canvasEl);
		engine.callbacks = {
			onHover: (t) => (hoverTarget = t),
			onSelect: (t) => dispatch('select', { target: t }),
			onCameraMoved: (m) => (cameraMoved = m),
			onGodHand: (drop) => onGodHand(drop)
		};
		engine.init();
		// dev-only escape hatch for scene inspection from the console
		if (import.meta.env.DEV) (window as unknown as Record<string, unknown>).__fleetEngine = engine;
		engine.setData(citizens, guilds);
		engine.setPaused(paused);
		void refreshHealth();
		healthTimer = setInterval(() => void refreshHealth(), 120_000);
		void refreshFleetState();
		fleetStateTimer = setInterval(() => void refreshFleetState(), 30_000);
		void refreshHandoffs();
		handoffTimer = setInterval(() => void refreshHandoffs(), 20_000);
		void refreshWeather();
		weatherTimer = setInterval(() => void refreshWeather(), 15 * 60_000);
		// true solar/lunar positions once coordinates are known (shared prompt)
		void getObserverCoords().then((c) => {
			if (c) engine?.setObserver(c.lat, c.lon);
		});
		attentionStore.start();
		// movement <-> work stays live: re-poll the roster so bench/wander
		// transitions track real executions (statuses would otherwise go stale)
		agentTimer = setInterval(() => {
			if (!backendDown) void loadAgents({ replace: true, clearError: true });
		}, 30_000);
		tickTimer = setInterval(() => (nowTick = Date.now()), 60_000);
		unsubHealth = v2Events.connectionStatus.subscribe((s) => {
			backendDown = s === 'disconnected';
		});
		unsubEvents = v2Events.subscribe((events) => onRealtimeEvents(events as unknown[]));
	});

	onDestroy(() => {
		if (healthTimer) clearInterval(healthTimer);
		if (fleetStateTimer) clearInterval(fleetStateTimer);
		if (handoffTimer) clearInterval(handoffTimer);
		if (weatherTimer) clearInterval(weatherTimer);
		if (agentTimer) clearInterval(agentTimer);
		if (tickTimer) clearInterval(tickTimer);
		if (handoffKick) clearTimeout(handoffKick);
		unsubEvents?.();
		unsubHealth?.();
		attentionStore.stop();
		engine?.dispose();
		engine = null;
	});
</script>

<div class="fleet-world" class:fleet-world--offline={backendDown} bind:this={hostEl}>
	<canvas bind:this={canvasEl}></canvas>
	{#if backendDown}
		<div class="fleet-world__offline-badge" role="status">
			<Icon name="alert" size={14} /> Backend offline - crew off duty
		</div>
	{/if}
	{#if engine}
		<FleetHud
			{engine}
			{citizens}
			{guilds}
			{hoverTarget}
			{selectedTarget}
			{cameraMoved}
			{needsByAgent}
			{godHandDrop}
			{authoritativeQuests}
			deliveries={fleetState?.deliveries ?? []}
			on:godhanddone={() => (godHandDrop = null)}
		/>
	{/if}
	{#if agents.length === 0}
		<div class="fleet-world__empty">Summoning the crew…</div>
	{/if}
</div>

<style>
	.fleet-world {
		position: absolute;
		inset: 0;
		overflow: hidden;
	}
	.fleet-world canvas {
		display: block;
		width: 100%;
		height: 100%;
		/* No image-rendering override: the engine renders at the device ratio,
		 * so there is no upscale to control. `pixelated` here used to pair with a
		 * deliberate 1/3 downscale and would now only fight the compositor. */
	}
	/* subtle offline treatment: thin red inner border + a small badge */
	.fleet-world--offline::after {
		content: '';
		position: absolute;
		inset: 0;
		pointer-events: none;
		box-shadow: inset 0 0 0 2px rgba(229, 72, 77, 0.55);
		z-index: 3;
	}
	.fleet-world__offline-badge {
		position: absolute;
		top: 0.75rem;
		left: 50%;
		transform: translateX(-50%);
		z-index: var(--game-layer-system, 50);
		padding: 0.25rem 0.6rem;
		border-radius: var(--game-radius-sm, 0.25rem);
		border: 1px solid rgba(229, 72, 77, 0.7);
		background: var(--bg-card, rgba(20, 20, 24, 0.7));
		color: var(--color-error, #e5484d);
		font-family: var(--font-primary, system-ui);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.03em;
		pointer-events: none;
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}
	.fleet-world__empty {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		color: var(--fleet-ink, #1f2b33);
		opacity: 0.65;
		font-family: var(--font-primary, system-ui);
		font-size: 0.9rem;
		letter-spacing: 0.04em;
		pointer-events: none;
	}
</style>
