<script lang="ts">
	/**
	 * TodayNewspaperLedger.svelte
	 *
	 * The Morning Edition's "Daily Index": an Operations carousel card.
	 * - Slide 1 "Economics of Operations": LLM spend, delta vs yesterday,
	 *   model calls, 24-hour spend histogram, Analytics link.
	 * - Slide 2 "State of Operations": Active / Succeeded / Failed pie + legend,
	 *   agent counts, Tasks / Agent Crew links, and the most recently updated
	 *   tasks as one-line rows.
	 * - Slide 3 "State of the Crew": per-agent cost, calls, tasks, success and
	 *   reliability over the last 24 hours (crewQueries.ts), refreshed every 60 s.
	 *
	 * Owns the store lifecycle; the carousel and slides are presentational.
	 * `customLlm` / `customTasks` / `customAgents` override the live stores for
	 * deterministic tests and embedding. With `customTasks` set, the crew slide
	 * reads those tasks and `customCrewLlm` instead of fetching.
	 */
	import { onMount } from 'svelte';
	import { todayPulseStore } from '$lib/stores/todayPulseStore';
	import { taskStore, type Task } from '$lib/stores/taskStore';
	import { agentList, runningAgentCount, loadAgents, type AgentSummary } from '$lib/stores/agentStore';
	import type { LlmPulse } from '$lib/today/pulseQueries';
	import type { OpsCarouselSlide } from '$lib/today/opsCarousel';
	import TodayOpsCarousel from '$lib/today/TodayOpsCarousel.svelte';
	import TodayOpsEconomicsSlide from '$lib/today/TodayOpsEconomicsSlide.svelte';
	import TodayOpsStateSlide from '$lib/today/TodayOpsStateSlide.svelte';
	import TodayOpsCrewSlide from '$lib/today/TodayOpsCrewSlide.svelte';
	import {
		buildCrewRows,
		CREW_WINDOW_MS,
		fetchCrewActivity,
		timestampToMs,
		type CrewLlmRow,
		type CrewTaskRow
	} from '$lib/today/crewQueries';

	interface Props {
		customLlm?: LlmPulse | null;
		customTasks?: Task[] | null;
		customAgents?: AgentSummary[] | null;
		/** Per-agent model use for the crew slide (test mode, with customTasks). */
		customCrewLlm?: CrewLlmRow[] | null;
		/** Carousel timing overrides (tests). */
		intervalMs?: number;
		manualPauseMs?: number;
	}

	let {
		customLlm = null,
		customTasks = null,
		customAgents = null,
		customCrewLlm = null,
		intervalMs = undefined,
		manualPauseMs = undefined
	}: Props = $props();

	const slides: OpsCarouselSlide[] = [
		{ id: 'economics', title: 'Economics of Operations' },
		{ id: 'state', title: 'State of Operations' },
		{ id: 'crew', title: 'State of the Crew' }
	];

	let nowMs = $state(Date.now());

	// --- Crew slide (last 24 hours) ---
	let crewLlm = $state<CrewLlmRow[] | null>(null);
	let crewTasks = $state<CrewTaskRow[] | null>(null);
	let crewError = $state<string | null>(null);
	let crewInFlight = false;

	async function refreshCrew() {
		if (customTasks || crewInFlight) return;
		crewInFlight = true;
		try {
			const activity = await fetchCrewActivity(Date.now() - CREW_WINDOW_MS);
			crewLlm = activity.llm;
			crewTasks = activity.tasks;
			crewError = null;
		} catch (error) {
			crewError = error instanceof Error ? error.message : 'Crew activity unavailable';
		} finally {
			crewInFlight = false;
		}
	}

	onMount(() => {
		todayPulseStore.start();
		taskStore.start();
		if (!customTasks) {
			void taskStore.loadTasks().catch(() => {});
		}
		if (!customAgents) {
			void loadAgents({ replace: true, clearError: true }).catch(() => {});
		}
		void refreshCrew();
		// Keep the task rows' relative times ("4m") current and the crew slide on the pulse cadence.
		const clock = setInterval(() => {
			nowMs = Date.now();
			void refreshCrew();
		}, 60_000);

		return () => {
			clearInterval(clock);
			todayPulseStore.stop();
			taskStore.stop();
		};
	});

	const llm = $derived(customLlm ?? $todayPulseStore.snapshot?.llm ?? null);
	const tasks = $derived(customTasks ?? $taskStore.tasks ?? []);
	const agents = $derived(customAgents ?? $agentList ?? []);
	const pulseCompletedToday = $derived($todayPulseStore.snapshot?.tasks?.completedToday ?? 0);

	const crewRows = $derived.by(() => {
		const sinceMs = nowMs - CREW_WINDOW_MS;
		if (customTasks) {
			const taskRows: CrewTaskRow[] = customTasks.map((t) => ({
				agentId: t.agentId ?? null,
				status: t.status,
				updatedAtMs: timestampToMs(t.updatedAt)
			}));
			return buildCrewRows({ agents, llm: customCrewLlm ?? [], tasks: taskRows, sinceMs });
		}
		if (!crewLlm || !crewTasks) return null;
		return buildCrewRows({ agents, llm: crewLlm, tasks: crewTasks, sinceMs });
	});
</script>

<TodayOpsCarousel {slides} label="Operations" {intervalMs} {manualPauseMs}>
	{#snippet aside()}
		<span class="np-ledger__time">Continuous real-time accounting</span>
	{/snippet}
	{#snippet slide(s: OpsCarouselSlide)}
		{#if s.id === 'economics'}
			<TodayOpsEconomicsSlide {llm} pulse={customLlm ? null : $todayPulseStore.snapshot} />
		{:else if s.id === 'state'}
			<TodayOpsStateSlide
				{tasks}
				{agents}
				runningAgentCount={$runningAgentCount}
				{pulseCompletedToday}
				{nowMs}
			/>
		{:else if s.id === 'crew'}
			<TodayOpsCrewSlide
				rows={crewRows}
				totalAgents={agents.length}
				error={crewError}
				onRetry={() => void refreshCrew()}
			/>
		{/if}
	{/snippet}
</TodayOpsCarousel>

<style>
	.np-ledger__time {
		font-size: 0.72rem;
		color: var(--text-muted, #64748b);
		font-style: italic;
		font-family: 'Newsreader', Georgia, serif;
		white-space: nowrap;
	}

	@media (max-width: 560px) {
		.np-ledger__time {
			display: none;
		}
	}
</style>
