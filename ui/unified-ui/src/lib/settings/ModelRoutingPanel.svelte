<script lang="ts">
	import { onMount } from 'svelte';
	import {
		fetchRoutingOverview,
		setOperationProfile,
		clearOperationProfile,
		setOperationEngine,
		clearOperationEngine,
		type EngineFollow,
		type RoutingOperation,
		type RoutingOverview,
		type RoutingProfile
	} from '$lib/stores/modelRoutingStore';
	import { reloadMagicianConfig } from '$lib/stores/magicianConfigStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	let overview = $state<RoutingOverview | null>(null);
	let error = $state<string | null>(null);
	let busyOp = $state<string | null>(null);
	let refreshing = $state(false);
	let reloading = $state(false);
	let filter = $state('');
	let groupFilter = $state('all');

	const classLabel: Record<string, string> = {
		local: 'Local',
		api: 'API',
		harness: 'Harness'
	};

	const sourceLabel: Record<RoutingOperation['routing_source'], string> = {
		config: 'Config',
		parent: 'Parent engine',
		override: 'Override'
	};

	const engineFollows: EngineFollow[] = ['parent', 'pinned'];
	const engineLabel: Record<EngineFollow, string> = {
		parent: 'Parent',
		pinned: 'Pinned'
	};

	/** The engines driving flows now, named for the banner; null is Magician's own loop. */
	const drivingEngines = $derived.by(() => {
		const driving = overview?.driving_engines;
		if (!driving || (!driving.chat && !driving.run)) return null;
		return `Chat: ${driving.chat ?? 'magician'} · Run: ${driving.run ?? 'magician'}`;
	});

	const sortedOperations = $derived(
		(overview?.operations ?? [])
			.slice()
			.sort(
				(a, b) =>
					Number(b.overridden) - Number(a.overridden) ||
					a.group.localeCompare(b.group) ||
					a.operation.localeCompare(b.operation)
			)
	);

	const groups = $derived(Array.from(new Set(sortedOperations.map((operation) => operation.group))));

	const visibleOperations = $derived.by(() => {
		const needle = filter.trim().toLowerCase();
		return sortedOperations
			.filter((operation) => groupFilter === 'all' || operation.group === groupFilter)
			.filter((operation) => {
				if (!needle) return true;
				const effective = profileFor(operation.effective_profile);
				return [
					operation.operation,
					operation.group,
					operation.description,
					operation.effective_profile,
					effective?.provider,
					effective?.model
				]
					.filter(Boolean)
					.some((value) => value!.toLowerCase().includes(needle));
			});
	});

	const overriddenCount = $derived(
		(overview?.operations ?? []).filter((operation) => operation.overridden).length
	);

	function profileFor(name: string): RoutingProfile | undefined {
		return overview?.profiles.find((profile) => profile.name === name);
	}

	function profileClass(name: string): string {
		return profileFor(name)?.class ?? 'api';
	}

	function profileInstalled(name: string): boolean {
		return profileFor(name)?.installed ?? true;
	}

	function profileRuntimeLabel(name: string): string {
		const profile = profileFor(name);
		if (!profile) return name;
		return `${profile.provider} · ${profile.model}`;
	}

	function profileOptionLabel(profile: RoutingProfile): string {
		const unavailable = profile.installed ? '' : ' — not installed';
		return `${profile.name} — ${profile.provider} · ${profile.model}${unavailable}`;
	}

	/** The profiles a following operation would ride, one per external driving engine. */
	function parentRides(operation: RoutingOperation): string[] {
		if (!operation.follows_parent) return [];
		const rides: string[] = [];
		if (operation.parent_profiles.chat) rides.push(`Chat → ${operation.parent_profiles.chat}`);
		if (operation.parent_profiles.run) rides.push(`Run → ${operation.parent_profiles.run}`);
		return rides;
	}

	function automaticChoiceLabel(operation: RoutingOperation): string {
		if (parentRides(operation).length > 0) return 'Follow the parent engine';
		return 'Use automatic routing';
	}

	function automaticStatus(operation: RoutingOperation): string {
		if (operation.routing_source === 'parent') return 'Follows the engine that starts each flow';
		if (operation.local_floor) return 'Local default; uses config mapping';
		return 'No explicit override';
	}

	function engineHint(operation: RoutingOperation): string {
		if (operation.local_floor) return 'local default — never follows';
		const rides = parentRides(operation);
		if (rides.length > 0) return `would ride ${rides.join(' · ')}`;
		if (operation.engine === 'pinned') return 'stays on its own profile';
		return 'follows when an external engine drives';
	}

	function configuredArms(operation: RoutingOperation): Array<{ label: string; profile: string }> {
		if (typeof operation.configured_selector === 'string') {
			return [{ label: 'Config', profile: operation.configured_selector }];
		}
		const arms = [{ label: 'Local/default', profile: operation.configured_selector.default }];
		if (operation.configured_selector.when_cloud) {
			arms.push({ label: 'Cloud', profile: operation.configured_selector.when_cloud });
		}
		if (operation.configured_selector.when_has_images) {
			arms.push({ label: 'With images', profile: operation.configured_selector.when_has_images });
		}
		return arms;
	}

	async function refresh(showToast = false): Promise<void> {
		error = null;
		refreshing = true;
		try {
			overview = await fetchRoutingOverview();
			if (showToast) showSuccess('Model routing refreshed');
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Routing overview failed';
			error = message;
			if (showToast) showError(message);
		} finally {
			refreshing = false;
		}
	}

	async function reloadFromDisk(): Promise<void> {
		if (reloading) return;
		reloading = true;
		error = null;
		try {
			await reloadMagicianConfig();
			await refresh();
			showSuccess('Model routing reloaded from the active config');
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Model routing reload failed';
			error = message;
			showError(message);
		} finally {
			reloading = false;
		}
	}

	async function switchProfile(operation: string, profile: string): Promise<void> {
		if (busyOp) return;
		busyOp = operation;
		error = null;
		try {
			await setOperationProfile(operation, profile);
			await refresh();
			showSuccess(`${operation} now uses ${profile}`);
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Switch failed';
			error = message;
			showError(message);
		} finally {
			busyOp = null;
		}
	}

	async function revert(operation: string): Promise<void> {
		if (busyOp) return;
		busyOp = operation;
		error = null;
		try {
			await clearOperationProfile(operation);
			await refresh();
			showSuccess(`${operation} returned to automatic routing`);
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Clear override failed';
			error = message;
			showError(message);
		} finally {
			busyOp = null;
		}
	}

	async function switchEngine(operation: string, engine: EngineFollow): Promise<void> {
		if (busyOp) return;
		busyOp = operation;
		error = null;
		try {
			await setOperationEngine(operation, engine);
			await refresh();
			showSuccess(
				engine === 'parent'
					? `${operation} now follows the parent engine`
					: `${operation} is pinned to its own profile`
			);
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Engine pin failed';
			error = message;
			showError(message);
		} finally {
			busyOp = null;
		}
	}

	async function revertEngine(operation: string): Promise<void> {
		if (busyOp) return;
		busyOp = operation;
		error = null;
		try {
			await clearOperationEngine(operation);
			await refresh();
			showSuccess(`${operation} returned to its config engine rule`);
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Engine unpin failed';
			error = message;
			showError(message);
		} finally {
			busyOp = null;
		}
	}

	onMount(() => {
		void refresh();
	});
