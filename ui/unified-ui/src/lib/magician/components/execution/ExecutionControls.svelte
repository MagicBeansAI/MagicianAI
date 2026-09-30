<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import TextArea from '$lib/magician/components/generative/TextArea.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		MAX_STEER_MESSAGE_BYTES,
		applyExecutionControl,
		executionControlBusy,
		executionControlInvalidations,
		getExecutionControlState,
		steerMessageByteLength,
		type ExecutionControlAction,
		type ExecutionControlState
	} from '$lib/magician/execution/controlClient';

	export let executionId: string | null | undefined = undefined;
	export let taskId: string | undefined = undefined;
	export let label: string | undefined = undefined;
	export let variant: 'row' | 'compact' = 'row';
	export let showCancel = true;
	export let refreshKey: string | number | null | undefined = undefined;

	const dispatch = createEventDispatcher<{
		changed: { action: ExecutionControlAction };
		state: { state: ExecutionControlState | null };
	}>();

	let state: ExecutionControlState | null = null;
	let loading = false;
	let controlError = '';
	let loadedControlKey = '';
	let loadGeneration = 0;
	let steerOpen = false;
	let steerText = '';
	let refreshTimers: Array<ReturnType<typeof setTimeout>> = [];

	$: targetExecutionId = executionId?.trim() ?? '';
	$: invalidationVersion = targetExecutionId
		? ($executionControlInvalidations.get(targetExecutionId) ?? 0)
		: 0;
	$: controlKey = `${targetExecutionId}\u0000${refreshKey ?? ''}\u0000${invalidationVersion}`;
	$: busy = targetExecutionId ? $executionControlBusy.has(targetExecutionId) : false;
	$: steerBytes = steerMessageByteLength(steerText);
	$: steerValid = steerText.trim().length > 0 && steerBytes <= MAX_STEER_MESSAGE_BYTES;
	$: hasActions = Boolean(
		state?.can_steer || state?.can_pause || state?.can_resume || (showCancel && state?.can_cancel)
	);

	$: if (controlKey !== loadedControlKey) {
		loadedControlKey = controlKey;
		state = null;
		controlError = '';
		steerOpen = false;
		steerText = '';
		void refreshState(targetExecutionId);
	}

	async function refreshState(target = targetExecutionId): Promise<void> {
		const generation = ++loadGeneration;
		if (!target) {
			loading = false;
			state = null;
			return;
		}
		loading = true;
		controlError = '';
		try {
			const next = await getExecutionControlState(target);
			if (generation !== loadGeneration || target !== targetExecutionId) return;
			state = next;
			dispatch('state', { state: next });
		} catch (error) {
			if (generation !== loadGeneration || target !== targetExecutionId) return;
			state = null;
			dispatch('state', { state: null });
			controlError = error instanceof Error ? error.message : 'Could not load execution controls.';
		} finally {
			if (generation === loadGeneration && target === targetExecutionId) loading = false;
		}
	}

	function scheduleSettlementRefreshes(): void {
		for (const timer of refreshTimers) clearTimeout(timer);
		refreshTimers = [250, 1_000, 3_000].map((delay) =>
			setTimeout(() => void refreshState(targetExecutionId), delay)
		);
	}

	function successMessage(action: ExecutionControlAction): string {
		switch (action) {
			case 'pause':
				return 'Run pause requested.';
			case 'resume':
				return 'Run resumed.';
			case 'steer':
				return 'Steer queued for the next decision turn.';
			case 'cancel':
				return 'Run stopped.';
		}
	}

	async function runAction(
		action: ExecutionControlAction,
		message?: string,
		expectedTarget = targetExecutionId
	): Promise<boolean> {
		if (!expectedTarget || expectedTarget !== targetExecutionId || busy) return false;
		try {
			await applyExecutionControl(expectedTarget, action, message);
			showSuccess(successMessage(action));
			dispatch('changed', { action });
			if (action === 'pause' || action === 'resume') scheduleSettlementRefreshes();
			return true;
		} catch (error) {
			showError(error instanceof Error ? error.message : `Could not ${action} the run.`);
			return false;
		}
	}

	async function submitSteer(): Promise<void> {
		const message = steerText.trim();
		if (!steerValid || busy) return;
		if (await runAction('steer', message)) {
			steerOpen = false;
			steerText = '';
		}
	}

	async function stop(): Promise<void> {
		if (!state?.can_cancel || busy) return;
		const confirmedTarget = targetExecutionId;
		if (!confirmedTarget) return;
		const confirmed = await requestConfirmation({
			title: 'Stop this run?',
			message: `This cancels ${label ? `"${label}"` : 'the run'} mid-flight and can't be undone.`,
			confirmLabel: 'Stop run',
			destructive: true
		});
		if (!confirmed) return;
		if (confirmedTarget !== targetExecutionId) {
			showError('The active run changed while confirmation was open. Review it before stopping.');
			return;
		}
		await runAction('cancel', undefined, confirmedTarget);
	}

	onDestroy(() => {
		loadGeneration += 1;
		for (const timer of refreshTimers) clearTimeout(timer);
	});
</script>

