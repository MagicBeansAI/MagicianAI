<script lang="ts">
	/**
	 * FleetHud — the Jarvis command layer over the game world (HTML, never
	 * in-canvas). Progressive disclosure per the design doc:
	 *   L0 always: [!] markers over citizens that need you, and health bars.
	 *   L0 as it fits: standing name plaques. Every citizen carries one, but
	 *     they compete for room: the engine places them in priority order
	 *     (selected, then needs-you, then live work, then the rest) and any
	 *     plaque that would land on an already-placed one stands down. So the
	 *     disclosure step is the room the camera gives, not a threshold —
	 *     zoomed into the plaza the whole crew is labelled, zoomed out to the
	 *     campus only the crew that matter keep their label. Nothing actionable
	 *     hides behind this: the [!] marker never competes and never culls.
	 *   L1 hover: ONE nameplate chip anchored to the hovered citizen/building.
	 *   L2 select: crew members go to the page command dock; landmarks and
	 *     guilds still open inspectors over the campus.
	 *   Home appears only once the camera has left its home framing.
	 * Coloured by APP theme tokens (Jarvis = behaviour, not palette).
	 * World-anchored elements are positioned by the engine every frame via
	 * direct DOM transforms (registerAnchor), no per-frame store churn.
	 */
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { goto } from '$app/navigation';
	import type { FleetEngine } from '../engine/engine';
	import type { CitizenVM, GuildVM } from '../engine/types';
	import { agentTarget, parseTarget } from '../engine/types';
	import { plaqueRole, VIBE_LABEL } from '../derive';
	import type { NeedsItem } from '../attentionGlue';
	import { applyExecutionControl } from '$lib/magician/execution/controlClient';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { taskStore } from '$lib/stores/taskStore';
	import {
		fetchProgramMissions,
		revertProgramMissions,
		type ProgramMissions
	} from '../fleetPrograms';
	import GuildInspector from './GuildInspector.svelte';
	import LandmarkInspector from './LandmarkInspector.svelte';
	import CouncilSheet from './CouncilSheet.svelte';
	import Minimap from './Minimap.svelte';
	import { fetchQuests, type Quest } from '../fleetQuests';
	import { playGameCue } from '../gameAudio';
	import type { FleetStateDelivery } from '../fleetState';
	import { GameWorkspace } from '../ui';
	import { healthBand } from '$lib/magician/crew/health';

	import { onDestroy, onMount } from 'svelte';

	export let engine: FleetEngine;
	export let citizens: CitizenVM[] = [];
	export let guilds: GuildVM[] = [];
	export let hoverTarget: string | null = null;
	export let selectedTarget: string | null = null;
	export let cameraMoved = false;
	export let needsByAgent: Map<string, NeedsItem[]> = new Map();
	$: void needsByAgent;
	/** A God-Hand drop awaiting its steer message (null = composer closed). */
	export let godHandDrop: { citizen: CitizenVM; guild: GuildVM } | null = null;
	export let authoritativeQuests: Quest[] | null = null;
	export let deliveries: FleetStateDelivery[] = [];

	const dispatch = createEventDispatcher<{ godhanddone: void }>();

	/** Fleet objectives for guild inspectors and game workspaces. */
	let fleetQuests: Quest[] = [];
	let questCountTimer: ReturnType<typeof setInterval> | null = null;
	async function refreshQuestCount(): Promise<void> {
		const quests = await fetchQuests();
		if (quests) fleetQuests = quests;
	}
	onMount(() => {
		if (!authoritativeQuests) {
			void refreshQuestCount();
			questCountTimer = setInterval(() => void refreshQuestCount(), 30_000);
		}
	});
	$: if (authoritativeQuests) fleetQuests = authoritativeQuests;
	onDestroy(() => {
		if (questCountTimer) clearInterval(questCountTimer);
	});

	// --- God-Hand steer composer ---
	let steerText = '';
	let steerBusy = false;
	let lastDropKey = '';
	$: if (godHandDrop) {
		const key = `${godHandDrop.citizen.id}->${godHandDrop.guild.id}`;
		if (key !== lastDropKey) {
			lastDropKey = key;
			steerText = `Refocus: prioritize "${godHandDrop.guild.name}" (${godHandDrop.guild.id}.md) now. Park your current thread cleanly first.`;
		}
	} else {
		lastDropKey = '';
	}

	function focusOnMount(el: HTMLTextAreaElement): void {
		el.focus();
		el.select();
	}

	function focusInput(el: HTMLInputElement): void {
		el.focus();
	}

	async function sendGodHandSteer(): Promise<void> {
		if (!godHandDrop?.citizen.executionId || steerBusy) return;
		const message = steerText.trim();
		if (!message) return;
		steerBusy = true;
		try {
			await applyExecutionControl(godHandDrop.citizen.executionId, 'steer', message);
			showSuccess(`${godHandDrop.citizen.name} redirected — lands on their next turn.`);
			dispatch('godhanddone');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Could not steer the run.');
		} finally {
			steerBusy = false;
		}
	}

	$: needy = citizens.filter((c) => c.vibe === 'needs');

	$: napping = citizens.filter((c) => c.resting && c.vibe === 'idle');
	$: citizenById = new Map(citizens.map((c) => [c.id, c]));
	$: guildById = new Map(guilds.map((g) => [g.id, g]));

	const LANDMARK_INFO: Record<string, { name: string; icon: 'settings' | 'git-branch' | 'flag'; blurb: string }> = {
		armory: { name: 'Capabilities', icon: 'settings', blurb: 'Tools available to each crew member' },
		council: { name: 'Delegation network', icon: 'git-branch', blurb: 'Observed and configured delegation routes' },
		hall: { name: 'Task router', icon: 'flag', blurb: 'Create and automatically assign a task' }
	};

	type QuestTarget =
		| { kind: 'citizen'; citizen: CitizenVM }
		| { kind: 'guild'; guild: GuildVM }
		| { kind: 'hall' };
	let questTarget: QuestTarget | null = null;
	let questTitle = '';
	let questDesc = '';
	/** 'auto' = let the fleet route it; otherwise a citizen id. */
	let questAssignee = 'auto';
	let questBusy = false;
	/** Hall composer offers two modes: a routed task, or a STRATEGIC GOAL the CEO
	 * decomposes into officer missions (approval-gated backend tool). */
	let questMode: 'task' | 'strategic' = 'task';
	$: ceoCitizen = citizens.find((c) => c.isCeo);

	/** The decomposition directive embedded in the strategic task description.
	 * The heavy tool CONTRACT ships with the tool's pack guide backend-side;
	 * this is the operator's framing of the assignment. */
	function strategicDescription(goalTitle: string, detail: string): string {
		const goal = detail.trim() || goalTitle.trim();
		return [
			`STRATEGIC GOAL: ${goal}`,
			'',
			'As CEO: read company_strategy.md and the relevant officer programs,',
			'then decompose this goal into missions using the',
			'propose_program_missions tool — one proposal per affected program,',
			'each mission with a crisp objective and success criteria. The tool',
			'is approval-gated: propose and await the owner decision. Do not do',
			'the work yourself; decompose and delegate through the programs.'
		].join('\n');
	}

	function openQuest(target: QuestTarget): void {
		questTarget = target;
		questTitle = '';
		questDesc = '';
		questMode = 'task';
		if (target.kind === 'guild') {
			const members = target.guild.memberIds
				.map((id) => citizenById.get(id))
				.filter((c): c is CitizenVM => Boolean(c));
			questAssignee = (members.find((c) => c.vibe === 'idle') ?? members[0])?.id ?? 'auto';
		} else {
			questAssignee = 'auto';
		}
	}

	function openQuickQuest(): void {
		playGameCue('command');
		questMode = 'task';
		questTarget = { kind: 'hall' };
	}
	function openDeliveryFollowup(delivery: FleetStateDelivery): void {
		openQuest({ kind: 'hall' });
		questTitle = `Follow up: ${delivery.title}`;
		questDesc = delivery.summary
			? `Review this delivered outcome and address the remaining work: ${delivery.summary}`
			: 'Review this delivered outcome and address any remaining work.';
		playGameCue('command');
	}
	function openTaskReview(taskId: string): void {
		const normalizedTaskId = taskId.trim();
		if (!normalizedTaskId) return;
		engine.select(null);
		playGameCue('select');
		void goto(`/tasks?selected=${encodeURIComponent(normalizedTaskId)}`);
	}
	// --- guild mission chips (the program's managed "Missions (CEO)" section) ---
	let guildMissions: ProgramMissions | null = null;
	let missionsForGuild = '';
	$: {
		const gid = selectedGuild?.id ?? '';
		if (gid && gid !== missionsForGuild) {
			missionsForGuild = gid;
			guildMissions = null;
			missionsRevertArmed = false;
			void fetchProgramMissions(`${gid}.md`).then((m) => {
				if (missionsForGuild === gid) guildMissions = m;
			});
		} else if (!gid && missionsForGuild) {
			missionsForGuild = '';
			guildMissions = null;
			missionsRevertArmed = false;
		}
	}

	// Owner revert (P3): restore the program from its newest history snapshot.
	// Two-step arm/confirm — no modal; disarms after 4s or on guild change.
	let missionsRevertArmed = false;
	let missionsReverting = false;
	let missionsRevertDisarm: ReturnType<typeof setTimeout> | null = null;
	async function revertGuildMissions(): Promise<void> {
		if (!missionsForGuild || missionsReverting) return;
		if (!missionsRevertArmed) {
			missionsRevertArmed = true;
			if (missionsRevertDisarm) clearTimeout(missionsRevertDisarm);
			missionsRevertDisarm = setTimeout(() => (missionsRevertArmed = false), 4000);
			return;
		}
		missionsRevertArmed = false;
		missionsReverting = true;
		const gid = missionsForGuild;
		const ok = await revertProgramMissions(`${gid}.md`);
		missionsReverting = false;
		if (ok) {
			showSuccess('Marching orders reverted to the previous snapshot.');
			guildMissions = null;
			void fetchProgramMissions(`${gid}.md`).then((m) => {
				if (missionsForGuild === gid) guildMissions = m;
			});
		} else {
			showError('Revert failed — the backend may not serve the revert endpoint yet.');
		}
	}

	function questTargetLabel(t: QuestTarget): string {
		return t.kind === 'citizen'
			? t.citizen.name
			: t.kind === 'guild'
				? t.guild.name
				: 'Task router (automatic assignment)';
	}
	async function submitQuest(): Promise<void> {
		if (!questTarget || questBusy) return;
		const title = questTitle.trim();
		if (!title) return;
		questBusy = true;
		const strategic = questTarget.kind === 'hall' && questMode === 'strategic' && ceoCitizen;
		const assigneeId = strategic
			? ceoCitizen.id
			: questTarget.kind === 'citizen'
				? questTarget.citizen.id
				: questTarget.kind === 'guild' && questAssignee !== 'auto'
					? questAssignee
					: undefined;
		const assignee = assigneeId ? citizenById.get(assigneeId) : undefined;
		const description = strategic
			? strategicDescription(title, questDesc)
			: questDesc.trim() || title;
		try {
			const created = await taskStore.createTask(
				strategic ? `Strategic goal: ${title}` : title,
				description,
				assignee ? { agentId: assignee.id, agentName: assignee.name } : {}
			);
			try {
				await taskStore.executeTaskDirect(created.id);
				playGameCue('success');
				showSuccess(
					strategic
						? `Strategic goal handed to ${assignee?.name ?? 'the CEO'} — expect a mission proposal at the [!].`
						: assignee
							? `Task created - ${assignee.name} is on it.`
							: 'Task created - assigning it to the crew.'
				);
			} catch (err) {
				showError(
					err instanceof Error
						? `Task created but did not start: ${err.message}`
						: 'Task created but did not start - run it from Tasks.'
				);
			}
			questTarget = null;
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Could not create the task.');
		} finally {
			questBusy = false;
		}
	}

	$: hoverParsed = hoverTarget ? parseTarget(hoverTarget) : null;
	$: hoverCitizen = hoverParsed?.kind === 'agent' ? citizenById.get(hoverParsed.id) : undefined;
	$: hoverGuild = hoverParsed?.kind === 'guild' ? guildById.get(hoverParsed.id) : undefined;
	$: hoverLandmark = hoverParsed?.kind === 'landmark' ? LANDMARK_INFO[hoverParsed.id] : undefined;

	$: selectedParsed = selectedTarget ? parseTarget(selectedTarget) : null;
	let lastSoundTarget: string | null | undefined;
	$: if (selectedTarget !== lastSoundTarget) {
		if (lastSoundTarget !== undefined && selectedTarget) playGameCue('select');
		lastSoundTarget = selectedTarget;
	}
	$: selectedCitizen =
		selectedParsed?.kind === 'agent' ? citizenById.get(selectedParsed.id) : undefined;
	$: selectedGuild = selectedParsed?.kind === 'guild' ? guildById.get(selectedParsed.id) : undefined;
	$: selectedLandmark = selectedParsed?.kind === 'landmark' ? selectedParsed.id : undefined;

	/** The role chip's word. Shared with the office floor — see derive.ts. */
	const roleChip = (c: CitizenVM): string => plaqueRole(c, guildById.get(c.guildId)?.name);

	/**
	 * Who keeps their plaque when two land on each other. Lower wins.
	 *
	 * Ranked by how much the crew member is asking of you: what you selected,
	 * then whoever needs you, then live work, then the agent that is yours,
	 * then everyone else. Every input changes on a roster poll or a click —
	 * none of it moves with the camera, which is what keeps the surviving set
	 * still while you pan.
	 */
	function plaquePriority(c: CitizenVM, selected: string | null): number {
		if (selected === agentTarget(c.id)) return 0;
		if (c.vibe === 'needs') return 1;
		if (c.vibe === 'working') return 2;
		if (c.isPrimary) return 3;
		if (c.vibe === 'paused') return 4;
		return 5;
	}

	/** Svelte action: keep an element anchored to a world target via the engine. */
	type AnchorParams = {
		key: string;
		target: string;
		offsetY?: number;
		interactive?: boolean;
		/** Compete for room against the other collidable anchors. */
		collide?: boolean;
		priority?: number;
	};
	function anchor(
		el: HTMLElement,
		params: AnchorParams
	): { update: (p: AnchorParams) => void; destroy: () => void } {
		let key = params.key;
		engine.registerAnchor(key, params.target, el, params);
		return {
			update(p) {
				// Only drop the registration when the key itself moves;
				// re-registering the same key updates it in place and keeps
				// whether it was standing, so a re-render never blinks it.
				if (p.key !== key) {
					engine.unregisterAnchor(key);
					key = p.key;
				}
				engine.registerAnchor(key, p.target, el, p);
			},
			destroy() {
				engine.unregisterAnchor(key);
			}
		};
	}

	function isTyping(e: KeyboardEvent): boolean {
		const t = e.target as HTMLElement | null;
		return !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
	}

	function onKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			if (crewDetailAgentId) {
				e.preventDefault();
				closeCrewDetail();
				return;
			}
			if (questTarget) {
				questTarget = null;
				return;
			}
			if (godHandDrop) {
				dispatch('godhanddone');
				return;
			}
			if (selectedTarget) engine.select(null);
			return;
		}
		if (isTyping(e) || e.metaKey || e.ctrlKey || e.altKey) return;
		if (e.key === 'f' || e.key === 'F') {
			if (selectedTarget) engine.focus(selectedTarget);
		}
	}

	let crewDetailAgentId: string | null = null;
	let crewDetailModule: Promise<
		typeof import('$lib/magician/crew/CrewMemberOverviewOverlay.svelte')
	> | null = null;

	function closeCrewDetail(): void {
		crewDetailAgentId = null;
		crewDetailModule = null;
	}
