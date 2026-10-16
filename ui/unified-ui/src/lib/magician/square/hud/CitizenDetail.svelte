<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';
	import * as THREE from 'three';
	import { hitlOpenTargetFromFeedItem, openAttentionCenter, openHitlPrompt } from '$lib/attention';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import ExecutionControls from '$lib/magician/components/execution/ExecutionControls.svelte';
	import { loadAgents, triggerAgent } from '$lib/stores/agentStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { approvalIdOf, type NeedsItem } from '../attentionGlue';
	import { cloneModel } from '../engine/assets';
	import { flattenMaterials } from '../engine/flatLook';
	import type { FleetEngine } from '../engine/engine';
	import type { CitizenVM } from '../engine/types';
	import { VIBE_LABEL, relativeTime } from '../derive';
	import type { Quest } from '../fleetQuests';
	import { buildCitizenWorkDrilldown } from '../fleetWorkDrilldown';
	import { playGameCue } from '../gameAudio';
	import { GameEncounter, GameSection } from '../ui';
	import WorkDrilldown from './WorkDrilldown.svelte';

	const PORTRAIT_MAX_PIXEL_RATIO = 2;

	export let engine: FleetEngine | null = null;
	export let citizen: CitizenVM;
	export let guildName: string | undefined = undefined;
	export let needsItems: NeedsItem[] = [];
	export let quests: Quest[] = [];
	export let showFocus = false;
	export let section: 'all' | 'work' | 'activity' | 'command' = 'all';

	const dispatch = createEventDispatcher<{ close: void; focus: void; detail: void; task: string }>();
	$: primaryWork = citizen.currentWork[0] ?? null;
	$: workDrilldown = buildCitizenWorkDrilldown(citizen, quests);
	$: highestPriorityNeed = needsItems[0] ?? null;
	$: showWork = section === 'all' || section === 'work';
	$: showActivity = section === 'all' || section === 'activity';
	$: showCommand = section === 'all' || section === 'command';

	let portraitCanvas: HTMLCanvasElement | null = null;
	let portraitRenderer: THREE.WebGLRenderer | null = null;
	let portraitMixer: THREE.AnimationMixer | null = null;
	let portraitRaf = 0;
	let portraitRetry: ReturnType<typeof setTimeout> | null = null;
	let portraitFor = '';
	let hasPortrait = false;
	const portraitClock = new THREE.Clock();

	function disposePortrait(): void {
		if (portraitRaf) cancelAnimationFrame(portraitRaf);
		if (portraitRetry) clearTimeout(portraitRetry);
		portraitRaf = 0;
		portraitRetry = null;
		portraitMixer = null;
		portraitRenderer?.dispose();
		portraitRenderer = null;
		hasPortrait = false;
	}

	function buildPortrait(attempt = 0): void {
		disposePortrait();
		if (!portraitCanvas || !engine) return;
		const template = engine.characterTemplateFor(citizen.id);
		if (!template) {
			if (attempt < 6) portraitRetry = setTimeout(() => buildPortrait(attempt + 1), 700);
			return;
		}
		const model = cloneModel(template.scene);
		model.updateMatrixWorld(true);
		const box = new THREE.Box3().setFromObject(model, true);
		const size = box.getSize(new THREE.Vector3());
		model.position.sub(box.getCenter(new THREE.Vector3()));
		const scene = new THREE.Scene();
		scene.add(new THREE.HemisphereLight('#dceeff', '#66706d', 1.1));
		const key = new THREE.DirectionalLight('#fff3dd', 1.45);
		key.position.set(1.4, 2.4, 2.2);
		scene.add(key, model);
		const width = portraitCanvas.clientWidth || 320;
		const height = portraitCanvas.clientHeight || 190;
		const camera = new THREE.PerspectiveCamera(30, width / height, 0.01, 60);
		camera.position.set(0, size.y * 0.03, Math.max(size.y, size.x, 0.2) * 2.05);
		camera.lookAt(0, 0, 0);
		flattenMaterials(model);
		portraitRenderer = new THREE.WebGLRenderer({
			canvas: portraitCanvas,
			antialias: true,
			alpha: true
		});
		portraitRenderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, PORTRAIT_MAX_PIXEL_RATIO));
		portraitRenderer.setSize(width, height, false);
		portraitMixer = new THREE.AnimationMixer(model);
		const idle = template.clips.find((clip) => /idle/i.test(clip.name)) ?? template.clips[0];
		if (idle) portraitMixer.clipAction(idle).play();
		hasPortrait = true;
		const loop = () => {
			portraitRaf = requestAnimationFrame(loop);
			portraitMixer?.update(portraitClock.getDelta());
			model.rotation.y = Math.sin(performance.now() / 2400) * 0.28;
			portraitRenderer?.render(scene, camera);
		};
		loop();
	}

	$: if (portraitCanvas && engine && citizen.id !== portraitFor) {
		portraitFor = citizen.id;
		buildPortrait();
	}

	onDestroy(disposePortrait);

	let busyApprovals = new Set<string>();
	async function reviewNeed(entry: NeedsItem): Promise<void> {
		const target = hitlOpenTargetFromFeedItem(entry.item);
		if (!target) {
			openAttentionCenter();
			return;
		}
		const approvalId = approvalIdOf(entry.item);
		if (approvalId) {
			if (busyApprovals.has(approvalId)) return;
			busyApprovals = new Set(busyApprovals).add(approvalId);
		}
		try {
			const result = await openHitlPrompt(target);
			if (result.status === 'error') throw new Error(result.error);
			if (result.status === 'resolved' && approvalId) {
				attentionStore.dropResolved(approvalId);
				playGameCue('success');
				showSuccess('Approval resolved.');
				void loadAgents({ replace: true, clearError: true });
			}
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Could not resolve the decision.');
		} finally {
			if (!approvalId) return;
			const next = new Set(busyApprovals);
			next.delete(approvalId);
			busyApprovals = next;
		}
	}

	let boardReviewBusy = false;
	async function callBoardReview(): Promise<void> {
		if (!citizen.boardReviewGoalId || boardReviewBusy) return;
		boardReviewBusy = true;
		try {
			await triggerAgent(citizen.id, { goal_id: citizen.boardReviewGoalId, trigger: 'manual' });
			playGameCue('command');
			showSuccess('Board review convened.');
			void loadAgents({ replace: true, clearError: true });
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Could not convene the board review.');
		} finally {
			boardReviewBusy = false;
		}
	}

	function onControlsChanged(): void {
		void loadAgents({ replace: true, clearError: true });
	}
