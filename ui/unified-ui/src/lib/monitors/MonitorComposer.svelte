<!--
  Monitor create/edit composer (Phase 4, plan §5.2 + §9.1).

  One fixed-size composer, not a wizard: the always-visible fields are
  "what to monitor", "how often", and "notify", with source/rule/strictness
  controls behind a single "More options" disclosure. Submitting moves to a
  REVIEW step that shows the interpreted contract (normalized spec) and the
  EXACT schedule (human cadence summary + raw cron in advanced details) —
  activation IS the POST/PATCH, which only happens from that review step.

  Validation is the local mirror of the backend admission gate
  (`specForm.ts`); the server's stable reasons render identically if it
  disagrees. All colors are theme tokens.
-->
<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import { convertTaskToMonitor, createMonitor, updateMonitor } from './api';
	import {
		CADENCE_PRESETS,
		cadenceSummary,
		matchModeLabel,
		notificationPolicyLabel,
		scheduleFromForm,
		specFromForm,
		specReasonLabel,
		validateAndNormalizeSpec,
		type MonitorFormValue
	} from './specForm';
	import type {
		MonitorMatchMode,
		MonitorNotificationPolicy,
		MonitorSpecV1,
		TaskScheduleWire
	} from '$lib/types/monitor';

	/**
	 * `convert` (Phase 7) turns an EXISTING task into a monitor: the POST
	 * body carries only `{spec, title?}` — the task keeps its id, schedule,
	 * and history, so the cadence editor is replaced by a read-only
	 * "keeps its current schedule" line (`keptCadence`).
	 */
	export let mode: 'create' | 'edit' | 'convert' = 'create';
	/** Task id being edited/converted (required for 'edit' and 'convert'). */
	export let taskId: string | null = null;
	export let form: MonitorFormValue;
	/**
	 * Convert mode only: the human summary of the schedule the task keeps
	 * (`keptScheduleSummary` in ./convert) — 'unscheduled' = run-on-demand.
	 */
	export let keptCadence: string | null = null;
	/**
	 * Hide the composer's own heading + Cancel row when a host surface
	 * already provides that chrome (the Tasks page hosts convert mode
	 * inside a Modal that owns title + close).
	 */
	export let showHeader = true;

	const dispatch = createEventDispatcher<{
		close: void;
		saved: { taskId: string; monitorRevision: number };
	}>();

	type Step = 'form' | 'review';
	let step: Step = 'form';
	let showAdvanced = false;
	let showRawSchedule = false;
	let formError: string | null = null;
	let submitError: string | null = null;
	let submitting = false;
	// Plain closure-level in-flight lock for activate(). `submitting` drives
	// the UI, but the review step has TWO triggers (the error-banner Retry
	// button and the primary Activate button) and a reactive guard alone is
	// not a safe mutex across them — this flag is checked FIRST and set
	// synchronously before any await, so at most one POST/PATCH can ever be
	// in flight. (No vitest covers this: the double-submit race lives in the
	// .svelte component, which the unit suite cannot mount.)
	let activateInFlight = false;

	// Frozen at review time so the user activates EXACTLY what they saw.
	let reviewedSpec: MonitorSpecV1 | null = null;
	let reviewedSchedule: TaskScheduleWire | null = null;

	// Phase 6 a11y: the form→review swap removes the focused "Review" button
	// from the DOM (and Back removes the review controls), which would drop
	// keyboard/screen-reader focus to <body>. Each step container takes
	// tabindex="-1" and receives focus after the swap so reading order
	// resumes at the top of the new step.
	let formRegion: HTMLDivElement | null = null;
	let reviewRegion: HTMLDivElement | null = null;

	$: heading =
		mode === 'create' ? 'New monitor' : mode === 'convert' ? 'Convert to monitor' : 'Edit monitor';
	$: activateLabel =
		mode === 'create'
			? submitting
				? 'Activating…'
				: 'Activate monitor'
			: mode === 'convert'
				? submitting
					? 'Converting…'
					: 'Convert to monitor'
				: submitting
					? 'Saving…'
					: 'Save changes';

	async function review(): Promise<void> {
		formError = null;
		const specResult = validateAndNormalizeSpec(specFromForm(form));
		if (!specResult.ok) {
			formError = specReasonLabel(specResult.reason);
			return;
		}
		if (mode === 'convert') {
			// Conversion never authors a schedule — the task keeps its own.
			reviewedSchedule = null;
		} else {
			const scheduleResult = scheduleFromForm(form);
			if (!scheduleResult.ok) {
				formError =
					scheduleResult.reason === 'monitor_schedule_required'
						? 'Enter a cron expression (5 fields) or pick a preset.'
						: 'That cron expression is not valid (needs 5 space-separated fields).';
				return;
			}
			reviewedSchedule = scheduleResult.schedule;
		}
		reviewedSpec = specResult.spec;
		submitError = null;
		step = 'review';
		await tick();
		reviewRegion?.focus();
	}

	async function backToForm(): Promise<void> {
		step = 'form';
		submitError = null;
		await tick();
		formRegion?.focus();
	}

	async function activate(): Promise<void> {
		// In-flight lock first (see declaration): both Retry and Activate
		// funnel here, and only one submission may ever be in flight.
		if (activateInFlight) return;
		const spec = reviewedSpec;
		if (!spec || submitting) return;
		activateInFlight = true;
		submitting = true;
		submitError = null;
		try {
			const title = form.title.trim() || undefined;
			const scheduleBody = reviewedSchedule ?? undefined;
			const saved =
				mode === 'create'
					? await createMonitor({ title, spec, schedule: scheduleBody })
					: mode === 'convert'
						? // Phase 7 contract: `{spec, title?}` only — no schedule key.
							await convertTaskToMonitor(taskId ?? '', { spec, title })
						: await updateMonitor(taskId ?? '', {
								title,
								spec,
								schedule: scheduleBody
							});
			dispatch('saved', { taskId: saved.task_id, monitorRevision: saved.monitor_revision });
			// On success `submitting` intentionally stays true — the parent
			// closes the composer on `saved`, and the button must not rearm.
		} catch (err) {
			// The server's stable reasons get the same human labels as local ones.
			const raw = err instanceof Error ? err.message : String(err);
			submitError = specReasonLabel(raw);
			submitting = false;
		} finally {
			activateInFlight = false;
		}
	}

