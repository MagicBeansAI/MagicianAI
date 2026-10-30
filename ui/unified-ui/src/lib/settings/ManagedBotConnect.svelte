<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { browserSignInViewerUrl } from '$lib/hitl/browserSignInViewer';
	import {
		methodLabel, methodNeedsPhone, parseBotState,
		type BotConnectMethod, type BotConnectState, type BotPendingKind
	} from './managedBot';

	/** Managed bot name, e.g. `gmail-work`, `telegram-self`, `whatsapp`. */
	export let bot: string;
	/** Connect methods in preference order (`methods` on the row). */
	export let methods: BotConnectMethod[];
	export let expectedScope: { principal: string; workspace: string } | undefined = undefined;
	/** Called when the panel should close; `connected` is true after a successful sign-in. */
	export let onDone: (connected: boolean) => void = () => {};

	const POLL_MS = 2_000;
	const BASE = '/api/magician/v2/auth/browser-signin/bot';
	let method: BotConnectMethod = methods[0];
	let stage: 'choose' | 'flow' = 'choose';
	let phone = '';
	let field = '';
	let fieldKind: BotPendingKind | undefined;
	let state: BotConnectState | null = null;
	let error = '';
	let busy = false;
	let copied = false;
	let openedTicket = '';
	let disposed = false;
	let latest = 0;
	let pollTimer: ReturnType<typeof setTimeout> | undefined;
	let doneTimer: ReturnType<typeof setTimeout> | undefined;

	$: needsPhone = methodNeedsPhone(method);
	$: pending = state?.pending ?? null;
	$: authenticating = state?.status === 'authenticating';
	$: if ((pending?.kind ?? undefined) !== fieldKind) { fieldKind = pending?.kind; field = ''; copied = false; }
	$: hasOtherMethod = methods.length > 1;

	function scopeChanged(): boolean {
		if (!expectedScope) return false;
		const scope = get(scopeIdentityStore);
		return scope.principal !== expectedScope.principal || scope.workspace !== expectedScope.workspace;
	}

	async function call(action: 'start' | 'input' | 'state' | 'cancel', body: Record<string, string | boolean>): Promise<BotConnectState | null> {
		if (scopeChanged()) throw new Error('Workspace changed. Reload the account list before signing in.');
		const mine = ++latest;
		const response = await timedFetch(`${BASE}/${action}`, {
			method: 'POST', cache: 'no-store', redirect: 'error', timeoutMs: 45_000,
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ bot, ...body })
		});
		const data = await response.json().catch(() => ({}));
		if (!response.ok) {
			throw new Error(typeof data?.message === 'string' && data.message ? data.message : 'Sign-in could not continue. Try again.');
		}
		// A poll that started before a newer request must not overwrite its answer.
		if (disposed || mine !== latest) return null;
		return parseBotState(bot, data);
	}

	function stopPolling() { if (pollTimer) clearTimeout(pollTimer); pollTimer = undefined; }

	function apply(next: BotConnectState | null) {
		if (!next || disposed) return;
		state = next;
		stopPolling();
		if (next.status === 'authenticating') pollTimer = setTimeout(poll, POLL_MS);
		else if (next.status === 'connected') doneTimer = setTimeout(() => onDone(true), 1_500);
	}

	async function poll() {
		pollTimer = undefined;
		if (disposed) return;
		if (busy) { pollTimer = setTimeout(poll, POLL_MS); return; }
		try { apply(await call('state', {})); }
		catch (reason) {
			if (disposed) return;
			error = reason instanceof Error ? reason.message : 'Could not check sign-in.';
			if (!scopeChanged()) pollTimer = setTimeout(poll, POLL_MS);
		}
	}

	async function run(action: 'start' | 'input' | 'cancel', body: Record<string, string | boolean>) {
		busy = true; error = '';
		try { apply(await call(action, body)); return true; }
		catch (reason) { if (!disposed) error = reason instanceof Error ? reason.message : 'Sign-in could not continue.'; return false; }
		finally { busy = false; }
	}

	/** `onComputer` opens Google's sign-in in the computer's own browser instead of the viewer. */
	async function start(onComputer = false) {
		if (busy || (needsPhone && !phone.trim())) return;
		openedTicket = '';
		const body: Record<string, string | boolean> = { method };
		if (needsPhone) body.phone = phone.trim();
		if (onComputer) body.open_on_computer = true;
		if (await run('start', body)) stage = 'flow';
	}

	async function submit(step: 'phone' | 'code' | 'password') {
		const value = field.trim();
		if (busy || !value) return;
		// Never keep a two-step password (or a code) in the form once it is sent.
		field = '';
		await run('input', { step, value });
	}

	async function cancel() {
		stopPolling();
		if (stage === 'flow' && state?.status === 'authenticating') await run('cancel', {});
		onDone(false);
	}

	function openViewer() {
		const signIn = pending?.sign_in;
		if (!signIn) return;
		try {
			// Opened inside the click, so no popup blocker; the ticket is single use.
			window.open(browserSignInViewerUrl(signIn, window.location.href).href, '_blank', 'noopener');
			openedTicket = String(signIn.ticket);
		} catch (reason) { error = reason instanceof Error ? reason.message : 'Could not open sign-in.'; }
	}

	async function copyCode() {
		if (!pending?.pairing_code) return;
		try { await navigator.clipboard.writeText(pending.pairing_code); copied = true; }
		catch { error = 'Copy failed. Select the code and copy it.'; }
	}

	function retry() { stopPolling(); state = null; error = ''; stage = 'choose'; }

	onMount(() => { if (!hasOtherMethod && !needsPhone) void start(); });
	onDestroy(() => { disposed = true; stopPolling(); if (doneTimer) clearTimeout(doneTimer); field = ''; });