{#if targetExecutionId && (loading || controlError || taskId || hasActions)}
	<div class="exec-controls exec-controls--{variant}" aria-label="Execution controls">
		{#if taskId}
			<a
				class="exec-link"
				class:exec-link--compact={variant === 'compact'}
				href={`/tasks?selected=${encodeURIComponent(taskId)}`}
				aria-label="Inspect run"
				title="Inspect run"
			>
				<Icon name="eye" size={14} />
				{#if variant === 'row'}<span>Inspect</span>{/if}
			</a>
		{/if}

		{#if state?.can_steer}
			<Button
				label="Steer"
				icon="send"
				iconOnly={variant === 'compact'}
				ariaLabel="Steer run"
				title="Guide the run's next decision turn"
				variant="outline"
				size="sm"
				disabled={busy}
				on:click={() => (steerOpen = true)}
			/>
		{/if}

		{#if state?.can_pause}
			<Button
				label="Pause"
				icon="pause"
				iconOnly={variant === 'compact'}
				ariaLabel="Pause run"
				title="Pause this run at a resumable checkpoint"
				variant="outline"
				size="sm"
				disabled={busy}
				on:click={() => void runAction('pause')}
			/>
		{:else if state?.can_resume}
			<Button
				label="Resume"
				icon="play"
				iconOnly={variant === 'compact'}
				ariaLabel="Resume run"
				title="Resume this manually paused run"
				variant="outline"
				size="sm"
				disabled={busy}
				on:click={() => void runAction('resume')}
			/>
		{/if}

		{#if showCancel && state?.can_cancel}
			<Button
				label="Stop"
				icon="square"
				iconOnly={variant === 'compact'}
				ariaLabel="Stop run"
				title="Stop this run"
				variant="outline"
				size="sm"
				className="exec-stop"
				disabled={busy}
				on:click={() => void stop()}
			/>
		{/if}

		{#if controlError && !state}
			<button
				type="button"
				class="exec-retry"
				disabled={loading}
				title={controlError}
				on:click={() => void refreshState()}
			>
				<Icon name="rotate-ccw" size={14} />
				{#if variant === 'row'}<span>{loading ? 'Loading' : 'Retry controls'}</span>{/if}
			</button>
		{:else if loading && !state && !taskId}
			<span class="exec-loading" aria-label="Loading execution controls"></span>
		{:else if !loading && !hasActions && !taskId && showCancel}
			<span class="exec-unavailable">No live controls</span>
		{/if}
	</div>
{/if}

<Modal
	open={steerOpen}
	title="Steer this run"
	size="sm"
	closable={!busy}
	initialFocusSelector="textarea"
	idBase="execution-steer"
	on:close={() => {
		if (!busy) steerOpen = false;
	}}
>
	<form class="steer-form" on:submit|preventDefault={() => void submitSteer()}>
		<TextArea
			bind:value={steerText}
			label="Guidance for the next decision turn"
			placeholder="Describe what the agent should change, prioritize, or avoid."
			rows={5}
			maxLength={MAX_STEER_MESSAGE_BYTES}
			disabled={busy}
			idBase="execution-steer-message"
		/>
		<p class:steer-limit-error={steerBytes > MAX_STEER_MESSAGE_BYTES} class="steer-byte-count">
			{steerBytes.toLocaleString()} / {MAX_STEER_MESSAGE_BYTES.toLocaleString()} bytes
		</p>
		<div class="steer-actions">
			<Button
				label="Cancel"
				variant="outline"
				disabled={busy}
				on:click={() => (steerOpen = false)}
			/>
			<Button label={busy ? 'Sending' : 'Send steer'} icon="send" type="submit" disabled={!steerValid || busy} />
		</div>
	</form>
</Modal>

<style>
	.exec-controls {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		min-height: 2rem;
	}

	.exec-controls--compact {
		gap: 0.25rem;
	}

	.exec-link,
	.exec-retry {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		min-height: 1.9rem;
		padding: 0.35rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: transparent;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1;
		text-decoration: none;
		cursor: pointer;
	}

	.exec-link--compact,
	.exec-controls--compact .exec-retry {
		width: 1.9rem;
		padding: 0;
	}

	.exec-link:hover,
	.exec-retry:not(:disabled):hover {
		background: var(--bg-hover);
	}

	.exec-retry:disabled {
		opacity: 0.5;
		cursor: default;
	}

	:global(.exec-stop) {
		border-color: color-mix(in srgb, var(--danger) 55%, var(--border-soft)) !important;
		color: var(--danger) !important;
	}

	.exec-loading {
		width: 1rem;
		height: 1rem;
		border: 2px solid var(--border-soft);
		border-top-color: var(--accent-primary);
		border-radius: 50%;
		animation: exec-spin 700ms linear infinite;
	}

	.exec-unavailable {
		color: var(--text-secondary);
		font-size: 0.75rem;
	}

	.steer-form {
		display: grid;
		gap: 0.65rem;
	}

	.steer-byte-count {
		margin: -0.35rem 0 0;
		color: var(--text-secondary);
		font-size: 0.6875rem;
		text-align: right;
	}

	.steer-limit-error {
		color: var(--danger);
	}

	.steer-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		padding-top: 0.35rem;
	}

	@keyframes exec-spin {
		to { transform: rotate(360deg); }
	}
</style>
