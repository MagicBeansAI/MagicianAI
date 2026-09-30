<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		fetchAppInteractiveRunState,
		requestAppInteractiveStop,
		type AppInteractiveRunStateSnapshot,
		type AppInteractiveStopReceipt
	} from './appInteractiveState';

	export let runRef: string;
	export let runTerminal = false;
	let snapshot: AppInteractiveRunStateSnapshot | null = null;
	let stopReceipt: AppInteractiveStopReceipt | null = null;
	let pendingStopKey = '';
	let loading = false;
	let stopping = false;
	let error = '';
	let request: AbortController | null = null;
	let stopRequest: AbortController | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let mounted = false;
	let appliedIdentity = '';
	$: identity = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace, runRef]);
	$: if (mounted && identity !== appliedIdentity) {
		appliedIdentity = identity;
		if (timer) { clearTimeout(timer); timer = null; }
		request?.abort();
		request = null;
		stopRequest?.abort();
		stopRequest = null;
		stopping = false;
		snapshot = null;
		stopReceipt = null;
		pendingStopKey = '';
		void refresh(identity);
	}

	function schedule(expectedIdentity: string): void {
		if (timer) clearTimeout(timer);
		if (runTerminal && snapshot?.currentSession?.phase !== 'stop_requested') return;
		timer = setTimeout(() => { timer = null; void refresh(expectedIdentity); }, 2_000);
	}

	function stopStorageKey(sessionRef: string): string {
		return `magician.apps.interactive-stop.v1:${identity}:${sessionRef}`;
	}

	function profileLabel(profile: string): string {
		if (profile === 'browser_session') return 'Browser session';
		if (profile === 'macos_host') return 'macOS application';
		return 'Android application';
	}

	function targetLabel(kind: string): string {
		if (kind === 'isolated_browser') return 'Isolated browser';
		if (kind === 'reviewed_macos_application') return 'Reviewed macOS application';
		return 'Reviewed Android application';
	}

	function unavailableLabel(reason: string | undefined): string {
		if (reason === 'not_declared') return 'not declared';
		if (reason === 'runtime_owner_unavailable') return 'runtime owner unavailable';
		if (reason === 'session_expired') return 'session expired';
		if (reason === 'run_terminal') return 'run terminal';
		if (reason === 'no_current_session') return 'no current session';
		if (reason === 'session_terminal') return 'session terminal';
		if (reason === 'runtime_owner_unavailable') return 'runtime owner unavailable';
		if (reason === 'owner_closing') return 'owner closing';
		return 'unavailable';
	}

	function retainStopKey(sessionRef: string): string {
		if (pendingStopKey) return pendingStopKey;
		const key = `interactive-stop-request:${crypto.randomUUID()}`;
		try {
			localStorage.setItem(stopStorageKey(sessionRef), key);
			if (localStorage.getItem(stopStorageKey(sessionRef)) !== key) throw new Error();
		} catch {
			throw new Error('Durable browser storage is required before requesting a physical stop.');
		}
		pendingStopKey = key;
		return key;
	}

	function readStopKey(sessionRef: string): string {
		try {
			const value = localStorage.getItem(stopStorageKey(sessionRef)) ?? '';
			if (value && !/^interactive-stop-request:[0-9a-f-]{36}$/.test(value)) {
				localStorage.removeItem(stopStorageKey(sessionRef));
				return '';
			}
			return value;
		} catch { return ''; }
	}

	function clearStopKey(sessionRef: string): void {
		try { localStorage.removeItem(stopStorageKey(sessionRef)); } catch { /* read-only state remains usable */ }
	}

	async function refresh(expectedIdentity = identity): Promise<void> {
		if (!mounted || expectedIdentity !== identity) return;
		if (timer) { clearTimeout(timer); timer = null; }
		request?.abort();
		const controller = new AbortController();
		request = controller;
		loading = true;
		error = '';
		try {
			const next = await fetchAppInteractiveRunState(runRef, controller.signal);
			if (controller.signal.aborted || expectedIdentity !== identity) return;
			snapshot = next;
			const session = next.currentSession;
			pendingStopKey = session ? readStopKey(session.sessionRef) : '';
			if (!session || session.phase === 'completed' || session.phase === 'cancelled_before_io' || session.phase === 'outcome_uncertain') {
				if (session) clearStopKey(session.sessionRef);
				pendingStopKey = '';
				stopReceipt = null;
			}
			schedule(expectedIdentity);
		} catch (cause) {
			if (!controller.signal.aborted && expectedIdentity === identity) {
				error = cause instanceof Error ? cause.message : 'Interactive state is unavailable.';
				if (!runTerminal) schedule(expectedIdentity);
			}
		} finally {
			if (request === controller) { request = null; loading = false; }
		}
	}

	async function stop(): Promise<void> {
		const session = snapshot?.currentSession;
		if (!session || !snapshot?.stopAvailable || stopping) return;
		const expectedIdentity = identity;
		const controller = new AbortController();
		stopRequest?.abort();
		stopRequest = controller;
		stopping = true;
		error = '';
		try {
			const key = retainStopKey(session.sessionRef);
			const nextReceipt = await requestAppInteractiveStop(
				runRef,
				session.sessionRef,
				key,
				controller.signal
			);
			if (controller.signal.aborted || expectedIdentity !== identity) return;
			stopReceipt = nextReceipt;
			await refresh(expectedIdentity);
		} catch (cause) {
			if (!controller.signal.aborted && expectedIdentity === identity) {
				error = cause instanceof Error ? cause.message : 'Interactive stop could not be requested.';
			}
		} finally {
			if (stopRequest === controller) { stopRequest = null; stopping = false; }
		}
	}

	onMount(() => { mounted = true; appliedIdentity = identity; void refresh(identity); });
	onDestroy(() => { mounted = false; request?.abort(); stopRequest?.abort(); if (timer) clearTimeout(timer); });