</script>

<section class="mon-composer" class:mon-composer--bare={!showHeader} aria-label={heading}>
	{#if showHeader}
		<header class="mon-composer__head">
			<h2>{heading}</h2>
			<Button variant="outline" size="sm" label="Cancel" on:click={() => dispatch('close')} />
		</header>
	{/if}

	{#if step === 'form'}
		<div class="mon-composer__grid" bind:this={formRegion} tabindex="-1">
			<label class="mon-field">
				<span class="mon-field__label">What to monitor</span>
				<textarea
					class="mon-field__input mon-field__input--area"
					rows="2"
					placeholder="e.g. Watch the Acme pricing page and tell me when plans or prices change"
					bind:value={form.objective}
				></textarea>
			</label>

			<label class="mon-field">
				<span class="mon-field__label">Page URLs (one per line)</span>
				<textarea
					class="mon-field__input mon-field__input--area"
					rows="2"
					placeholder="https://acme.example/pricing"
					bind:value={form.urlsText}
				></textarea>
			</label>

			<div class="mon-field-row">
				{#if mode === 'convert'}
					<!-- Conversion never touches the task's schedule (the POST
					     carries no schedule key) — show what it keeps instead. -->
					<div class="mon-field">
						<span class="mon-field__label">Schedule</span>
						<p class="mon-field__static">
							Keeps its current schedule: {keptCadence ?? 'unscheduled'}
						</p>
					</div>
				{:else}
					<label class="mon-field">
						<span class="mon-field__label">How often</span>
						<select class="mon-field__input" bind:value={form.cadence}>
							{#each CADENCE_PRESETS as preset (preset.id)}
								<option value={preset.id}>{preset.label}</option>
							{/each}
							<option value="custom">Custom cron…</option>
							<option value="none">On demand only</option>
						</select>
					</label>
				{/if}
				<label class="mon-field">
					<span class="mon-field__label">Notify</span>
					<!-- value+on:change instead of bind: the bound field is a
					     string union and select binding is string-typed. -->
					<select
						class="mon-field__input"
						value={form.notificationPolicy}
						on:change={(event) =>
							(form.notificationPolicy = event.currentTarget
								.value as MonitorNotificationPolicy)}
					>
						<option value="material_changes">Material changes only</option>
						<option value="every_run">Every run</option>
						<option value="never">Never</option>
					</select>
				</label>
			</div>

			{#if form.cadence === 'custom' && mode !== 'convert'}
				<div class="mon-field-row">
					<label class="mon-field">
						<span class="mon-field__label">Cron expression</span>
						<input
							class="mon-field__input"
							type="text"
							placeholder="0 9 * * 1"
							bind:value={form.cronExpression}
						/>
					</label>
					<label class="mon-field">
						<span class="mon-field__label">Timezone (IANA)</span>
						<input
							class="mon-field__input"
							type="text"
							placeholder="America/New_York"
							bind:value={form.timezone}
						/>
					</label>
				</div>
			{/if}

			<button
				type="button"
				class="mon-composer__disclosure"
				aria-expanded={showAdvanced}
				on:click={() => (showAdvanced = !showAdvanced)}
			>
				{showAdvanced ? '▾' : '▸'} More options
			</button>

			{#if showAdvanced}
				<div class="mon-composer__grid mon-composer__grid--advanced">
					<label class="mon-field">
						<span class="mon-field__label">Title (optional)</span>
						<input
							class="mon-field__input"
							type="text"
							placeholder="Derived from the objective when empty"
							bind:value={form.title}
						/>
					</label>
					<div class="mon-field-row">
						<label class="mon-field">
							<span class="mon-field__label">Domains (one per line)</span>
							<textarea
								class="mon-field__input mon-field__input--area"
								rows="2"
								placeholder="acme.example"
								bind:value={form.domainsText}
							></textarea>
						</label>
						<label class="mon-field">
							<span class="mon-field__label">Search phrases (one per line)</span>
							<textarea
								class="mon-field__input mon-field__input--area"
								rows="2"
								placeholder="acme pricing change"
								bind:value={form.querySeedsText}
							></textarea>
						</label>
					</div>
					<div class="mon-field-row">
						<label class="mon-field">
							<span class="mon-field__label">Include rules</span>
							<textarea
								class="mon-field__input mon-field__input--area"
								rows="2"
								placeholder="plan price changes"
								bind:value={form.includeRulesText}
							></textarea>
						</label>
						<label class="mon-field">
							<span class="mon-field__label">Exclude rules</span>
							<textarea
								class="mon-field__input mon-field__input--area"
								rows="2"
								placeholder="blog posts"
								bind:value={form.excludeRulesText}
							></textarea>
						</label>
					</div>
					<div class="mon-field-row">
						<label class="mon-field">
							<span class="mon-field__label">Matching</span>
							<select
								class="mon-field__input"
								value={form.matchMode}
								on:change={(event) =>
									(form.matchMode = event.currentTarget.value as MonitorMatchMode)}
							>
								<option value="strict">Strict — only squarely in scope</option>
								<option value="balanced">Balanced (recommended)</option>
								<option value="broad">Broad — flag anything adjacent</option>
							</select>
						</label>
						<label class="mon-field">
							<span class="mon-field__label">Signed-in sources (one per line)</span>
							<textarea
								class="mon-field__input mon-field__input--area"
								rows="2"
								placeholder="acme-dashboard"
								bind:value={form.authenticatedText}
							></textarea>
						</label>
					</div>
					<label class="mon-check">
						<input type="checkbox" bind:checked={form.notifyInitialBaseline} />
						<span>Notify me about the initial baseline scan</span>
					</label>
				</div>
			{/if}

			{#if formError}
				<p class="mon-composer__error" role="alert">{formError}</p>
			{/if}

			<div class="mon-composer__actions">
				<Button variant="primary" size="sm" label="Review" on:click={review} />
			</div>
		</div>
	{:else if reviewedSpec}
		<div class="mon-review" aria-label="Review monitor contract" bind:this={reviewRegion} tabindex="-1">
			<p class="mon-review__intro">
				Review the interpreted contract — nothing runs until you activate it.
			</p>
			<dl class="mon-review__list">
				<div class="mon-review__row">
					<dt>Objective</dt>
					<dd>{reviewedSpec.objective}</dd>
				</div>
				<div class="mon-review__row">
					<dt>Sources</dt>
					<dd>
						{#if reviewedSpec.sources.urls.length === 0 && reviewedSpec.sources.domains.length === 0 && reviewedSpec.query_seeds.length === 0}
							<span>—</span>
						{:else}
							<ul class="mon-review__sources">
								{#each reviewedSpec.sources.urls as url (url)}
									<li><span class="mon-review__source-kind">URL</span> {url}</li>
								{/each}
								{#each reviewedSpec.sources.domains as domain (domain)}
									<li><span class="mon-review__source-kind">Domain</span> {domain}</li>
								{/each}
								{#each reviewedSpec.sources.authenticated_sources as source (source)}
									<li>
										<span class="mon-review__source-kind">Signed-in</span> {source}
									</li>
								{/each}
								{#each reviewedSpec.query_seeds as seed (seed)}
									<li><span class="mon-review__source-kind">Search</span> {seed}</li>
								{/each}
							</ul>
						{/if}
					</dd>
				</div>
				<div class="mon-review__row">
					<dt>Schedule</dt>
					<dd>
						{#if mode === 'convert'}
							{keptCadence ?? 'unscheduled'}
							<span class="mon-review__note">
								— the task keeps its existing schedule{(keptCadence ?? 'unscheduled') ===
								'unscheduled'
									? ' (runs only when you press Run now)'
									: ''}
							</span>
						{:else}
							{cadenceSummary(reviewedSchedule)}
						{/if}
						{#if reviewedSchedule}
							<button
								type="button"
								class="mon-review__raw-toggle"
								aria-expanded={showRawSchedule}
								on:click={() => (showRawSchedule = !showRawSchedule)}
							>
								{showRawSchedule ? 'Hide raw schedule' : 'Show raw schedule'}
							</button>
							{#if showRawSchedule}
								<pre class="mon-review__raw">{JSON.stringify(reviewedSchedule, null, 2)}</pre>
							{/if}
						{:else if mode !== 'convert'}
							<span class="mon-review__note">— runs only when you press Run now</span>
						{/if}
					</dd>
				</div>
				{#if reviewedSpec.include_rules.length > 0 || reviewedSpec.exclude_rules.length > 0}
					<div class="mon-review__row">
						<dt>Rules</dt>
						<dd>
							{#if reviewedSpec.include_rules.length > 0}
								<div>Include: {reviewedSpec.include_rules.join(' · ')}</div>
							{/if}
							{#if reviewedSpec.exclude_rules.length > 0}
								<div>Exclude: {reviewedSpec.exclude_rules.join(' · ')}</div>
							{/if}
						</dd>
					</div>
				{/if}
				<div class="mon-review__row">
					<dt>Behavior</dt>
					<dd class="mon-review__badges">
						<Badge text={matchModeLabel(reviewedSpec.match_mode)} color="default" />
						<Badge
							text={notificationPolicyLabel(reviewedSpec.notification_policy)}
							color="info"
						/>
						{#if reviewedSpec.notify_initial_baseline}
							<Badge text="Baseline notifies" color="warning" />
						{/if}
					</dd>
				</div>
			</dl>

			{#if submitError}
				<div class="mon-composer__error-banner" role="alert">
					<span
						>{mode === 'create'
							? "Couldn't activate"
							: mode === 'convert'
								? "Couldn't convert"
								: "Couldn't save"}: {submitError}</span
					>
					<Button variant="outline" size="sm" label="Retry" on:click={activate} />
				</div>
			{/if}

			<div class="mon-composer__actions">
				<Button variant="outline" size="sm" label="Back to edit" on:click={backToForm} />
				<Button
					variant="primary"
					size="sm"
					label={activateLabel}
					interactive={!submitting}
					on:click={activate}
				/>
			</div>
		</div>
	{/if}
</section>

<style>
	.mon-composer {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
	}

	/* Headerless (modal-hosted) variant: the host owns chrome + padding. */
	.mon-composer--bare {
		padding: 0;
		border: 0;
		box-shadow: none;
		background: transparent;
	}

	.mon-composer__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.mon-composer__head h2 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.05rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.mon-composer__grid {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	.mon-field-row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		gap: 0.7rem;
	}

	.mon-field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 0;
	}

	.mon-field__label {
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.mon-field__input {
		width: 100%;
		box-sizing: border-box;
		padding: 0.45rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.85rem;
	}

	.mon-field__input--area {
		resize: vertical;
		min-height: 2.4rem;
	}

	/* Read-only field body (convert mode's kept-schedule line). */
	.mon-field__static {
		margin: 0;
		padding: 0.45rem 0;
		font-size: 0.85rem;
		color: var(--text-secondary);
	}

	.mon-field__input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.mon-check {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.82rem;
		color: var(--text-secondary);
	}

	.mon-composer__disclosure,
	.mon-review__raw-toggle {
		align-self: flex-start;
		border: 0;
		background: transparent;
		padding: 0;
		color: var(--accent-primary, var(--text-secondary));
		font-family: var(--font-primary);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.mon-composer__disclosure:focus-visible,
	.mon-review__raw-toggle:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.mon-composer__grid--advanced {
		padding: 0.7rem;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm, 6px);
	}

	.mon-composer__error {
		margin: 0;
		font-size: 0.8rem;
		color: var(--color-error, var(--status-failed));
	}

	.mon-composer__error-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
		padding: 0.5rem 0.7rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.8rem;
	}

	.mon-composer__actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	.mon-review {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}

	.mon-review__intro {
		margin: 0;
		font-size: 0.82rem;
		color: var(--text-secondary);
	}

	.mon-review__list {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		margin: 0;
		padding: 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
	}

	.mon-review__row {
		display: grid;
		grid-template-columns: 7rem 1fr;
		gap: 0.6rem;
		min-width: 0;
	}

	.mon-review__row dt {
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.mon-review__row dd {
		margin: 0;
		min-width: 0;
		font-size: 0.85rem;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-review__sources {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.mon-review__source-kind {
		display: inline-block;
		min-width: 4.2rem;
		font-size: 0.7rem;
		font-weight: 700;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.mon-review__badges {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}

	.mon-review__raw {
		margin: 0.35rem 0 0;
		padding: 0.5rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--text-primary) 6%, transparent);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		color: var(--text-secondary);
		overflow-x: auto;
	}

	.mon-review__note {
		color: var(--text-muted);
		font-size: 0.78rem;
	}
</style>