</script>

<section class="routing">
	<div class="heading">
		<div>
			<h3>Operation routing</h3>
			<p class="hint">
				Every configured LLM operation, what it does, and the profile and model serving it now.
				Choosing a profile applies an install-level override immediately; reload reads external YAML
				changes into the running backend.
			</p>
		</div>
		<div class="actions">
			<button type="button" disabled={refreshing || reloading} onclick={() => void refresh(true)}>
				{refreshing ? 'Refreshing…' : 'Refresh'}
			</button>
			<button class="primary" type="button" disabled={refreshing || reloading} onclick={() => void reloadFromDisk()}>
				{reloading ? 'Reloading…' : 'Reload config'}
			</button>
		</div>
	</div>

	{#if error}
		<p class="error" role="alert">{error}</p>
	{/if}

	{#if overview}
		<div class="affinity-banner" class:quiet={!drivingEngines} data-testid="affinity-banner">
			<strong>
				Background operations follow the engine that starts each flow — the chat mouth, the run
				engine, or a connected CLI — unless an operation is pinned.
			</strong>
			{#if drivingEngines}
				<span data-testid="driving-engines">Driving now — {drivingEngines}</span>
			{:else}
				<span>
					Magician's own loop drives now, so operations use explicit overrides first, then the
					{overview.locality ?? 'local'} config mapping.
				</span>
			{/if}
		</div>
	{/if}

	{#if overview}
		<div class="controls">
			<input
				class="filter"
				type="search"
				placeholder="Filter by operation, purpose, profile, or model…"
				bind:value={filter}
			/>
			<select aria-label="Filter operations by group" bind:value={groupFilter}>
				<option value="all">All groups</option>
				{#each groups as group (group)}
					<option value={group}>{group}</option>
				{/each}
			</select>
			<span class="count">{visibleOperations.length} of {overview.operations.length} operations · {overriddenCount} overrides</span>
		</div>

		<div class="rows">
			{#each visibleOperations as operation (operation.operation)}
				<article class="row" class:overridden={operation.overridden}>
					<div class="identity">
						<div class="title-line">
							<span class="op" title={operation.operation}>{operation.operation}</span>
							<span class="group">{operation.group}</span>
						</div>
						<p>{operation.description}</p>
					</div>

					<div class="effective">
						<div class="effective-heading">
							<span class="source {operation.routing_source}">{sourceLabel[operation.routing_source]}</span>
							<span class="chip {profileClass(operation.effective_profile)}">
								{classLabel[profileClass(operation.effective_profile)] ?? 'API'}
							</span>
						</div>
						<strong title={operation.effective_profile}>{operation.effective_profile}</strong>
						<span class="runtime">{profileRuntimeLabel(operation.effective_profile)}</span>
						{#if !profileInstalled(operation.effective_profile)}
							<em title="CLI not on PATH">CLI not installed</em>
						{/if}
					</div>

					<div class="mapping" aria-label={`Configured mappings for ${operation.operation}`}>
						<span class="mapping-title">Configured mapping</span>
						{#each configuredArms(operation) as arm (`${arm.label}:${arm.profile}`)}
							<span class:active-arm={arm.profile === operation.configured_profile}>
								<b>{arm.label}</b> · {arm.profile}
							</span>
						{/each}
					</div>

					<div class="choice">
						<label for={`profile-${operation.operation.replaceAll(':', '-')}`}>Choose profile</label>
						<select
							id={`profile-${operation.operation.replaceAll(':', '-')}`}
							disabled={busyOp === operation.operation}
							value={operation.effective_profile}
							onchange={(event) => {
								const next = event.currentTarget.value;
								if (next && next !== operation.effective_profile) {
									void switchProfile(operation.operation, next);
								}
							}}
						>
							<optgroup label="Local">
								{#each overview.profiles.filter((profile) => profile.class === 'local') as profile (profile.name)}
									<option value={profile.name} disabled={profile.selectable === false}>{profileOptionLabel(profile)}</option>
								{/each}
							</optgroup>
							<optgroup label="API">
								{#each overview.profiles.filter((profile) => profile.class === 'api') as profile (profile.name)}
									<option value={profile.name} disabled={profile.selectable === false}>{profileOptionLabel(profile)}</option>
								{/each}
							</optgroup>
							<optgroup label="Harness (CLI subscription)">
								{#each overview.profiles.filter((profile) => profile.class === 'harness') as profile (profile.name)}
									<option value={profile.name} disabled={profile.selectable === false}>{profileOptionLabel(profile)}</option>
								{/each}
							</optgroup>
						</select>
						{#if operation.stale_override}
							<button class="clear stale" type="button" disabled={busyOp === operation.operation} onclick={() => void revert(operation.operation)}>Clear stale override</button>
						{:else if operation.overridden}
							<button class="clear" type="button" disabled={busyOp === operation.operation} onclick={() => void revert(operation.operation)}>{automaticChoiceLabel(operation)}</button>
						{:else}
							<span class="automatic">{automaticStatus(operation)}</span>
						{/if}
						<div class="engine" role="group" aria-label={`Parent engine rule for ${operation.operation}`}>
							<span class="engine-toggle">
								{#each engineFollows as follow (follow)}
									<button
										class="segment"
										type="button"
										aria-pressed={operation.engine === follow}
										disabled={busyOp === operation.operation}
										onclick={() => {
											if (operation.engine !== follow) void switchEngine(operation.operation, follow);
										}}
									>
										{engineLabel[follow]}
									</button>
								{/each}
							</span>
							<span class="engine-hint">{engineHint(operation)}</span>
							{#if operation.engine_source === 'override'}
								<button class="clear" type="button" disabled={busyOp === operation.operation} onclick={() => void revertEngine(operation.operation)}>Use config engine rule</button>
							{/if}
						</div>
					</div>
				</article>
			{:else}
				<p class="empty">No operation matches those filters.</p>
			{/each}
		</div>
	{:else if !error}
		<p class="hint">Loading routing…</p>
	{/if}
</section>

<style>
	.routing { color: var(--text-primary); display: flex; flex-direction: column; gap: 0.8rem; }
	.heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; }
	h3 { margin: 0 0 0.25rem; }
	.hint { font-size: 0.85rem; opacity: 0.78; margin: 0; max-width: 72ch; }
	.actions, .controls { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; }
	button, select, input { font: inherit; }
	button { padding: 0.4rem 0.7rem; border-radius: 8px; border: 1px solid var(--button-secondary-border, var(--border-soft)); background: var(--button-secondary-bg, var(--bg-soft)); color: var(--button-secondary-color, var(--text-primary)); cursor: pointer; }
	button.primary { background: var(--accent-primary); border-color: var(--accent-primary); color: var(--text-on-accent, var(--accent-on-primary, #fff)); }
	button:disabled { cursor: wait; opacity: 0.55; }
	.error { color: var(--color-error, #c2502a); font-size: 0.85rem; margin: 0; }
	.affinity-banner { display: grid; gap: 0.2rem; font-size: 0.82rem; padding: 0.65rem 0.75rem; border-radius: 9px; border: 1px solid color-mix(in srgb, var(--color-info, #4d9de0) 30%, var(--border-soft)); background: var(--color-info-soft, color-mix(in srgb, var(--color-info, #4d9de0) 10%, var(--bg-card))); color: var(--text-secondary); }
	.affinity-banner.quiet { border-color: var(--border-soft); background: var(--bg-soft, var(--bg-card)); }
	.filter { flex: 1 1 320px; min-width: 0; padding: 0.5rem 0.65rem; border-radius: 8px; border: 1px solid var(--input-border, var(--border-soft)); background: var(--input-bg, var(--bg-soft)); color: var(--text-primary); }
	.controls > select { min-width: 180px; padding: 0.5rem 0.65rem; border-radius: 8px; border: 1px solid var(--input-border, var(--border-soft)); background: var(--input-bg, var(--bg-soft)); color: var(--text-primary); }
	.count { margin-left: auto; font-size: 0.75rem; color: var(--text-secondary); }
	.rows { display: flex; flex-direction: column; gap: 0.55rem; max-height: 680px; overflow-y: auto; padding-right: 0.15rem; }
	.row { display: grid; grid-template-columns: minmax(220px, 1.3fr) minmax(190px, 1fr) minmax(190px, 1fr) minmax(230px, 1.15fr); gap: 0.8rem; align-items: start; padding: 0.75rem; border: 1px solid var(--border-soft); border-radius: 10px; background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%); }
	.row.overridden { border-color: color-mix(in srgb, var(--accent-primary) 38%, var(--border-soft)); background: var(--accent-primary-soft, rgba(255, 107, 107, 0.06)); }
	.identity, .effective, .mapping, .choice { min-width: 0; display: flex; flex-direction: column; gap: 0.28rem; }
	.title-line, .effective-heading { display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; }
	.op { font-family: var(--font-mono, monospace); font-size: 0.8rem; font-weight: 650; overflow-wrap: anywhere; }
	.identity p { margin: 0; color: var(--text-secondary); font-size: 0.79rem; line-height: 1.42; }
	.group, .source, .chip { width: fit-content; font-size: 0.7rem; padding: 0.12rem 0.42rem; border-radius: 999px; white-space: nowrap; }
	.group { background: var(--bg-soft); color: var(--text-secondary); }
	.source { background: var(--bg-soft); color: var(--text-secondary); }
	.source.parent { background: var(--color-info-soft, rgba(90, 140, 255, 0.15)); }
	.source.override { background: var(--accent-primary-soft, rgba(170, 110, 255, 0.16)); color: var(--accent-primary, var(--text-secondary)); }
	.chip.local { background: var(--color-success-soft, rgba(78, 205, 196, 0.18)); color: var(--color-success, var(--text-secondary)); }
	.chip.api { background: var(--color-info-soft, rgba(90, 140, 255, 0.15)); color: var(--color-info, var(--text-secondary)); }
	.chip.harness { background: var(--accent-primary-soft, rgba(170, 110, 255, 0.16)); color: var(--accent-primary, var(--text-secondary)); }
	.effective strong { font-family: var(--font-mono, monospace); font-size: 0.77rem; overflow-wrap: anywhere; }
	.runtime, .mapping span, .choice label, .automatic { color: var(--text-secondary); font-size: 0.74rem; }
	.effective em { color: var(--color-error, #c2502a); font-size: 0.72rem; }
	.mapping-title { font-size: 0.7rem !important; font-weight: 650; text-transform: uppercase; letter-spacing: 0.04em; }
	.mapping span { overflow-wrap: anywhere; }
	.mapping .active-arm { color: var(--text-primary); }
	.choice select { width: 100%; min-width: 0; padding: 0.42rem 0.5rem; border-radius: 8px; border: 1px solid var(--input-border, var(--border-soft)); background: var(--input-bg, var(--bg-soft)); color: var(--text-primary); font-size: 0.78rem; }
	.clear { width: fit-content; padding: 0.28rem 0.55rem; font-size: 0.74rem; }
	.clear.stale { color: var(--color-error, #c2502a); }
	.engine { display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; margin-top: 0.15rem; }
	.engine-toggle { display: inline-flex; border: 1px solid var(--button-secondary-border, var(--border-soft)); border-radius: 999px; overflow: hidden; }
	.segment { padding: 0.2rem 0.6rem; font-size: 0.72rem; border: 0; border-radius: 0; background: transparent; color: var(--text-secondary); }
	.segment + .segment { border-left: 1px solid var(--button-secondary-border, var(--border-soft)); }
	.segment[aria-pressed='true'] { background: var(--accent-primary); color: var(--text-on-accent, var(--accent-on-primary, #fff)); }
	.engine-hint { color: var(--text-secondary); font-size: 0.72rem; overflow-wrap: anywhere; }
	.empty { color: var(--text-secondary); font-size: 0.85rem; margin: 0.5rem; }
	@media (max-width: 1100px) { .row { grid-template-columns: minmax(220px, 1.2fr) minmax(200px, 1fr); } }
	@media (max-width: 680px) { .heading { flex-direction: column; } .actions { width: 100%; } .row { grid-template-columns: 1fr; } .count { width: 100%; margin-left: 0; } .rows { max-height: none; } }
</style>
