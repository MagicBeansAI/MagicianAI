<script lang="ts">
	import { browser } from '$app/environment';
	import { createEventDispatcher } from 'svelte';
	import type { TaskOutputMode } from '$lib/stores/taskStore';
	import type { AgentPickerOption, TaskCreateSubmitValues } from './types';

	export let title = '';
	export let description = '';
	export let outputMode: TaskOutputMode = 'accumulate';
	export let agentOptions: AgentPickerOption[] = [];
	export let selectedAgentId = '';
	export let threadOptions: Array<{ id: string; name: string }> = [];
	export let selectedThreadId = 'general';
	export let threadLocked = false;
	export let scheduleExpanded = false;
	export let scheduleCron = '';
	export let scheduleTimezone = '';
	export let scheduleTimezoneOptions: Array<{ value: string; label: string }> = [];
	export let disabled = false;
	export let showDescriptionField = true;
	export let descriptionLabel = 'Description';
	export let descriptionPlaceholder = 'Describe what you want done...';
	export let titlePlaceholder = 'Title - e.g. "Book Tokyo flights"';
	export let submitLabel = 'Create Task';
	export let defaultTimezone =
		browser ? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' : 'UTC';

	const dispatch = createEventDispatcher<{
		submit: { values: TaskCreateSubmitValues };
		clear: void;
	}>();

	const SCHEDULE_PRESETS: Array<{ label: string; cron: string }> = [
		{ label: 'Every hour', cron: '0 * * * *' },
		{ label: 'Daily at 9 AM', cron: '0 9 * * *' },
		{ label: 'Weekly (Mon 9 AM)', cron: '0 9 * * 1' },
		{ label: 'Monthly (1st, 9 AM)', cron: '0 9 1 * *' }
	];

	$: {
		if (selectedAgentId && !agentOptions.some((agent) => agent.agent_id === selectedAgentId)) {
			selectedAgentId = '';
		}
		if (!selectedAgentId && agentOptions.length > 0) {
			selectedAgentId = agentOptions[0].agent_id;
		}
	}
	$: if (!selectedThreadId && threadOptions.length > 0) {
		selectedThreadId = threadOptions[0].id;
	}

	function normalizedTimezoneOptions(): Array<{ value: string; label: string }> {
		if (scheduleTimezoneOptions.length > 0) return scheduleTimezoneOptions;
		return defaultTimezone ? [{ value: defaultTimezone, label: `${defaultTimezone} (device)` }] : [];
	}

	function applySchedulePreset(cron: string): void {
		scheduleExpanded = true;
		scheduleCron = cron;
		if (!scheduleTimezone.trim()) {
			scheduleTimezone = defaultTimezone;
		}
	}

	function clearSchedule(): void {
		scheduleExpanded = false;
		scheduleCron = '';
		scheduleTimezone = defaultTimezone;
	}

	function clearForm(): void {
		title = '';
		description = '';
		outputMode = 'accumulate';
		clearSchedule();
		dispatch('clear');
	}

	function submitForm(): void {
		dispatch('submit', {
			values: {
				task_title: title,
				task_description: description,
				task_output_mode: outputMode,
				task_agent: selectedAgentId,
				task_thread: selectedThreadId,
				schedule_cron: scheduleCron,
				schedule_timezone: scheduleTimezone
			}
		});
	}
</script>

