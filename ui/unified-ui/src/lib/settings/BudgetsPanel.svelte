<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		ACTION_LIMIT_PERIODS,
		ACTION_LIMIT_SCOPES,
		actionLimitKey,
		actionLimitLabel,
		capInputText,
		describeReset,
		describeWindow,
		fetchBudgetDefaultsState,
		formatCap,
		formatUsd,
		fetchActionLimits,
		formatLimitCeiling,
		grantSpendEnvelope,
		parseDollarInput,
		parseLimitInput,
		parsePercentInput,
		periodLabel,
		removeActionLimit,
		saveActionLimit,
		saveBudgetDefaultsState,
		saveSpendEnvelope,
		sharedPerRunLimit,
		spendClassLabel,
		spendEnvelopeError,
		spendEnvelopePoll,
		type ActionLimit,
		type ActionLimitPeriod,
		type ActionLimitScope,
		type ActionLimitsState,
		type BudgetCap,
		type BudgetDefaultsState,
		type SpendEnvelopeState
	} from '$lib/settings/budgets';

	const SCOPE_LABELS: Record<ActionLimitScope, string> = { tool: 'A tool', agent: 'An agent', principal: 'A person (principal)' };
	const KNOWN_COMMODITIES = ['EMAIL_SENDS', 'WHATSAPP_SENDS', 'INR', 'USD'];

	const polledEnvelope = spendEnvelopePoll.value;

	// Per-run limit
	let defaults = $state<BudgetDefaultsState | null>(null);
	let defaultsError = $state<string | null>(null);
	let perRunText = $state('');
	let perRunUnlimited = $state(false);
	let split = $state(false);
	let nativeText = $state('');
	let nativeUnlimited = $state(false);
	let harnessText = $state('');
	let harnessUnlimited = $state(false);
	let perRunFieldError = $state<string | null>(null);
	let perRunBusy = $state(false);
	let perRunSaved = $state(false);

	// Daily spend limit
	let envelope = $state<SpendEnvelopeState | null>(null);
	let envelopeDirty = $state(false);
	let dailyEnabled = $state(true);
	let dailyText = $state('');
	let dailyUnlimited = $state(false);
	let warnText = $state('');
	let dailyFieldError = $state<string | null>(null);
	let warnFieldError = $state<string | null>(null);
	let dailyBusy = $state(false);
	let dailySaved = $state(false);

	// Add to today
	let grantText = $state('');
	let grantNote = $state('');
	let grantFieldError = $state<string | null>(null);
	let grantBusy = $state(false);

	// Action limits: hidden on a server without `/budgets/limits`
	let limits = $state<ActionLimitsState | null>(null);
	let limitsAvailable = $state(true);
	let limitsError = $state<string | null>(null);
	let limitDrafts = $state<Record<string, { ceiling: string; period: ActionLimitPeriod }>>({});
	let limitRowErrors = $state<Record<string, string>>({});
	let limitsBusy = $state(false);
	let addScope = $state<ActionLimitScope>('tool');
	let addId = $state('');
	let addCommodity = $state('');
	let addCeiling = $state('');
	let addPeriod = $state<ActionLimitPeriod>('daily');
	let addError = $state<string | null>(null);

	onMount(() => {
		void loadDefaults();
		void loadLimits();
		const releaseFast = spendEnvelopePoll.requestFast();
		return releaseFast;
	});

	// Follow the poll only; a save's own answer is not overwritten until the
	// next poll brings a newer snapshot.
	$effect(() => {
		const polled = $polledEnvelope;
		if (!polled) return;
		untrack(() => {
			envelope = polled;
			if (!envelopeDirty) syncEnvelopeDraft(polled);
		});
	});

	function message(caught: unknown, fallback: string): string {
		return caught instanceof Error && caught.message ? caught.message : fallback;
	}

	function syncDefaultsDraft(next: BudgetDefaultsState): void {
		const shared = sharedPerRunLimit(next);
		split = shared === null;
		perRunUnlimited = (shared ?? next.harness_max_cost_usd) === 'unlimited';
		perRunText = capInputText(shared ?? next.harness_max_cost_usd);
		nativeUnlimited = next.native_max_cost_usd === 'unlimited';
		nativeText = capInputText(next.native_max_cost_usd);
		harnessUnlimited = next.harness_max_cost_usd === 'unlimited';
		harnessText = capInputText(next.harness_max_cost_usd);
		perRunFieldError = null;
	}

	function syncEnvelopeDraft(next: SpendEnvelopeState): void {
		dailyEnabled = next.settings.enabled;
		dailyUnlimited = next.settings.daily_usd === 'unlimited';
		dailyText = capInputText(next.settings.daily_usd);
		warnText = String(next.settings.warn_at_pct);
		dailyFieldError = null;
		warnFieldError = null;
	}

	async function loadDefaults(): Promise<void> {
		try {
			defaults = await fetchBudgetDefaultsState();
			defaultsError = null;
			syncDefaultsDraft(defaults);
		} catch (caught) {
			defaultsError = message(caught, 'Budget defaults are unavailable.');
		}
	}

	function readCap(text: string, unlimited: boolean): { value: BudgetCap } | { error: string } {
		if (unlimited) return { value: 'unlimited' };
		return parseDollarInput(text);
	}

	async function savePerRun(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		let update: { native_max_cost_usd: BudgetCap; harness_max_cost_usd: BudgetCap };
		if (split) {
			const native = readCap(nativeText, nativeUnlimited);
			const harness = readCap(harnessText, harnessUnlimited);
			if ('error' in native) {
				perRunFieldError = `Magician runs: ${native.error}`;
				return;
			}
			if ('error' in harness) {
				perRunFieldError = `Harness runs: ${harness.error}`;
				return;
			}
			update = { native_max_cost_usd: native.value, harness_max_cost_usd: harness.value };
		} else {
			const both = readCap(perRunText, perRunUnlimited);
			if ('error' in both) {
				perRunFieldError = both.error;
				return;
			}
			update = { native_max_cost_usd: both.value, harness_max_cost_usd: both.value };
		}
		perRunFieldError = null;
		perRunBusy = true;
		perRunSaved = false;
		try {
			defaults = await saveBudgetDefaultsState(update);
			syncDefaultsDraft(defaults);
			perRunSaved = true;
			showSuccess('Per-run limit saved');
		} catch (caught) {
			perRunFieldError = message(caught, 'The per-run limit could not be saved.');
			showError(perRunFieldError);
		} finally {
			perRunBusy = false;
		}
	}

	function markEnvelopeDirty(): void {
		envelopeDirty = true;
		dailySaved = false;
	}

	async function saveDaily(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const daily = readCap(dailyText, dailyUnlimited);
		const warn = parsePercentInput(warnText);
		dailyFieldError = 'error' in daily ? daily.error : null;
		warnFieldError = 'error' in warn ? warn.error : null;
		if ('error' in daily || 'error' in warn) return;
		dailyBusy = true;
		dailySaved = false;
		try {
			envelope = await saveSpendEnvelope({ enabled: dailyEnabled, daily_usd: daily.value, warn_at_pct: warn.value });
			envelopeDirty = false;
			syncEnvelopeDraft(envelope);
			dailySaved = true;
			spendEnvelopePoll.pollNow();
			showSuccess('Daily spend limit saved');
		} catch (caught) {
			dailyFieldError = message(caught, 'The daily spend limit could not be saved.');
			showError(dailyFieldError);
		} finally {
			dailyBusy = false;
		}
	}

	async function addToToday(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const amount = parseDollarInput(grantText);
		if ('error' in amount) {
			grantFieldError = amount.error;
			return;
		}
		grantFieldError = null;
		grantBusy = true;
		try {
			envelope = await grantSpendEnvelope(amount.value, grantNote);
			grantText = '';
			grantNote = '';
			spendEnvelopePoll.pollNow();
			showSuccess(`Added ${formatUsd(amount.value)} to today`);
		} catch (caught) {
			grantFieldError = message(caught, 'The top-up could not be added.');
			showError(grantFieldError);
		} finally {
			grantBusy = false;
		}
	}

	/** Take the server's rows. A row the owner has edited and not saved keeps
	 * its draft (and error); only `saved` — the row just written — is reset. */
	function applyLimits(next: ActionLimitsState, saved?: string): void {
		const previous = new Map((limits?.limits ?? []).map((limit) => [actionLimitKey(limit), limit]));
		const drafts: Record<string, { ceiling: string; period: ActionLimitPeriod }> = {};
		const errors: Record<string, string> = {};
		for (const limit of next.limits) {
			const key = actionLimitKey(limit);
			const draft = limitDrafts[key];
			const before = previous.get(key);
			const edited =
				key !== saved &&
				draft !== undefined &&
				before !== undefined &&
				(draft.ceiling !== before.ceiling || draft.period !== before.period);
			drafts[key] = edited ? draft : { ceiling: limit.ceiling, period: limit.period };
			if (edited && limitRowErrors[key]) errors[key] = limitRowErrors[key];
		}
		limitDrafts = drafts;
		limitRowErrors = errors;
		limits = next;
	}

	async function loadLimits(): Promise<void> {
		try {
			const next = await fetchActionLimits();
			if (!next) {
				limitsAvailable = false;
				return;
			}
			limitsError = null;
			applyLimits(next);
		} catch (caught) {
			limitsError = message(caught, 'Action limits are unavailable.');
		}
	}

	async function saveLimitRow(event: SubmitEvent, limit: ActionLimit): Promise<void> {
		event.preventDefault();
		const key = actionLimitKey(limit);
		const draft = limitDrafts[key];
		if (!draft) return;
		const ceiling = parseLimitInput(draft.ceiling);
		if ('error' in ceiling) {
			limitRowErrors = { ...limitRowErrors, [key]: ceiling.error };
			return;
		}
		limitsBusy = true;
		try {
			applyLimits(await saveActionLimit({ ...limit, ceiling: ceiling.value, period: draft.period }), key);
			showSuccess(`${actionLimitLabel(limit)} limit saved`);
		} catch (caught) {
			const text = message(caught, 'The action limit could not be saved.');
			limitRowErrors = { ...limitRowErrors, [key]: text };
			showError(text);
		} finally {
			limitsBusy = false;
		}
	}

	async function removeLimitRow(limit: ActionLimit): Promise<void> {
		const key = actionLimitKey(limit);
		limitsBusy = true;
		try {
			applyLimits(await removeActionLimit(limit));
			showSuccess(`${actionLimitLabel(limit)} limit removed`);
		} catch (caught) {
			const text = message(caught, 'The action limit could not be removed.');
			limitRowErrors = { ...limitRowErrors, [key]: text };
			showError(text);
		} finally {
			limitsBusy = false;
		}
	}

	async function addLimit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const id = addId.trim();
		const commodity = addCommodity.trim().toUpperCase();
		if (!id) {
			addError = 'Name the tool, agent or person this limit is for.';
			return;
		}
		if (!commodity) {
			addError = 'Name what the limit counts, like EMAIL_SENDS or INR.';
			return;
		}
		const ceiling = parseLimitInput(addCeiling);
		if ('error' in ceiling) {
			addError = ceiling.error;
			return;
		}
		addError = null;
		limitsBusy = true;
		try {
			const added: ActionLimit = { scope: addScope, id, commodity, ceiling: ceiling.value, period: addPeriod };
			applyLimits(await saveActionLimit(added), actionLimitKey(added));
			addId = '';
			addCommodity = '';
			addCeiling = '';
			showSuccess(`${actionLimitLabel(added)} limit added`);
		} catch (caught) {
			addError = message(caught, 'The action limit could not be saved.');
			showError(addError);
		} finally {
			limitsBusy = false;
		}
	}

	const status = $derived(envelope?.status ?? null);
	const usedPct = $derived(status?.used_pct === null || status?.used_pct === undefined ? null : Math.min(100, status.used_pct));
