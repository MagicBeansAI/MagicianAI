<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import {
		getCurrentScopeCredentialRevision,
		scopeCredentialIdentityIsCurrent,
		scopeIdentityStore
	} from '$lib/stores/scopeIdentityStore';
	import {
		fetchAppIndicators,
		type AppIndicatorListResponse,
		type AppMaterializedIndicator,
		type AppResponseCache
	} from './appWidgets';

	export let ariaLabel = 'App indicators';

	let cache: AppResponseCache<AppIndicatorListResponse> | undefined;
	let indicators: AppMaterializedIndicator[] = [];
	let request: AbortController | null = null;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let mounted = false;
	let appliedScope = '';
	let appliedCredentialRevision = 0;
	$: scopeKey = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace]);
	$: if (mounted && scopeKey !== appliedScope) void load(scopeKey, false);

	function schedule(): void {
		if (timer) clearTimeout(timer);
		const now = Date.now();
		const expiryDelay = indicators.reduce((minimum, item) => Math.min(minimum, Math.max(0, Date.parse(item.expires_at) - now)), 60_000);
		timer = setTimeout(() => {
			timer = undefined;
			indicators = indicators.filter((item) => Date.parse(item.expires_at) > Date.now());
			if (document.visibilityState === 'visible') void load(scopeKey, true);
		}, expiryDelay > 0 ? Math.min(60_000, expiryDelay) : 1);
	}

	async function load(expectedScope: string, retainCache: boolean): Promise<void> {
		const expectedCredentialRevision = getCurrentScopeCredentialRevision();
		if (!scopeCredentialIdentityIsCurrent(expectedCredentialRevision)) {
			request?.abort();
			request = null;
			appliedScope = expectedScope;
			appliedCredentialRevision = 0;
			cache = undefined;
			indicators = [];
			if (timer) clearTimeout(timer);
			timer = setTimeout(() => {
				timer = undefined;
				if (mounted && document.visibilityState === 'visible') void load(scopeKey, false);
			}, 1_000);
			return;
		}
		if (timer) {
			clearTimeout(timer);
			timer = undefined;
		}
		request?.abort();
		const controller = new AbortController();
		request = controller;
		appliedScope = expectedScope;
		const canRetainCache = retainCache && appliedCredentialRevision === expectedCredentialRevision;
		if (!canRetainCache) {
			cache = undefined;
			indicators = [];
		}
		try {
			const next = await fetchAppIndicators(canRetainCache ? cache : undefined, controller.signal);
			if (controller.signal.aborted || expectedScope !== scopeKey) return;
			if (!scopeCredentialIdentityIsCurrent(expectedCredentialRevision)) {
				cache = undefined;
				indicators = [];
				void load(scopeKey, false);
				return;
			}
			appliedCredentialRevision = expectedCredentialRevision;
			cache = next;
			const now = Date.now();
			indicators = next.value.indicators.filter((item) => Date.parse(item.expires_at) > now);
		} catch {
			if (!controller.signal.aborted && expectedScope === scopeKey) {
				cache = undefined;
				indicators = [];
				if (!scopeCredentialIdentityIsCurrent(expectedCredentialRevision)) {
					void load(scopeKey, false);
					return;
				}
			}
		} finally {
			if (request === controller) {
				request = null;
				if (mounted && expectedScope === scopeKey) schedule();
			}
		}
	}

	function refreshOnFocus(): void {
		if (document.visibilityState === 'visible' && request === null) void load(scopeKey, true);
	}

	// Browser listeners are attached and detached inside onMount: Svelte runs
	// onDestroy during server rendering too, where `window` does not exist.
	onMount(() => {
		mounted = true;
		appliedScope = scopeKey;
		if (document.visibilityState === 'visible') void load(scopeKey, false);
		window.addEventListener('focus', refreshOnFocus);
		document.addEventListener('visibilitychange', refreshOnFocus);
		return () => {
			window.removeEventListener('focus', refreshOnFocus);
			document.removeEventListener('visibilitychange', refreshOnFocus);
		};
	});

	onDestroy(() => {
		mounted = false;
		request?.abort();
		if (timer) clearTimeout(timer);
	});
</script>

{#if indicators.length > 0}
	<div class="app-indicators" aria-label={ariaLabel}>
		{#each indicators as indicator (`${indicator.installation_id}:${indicator.indicator_id}:${indicator.revision}`)}
			<span class:badge={indicator.model.kind === 'badge'} class:state={indicator.model.kind === 'state'} title={indicator.title}>
				<span class="indicator-title">{indicator.title}</span>
				<strong>{indicator.model.kind === 'badge' ? indicator.model.count : indicator.model.kind === 'chip' ? indicator.model.text : indicator.model.label}</strong>
			</span>
		{/each}
	</div>
{/if}

<style>
	.app-indicators { display: flex; align-items: center; gap: .3rem; width: 100%; min-width: 0; overflow: hidden; }
	.app-indicators > span { display: inline-flex; align-items: center; gap: .3rem; min-width: 0; height: 24px; padding: 0 .5rem; border: 1px solid var(--border-soft); border-radius: var(--radius-full, 999px); background: var(--bg-soft); color: var(--text-secondary); font-size: var(--text-2xs, .72rem); line-height: 1; white-space: nowrap; }
	.indicator-title { min-width: 0; max-width: 5.5rem; overflow: hidden; text-overflow: ellipsis; }
	strong { color: var(--text-primary); font-weight: 700; }
	.badge strong { display: grid; min-width: 1.05rem; height: 1.05rem; place-items: center; padding: 0 .15rem; border-radius: var(--radius-full, 999px); background: var(--accent-primary); color: var(--text-on-accent, #fff); }
	.state { border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft)); }
	@media (max-width: 1100px) { .indicator-title { display: none; } }
	@media (max-width: 760px) { .app-indicators { display: none; } }
</style>