<form class="task-create-form" on:submit|preventDefault={submitForm}>
	<slot name="description">
		{#if showDescriptionField}
			<label class="task-create-field task-create-field--wide">
				<span>{descriptionLabel}</span>
				<textarea
					bind:value={description}
					disabled={disabled}
					placeholder={descriptionPlaceholder}
					rows="4"
				></textarea>
			</label>
		{/if}
	</slot>

	<div class="task-create-grid">
		<label class="task-create-field task-create-field--wide">
			<span>Title</span>
			<input
				class="task-create-title-input"
				bind:value={title}
				disabled={disabled}
				placeholder={titlePlaceholder}
				type="text"
			/>
		</label>

		<label class="task-create-field">
			<span>Assign crew member</span>
			<select bind:value={selectedAgentId} disabled={disabled || agentOptions.length === 0} required>
				{#if agentOptions.length === 0}
					<option value="">No crew members available</option>
				{:else}
					{#each agentOptions as agent (agent.agent_id)}
						<option value={agent.agent_id}>{agent.name || agent.agent_id}</option>
					{/each}
				{/if}
			</select>
		</label>

		<label class="task-create-field">
			<span>Run output mode</span>
			<select bind:value={outputMode} disabled={disabled}>
				<option value="accumulate">Accumulate outputs</option>
				<option value="overwrite">Overwrite latest outputs</option>
			</select>
		</label>

		{#if threadOptions.length > 0}
			<label class="task-create-field">
				<span>Thread</span>
				<select bind:value={selectedThreadId} disabled={disabled || threadLocked}>
					{#each threadOptions as thread (thread.id)}
						<option value={thread.id}>#{thread.name}</option>
					{/each}
				</select>
			</label>
		{/if}
	</div>

	<div class:open={scheduleExpanded} class="task-create-schedule">
		{#if !scheduleExpanded}
			<button
				class="task-create-button task-create-button--outline"
				disabled={disabled}
				type="button"
				on:click={() => (scheduleExpanded = true)}
			>
				+ Add Schedule
			</button>
		{:else}
			<div class="task-create-schedule-card">
				<div class="task-create-schedule-head">
					<span>Schedule (optional)</span>
					<button
						class="task-create-button task-create-button--ghost"
						disabled={disabled}
						type="button"
						on:click={clearSchedule}
					>
						Remove Schedule
					</button>
				</div>

				<div class="task-create-presets" aria-label="Schedule presets">
					{#each SCHEDULE_PRESETS as preset (preset.label)}
						<button
							class="task-create-button task-create-button--outline"
							disabled={disabled}
							type="button"
							on:click={() => applySchedulePreset(preset.cron)}
						>
							{preset.label}
						</button>
					{/each}
				</div>

				<div class="task-create-grid task-create-grid--schedule">
					<label class="task-create-field">
						<span>Custom cron</span>
						<input
							bind:value={scheduleCron}
							disabled={disabled}
							maxlength="50"
							placeholder="0 9 * * *"
							type="text"
						/>
					</label>
					<label class="task-create-field">
						<span>Timezone</span>
						<select bind:value={scheduleTimezone} disabled={disabled}>
							{#each normalizedTimezoneOptions() as option (option.value)}
								<option value={option.value}>{option.label}</option>
							{/each}
						</select>
					</label>
				</div>
			</div>
		{/if}
	</div>

	<div class="task-create-actions">
		<button
			class="task-create-button task-create-button--outline"
			disabled={disabled}
			type="button"
			on:click={clearForm}
		>
			Clear
		</button>
		<button class="task-create-button task-create-button--primary" disabled={disabled} type="submit">
			{disabled ? 'Creating...' : submitLabel}
		</button>
	</div>
</form>

<style>
	.task-create-form {
		display: grid;
		gap: 0.75rem;
	}

	.task-create-grid {
		display: grid;
		grid-template-columns: minmax(0, 1.4fr) minmax(180px, 0.6fr);
		gap: 0.65rem;
		align-items: end;
	}

	.task-create-grid--schedule {
		grid-template-columns: minmax(0, 1fr) minmax(180px, 0.6fr);
	}

	.task-create-field {
		display: grid;
		gap: 0.32rem;
		min-width: 0;
		font-size: 0.74rem;
		font-weight: 700;
		color: var(--text-secondary, #6b7280);
	}

	.task-create-field--wide {
		grid-column: 1 / -1;
	}

	.task-create-field input,
	.task-create-field select,
	.task-create-field textarea {
		width: 100%;
		min-width: 0;
		box-sizing: border-box;
		border: 1px solid var(--input-border, var(--border-soft, #d8d2c8));
		border-radius: 6px;
		background: var(--input-bg, var(--bg-card, #fff));
		color: var(--text-primary, #111827);
		font: inherit;
		font-size: 0.82rem;
		font-weight: 500;
		line-height: 1.35;
		padding: 0.52rem 0.6rem;
		transition:
			border-color 0.15s ease,
			box-shadow 0.15s ease,
			background 0.15s ease;
	}

	.task-create-field textarea {
		min-height: 84px;
		resize: vertical;
	}

	.task-create-field input:focus,
	.task-create-field select:focus,
	.task-create-field textarea:focus {
		outline: none;
		background: var(--input-focus-bg, var(--bg-card, #fff));
		border-color: var(--input-focus-border, var(--accent-primary, #c2502a));
		box-shadow: var(--input-focus-shadow, 0 0 0 3px rgba(194, 80, 42, 0.12));
	}

	.task-create-field input:disabled,
	.task-create-field select:disabled,
	.task-create-field textarea:disabled {
		cursor: not-allowed;
		opacity: 0.7;
	}

	.task-create-schedule {
		display: flex;
	}

	.task-create-schedule.open {
		display: block;
	}

	.task-create-schedule-card {
		display: grid;
		gap: 0.65rem;
		border: 1px solid var(--border-soft, #e5e7eb);
		border-radius: 8px;
		background: color-mix(in srgb, var(--bg-card, #fff) 86%, var(--accent-secondary-soft, rgba(78, 205, 196, 0.12)));
		padding: 0.7rem;
	}

	.task-create-schedule-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		font-size: 0.76rem;
		font-weight: 800;
		color: var(--text-primary, #111827);
	}

	.task-create-presets {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.task-create-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	.task-create-button {
		min-height: 1.85rem;
		border-radius: 6px;
		cursor: pointer;
		font: inherit;
		font-size: 0.76rem;
		font-weight: 800;
		line-height: 1;
		padding: 0.42rem 0.75rem;
		transition:
			background 0.15s ease,
			border-color 0.15s ease,
			color 0.15s ease,
			transform 0.15s ease;
	}

	.task-create-button:disabled {
		cursor: not-allowed;
		opacity: 0.62;
	}

	.task-create-button--primary {
		border: 1px solid var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--button-primary-color, #fff);
	}

	.task-create-button--outline,
	.task-create-button--ghost {
		border: 1px solid var(--border-soft, #d8d2c8);
		background: var(--bg-card, #fff);
		color: var(--text-secondary, #5f6668);
	}

	.task-create-button--ghost {
		border-color: transparent;
		background: transparent;
	}

	.task-create-button:hover:not(:disabled) {
		transform: translateY(-1px);
	}

	@media (max-width: 760px) {
		.task-create-grid,
		.task-create-grid--schedule {
			grid-template-columns: 1fr;
		}

		.task-create-schedule-head,
		.task-create-actions {
			align-items: stretch;
			flex-direction: column;
		}

		.task-create-actions .task-create-button {
			width: 100%;
		}
	}
</style>
