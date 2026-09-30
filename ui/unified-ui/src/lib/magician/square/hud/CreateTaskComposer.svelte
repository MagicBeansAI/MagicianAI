<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { taskStore } from '$lib/stores/taskStore';
	import type { CitizenVM } from '../engine/types';
	import { playGameCue } from '../gameAudio';

	export let citizens: CitizenVM[] = [];

	const dispatch = createEventDispatcher<{ close: void }>();
	$: ceoCitizen = citizens.find((c) => c.isCeo);

	let title = '';
	let detail = '';
	let mode: 'task' | 'strategic' = 'task';
	let busy = false;

	function strategicDescription(goalTitle: string, extra: string): string {
		const goal = extra.trim() || goalTitle.trim();
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

	function focusInput(el: HTMLInputElement): void {
		el.focus();
	}

	async function submit(): Promise<void> {
		const trimmed = title.trim();
		if (!trimmed || busy) return;
		busy = true;
		const strategic = mode === 'strategic' && ceoCitizen;
		const description = strategic ? strategicDescription(trimmed, detail) : detail.trim() || trimmed;
		try {
			const created = await taskStore.createTask(
				strategic ? `Strategic goal: ${trimmed}` : trimmed,
				description,
				strategic && ceoCitizen ? { agentId: ceoCitizen.id, agentName: ceoCitizen.name } : {}
			);
			try {
				await taskStore.executeTaskDirect(created.id);
				playGameCue('success');
				showSuccess(
					strategic
						? `Strategic goal handed to ${ceoCitizen?.name ?? 'the CEO'} — expect a mission proposal at the [!].`
						: 'Task created - assigning it to the crew.'
				);
			} catch (err) {
				showError(
					err instanceof Error
						? `Task created but did not start: ${err.message}`
						: 'Task created but did not start - run it from Tasks.'
				);
			}
			dispatch('close');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Could not create the task.');
		} finally {
			busy = false;
		}
	}
</script>

<div class="fw-godhand" role="dialog" aria-label="Create a task">
	<header class="fw-godhand__head">
		<span class="fw-godhand__emblem" aria-hidden="true"><Icon name="flag" size={18} /></span>
		<div><span>New assignment</span><strong>Task router (automatic assignment)</strong></div>
		<button class="fw-panel__close" type="button" on:click={() => dispatch('close')} aria-label="Cancel">
			<Icon name="x" size={17} />
		</button>
	</header>
	<div class="fw-quest-mode" role="radiogroup" aria-label="Task mode">
		<button type="button" class:active={mode === 'task'} on:click={() => (mode = 'task')}>
			<Icon name="file-text" size={15} /> Task - auto-route
		</button>
		<button
			type="button"
			class:active={mode === 'strategic'}
			disabled={!ceoCitizen}
			title={ceoCitizen ? 'The CEO turns this into program goals (approval required)' : 'No CEO in the crew'}
			on:click={() => (mode = 'strategic')}
		>
			<Icon name="git-branch" size={15} /> Strategic goal - CEO delegates
		</button>
	</div>
	<input
		class="fw-quest-title"
		type="text"
		placeholder={mode === 'strategic'
			? 'The strategic goal — what should the company achieve?'
			: 'Task title — what must be done?'}
		bind:value={title}
		use:focusInput
	/>
	<textarea
		class="fw-godhand__text"
		rows="3"
		placeholder="Details (optional — the title is used if empty)"
		bind:value={detail}
		on:keydown={(e) => {
			if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit();
		}}
	></textarea>
	<div class="fw-godhand__actions">
		<button class="fw-godhand__send" disabled={busy || title.trim().length === 0} on:click={() => void submit()}>
			{busy ? 'Creating…' : 'Create task'}
		</button>
		<button class="fw-godhand__cancel" type="button" on:click={() => dispatch('close')}>Cancel</button>
	</div>
</div>

<style>
	.fw-godhand {
		pointer-events: auto;
		position: absolute;
		left: 50%;
		top: 50%;
		transform: translate(-50%, -50%);
		z-index: 8;
		width: min(28rem, calc(100% - 2rem));
		padding: 0.85rem;
		border: 1px solid var(--game-border-strong, rgba(0, 0, 0, 0.4));
		background: var(--game-material-panel, var(--bg-elevated, #fff));
		color: var(--game-text, inherit);
	}
	.fw-godhand__head {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		margin-bottom: 0.65rem;
	}
	.fw-godhand__head div {
		flex: 1;
		min-width: 0;
		display: grid;
	}
	.fw-godhand__head span {
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		color: var(--game-text-muted, #667085);
	}
	.fw-godhand__emblem {
		display: grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
	}
	.fw-panel__close,
	.fw-godhand__cancel,
	.fw-godhand__send,
	.fw-quest-mode button {
		border: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
		background: var(--game-material-muted, rgba(0, 0, 0, 0.06));
		color: inherit;
		cursor: pointer;
	}
	.fw-panel__close {
		display: grid;
		place-items: center;
		width: 1.75rem;
		height: 1.75rem;
		padding: 0;
	}
	.fw-quest-mode {
		display: flex;
		gap: 0.35rem;
		margin-bottom: 0.55rem;
	}
	.fw-quest-mode button {
		flex: 1;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.3rem;
		min-height: 2rem;
		padding: 0.3rem 0.4rem;
		font-size: 0.75rem;
	}
	.fw-quest-mode button.active {
		background: var(--game-text, #1c1c1c);
		color: var(--game-material-panel, #fff);
	}
	.fw-quest-title,
	.fw-godhand__text {
		width: 100%;
		margin: 0 0 0.45rem;
		padding: 0.45rem 0.5rem;
		border: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
		background: var(--game-material-raised, #fff);
		color: inherit;
		font: inherit;
	}
	.fw-godhand__actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.4rem;
	}
	.fw-godhand__send,
	.fw-godhand__cancel {
		min-height: 2rem;
		padding: 0 0.7rem;
	}
	.fw-godhand__send:disabled {
		opacity: 0.5;
		cursor: wait;
	}
</style>