</script>

<section id="budgets" class="budget-card" aria-labelledby="budgets-title">
	<div class="budget-header">
		<p class="budget-overline">Spending</p>
		<h2 id="budgets-title">How much agents may spend</h2>
		<p>
			A run stops and asks you when it reaches its per-run limit. All autonomous model work together pauses at the
			daily spend limit until the window resets or you add more for today. Action limits cap what tools and agents
			may send or spend.
		</p>
	</div>

	<section id="model-spend" class="budget-group" aria-labelledby="model-spend-title">
		<h3 id="model-spend-title">Model spend</h3>
		<p class="budget-hint">These limits live in resource authority; model spend is posted to its ledger every 15 minutes.</p>

		<form class="budget-block" onsubmit={savePerRun} aria-labelledby="per-run-title" novalidate>
			<h4 id="per-run-title">Per run limit</h4>
			{#if defaultsError}
				<p class="budget-error" role="alert">{defaultsError}</p>
			{:else if !defaults}
				<p>Loading the per-run limit…</p>
			{:else}
				{#if !split}
					<div class="budget-row">
						<label class="budget-field">
							<span>Each run may spend up to</span>
							<span class="budget-money">
								<span aria-hidden="true">$</span>
								<input
									type="text"
									inputmode="decimal"
									aria-label="Per run limit in dollars"
									aria-invalid={perRunFieldError ? 'true' : undefined}
									bind:value={perRunText}
									disabled={perRunUnlimited || perRunBusy}
									oninput={() => (perRunSaved = false)}
								/>
							</span>
						</label>
						<label class="budget-check">
							<input type="checkbox" bind:checked={perRunUnlimited} disabled={perRunBusy} onchange={() => (perRunSaved = false)} />
							Unlimited
						</label>
					</div>
				{:else}
					<p class="budget-note">Magician runs and harness runs have different limits.</p>
				{/if}
				<details class="budget-advanced" bind:open={split}>
					<summary>Advanced: separate limits for Magician and harness runs</summary>
					<div class="budget-row">
						<label class="budget-field">
							<span>Magician runs (own models)</span>
							<span class="budget-money">
								<span aria-hidden="true">$</span>
								<input type="text" inputmode="decimal" aria-label="Magician run limit in dollars" bind:value={nativeText} disabled={nativeUnlimited || perRunBusy} />
							</span>
						</label>
						<label class="budget-check"><input type="checkbox" bind:checked={nativeUnlimited} disabled={perRunBusy} /> Unlimited</label>
					</div>
					<div class="budget-row">
						<label class="budget-field">
							<span>Harness runs (Claude Code, Codex, …)</span>
							<span class="budget-money">
								<span aria-hidden="true">$</span>
								<input type="text" inputmode="decimal" aria-label="Harness run limit in dollars" bind:value={harnessText} disabled={harnessUnlimited || perRunBusy} />
							</span>
						</label>
						<label class="budget-check"><input type="checkbox" bind:checked={harnessUnlimited} disabled={perRunBusy} /> Unlimited</label>
					</div>
					<p class="budget-hint">
						Now: Magician {formatCap(defaults.native_max_cost_usd)} · harness {formatCap(defaults.harness_max_cost_usd)}. Close
						this to set one limit for both.
					</p>
				</details>
				{#if defaults.native_env_override}
					<p class="budget-note" role="note">
						<code>MAGICIAN_AGENTIC_MAX_COST_USD</code> is set on the server and overrides the Magician run limit.
					</p>
				{/if}
				{#if perRunFieldError}
					<p class="budget-error" role="alert">{perRunFieldError}</p>
				{/if}
				<div class="budget-actions">
					<button class="budget-button" type="submit" disabled={perRunBusy}>{perRunBusy ? 'Saving…' : 'Save per run limit'}</button>
					{#if perRunSaved}<span class="budget-saved" role="status">Saved</span>{/if}
				</div>
			{/if}
		</form>

		<form class="budget-block" onsubmit={saveDaily} aria-labelledby="daily-title" novalidate>
			<h4 id="daily-title">Daily spend limit</h4>
			{#if !envelope}
				{#if $spendEnvelopeError}
					<p class="budget-error" role="alert">{$spendEnvelopeError}</p>
				{:else}
					<p>Loading the daily spend limit…</p>
				{/if}
			{:else}
				<label class="budget-check">
					<input type="checkbox" bind:checked={dailyEnabled} disabled={dailyBusy} onchange={markEnvelopeDirty} />
					Pause autonomous work when the limit is reached
				</label>
				<div class="budget-row">
					<label class="budget-field">
						<span>Limit per {describeWindow(envelope.settings)}</span>
						<span class="budget-money">
							<span aria-hidden="true">$</span>
							<input
								type="text"
								inputmode="decimal"
								aria-label="Daily spend limit in dollars"
								aria-invalid={dailyFieldError ? 'true' : undefined}
								bind:value={dailyText}
								disabled={dailyUnlimited || dailyBusy}
								oninput={markEnvelopeDirty}
							/>
						</span>
					</label>
					<label class="budget-check">
						<input type="checkbox" bind:checked={dailyUnlimited} disabled={dailyBusy} onchange={markEnvelopeDirty} />
						Unlimited
					</label>
				</div>
				<label class="budget-field">
					<span>Warn me at</span>
					<span class="budget-money">
						<input
							type="text"
							inputmode="numeric"
							aria-label="Warn at percent of the daily limit"
							aria-invalid={warnFieldError ? 'true' : undefined}
							bind:value={warnText}
							disabled={dailyBusy}
							oninput={markEnvelopeDirty}
						/>
						<span aria-hidden="true">%</span>
					</span>
				</label>
				{#if dailyFieldError}<p class="budget-error" role="alert">{dailyFieldError}</p>{/if}
				{#if warnFieldError}<p class="budget-error" role="alert">{warnFieldError}</p>{/if}
				<div class="budget-actions">
					<button class="budget-button" type="submit" disabled={dailyBusy}>{dailyBusy ? 'Saving…' : 'Save daily limit'}</button>
					{#if dailySaved}<span class="budget-saved" role="status">Saved</span>{/if}
				</div>
			{/if}
		</form>

		{#if envelope}
			<section class="budget-block" aria-labelledby="today-title">
				<h4 id="today-title">Today</h4>
				{#if !envelope.installed || !status}
					<p>Live spend is not available from this server process.</p>
				{:else}
					<div class="budget-status" class:budget-warning={status.warning} class:budget-exhausted={status.exhausted}>
						<dl>
							<div><dt>Spent</dt><dd>{formatUsd(status.spent_usd)}</dd></div>
							<div>
								<dt>Remaining</dt>
								<dd>{status.remaining_usd === null ? 'Unlimited' : formatUsd(Math.max(0, status.remaining_usd))}</dd>
							</div>
							<div>
								<dt>Limit today</dt>
								<dd>
									{status.effective_limit_usd === null ? 'Unlimited' : formatUsd(status.effective_limit_usd)}
									{#if status.granted_usd > 0}<small>(includes {formatUsd(status.granted_usd)} added)</small>{/if}
								</dd>
							</div>
							<div><dt>Resets</dt><dd>{describeReset(status.resets_at_ms)}</dd></div>
						</dl>
						{#if usedPct !== null}
							<div
								class="budget-meter"
								role="progressbar"
								aria-label="Share of today's limit spent"
								aria-valuemin="0"
								aria-valuemax="100"
								aria-valuenow={Math.round(usedPct)}
							>
								<span style={`width: ${usedPct}%`}></span>
							</div>
						{/if}
						{#if status.exhausted}
							<p class="budget-error" role="status">
								The {status.exhausted_by === 'total' ? 'total' : 'daily'} limit is reached: autonomous work is paused.
							</p>
						{:else if !status.enabled}
							<p class="budget-note">The limit is off: spend is tracked but nothing pauses.</p>
						{:else if status.warning}
							<p class="budget-note" role="status">Over {envelope.settings.warn_at_pct}% of today's limit is spent.</p>
						{/if}
					</div>
					{#if status.by_class.length > 0}
						<details class="budget-advanced">
							<summary>Spend by kind of work</summary>
							<ul class="budget-classes">
								{#each status.by_class as row (row.class)}
									<li><span>{spendClassLabel(row.class)}</span><span>{formatUsd(row.spent_usd)}</span></li>
								{/each}
							</ul>
						</details>
					{/if}
					<form class="budget-grant" onsubmit={addToToday} novalidate>
						<label class="budget-field">
							<span>Add to today</span>
							<span class="budget-money">
								<span aria-hidden="true">$</span>
								<input
									type="text"
									inputmode="decimal"
									aria-label="Amount to add to today in dollars"
									aria-invalid={grantFieldError ? 'true' : undefined}
									bind:value={grantText}
									disabled={grantBusy}
									placeholder="50"
								/>
							</span>
						</label>
						<label class="budget-field budget-note-field">
							<span>Note (optional)</span>
							<input type="text" maxlength="200" bind:value={grantNote} disabled={grantBusy} />
						</label>
						<button class="budget-button" type="submit" disabled={grantBusy}>{grantBusy ? 'Adding…' : 'Add to today'}</button>
					</form>
					<p class="budget-hint">Only for the current window; the daily limit itself is unchanged.</p>
					{#if grantFieldError}<p class="budget-error" role="alert">{grantFieldError}</p>{/if}
				{/if}
			</section>
		{/if}
	</section>

	{#if limitsAvailable}
		<section id="action-limits" class="budget-group" aria-labelledby="action-limits-title">
			<h3 id="action-limits-title">Action limits</h3>
			<p class="budget-hint">How many emails or messages, or how much money, a tool, agent or person may use per period.</p>
			{#if !limits}
				{#if limitsError}
					<p class="budget-error" role="alert">{limitsError}</p>
				{:else}
					<p>Loading action limits…</p>
				{/if}
			{:else}
				{#if !limits.enabled}
					<p class="budget-note" role="note">Resource authority is off, so these limits are not enforced.</p>
				{/if}
				{#if limits.limits.length === 0}
					<p class="budget-note">No action limits yet.</p>
				{:else}
					<ul class="limit-list">
						{#each limits.limits as limit (actionLimitKey(limit))}
							{@const key = actionLimitKey(limit)}
							{@const label = actionLimitLabel(limit)}
							<li class="limit-row">
								<form onsubmit={(event) => saveLimitRow(event, limit)} aria-label={label} novalidate>
									<div class="limit-head">
										<span class="limit-name">{label}</span>
										<span class="limit-now">{formatLimitCeiling(limit)} {periodLabel(limit.period)}</span>
									</div>
									{#if limitDrafts[key]}
										<div class="budget-row">
											<label class="budget-field">
												<span>Limit</span>
												<input
													type="text"
													inputmode="decimal"
													aria-label={`Limit for ${label}`}
													aria-invalid={limitRowErrors[key] ? 'true' : undefined}
													bind:value={limitDrafts[key].ceiling}
													disabled={limitsBusy}
												/>
											</label>
											<label class="budget-field">
												<span>Period</span>
												<select aria-label={`Period for ${label}`} bind:value={limitDrafts[key].period} disabled={limitsBusy}>
													{#each ACTION_LIMIT_PERIODS as period (period)}
														<option value={period}>{periodLabel(period)}</option>
													{/each}
												</select>
											</label>
											<div class="budget-actions">
												<button class="budget-button" type="submit" aria-label={`Save ${label}`} disabled={limitsBusy}>Save</button>
												<button
													class="budget-button budget-button-secondary"
													type="button"
													aria-label={`Remove ${label}`}
													disabled={limitsBusy}
													onclick={() => removeLimitRow(limit)}
												>
													Remove
												</button>
											</div>
										</div>
									{/if}
									{#if limitRowErrors[key]}<p class="budget-error" role="alert">{limitRowErrors[key]}</p>{/if}
								</form>
							</li>
						{/each}
					</ul>
				{/if}
				<form class="budget-block" onsubmit={addLimit} aria-labelledby="add-limit-title" novalidate>
					<h4 id="add-limit-title">Add limit</h4>
					<div class="budget-row">
						<label class="budget-field">
							<span>Applies to</span>
							<select aria-label="New limit applies to" bind:value={addScope} disabled={limitsBusy}>
								{#each ACTION_LIMIT_SCOPES as scope (scope)}
									<option value={scope}>{SCOPE_LABELS[scope]}</option>
								{/each}
							</select>
						</label>
						<label class="budget-field">
							<span>Id</span>
							<input type="text" aria-label="New limit id" bind:value={addId} disabled={limitsBusy} placeholder={addScope === 'tool' ? 'agentmail-send' : '*'} />
						</label>
						<label class="budget-field">
							<span>Counts</span>
							<input type="text" aria-label="New limit counts" list="action-limit-commodities" bind:value={addCommodity} disabled={limitsBusy} placeholder="EMAIL_SENDS" />
						</label>
						<label class="budget-field">
							<span>Limit</span>
							<input
								type="text"
								inputmode="decimal"
								aria-label="New limit amount"
								aria-invalid={addError ? 'true' : undefined}
								bind:value={addCeiling}
								disabled={limitsBusy}
								placeholder="50"
							/>
						</label>
						<label class="budget-field">
							<span>Period</span>
							<select aria-label="New limit period" bind:value={addPeriod} disabled={limitsBusy}>
								{#each ACTION_LIMIT_PERIODS as period (period)}
									<option value={period}>{periodLabel(period)}</option>
								{/each}
							</select>
						</label>
					</div>
					<datalist id="action-limit-commodities">
						{#each KNOWN_COMMODITIES as commodity (commodity)}<option value={commodity}></option>{/each}
					</datalist>
					{#if addError}<p class="budget-error" role="alert">{addError}</p>{/if}
					<div class="budget-actions">
						<button class="budget-button" type="submit" disabled={limitsBusy}>Add limit</button>
					</div>
				</form>
				{#if limits.applies === 'on_restart'}
					<p class="budget-note" role="note">Changes apply when Magician restarts.</p>
				{/if}
			{/if}
		</section>
	{/if}

	<p class="budget-ledger"><a href="/budget">Open the spend ledger</a></p>
</section>

<style>
	.budget-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		padding: 1rem;
	}
	.budget-overline,
	h2,
	h3,
	h4,
	p {
		margin: 0;
	}
	.budget-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	h2 {
		font-size: 1.05rem;
		margin: 0.15rem 0 0.35rem;
	}
	h3 {
		font-size: 1rem;
	}
	h4 {
		font-size: 0.95rem;
	}
	.budget-group {
		border-top: 1px solid var(--border-soft);
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		padding-top: 0.9rem;
	}
	p {
		color: var(--text-secondary);
		line-height: 1.45;
	}
	.budget-block {
		border-top: 1px solid var(--border-soft);
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		padding-top: 0.75rem;
	}
	.budget-row,
	.budget-grant {
		align-items: flex-end;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
	}
	.budget-field {
		display: flex;
		flex-direction: column;
		font-size: 0.875rem;
		gap: 0.25rem;
	}
	.budget-field > span:first-child {
		color: var(--text-secondary);
		font-weight: 600;
	}
	.budget-money {
		align-items: center;
		display: inline-flex;
		gap: 0.3rem;
	}
	input[type='text'],
	select {
		background: var(--bg-input, var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		padding: 0.35rem 0.5rem;
		width: 8rem;
	}
	.budget-note-field input {
		width: 14rem;
		max-width: 100%;
	}
	input[aria-invalid='true'] {
		border-color: var(--danger);
	}
	input:disabled {
		opacity: 0.6;
	}
	.budget-check {
		align-items: center;
		display: inline-flex;
		font-size: 0.875rem;
		gap: 0.4rem;
		padding-bottom: 0.4rem;
	}
	.budget-advanced summary {
		color: var(--text-secondary);
		cursor: pointer;
		font-size: 0.875rem;
	}
	.budget-advanced[open] {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.budget-actions {
		align-items: center;
		display: flex;
		gap: 0.75rem;
	}
	.budget-button {
		background: var(--accent-primary);
		border: 1px solid var(--accent-primary);
		border-radius: 6px;
		color: var(--accent-on-primary);
		cursor: pointer;
		font-weight: 600;
		padding: 0.45rem 0.9rem;
	}
	.budget-button-secondary {
		background: transparent;
		color: var(--text-primary);
		border-color: var(--border-soft);
	}
	.budget-button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.budget-saved {
		color: var(--success);
		font-size: 0.875rem;
	}
	.budget-hint,
	.budget-note {
		font-size: 0.85rem;
	}
	.budget-error {
		color: var(--danger);
		font-size: 0.875rem;
	}
	.budget-status {
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.6rem 0.75rem;
	}
	.budget-status.budget-warning {
		border-color: var(--warning);
	}
	.budget-status.budget-exhausted {
		border-color: var(--danger);
	}
	dl {
		display: grid;
		gap: 0.5rem 1rem;
		grid-template-columns: repeat(auto-fit, minmax(8rem, 1fr));
		margin: 0;
	}
	dt {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	dd {
		font-weight: 600;
		margin: 0.1rem 0 0;
	}
	dd small {
		color: var(--text-secondary);
		font-weight: 400;
	}
	.budget-meter {
		background: var(--border-soft);
		border-radius: 999px;
		height: 6px;
		overflow: hidden;
	}
	.budget-meter span {
		background: var(--accent-primary);
		display: block;
		height: 100%;
	}
	.budget-warning .budget-meter span {
		background: var(--warning);
	}
	.budget-exhausted .budget-meter span {
		background: var(--danger);
	}
	.budget-classes {
		display: flex;
		flex-direction: column;
		font-size: 0.875rem;
		gap: 0.25rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.budget-classes li {
		display: flex;
		justify-content: space-between;
		max-width: 22rem;
	}
	.limit-list {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.limit-row form {
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.6rem 0.75rem;
	}
	.limit-head {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 0.75rem;
		justify-content: space-between;
	}
	.limit-name {
		font-weight: 600;
	}
	.limit-now {
		color: var(--text-secondary);
		font-size: 0.875rem;
	}
	.budget-ledger a {
		color: var(--accent-primary);
		font-size: 0.875rem;
		font-weight: 600;
	}
</style>
