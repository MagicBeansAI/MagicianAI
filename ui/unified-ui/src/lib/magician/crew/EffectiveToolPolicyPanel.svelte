<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		effectiveToolCount,
		fetchEffectiveToolPolicy,
		fetchRuntimeContextCache,
		refreshRuntimeContextCache,
		shortPolicyId,
		type EffectiveStructuralToolGrant,
		type EffectiveToolGrant,
		type EffectiveToolPolicyPreview,
		type EffectiveToolSurface,
		type RuntimeContextCachePreview
	} from './effectiveTools';

	export let agentId: string;
	export let compact = false;

	let mounted = false;
	let preview: EffectiveToolPolicyPreview | null = null;
	let cachePreview: RuntimeContextCachePreview | null = null;
	let selectedSurface: EffectiveToolSurface | '' = '';
	let loading = false;
	let error: string | null = null;
	let requestSequence = 0;
	let cacheRequestSequence = 0;
	let lastRequestKey = '';
	let scopeKey = '';
	let policyIdentityKey = '';
	let lastPolicyIdentityKey = '';
	let desiredRequestKey = '';
	let refreshConfirmed = false;

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: policyIdentityKey = `${scopeKey}:${agentId.trim()}`;
	$: if (mounted && agentId.trim() && policyIdentityKey !== lastPolicyIdentityKey) {
		lastPolicyIdentityKey = policyIdentityKey;
		selectedSurface = '';
		preview = null;
		cachePreview = null;
		cacheRequestSequence += 1;
		error = null;
		lastRequestKey = `${policyIdentityKey}:default`;
		void loadPolicy(undefined, true);
		void loadCacheStatus();
	}
	$: desiredRequestKey = `${scopeKey}:${agentId.trim()}:${selectedSurface || 'default'}`;
	$: if (mounted && agentId.trim() && desiredRequestKey !== lastRequestKey) {
		void loadPolicy(selectedSurface || undefined);
	}

	function toolTitle(tool: EffectiveToolGrant | EffectiveStructuralToolGrant): string {
		return tool.description?.trim() || tool.name;
	}

	async function loadPolicy(surface?: EffectiveToolSurface, force = false): Promise<void> {
		const normalized = agentId.trim();
		if (!normalized) return;
		const sequence = ++requestSequence;
		const requestedKey = `${scopeKey}:${normalized}:${surface || 'default'}`;
		if (!force) lastRequestKey = requestedKey;
		loading = true;
		error = null;
		try {
			const next = await fetchEffectiveToolPolicy(normalized, surface);
			if (sequence !== requestSequence) return;
			const resolvedKey = `${scopeKey}:${normalized}:${next.selected_surface}`;
			lastRequestKey = resolvedKey;
			preview = next;
			selectedSurface = next.selected_surface;
		} catch (cause) {
			if (sequence !== requestSequence) return;
			preview = null;
			error = cause instanceof Error ? cause.message : 'Effective tools are unavailable';
		} finally {
			if (sequence === requestSequence) loading = false;
		}
	}

	async function refreshPolicy(): Promise<void> {
		const normalized = agentId.trim();
		if (!normalized) return;
		const sequence = ++requestSequence;
		loading = true;
		error = null;
		refreshConfirmed = false;
		try {
			const refreshedCache = await refreshRuntimeContextCache(normalized);
			const next = await fetchEffectiveToolPolicy(normalized, selectedSurface || undefined);
			if (sequence !== requestSequence) return;
			lastRequestKey = `${scopeKey}:${normalized}:${next.selected_surface}`;
			preview = next;
			selectedSurface = next.selected_surface;
			cachePreview = refreshedCache;
			refreshConfirmed = true;
		} catch (cause) {
			if (sequence !== requestSequence) return;
			error = cause instanceof Error ? cause.message : 'Runtime tool cache refresh failed';
		} finally {
			if (sequence === requestSequence) loading = false;
		}
	}

	async function loadCacheStatus(): Promise<void> {
		const normalized = agentId.trim();
		if (!normalized) return;
		const sequence = ++cacheRequestSequence;
		const requestedIdentity = policyIdentityKey;
		try {
			const next = await fetchRuntimeContextCache(normalized);
			if (sequence !== cacheRequestSequence || requestedIdentity !== policyIdentityKey) return;
			cachePreview = next;
		} catch {
			if (sequence !== cacheRequestSequence || requestedIdentity !== policyIdentityKey) return;
			// Effective-policy errors remain the primary operator signal. Cache
			// health is supplementary and is retried by the explicit refresh.
			cachePreview = null;
		}
	}

	function handleSurfaceChange(event: Event): void {
		selectedSurface = (event.currentTarget as HTMLSelectElement).value as EffectiveToolSurface;
	}

	onMount(() => {
		mounted = true;
		return () => {
			mounted = false;
			requestSequence += 1;
			cacheRequestSequence += 1;
		};
	});