</script>

{#if showCommand}
	<div class="ci__status" data-vibe={citizen.vibe}>
		<span></span>{VIBE_LABEL[citizen.vibe] ?? citizen.vibe} · {relativeTime(citizen.updatedAt)}
	</div>

	<div class="ci__portrait" data-vibe={citizen.vibe}>
		<canvas bind:this={portraitCanvas}></canvas>
		{#if !hasPortrait}<span aria-hidden="true">{citizen.name.charAt(0).toUpperCase()}</span>{/if}
		<div class="ci__portrait-caption">
			<strong>{primaryWork?.title ?? citizen.lastGoal ?? 'Available for a task'}</strong>
			<span>{primaryWork?.currentSubstep ?? primaryWork?.currentStep ?? citizen.lastOutcome ?? citizen.title}</span>
		</div>
	</div>

	<GameSection title="Command status" compact>
		<div class="ci__context">
			<div>
				<span>Status</span>
				<strong>{VIBE_LABEL[citizen.vibe] ?? citizen.vibe}</strong>
			</div>
			<div>
				<span>Program</span>
				<strong>{guildName ?? citizen.guildId}</strong>
			</div>
		</div>
	</GameSection>
{/if}

{#if showWork}
	<WorkDrilldown
		model={workDrilldown}
		title="Work drill-down"
		description="Objective, live step, blocker, outcome, and artifacts"
		emptyLabel="No current operation"
		on:task={(event) => dispatch('task', event.detail)}
	/>
{/if}

{#if showActivity}
	{#if highestPriorityNeed}
		<GameSection title={`Needs you · ${needsItems.length}`} compact>
			{@const approvalId = approvalIdOf(highestPriorityNeed.item)}
			<GameEncounter
				title={highestPriorityNeed.item.title}
				summary={highestPriorityNeed.item.summary ?? ''}
				kind={highestPriorityNeed.lane === 'approvals' ? 'decision' : 'blocker'}
				urgent
			>
				<svelte:fragment slot="actions">
					{#if highestPriorityNeed.lane === 'approvals' && approvalId}
						<button
							type="button"
							class="ci__command ci__command--primary"
							disabled={busyApprovals.has(approvalId)}
							on:click={() => reviewNeed(highestPriorityNeed)}
						>Review approval</button>
					{:else}
						<button
							type="button"
							class="ci__command ci__command--primary"
							aria-haspopup="dialog"
							on:click={() => reviewNeed(highestPriorityNeed)}
						>Review encounter</button>
					{/if}
				</svelte:fragment>
			</GameEncounter>
			{#if needsItems.length > 1}
				<p class="ci__remaining">{needsItems.length - 1} more in the attention queue</p>
			{/if}
		</GameSection>
	{:else}
		<p class="ci__empty">Nothing needs you from {citizen.name}.</p>
	{/if}
{/if}

{#if showCommand && citizen.executionId}
	<GameSection title="Live execution" compact>
		<ExecutionControls
			executionId={citizen.executionId}
			taskId={primaryWork?.questId}
			label={citizen.name}
			refreshKey={citizen.vibe}
			on:changed={onControlsChanged}
		/>
	</GameSection>
{/if}

{#if showCommand}
	<div class="ci__footer">
		{#if showFocus}
			<button type="button" class="ci__command ci__command--primary" on:click={() => dispatch('focus')}
				><Icon name="eye" size={14} /> Focus camera</button
			>
		{/if}
		{#if citizen.isCeo && citizen.boardReviewGoalId}
			<button type="button" class="ci__command" disabled={boardReviewBusy} on:click={callBoardReview}
				><Icon name="play" size={14} /> {boardReviewBusy ? 'Convening' : 'Board review'}</button
			>
		{/if}
		<button type="button" class="ci__command" on:click={() => dispatch('detail')}
			><Icon name="file-text" size={14} /> Crew overview</button
		>
	</div>
{/if}

<style>
	.ci__status {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		width: max-content;
		max-width: 100%;
		margin-top: 0.4rem;
		padding: 0.15rem 0.35rem;
		border: 1px solid var(--game-border);
		color: var(--game-text-muted);
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm);
		text-transform: uppercase;
	}
	.ci__status > span {
		width: 0.5rem;
		height: 0.5rem;
		background: var(--game-text-muted);
	}
	.ci__status[data-vibe='working'] > span { background: var(--game-state-success); }
	.ci__status[data-vibe='needs'] > span { background: var(--game-state-danger); }
	.ci__status[data-vibe='paused'] > span { background: var(--game-state-attention); }
	.ci__portrait {
		position: relative;
		height: 12rem;
		margin: 0.85rem 0;
		border: 1px solid var(--game-border);
		background: color-mix(in srgb, var(--game-material-muted) 88%, transparent);
		overflow: hidden;
	}
	.ci__portrait canvas {
		display: block;
		width: 100%;
		height: 100%;
	}
	.ci__portrait > span {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		color: var(--game-text-muted);
		font-size: 4rem;
		font-weight: 800;
	}
	.ci__portrait-caption {
		position: absolute;
		inset: auto 0 0;
		display: grid;
		gap: 0.1rem;
		padding: 0.5rem 0.7rem 0.55rem;
		border-top: 1px solid var(--game-border);
		background: var(--game-material-panel);
	}
	.ci__portrait-caption strong,
	.ci__portrait-caption span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.ci__portrait-caption strong { font-size: var(--game-type-3); }
	.ci__portrait-caption span { color: var(--game-text-muted); font-size: var(--game-type-2); }
	.ci__context { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0.45rem; }
	.ci__context > div { display: grid; gap: 0.2rem; min-width: 0; }
	.ci__context span {
		color: var(--game-text-muted);
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm);
		text-transform: uppercase;
	}
	.ci__context strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.ci__remaining { margin: 0.45rem 0 0; color: var(--game-text-muted); font-size: var(--game-type-2); text-align: right; }
	.ci__empty { margin: 0; padding: 0.8rem 0; color: var(--game-text-muted); font-size: var(--game-type-2); }
	.ci__footer { display: flex; align-items: center; flex-wrap: wrap; gap: 0.45rem; width: 100%; margin-top: 0.8rem; }
	.ci__command {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		min-height: 2.15rem;
		padding: 0.4rem 0.65rem;
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm);
		background: var(--game-material-muted);
		color: var(--game-text);
		font: inherit;
		font-size: var(--game-type-2);
		font-weight: 700;
		text-decoration: none;
		cursor: pointer;
	}
	.ci__command--primary {
		border-color: var(--game-state-active);
		background: var(--game-text);
		color: var(--game-material-panel);
	}
	.ci__command:disabled { opacity: 0.5; cursor: wait; }
</style>
