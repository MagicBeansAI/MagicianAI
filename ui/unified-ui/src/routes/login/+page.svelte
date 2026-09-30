<script lang="ts">
	// The auth gate's other half. `(app)`'s layout sends any surface without a
	// live session here with ?redirectTo=…; a successful sign-in restores the
	// bearer (sessionStorage on the web, the Tauri credential store in native
	// webviews) and resumes exactly where the visitor was going.
	//
	// The form is deliberately minimal — username and password. Workspace
	// choice happens after login, in the TopBar switcher, where the session's
	// owned list is known. On an install with no identity yet, the FIRST
	// sign-in creates the owner account (adopting scopes/anonymous), which is
	// the bootstrap the hint below speaks to.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { BRAND_MARK_PATH, BRAND_MARK_VIEWBOX } from '$lib/shared/brand/mark';
	import { loginScopeSession, refreshScopeSession } from '$lib/stores/scopeIdentityStore';

	let username = $state('');
	let password = $state('');
	let busy = $state(false);
	let message = $state<string | null>(null);
	let resolved = $state(false);

	const redirectTo = $derived.by(() => {
		// `searchParams.get` already returns percent-DECODED text — decoding
		// again would throw on any literal '%' in the target and strand the
		// visitor at '/' instead of their page.
		const raw = $page.url.searchParams.get('redirectTo') ?? '/';
		// Only same-app paths; never bounce outward off a crafted param.
		// `//host` and `/\host` are both scheme-relative in browsers, and
		// control characters have no business in a navigation target.
		const sameAppPath =
			raw.startsWith('/')
			&& !raw.startsWith('//')
			&& !raw.startsWith('/\\')
			&& !/[\u0000-\u001f\u007f]/.test(raw);
		return sameAppPath ? raw : '/';
	});

	onMount(async () => {
		// Already signed in (second tab, or a stale redirect): go straight
		// through. A bad token resolves to null and falls through to the form.
		try {
			const session = await refreshScopeSession();
			if (session) {
				await goto(redirectTo);
				return;
			}
		} catch {
			// Backend unreachable: the form still renders; submitting will
			// surface the real error.
		}
		// Both the no-session and the unreachable cases present the form; the
		// hint must not claim "checking" forever.
		resolved = true;
	});

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (busy) return;
		const name = username.trim();
		if (!name || !password) {
			message = 'Username and password are required.';
			return;
		}
		busy = true;
		message = null;
		try {
			await loginScopeSession({ username: name, password });
			await goto(redirectTo);
		} catch (error) {
			message = error instanceof Error ? error.message : 'Sign-in failed.';
			password = '';
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head>
	<title>Sign in — Magican</title>
</svelte:head>

<main class="login-shell">
	<form class="login-card" onsubmit={submit}>
		<div class="login-mark brand-mark" aria-hidden="true">
			<svg width="24" height="24" viewBox={BRAND_MARK_VIEWBOX} fill="currentColor">
				<path d={BRAND_MARK_PATH} />
			</svg>
		</div>
		<h1>Sign in</h1>
		<p class="login-hint">
			Your session decides which principal and workspace this surface operates on.
			{#if !resolved}
				Checking for an existing session…
			{:else}
				First sign-in on a fresh install creates the owner account.
			{/if}
		</p>

		<label class="field">
			<span>Username</span>
			<!-- A phone keyboard capitalises the first letter of a text input by
			     default, and the server compares identity names exactly, so
			     without these a mobile sign-in sends `Owner` for `owner`
			     and reads back as a wrong password. `off` rather than `none`:
			     the spec makes them the same state, and WebKit before iOS 10
			     understood only `off`. -->
			<input
				type="text"
				bind:value={username}
				autocomplete="username"
				autocapitalize="off"
				autocorrect="off"
				spellcheck="false"
				placeholder="you"
				disabled={busy}
			/>
		</label>

		<label class="field">
			<span>Password</span>
			<input
				type="password"
				bind:value={password}
				autocomplete="current-password"
				autocapitalize="off"
				autocorrect="off"
				spellcheck="false"
				placeholder="••••••••"
				disabled={busy}
			/>
		</label>

		{#if message}
			<p class="login-error" role="alert">{message}</p>
		{/if}

		<button type="submit" class="login-submit" disabled={busy}>
			{busy ? 'Signing in…' : 'Sign in'}
		</button>
	</form>
</main>

<style>
	.login-shell {
		min-height: 100dvh;
		display: grid;
		place-items: center;
		background: var(--bg-base);
		padding: 1.5rem;
	}

	.login-card {
		width: min(400px, 100%);
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 18px;
		padding: 2rem 1.75rem 1.75rem;
		box-shadow: 0 18px 48px rgba(60, 40, 30, 0.12);
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
	}

	/* Same tile as the top bar's brand mark, scaled up: the mark is the shared
	   SVG path, so it centers by geometry. The extra `brand-mark` class lets
	   theme glows in app.css (e.g. jarvis) apply here as well. */
	.login-mark {
		width: 40px;
		height: 40px;
		border-radius: 12px;
		background: var(--coral, #FF6B6B);
		color: #ffffff;
		display: grid;
		place-items: center;
		margin-bottom: 0.35rem;
	}

	.login-mark svg {
		display: block;
		width: 24px;
		height: 24px;
	}

	h1 {
		margin: 0;
		font-family: var(--font-display);
		font-size: var(--text-xl);
		line-height: var(--leading-tight);
	}

	.login-hint {
		margin: 0 0 0.5rem;
		font-size: var(--text-sm);
		line-height: var(--leading-relaxed);
		opacity: 0.75;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		font-size: var(--text-xs);
		font-weight: 600;
	}

	.field input {
		font: inherit;
		font-weight: 400;
		font-size: var(--text-md);
		padding: 0.65rem 0.75rem;
		border-radius: 10px;
		border: 1px solid var(--border-default);
		background: var(--bg-elevated);
		color: inherit;
	}

	.field input:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 1px;
	}

	.login-error {
		margin: 0;
		font-size: var(--text-sm);
		color: var(--accent-primary-hover);
	}

	.login-submit {
		margin-top: 0.4rem;
		font: inherit;
		font-weight: 700;
		font-size: var(--text-md);
		padding: 0.7rem 1rem;
		border-radius: 12px;
		border: none;
		background: var(--accent-primary);
		color: var(--bg-card, #fff);
		cursor: pointer;
		transition: background var(--ease-settle), transform 0.12s var(--ease-settle);
	}

	.login-submit:hover:not(:disabled) {
		background: var(--accent-primary-hover);
	}

	.login-submit:disabled {
		opacity: 0.6;
		cursor: wait;
	}
</style>