</script>

<section class="interactive-run" aria-labelledby="interactive-run-title" aria-busy={loading || stopping}>
	<header><strong id="interactive-run-title">Physical activity</strong><button type="button" disabled={loading} on:click={() => void refresh()}>{loading ? 'Refreshing…' : 'Refresh'}</button></header>
	<p class="privacy">Payload-free owner state only. Captured content, raw device/package IDs, provider handles, and coordinates are never shown here.</p>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
	{#if snapshot}
		<div class="availability" aria-label="Interactive owner availability">
			<strong>Availability</strong>
			<ul>{#each snapshot.ownerAvailability as owner (owner.profile)}<li><span>{profileLabel(owner.profile)}</span><span class:unavailable={owner.state === 'unavailable'}>{owner.state === 'unavailable' ? unavailableLabel(owner.unavailableReason) : owner.state.replaceAll('_', ' ')}</span></li>{/each}</ul>
		</div>
	{/if}
	{#if snapshot?.currentSession}
		{@const session = snapshot.currentSession}
		<div class="session">
			<div><span class:live={session.phase === 'active'}>{session.phase.replaceAll('_', ' ')}</span><code>{session.sessionRef}</code></div>
			<dl>
				<dt>Owner</dt><dd>{session.profile.replaceAll('_', ' ')}</dd>
				<dt>Target</dt><dd>{targetLabel(session.targetSummary.kind)} · <code>{session.targetSummary.targetRef}</code></dd>
				<dt>Activity</dt><dd>{session.activity.replaceAll('_', ' ')}</dd>
				<dt>Window</dt><dd>{new Date(session.startedAt).toLocaleString()} – {new Date(session.expiresAt).toLocaleString()}</dd>
				<dt>Claim</dt><dd>{session.resourceClaim.evidenceNodes.toLocaleString()} nodes · {session.resourceClaim.evidenceBytes.toLocaleString()} evidence bytes · {session.resourceClaim.pixels.toLocaleString()} pixels</dd>
			</dl>
			{#if snapshot.stopAvailable}<button class="stop" type="button" disabled={stopping} on:click={() => void stop()}>{stopping ? 'Requesting…' : 'Request stop'}</button>{/if}
			{#if stopReceipt}<p class="notice" role="status">Stop requested at {new Date(stopReceipt.requestedAt).toLocaleString()}. Terminal settlement is still pending.</p>{/if}
		</div>
	{:else if snapshot && snapshot.recentAuditReceipts.length === 0}
		<p class="empty">No interactive physical session is recorded for this run.</p>
	{/if}
	{#if snapshot}
		<p class="stop-state" role="status">
			<strong>Declared stop:</strong>
			{#if snapshot.declaredStopState.phase === 'requested'} requested at {new Date(snapshot.declaredStopState.requestedAt ?? '').toLocaleString()} · <code>{snapshot.declaredStopState.stopRef}</code>
			{:else if snapshot.declaredStopState.phase === 'available'} available for the current session
			{:else} {unavailableLabel(snapshot.declaredStopState.unavailableReason)}{/if}
		</p>
	{/if}
	{#if snapshot && snapshot.recentAuditReceipts.length > 0}
		<details><summary>Recent settlement audit ({snapshot.recentAuditReceipts.length})</summary><ul>{#each snapshot.recentAuditReceipts as receipt (receipt.receiptRef)}<li><span>{receipt.activity.replaceAll('_', ' ')} · {receipt.terminal.replaceAll('_', ' ')}</span><code>{receipt.receiptRef}</code><small>{receipt.resultBytes.toLocaleString()} result bytes · {receipt.evidenceBytes.toLocaleString()} evidence bytes</small></li>{/each}</ul></details>
	{/if}
</section>

<style>
	.interactive-run { margin-top: 12px; padding: 12px; border: 1px solid var(--border-soft); border-radius: 12px; background: var(--bg-soft); }
	header, .session > div, li { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
	.privacy, .empty, .notice, .error, .stop-state { margin: 7px 0 0; color: var(--text-secondary); font-size: .72rem; }
	.error { color: var(--color-error, #c43f3f); } .notice { color: var(--accent-primary); }
	.session { margin-top: 10px; } code { max-width: 70%; overflow-wrap: anywhere; font-size: var(--text-2xs, .72rem); }
	.session span { border-radius: 999px; padding: 3px 7px; background: var(--bg-elevated); text-transform: capitalize; font-size: .72rem; }
	.session span.live { color: var(--color-success, #238a58); }
	dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 4px 8px; margin: 9px 0; font-size: .72rem; } dt { font-weight: 700; } dd { margin: 0; overflow-wrap: anywhere; }
	button { border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-elevated); color: var(--text-primary); padding: 6px 9px; font: inherit; cursor: pointer; } button:disabled { opacity: .55; }
	.stop { color: var(--color-error, #c43f3f); } details { margin-top: 10px; font-size: .72rem; } ul { display: grid; gap: 6px; margin: 8px 0 0; padding: 0; list-style: none; }
	li { align-items: flex-start; flex-wrap: wrap; padding: 6px; border: 1px solid var(--border-soft); border-radius: 8px; } li small { width: 100%; color: var(--text-muted); }
	.availability { margin-top: 10px; font-size: .72rem; } .availability ul { grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); } .availability li { align-items: center; } .availability .unavailable { color: var(--text-muted); }
</style>