</script>

<section class:compact class="effective-policy" aria-label="Runtime-effective tools">
	<header class="policy-header">
		<div class="policy-heading">
			<div class="policy-icon" aria-hidden="true"><Icon name="check" size={16} /></div>
			<div>
				<h3>Effective tools</h3>
				<p>After scope, trust, surface, deny, ownership, and delegation filters.</p>
			</div>
		</div>
		<div class="policy-controls">
			{#if preview && preview.available_surfaces.length > 1}
				<label>
					<span class="sr-only">Invocation surface</span>
					<select value={selectedSurface} on:change={handleSurfaceChange} aria-label="Invocation surface">
						{#each preview.available_surfaces as option (option.surface)}
							<option value={option.surface}>{option.label}</option>
						{/each}
					</select>
				</label>
			{/if}
			<button
				class="refresh-button"
				type="button"
				aria-label="Refresh effective tools"
				title="Rebuild runtime tool cache and resolve current policy"
				disabled={loading}
				on:click={refreshPolicy}
			>
				<Icon name="rotate-ccw" size={14} />
			</button>
		</div>
	</header>

	{#if loading && !preview}
		<div class="policy-loading" role="status">
			<span></span><span></span><span></span>
			Resolving the canonical policy snapshot…
		</div>
	{:else if error}
		<div class="policy-error" role="status">
			<Icon name="alert" size={15} />
			<div><strong>Effective policy unavailable</strong><span>{error}</span></div>
		</div>
	{:else if preview}
		<div class="policy-facts" aria-label="Effective policy summary">
			<span class="surface-badge">{preview.available_surfaces.find((entry) => entry.surface === preview?.selected_surface)?.label || preview.selected_surface}</span>
			<span>{preview.provider_tool_count} model-visible</span>
			<span>{preview.dispatch_tool_count} dispatchable</span>
			<span>{effectiveToolCount(preview)} total entries</span>
			<span>trust: {preview.trust_level}</span>
			{#if cachePreview}<span>{cachePreview.tool_index_count} indexed leaves</span>{/if}
			{#if cachePreview}<span class="cache-badge" title="Stable system-prompt prefixes cached for this runtime">{cachePreview.surface_plan_cache.static_prompt_entry_count} prompt prefixes · {cachePreview.surface_plan_cache.static_prompt_hits} hits</span>{/if}
		</div>

		<div class="policy-groups">
			<details open={!compact || preview.direct.length > 0}>
				<summary><span>Callable now</span><strong>{preview.direct.length}</strong></summary>
				{#if preview.direct.length > 0}
					<div class="tool-cloud">
						{#each preview.direct as tool (tool.name)}
							<span class="tool-chip" title={toolTitle(tool)}>{tool.name}</span>
						{/each}
					</div>
				{:else}<p class="empty-group">No direct callable tools on this surface.</p>{/if}
			</details>

			{#if preview.runtime.length > 0}
				<details open={!compact}>
					<summary><span>Runtime controls</span><strong>{preview.runtime.length}</strong></summary>
					<div class="tool-cloud">
						{#each preview.runtime as tool (tool.name)}
							<span class="tool-chip runtime" title={toolTitle(tool)}>{tool.name}</span>
						{/each}
					</div>
				</details>
			{/if}

			{#if preview.structural.length > 0}
				<details open={!compact}>
					<summary><span>Structural actions</span><strong>{preview.structural.length}</strong></summary>
					<ul class="structural-list">
						{#each preview.structural as tool (tool.name)}
							<li title={toolTitle(tool)}>
								<div><code>{tool.name}</code>{#if tool.requires_approval}<span class="approval-badge">approval</span>{/if}</div>
								{#if tool.allowed_targets.length > 0}<small>{tool.allowed_targets.join(', ')}</small>{/if}
							</li>
						{/each}
					</ul>
				</details>
			{/if}

			{#if preview.deferred.length > 0}
				<details>
					<summary><span>Deferred · discoverable later</span><strong>{preview.deferred.length}</strong></summary>
					<div class="tool-cloud">
						{#each preview.deferred as tool (tool.name)}
							<span class="tool-chip deferred" title={toolTitle(tool)}>{tool.name}</span>
						{/each}
					</div>
				</details>
			{/if}

			{#if preview.internal.length > 0}
				<details>
					<summary><span>Internal · server-owned</span><strong>{preview.internal.length}</strong></summary>
					<div class="tool-cloud">
						{#each preview.internal as tool (tool.name)}
							<span class="tool-chip internal" title="Authorized server metadata; never sent as a model-callable schema">{tool.name}</span>
						{/each}
					</div>
				</details>
			{/if}

			{#if preview.delegation_targets.length > 0 || preview.handover_targets.length > 0}
				<details>
					<summary><span>Authorized transitions</span><strong>{preview.delegation_targets.length + preview.handover_targets.length}</strong></summary>
					<div class="transition-grid">
						{#if preview.delegation_targets.length > 0}<div><small>Delegate</small><p>{preview.delegation_targets.join(', ')}</p></div>{/if}
						{#if preview.handover_targets.length > 0}<div><small>Handover</small><p>{preview.handover_targets.join(', ')}</p></div>{/if}
					</div>
				</details>
			{/if}

			{#if preview.denied_tool_names.length > 0}
				<details>
					<summary><span>Filtered by definition</span><strong>{preview.denied_tool_names.length}</strong></summary>
					<div class="tool-cloud muted">
						{#each preview.denied_tool_names as name (name)}<span class="tool-chip denied">{name}</span>{/each}
					</div>
				</details>
			{/if}
		</div>

		<footer class="policy-provenance">
			<span>Snapshot <code title={preview.snapshot_id}>{shortPolicyId(preview.snapshot_id)}</code></span>
			<span>Definition v{preview.definition_version}</span>
			{#if cachePreview}<span>Registry <code title={cachePreview.registry_revision}>{shortPolicyId(cachePreview.registry_revision)}</code></span>{/if}
			<span>{preview.approval_rule_count} approval {preview.approval_rule_count === 1 ? 'rule' : 'rules'}</span>
			{#if loading}<span class="refreshing">Refreshing…</span>{/if}
			{#if refreshConfirmed && !loading}<span class="refresh-confirmed">Runtime cache refreshed</span>{/if}
		</footer>
	{/if}
</section>

<style>
	.effective-policy {
		--policy-border: var(--component-card-border, var(--border-soft));
		--policy-surface: var(--bg-elevated, var(--bg-card));
		border: 1px solid var(--policy-border);
		border-radius: var(--radius-md, 16px);
		background: var(--component-card-bg, var(--policy-surface));
		box-shadow: var(--component-card-shadow, var(--shadow-sm));
		padding: 1rem;
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
		min-width: 0;
		color: var(--text-primary);
		container-type: inline-size;
	}

	.effective-policy.compact { border-radius: var(--radius-sm, 12px); box-shadow: none; padding: .85rem; }
	.policy-header, .policy-heading, .policy-controls, .policy-facts, .policy-provenance { display: flex; align-items: center; }
	.policy-header { justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
	.policy-heading { gap: .7rem; min-width: 0; }
	.policy-heading > div:last-child { min-width: 0; }
	.policy-heading h3 { margin: 0; font-size: .96rem; letter-spacing: -.01em; }
	.policy-heading p { margin: .18rem 0 0; font-size: .76rem; color: var(--text-secondary); overflow-wrap: anywhere; }
	.policy-icon { display: grid; place-items: center; width: 30px; height: 30px; flex: 0 0 auto; border-radius: 9px; color: var(--accent-primary); background: var(--accent-primary-soft); }
	.policy-controls { gap: .45rem; flex: 0 0 auto; }
	.policy-controls select { max-width: 10rem; min-height: 30px; border: 1px solid var(--input-border, var(--policy-border)); border-radius: 8px; background: var(--input-bg, var(--policy-surface)); color: inherit; padding: 0 1.8rem 0 .6rem; font: 500 .76rem/1 var(--font-primary); }
	.refresh-button { display: grid; place-items: center; width: 30px; height: 30px; border: 1px solid var(--button-secondary-border, var(--policy-border)); border-radius: 8px; color: var(--button-secondary-color, var(--text-secondary)); background: var(--button-secondary-bg, var(--policy-surface)); box-shadow: var(--button-secondary-shadow, none); cursor: pointer; }
	.refresh-button:hover:not(:disabled) { color: var(--accent-primary); border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--policy-border)); background: var(--button-secondary-hover-bg, var(--accent-primary-soft)); }
	.policy-controls select:focus-visible, .refresh-button:focus-visible, summary:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	.refresh-button:disabled { opacity: .5; cursor: default; }
	.policy-facts { flex-wrap: wrap; gap: .4rem .75rem; margin: .85rem 0 .6rem; font-size: .7rem; color: var(--text-secondary); }
	.surface-badge { color: var(--accent-primary); font-weight: 700; background: var(--accent-primary-soft); border-radius: var(--radius-full, 999px); padding: .23rem .55rem; }
	.cache-badge { color: var(--accent-secondary); font-weight: 650; }
	.policy-groups { display: grid; gap: .35rem; }
	details { border-top: 1px solid var(--policy-border); padding-top: .35rem; }
	summary { display: flex; align-items: center; justify-content: space-between; gap: 1rem; min-height: 30px; border-radius: 5px; cursor: pointer; list-style: none; font-size: .76rem; color: var(--text-secondary); }
	summary::-webkit-details-marker { display: none; }
	summary strong { display: grid; place-items: center; min-width: 23px; height: 20px; padding: 0 .3rem; border-radius: var(--radius-full, 999px); background: color-mix(in srgb, var(--text-secondary) 10%, transparent); color: var(--text-primary); font-size: .67rem; }
	.tool-cloud { display: flex; flex-wrap: wrap; gap: .38rem; padding: .32rem 0 .5rem; }
	.tool-chip { max-width: 100%; overflow: hidden; text-overflow: ellipsis; border: 1px solid color-mix(in srgb, var(--accent-primary) 22%, var(--policy-border)); border-radius: 7px; padding: .27rem .47rem; background: color-mix(in srgb, var(--accent-primary) 6%, var(--policy-surface)); color: var(--text-primary); font: 600 .68rem/1.2 var(--font-mono); }
	.tool-chip.runtime { border-color: color-mix(in srgb, var(--accent-secondary) 30%, var(--policy-border)); background: color-mix(in srgb, var(--accent-secondary) 9%, var(--policy-surface)); }
	.tool-chip.deferred { border-style: dashed; }
	.tool-chip.internal { border-color: color-mix(in srgb, var(--color-warning) 35%, var(--policy-border)); background: var(--color-warning-soft); }
	.tool-chip.denied { text-decoration: line-through; opacity: .67; }
	.structural-list { display: grid; gap: .35rem; margin: .15rem 0 .55rem; padding: 0; list-style: none; }
	.structural-list li { min-width: 0; border-radius: 8px; padding: .45rem .55rem; background: color-mix(in srgb, var(--policy-surface) 75%, transparent); }
	.structural-list li > div { display: flex; flex-wrap: wrap; gap: .4rem; align-items: center; min-width: 0; }
	.structural-list code { overflow-wrap: anywhere; }
	.structural-list code, .policy-provenance code { font-size: .69rem; }
	.structural-list small { display: block; margin-top: .25rem; color: var(--text-secondary); overflow-wrap: anywhere; }
	.approval-badge { border: 1px solid color-mix(in srgb, var(--color-warning) 45%, transparent); border-radius: var(--radius-full, 999px); padding: .12rem .35rem; font-size: .6rem; color: var(--text-primary); background: var(--color-warning-soft); }
	.transition-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(150px, 100%), 1fr)); gap: .45rem; padding: .2rem 0 .55rem; }
	.transition-grid div { min-width: 0; border-radius: 8px; background: color-mix(in srgb, var(--policy-surface) 78%, transparent); padding: .45rem .55rem; }
	.transition-grid small { color: var(--text-secondary); text-transform: uppercase; letter-spacing: .05em; font-size: .59rem; }
	.transition-grid p { margin: .2rem 0 0; font-size: .7rem; overflow-wrap: anywhere; }
	.empty-group { margin: .15rem 0 .55rem; font-size: .72rem; color: var(--text-secondary); }
	.policy-provenance { flex-wrap: wrap; gap: .4rem .8rem; border-top: 1px solid var(--policy-border); margin-top: .35rem; padding-top: .65rem; font-size: .64rem; color: var(--text-muted); }
	.refreshing { color: var(--accent-primary); }
	.refresh-confirmed { color: var(--color-success, var(--accent-secondary)); }
	.policy-loading { display: flex; flex-wrap: wrap; gap: .4rem; align-items: center; min-height: 60px; font-size: .73rem; color: var(--text-secondary); }
	.policy-loading span { width: 5px; height: 5px; border-radius: 50%; background: var(--accent-primary); animation: pulse 1s ease-in-out infinite alternate; }
	.policy-loading span:nth-child(2) { animation-delay: .15s; }
	.policy-loading span:nth-child(3) { animation-delay: .3s; margin-right: .2rem; }
	.policy-error { display: flex; align-items: flex-start; gap: .55rem; margin-top: .8rem; border: 1px solid color-mix(in srgb, var(--color-error) 32%, transparent); border-radius: 10px; padding: .65rem; color: var(--color-error); background: var(--color-error-soft); }
	.policy-error strong, .policy-error span { display: block; }
	.policy-error strong { font-size: .74rem; }
	.policy-error span { margin-top: .12rem; font-size: .68rem; color: var(--text-secondary); }
	.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
	@keyframes pulse { from { opacity: .25; transform: translateY(1px); } to { opacity: 1; transform: translateY(-1px); } }
	@container (max-width: 520px) {
		.policy-header { align-items: stretch; flex-direction: column; gap: .75rem; }
		.policy-heading { align-items: flex-start; }
		.policy-controls { width: 100%; justify-content: flex-end; }
		.policy-controls label { flex: 1 1 auto; min-width: 0; }
		.policy-controls select { width: 100%; max-width: none; }
	}
	@media (max-width: 640px) {
		.policy-header { align-items: stretch; flex-direction: column; gap: .75rem; }
		.policy-heading { align-items: flex-start; }
		.policy-controls { width: 100%; justify-content: flex-end; }
		.policy-controls label { flex: 1 1 auto; min-width: 0; }
		.policy-controls select { width: 100%; max-width: none; }
	}
</style>