</script>

<div class="bot-connect" role="group" aria-label="Connect {bot}" data-testid="managed-bot-connect">
	{#if stage === 'choose'}
		<form on:submit|preventDefault={() => start()}>
			{#if hasOtherMethod}
				<fieldset>
					<legend>How to connect</legend>
					{#each methods as option (option)}
						<label class="method"><input type="radio" name="method-{bot}" value={option} bind:group={method} /> {methodLabel(bot, option)}</label>
					{/each}
				</fieldset>
			{/if}
			{#if needsPhone}
				<label class="field">Phone number
					<input type="tel" autocomplete="tel" inputmode="tel" placeholder="+91 98765 43210" bind:value={phone} />
				</label>
				<p class="hint">Include the country code.</p>
			{/if}
			<div class="actions">
				<button class="primary" type="submit" disabled={busy || (needsPhone && !phone.trim())}>{busy ? 'Starting…' : 'Continue'}</button>
				<button type="button" on:click={() => onDone(false)}>Cancel</button>
			</div>
		</form>
	{:else if !state}
		<p role="status">Starting…</p>
	{:else}
		{#if state.status === 'connected'}
			<p class="done" role="status">Connected.</p>
		{:else}
			{#if state.detail}<p class="detail" role="status">{state.detail}</p>{/if}
			{#if authenticating && pending}
				{#if pending.kind === 'phone'}
					<form on:submit|preventDefault={() => submit('phone')}>
						<label class="field">{pending.label ?? 'Phone number'}
							<input type="tel" autocomplete="tel" inputmode="tel" placeholder="+91 98765 43210" bind:value={field} />
						</label>
						<p class="hint">Include the country code.</p>
						<button class="primary" type="submit" disabled={busy || !field.trim()}>Send</button>
					</form>
				{:else if pending.kind === 'code'}
					<form on:submit|preventDefault={() => submit('code')}>
						<label class="field">{pending.label ?? 'Code'}
							<input type="text" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]*" bind:value={field} />
						</label>
						<button class="primary" type="submit" disabled={busy || !field.trim()}>Send code</button>
					</form>
				{:else if pending.kind === 'password'}
					<form on:submit|preventDefault={() => submit('password')}>
						<label class="field">{pending.label ?? 'Two-step password'}
							<input type="password" autocomplete="off" bind:value={field} />
						</label>
						<p class="hint">Sent once to the sign-in. Never saved.</p>
						<button class="primary" type="submit" disabled={busy || !field.trim()}>Send</button>
					</form>
				{:else if pending.kind === 'pairing_code' && pending.pairing_code}
					<div class="pairing">
						<code class="pairing-code" aria-label="Pairing code">{pending.pairing_code}</code>
						<button type="button" on:click={copyCode}>{copied ? 'Copied' : 'Copy'}</button>
					</div>
					<p class="hint">In WhatsApp: Settings → Linked devices → Link a device → Link with phone number instead, then enter this code.</p>
				{:else if pending.kind === 'qr' && pending.qr_data_url}
					<img class="qr" src={pending.qr_data_url} alt="QR code to scan" />
					{#if hasOtherMethod}<p class="hint">On this phone? Scan from another device, or use the code option.</p>{/if}
				{:else if pending.kind === 'login_url' && pending.sign_in}
					{#if pending.sign_in.mode === 'computer_opened'}
						<p class="hint">Finish the sign-in on your computer — this updates automatically.</p>
					{:else}
						{@const viewerOpened = !!openedTicket && openedTicket === pending.sign_in.ticket}
						{#if viewerOpened}<p class="hint">Finish signing in in the new window.</p>{/if}
						<div class="actions">
							{#if !viewerOpened}<button class="primary" type="button" on:click={openViewer}>Open sign-in ↗</button>{/if}
							<button type="button" disabled={busy} on:click={() => start(true)}>Open on computer</button>
						</div>
					{/if}
				{/if}
			{:else if authenticating}
				<p class="hint">Waiting…</p>
			{/if}
		{/if}
		{#if state.status !== 'connected'}
			<div class="actions">
				{#if authenticating}
					<button type="button" disabled={busy} on:click={cancel}>Cancel</button>
				{:else}
					<button class="primary" type="button" on:click={retry}>Try again</button>
					<button type="button" on:click={() => onDone(false)}>Close</button>
				{/if}
			</div>
		{/if}
	{/if}
	{#if error}<p class="error" role="alert">{error}</p>{/if}
</div>

<style>
	.bot-connect { display: flex; flex-direction: column; gap: .5rem; padding: .625rem; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-card); font-size: .8rem; }
	form { display: flex; flex-direction: column; gap: .5rem; margin: 0; }
	fieldset { display: flex; flex-direction: column; gap: .35rem; margin: 0; padding: 0; border: 0; }
	legend { margin-bottom: .25rem; color: var(--text-secondary); font-size: .75rem; font-weight: 600; }
	.method { display: flex; align-items: center; gap: .4rem; color: var(--text-primary); }
	.field { display: flex; flex-direction: column; gap: .25rem; color: var(--text-secondary); font-size: .75rem; font-weight: 600; }
	.field input { min-height: 32px; padding: .375rem .5rem; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-secondary); color: var(--text-primary); font: inherit; font-size: .85rem; font-weight: 400; }
	.field input:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 1px; }
	p { margin: 0; line-height: 1.4; }
	.hint, .detail { color: var(--text-secondary); }
	.done { color: var(--color-success, #00bb7f); font-weight: 600; }
	.error { color: var(--status-error, #dc4b4b); }
	.actions { display: flex; flex-wrap: wrap; gap: .5rem; }
	button { min-height: 32px; padding: .375rem .625rem; border: 1px solid var(--button-secondary-border, var(--border-soft)); border-radius: 8px; background: var(--button-secondary-bg); color: var(--button-secondary-color, var(--text-primary)); font: inherit; font-size: .8rem; font-weight: 600; cursor: pointer; }
	button.primary { border-color: transparent; background: var(--button-primary-bg, var(--accent-fill)); color: var(--button-primary-color, var(--text-on-accent)); }
	button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	button:disabled { opacity: .5; cursor: default; }
	.pairing { display: flex; align-items: center; flex-wrap: wrap; gap: .5rem; }
	.pairing-code { padding: .35rem .6rem; border-radius: 8px; background: var(--bg-secondary); color: var(--text-primary); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 1.5rem; font-weight: 700; letter-spacing: .15em; user-select: all; }
	.qr { width: 100%; max-width: 14rem; aspect-ratio: 1; align-self: center; border-radius: 8px; background: #fff; image-rendering: pixelated; }
	@media (pointer: coarse) { button, .field input { min-height: 40px; } .field input { font-size: 16px; } }
</style>