</script>

<svelte:window on:keydown={onKeydown} />

<div class="fw-hud">
	<Minimap {engine} needyIds={needy.map((c) => c.id)} />

	<!-- L0: needs-you markers (always visible — actionable, steer-by-exception) -->
	{#each needy as c (c.id)}
		<button
			class="fw-marker"
			use:anchor={{ key: `marker:${c.id}`, target: agentTarget(c.id), offsetY: 1.05 }}
			on:click={() => engine.select(agentTarget(c.id))}
			title={`${c.name} needs you`}
			aria-label={`${c.name} needs you`}
		>!</button>
	{/each}

	<!-- ambient: napping citizens snore (not interactive, just life) -->
	{#each napping as c (c.id)}
		<div
			class="fw-zzz"
			use:anchor={{ key: `zzz:${c.id}`, target: agentTarget(c.id), offsetY: 1.15 }}
			aria-hidden="true"
		>💤</div>
	{/each}

	<!-- health bars over every citizen (game-style unit bars; band-coloured) -->
	{#each citizens.filter((c) => c.health != null) as c (c.id)}
		<div
			class="fw-health"
			use:anchor={{
				key: `health:${c.id}`,
				target: agentTarget(c.id),
				offsetY: 0.92,
				interactive: false
			}}
			aria-hidden="true"
		>
			<span
				class="fw-health__fill"
				data-band={healthBand(c.health ?? 0)}
				style={`width:${Math.max(4, c.health ?? 0)}%`}
			></span>
		</div>
	{/each}

	<!-- L0 as it fits: standing name plaques. Every citizen carries their name
	     and role on the world, so the floor is readable without hovering
	     anything — an anonymous crowd is the difference between a populated
	     world and a busy one. They compete for room (collide) rather than pile
	     up: the engine places them in plaquePriority order and stands down any
	     that would land on a more important one. Crisp HTML over the pixelated
	     canvas, by design. -->
	{#each citizens as c (c.id)}
		<div
			class="fw-plaque"
			use:anchor={{
				key: `plaque:${c.id}`,
				target: agentTarget(c.id),
				offsetY: 1.34,
				interactive: false,
				collide: true,
				priority: plaquePriority(c, selectedTarget)
			}}
			aria-hidden="true"
		>
			<span class="fw-plaque__name">{c.name}</span>
			<span class="fw-plaque__role" data-vibe={c.vibe}>{roleChip(c)}</span>
		</div>
	{/each}

	<!-- L1: single hover nameplate chip -->
	{#if hoverTarget && hoverTarget !== selectedTarget && (hoverCitizen || hoverGuild || hoverLandmark)}
		<div
			class="fw-chip"
			use:anchor={{ key: 'chip', target: hoverTarget, offsetY: hoverCitizen ? 1.0 : 0.4 }}
		>
			{#if hoverCitizen}
				<strong>{hoverCitizen.name}</strong>
				<span>{hoverCitizen.currentWork[0]?.currentSubstep ?? hoverCitizen.currentWork[0]?.currentStep ?? hoverCitizen.currentWork[0]?.title ?? hoverCitizen.title}</span>
			{:else if hoverGuild}
				<strong>{hoverGuild.name}</strong>
				<span>{hoverGuild.memberIds.length} member{hoverGuild.memberIds.length === 1 ? '' : 's'}</span>
			{:else if hoverLandmark}
				<strong><Icon name={hoverLandmark.icon} size={14} /> {hoverLandmark.name}</strong>
				<span>{hoverLandmark.blurb}</span>
			{/if}
		</div>
	{/if}

	<!-- L2: crew selection is the command dock. Landmarks still overlay. -->
	{#if selectedLandmark === 'hall'}
		<LandmarkInspector
			landmark="hall"
			{citizens}
			{deliveries}
			on:close={() => engine.select(null)}
			on:quest={openQuickQuest}
			on:reviewtask={(event) => openTaskReview(event.detail)}
			on:followup={(event) => openDeliveryFollowup(event.detail)}
		/>
	{:else if selectedLandmark === 'armory'}
		<LandmarkInspector
			landmark="armory"
			{citizens}
			on:close={() => engine.select(null)}
			on:citizen={(event) => engine.select(agentTarget(event.detail))}
		/>
	{:else if selectedLandmark === 'council'}
		<CouncilSheet
			{citizens}
			on:close={() => engine.select(null)}
			on:citizen={(e) => engine.select(agentTarget(e.detail))}
		/>
	{:else if selectedGuild}
		<GuildInspector
			guild={selectedGuild}
			{citizens}
			quests={fleetQuests}
			missions={guildMissions}
			revertArmed={missionsRevertArmed}
			reverting={missionsReverting}
			on:close={() => engine.select(null)}
			on:citizen={(event) => engine.select(agentTarget(event.detail))}
			on:task={(event) => openTaskReview(event.detail)}
			on:revert={revertGuildMissions}
		/>
	{/if}

	{#if crewDetailAgentId && crewDetailModule}
		{#await crewDetailModule}
			<GameWorkspace
				open
				title="Crew overview"
				subtitle={crewDetailAgentId}
				navigation="close"
				showContext={false}
				showNavigation={false}
				presentation="overlay"
				on:back={closeCrewDetail}
			>
				<div class="fw-detail-state" role="status">Loading crew overview...</div>
			</GameWorkspace>
		{:then detailOverlay}
			<detailOverlay.default agentId={crewDetailAgentId} on:back={closeCrewDetail} />
		{:catch}
			<GameWorkspace
				open
				title="Crew overview unavailable"
				subtitle={crewDetailAgentId}
				navigation="close"
				showContext={false}
				showNavigation={false}
				presentation="overlay"
				on:back={closeCrewDetail}
			>
				<div class="fw-detail-state" role="alert">The crew overview could not be opened.</div>
			</GameWorkspace>
		{/await}
	{/if}

	<!-- Task composer: appears after dropping the command marker on a valid target. -->
	{#if questTarget}
		<div class="fw-godhand" role="dialog" aria-label="Create a task">
			<header class="fw-godhand__head">
				<span class="fw-godhand__emblem" aria-hidden="true"><Icon name="flag" size={18} /></span>
				<div><span>New assignment</span><strong>Task for {questTargetLabel(questTarget)}</strong></div>
				<button class="fw-panel__close" on:click={() => (questTarget = null)} aria-label="Cancel"><Icon name="x" size={17} /></button>
			</header>
			{#if questTarget.kind === 'hall'}
				<div class="fw-quest-mode" role="radiogroup" aria-label="Task mode">
					<button
						type="button"
						class:active={questMode === 'task'}
						on:click={() => (questMode = 'task')}
					><Icon name="file-text" size={15} /> Task - auto-route</button>
					<button
						type="button"
						class:active={questMode === 'strategic'}
						disabled={!ceoCitizen}
						title={ceoCitizen ? 'The CEO turns this into program goals (approval required)' : 'No CEO in the crew'}
						on:click={() => (questMode = 'strategic')}
					><Icon name="git-branch" size={15} /> Strategic goal - CEO delegates</button>
				</div>
			{/if}
			<input
				class="fw-quest-title"
				type="text"
				placeholder={questTarget.kind === 'hall' && questMode === 'strategic'
					? 'The strategic goal — what should the company achieve?'
					: 'Task title — what must be done?'}
				bind:value={questTitle}
				use:focusInput
			/>
			<textarea
				class="fw-godhand__text"
				rows="3"
				placeholder="Details (optional — the title is used if empty)"
				bind:value={questDesc}
				on:keydown={(e) => {
					if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submitQuest();
				}}
			></textarea>
			{#if questTarget.kind === 'guild'}
				<label class="fw-quest-assign">
					<span>Assign to</span>
					<select bind:value={questAssignee}>
						{#each questTarget.guild.memberIds as id (id)}
							{@const member = citizenById.get(id)}
							{#if member}
								<option value={id}>{member.name} ({VIBE_LABEL[member.vibe]})</option>
							{/if}
						{/each}
						<option value="auto">Anyone - let the system assign it</option>
					</select>
				</label>
			{/if}
			<div class="fw-godhand__actions">
				<button
					class="fw-godhand__send"
					disabled={questBusy || questTitle.trim().length === 0}
					on:click={() => void submitQuest()}
				>{questBusy ? 'Creating…' : 'Create task'}</button>
				<button class="fw-godhand__cancel" on:click={() => (questTarget = null)}>Cancel</button>
			</div>
		</div>
	{/if}

	<!-- God-Hand steer composer: appears after dropping a citizen on a guild -->
	{#if godHandDrop}
		<div class="fw-godhand" role="dialog" aria-label="Redirect crew member">
			<header class="fw-godhand__head">
				<span class="fw-godhand__emblem" aria-hidden="true"><Icon name="git-branch" size={18} /></span>
				<div><span>Live redirect</span><strong>{godHandDrop.citizen.name} to {godHandDrop.guild.name}</strong></div>
				<button class="fw-panel__close" on:click={() => dispatch('godhanddone')} aria-label="Cancel"><Icon name="x" size={17} /></button>
			</header>
			<p class="fw-godhand__hint">
				This message is injected into {godHandDrop.citizen.name}'s live run on their next turn.
			</p>
			<textarea
				class="fw-godhand__text"
				rows="3"
				bind:value={steerText}
				use:focusOnMount
				on:keydown={(e) => {
					if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void sendGodHandSteer();
				}}
			></textarea>
			<div class="fw-godhand__actions">
				<button
					class="fw-godhand__send"
					disabled={steerBusy || steerText.trim().length === 0}
					on:click={() => void sendGodHandSteer()}
				>{steerBusy ? 'Sending…' : 'Send redirect'}</button>
				<button class="fw-godhand__cancel" on:click={() => dispatch('godhanddone')}>Cancel</button>
			</div>
		</div>
	{/if}

	<!-- Home: appears only once the camera left home (progressive disclosure) -->
	{#if cameraMoved}
		<button class="fw-home" on:click={() => engine.resetView()} title="Reset view" aria-label="Reset view"><Icon name="rotate-ccw" size={16} /></button>
	{/if}
</div>

<style>
	.fw-detail-state {
		display: grid;
		min-height: 12rem;
		place-items: center;
		padding: var(--game-space-5);
		color: var(--game-text-muted);
		font-size: var(--game-type-3);
	}

	.fw-hud {
		position: absolute;
		inset: 0;
		pointer-events: none;
		z-index: var(--game-layer-command, 24);
		font-family: var(--font-primary, system-ui);
	}

	/* world-anchored: engine drives transform; top/left stay 0 */
	.fw-marker,
	.fw-chip,
	.fw-health,
	.fw-plaque {
		position: absolute;
		top: 0;
		left: 0;
		will-change: transform;
	}

	/* The plaque's own look — border, fill, bitmap caps, the status-coloured
	 * role chip — is in game-chrome.css, because the office floor hangs the
	 * same plaque over a seated head and there must be exactly one of it. Only
	 * the world anchoring above belongs to this file. */

	/* game-style unit health bar (band-coloured, pointer-transparent; the
	 * engine centers anchors via translate(-50%,-100%)) */
	.fw-health {
		pointer-events: none;
		width: 26px;
		height: 4px;
		border-radius: 999px;
		background: rgba(10, 14, 20, 0.4);
		overflow: hidden;
	}
	.fw-health__fill {
		display: block;
		height: 100%;
		border-radius: 999px;
		background: var(--fleet-working, #20773d);
	}
	.fw-health__fill[data-band='fair'] {
		background: var(--fleet-needs, #ae6500);
	}
	.fw-health__fill[data-band='poor'] {
		background: var(--color-error, #e5484d);
	}

	.fw-marker {
		pointer-events: auto;
		width: 1.45rem;
		height: 1.45rem;
		border-radius: 50%;
		border: 2px solid var(--bg-card, #fff);
		background: var(--color-warning, #f0b232);
		color: #3b2a00;
		font-weight: 800;
		font-size: 0.85rem;
		line-height: 1;
		cursor: pointer;
		box-shadow: 0 2px 10px rgba(0, 0, 0, 0.25);
		animation: fw-bounce 1.6s ease-in-out infinite;
	}
	@keyframes fw-bounce {
		0%,
		100% {
			margin-top: 0;
		}
		50% {
			margin-top: -5px;
		}
	}

	.fw-zzz {
		position: absolute;
		top: 0;
		left: 0;
		will-change: transform;
		pointer-events: none;
		font-size: 0.85rem;
		opacity: 0.75;
		animation: fw-snore 2.6s ease-in-out infinite;
	}
	@keyframes fw-snore {
		0%,
		100% {
			margin-top: 0;
			opacity: 0.55;
		}
		50% {
			margin-top: -4px;
			opacity: 0.85;
		}
	}

	.fw-chip {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		padding: 0.35rem 0.55rem;
		margin-top: -6px;
		border-radius: 0.5rem;
		border: 1px solid var(--game-border);
		background: var(--game-material-panel);
		color: var(--game-text);
		box-shadow: 0 6px 18px rgba(0, 0, 0, 0.18);
		font-size: 0.78rem;
		max-width: 240px;
		white-space: nowrap;
	}
	.fw-chip strong {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.82rem;
	}
	.fw-chip span {
		opacity: 0.75;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.fw-panel__close {
		display: grid;
		place-items: center;
		width: var(--game-target-sm);
		height: var(--game-target-sm);
		padding: 0;
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-md);
		background: var(--game-material-muted);
		color: inherit;
		cursor: pointer;
	}
	.fw-panel__close:hover {
		border-color: var(--game-border-strong);
		background: var(--game-material-raised);
	}
	.fw-quest-mode {
		display: flex;
		gap: 0.35rem;
	}
	.fw-quest-mode button {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: var(--game-space-2);
		flex: 1;
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-3);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border);
		background: var(--game-material-muted);
		color: inherit;
		font-size: var(--game-type-2);
		cursor: pointer;
	}
	.fw-quest-mode button.active {
		border-color: var(--game-state-active);
		background: color-mix(in srgb, var(--game-state-active) 14%, var(--game-material-raised));
		color: var(--game-state-active);
		font-weight: 700;
	}
	.fw-quest-mode button:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}
	.fw-quest-title {
		width: 100%;
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-3);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border);
		background: var(--game-material-workspace);
		color: inherit;
		font-family: var(--font-primary, system-ui);
		font-size: var(--game-type-3);
		font-weight: 600;
	}
	.fw-quest-assign {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.78rem;
	}
	.fw-quest-assign span {
		opacity: 0.65;
	}
	.fw-quest-assign select {
		flex: 1;
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-2);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border);
		background: var(--game-material-workspace);
		color: inherit;
		font-family: var(--font-primary, system-ui);
		font-size: 0.8rem;
	}

	.fw-godhand {
		pointer-events: auto;
		position: absolute;
		left: 50%;
		bottom: var(--game-space-5);
		z-index: var(--game-layer-system);
		transform: translateX(-50%);
		width: min(34rem, calc(100% - 3rem));
		display: flex;
		flex-direction: column;
		gap: var(--game-space-3);
		padding: var(--game-space-4);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border-strong);
		border-top: 2px solid var(--game-state-active);
		background: var(--game-material-panel);
		color: var(--game-text);
		box-shadow: 0 1.5rem 4rem rgba(0, 0, 0, 0.42);
		backdrop-filter: blur(18px);
	}
	.fw-godhand__head {
		display: flex;
		align-items: center;
		gap: var(--game-space-3);
	}
	.fw-godhand__head > div {
		display: flex;
		flex: 1;
		min-width: 0;
		flex-direction: column;
		gap: 0.1rem;
	}
	.fw-godhand__head span:not(.fw-godhand__emblem) {
		color: var(--game-state-active);
		font-size: var(--game-type-1);
		font-weight: var(--game-weight-strong);
		text-transform: uppercase;
	}
	.fw-godhand__head strong {
		overflow: hidden;
		font-size: var(--game-type-4);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.fw-godhand__emblem {
		display: grid;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		border: 1px solid color-mix(in srgb, var(--game-state-active) 60%, var(--game-border));
		border-radius: 50%;
		color: var(--game-state-active);
	}
	.fw-godhand__hint {
		margin: 0;
		color: var(--game-text-muted);
		font-size: var(--game-type-2);
		line-height: var(--game-line-body);
	}
	.fw-godhand__text {
		width: 100%;
		resize: vertical;
		padding: var(--game-space-3);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border);
		background: var(--game-material-workspace);
		color: inherit;
		font-family: var(--font-primary, system-ui);
		font-size: var(--game-type-3);
		line-height: var(--game-line-body);
	}
	.fw-godhand__actions {
		display: flex;
		gap: 0.45rem;
	}
	.fw-godhand__send {
		flex: 1;
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-3);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-state-active);
		background: color-mix(in srgb, var(--game-state-active) 18%, var(--game-material-raised));
		color: var(--game-state-active);
		font-weight: 700;
		font-size: 0.82rem;
		cursor: pointer;
	}
	.fw-godhand__send:disabled {
		opacity: 0.5;
		cursor: wait;
	}
	.fw-godhand__cancel {
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-3);
		border-radius: var(--game-radius-md);
		border: 1px solid var(--game-border);
		background: var(--game-material-muted);
		color: inherit;
		font-size: 0.82rem;
		cursor: pointer;
	}

	.fw-home {
		pointer-events: auto;
		position: absolute;
		right: 0.75rem;
		bottom: 10.75rem;
		width: 2rem;
		height: 2rem;
		border-radius: 0.5rem;
		border: 1px solid var(--game-border);
		background: var(--game-material-hud);
		color: var(--game-text);
		cursor: pointer;
		backdrop-filter: blur(6px);
	}
	.fw-home:hover {
		border-color: var(--accent-primary, #888);
	}
</style>
