<script lang="ts">
	import { timedFetch } from '$lib/shared/fetch';
	import NotifyCardView from '$lib/notify/NotifyCard.svelte';
	import type { NotifyCard } from '$lib/notify/cardModel';

	type Kind = 'approval' | 'approval_input' | 'error' | 'completion';

	const KINDS: { value: Kind; label: string }[] = [
		{ value: 'approval', label: 'Confirmation HITL' },
		{ value: 'approval_input', label: 'Input HITL' },
		{ value: 'error', label: 'Error' },
		{ value: 'completion', label: 'Completion' }
	];

	let kind: Kind = 'approval';
	let text = '';
	let hint = '';
	let principal = 'anonymous';
	let workspace = 'default';

	let sending = false;
	let result: string | null = null;
	let error: string | null = null;

	// The `text` field means different things per kind. Label adapts so the
	// operator knows what they're filling in.
	$: textLabel =
		kind === 'error' ? 'Error message' : kind === 'completion' ? 'Title' : 'Prompt';
	$: textPlaceholder =
		kind === 'approval'
			? 'Debug confirmation request'
			: kind === 'approval_input'
				? 'What input does the agent need?'
				: kind === 'error'
					? 'Debug error'
					: 'Completed';
	// `hint` only renders a card subtext for the approval kinds.
	$: showHint = kind === 'approval' || kind === 'approval_input';

	async function send(): Promise<void> {
		sending = true;
		result = null;
		error = null;
		try {
			const body = {
				kind,
				text: text.trim() === '' ? undefined : text.trim(),
				hint: showHint && hint.trim() !== '' ? hint.trim() : undefined,
				principal: principal.trim() === '' ? undefined : principal.trim(),
				workspace: workspace.trim() === '' ? undefined : workspace.trim()
			};
			const response = await timedFetch('/api/magician/v3/events/debug-emit', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(body)
			});
			const payloadText = await response.text();
			if (!response.ok) {
				error = `${response.status}: ${payloadText}`;
				return;
			}
			try {
				result = JSON.stringify(JSON.parse(payloadText), null, 2);
			} catch {
				result = payloadText;
			}
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to send debug event';
		} finally {
			sending = false;
		}
	}

	// ── Static preview gallery ────────────────────────────────────────────────
	// Renders the REAL NotifyCard component for every card type so the look can be
	// reviewed + refined here without driving the desktop overlay. The long
	// samples exercise the 2-line clamp + "Show more" scrollable expand.
	const LONG_MESSAGE =
		'The browser automation step failed while waiting for the checkout button to ' +
		'become clickable. The page never reached a stable state within the 30 second ' +
		'budget: the network panel showed three pending requests to the payments ' +
		'provider, and a cookie-consent modal intercepted the click. The step was ' +
		'retried twice with exponential backoff and both attempts hit the same ' +
		'timeout. This sample is intentionally long so you can see the two-line clamp, ' +
		'the Show more toggle, and the scrollable expanded view in action.';

	const SAMPLES: NotifyCard[] = [
		{
			id: 'demo-approval',
			kind: 'actionable',
			correlationId: 'demo-approval',
			source: 'approval',
			inputType: 'confirmation',
			prompt: 'Permission required before sending this email to team@acme.com.',
			hint: 'Subject: Q3 report — report.pdf attached'
		},
		{
			id: 'demo-open',
			kind: 'actionable',
			correlationId: 'demo-open',
			source: 'clarification',
			inputType: 'text',
			prompt: 'The agent needs more detail before continuing.',
			hint: 'Which environment should it deploy to — staging or production?'
		},
		{ id: 'demo-info', kind: 'info', title: 'Calendar synced', message: '3 events were updated from your Google calendar.' },
		{ id: 'demo-success', kind: 'success', title: 'Task completed', message: 'Your weekly report finished generating.' },
		{ id: 'demo-error', kind: 'error', title: 'Execution failed', message: 'The browser step timed out after 30s.' },
		{ id: 'demo-long-error', kind: 'error', title: 'Execution failed', message: LONG_MESSAGE },
		{
			id: 'demo-long-approval',
			kind: 'actionable',
			correlationId: 'demo-long-approval',
			source: 'approval',
			inputType: 'confirmation',
			prompt: 'Permission required for an email with 4 recipients and attachments.',
			hint: LONG_MESSAGE
		}
	];

	const LABELS: Record<string, string> = {
		'demo-approval': 'Actionable — confirmation launcher',
		'demo-open': 'Actionable — input launcher',
		'demo-info': 'Info',
		'demo-success': 'Success',
		'demo-error': 'Error',
		'demo-long-error': 'Error — long message (Show more → scrollable)',
		'demo-long-approval': 'Actionable — long hint (Show more → scrollable)'
	};

	let previews: NotifyCard[] = [...SAMPLES];
	let previewStatus = '';

	function resetPreviews(): void {
		previews = [...SAMPLES];
		previewStatus = '';
	}
	function onPreviewDismiss(e: CustomEvent<NotifyCard>): void {
		previews = previews.filter((c) => c.id !== e.detail.id);
		previewStatus = `Dismissed ${e.detail.id}`;
	}
	function flashPreview(label: string, c: NotifyCard): void {
		previewStatus = `${label} → ${c.id}`;
	}
</script>

<svelte:head>
	<title>Debug · Notify overlay</title>
</svelte:head>

<main class="notify-debug">
	<header class="page-head">
		<p class="eyebrow">Debug</p>
		<h1>Notify overlay injector</h1>
		<p class="lede">
			Inject a synthetic event onto the live V3 event stream the desktop notify-overlay reads —
			decoupled from the agent / HITL path. Use it to bisect overlay-vs-upstream when real
			notifications aren't appearing.
		</p>
	</header>

	<section class="blurb" aria-label="How this works">
		<ul>
			<li>
				The card appears on the <strong>desktop notify-overlay</strong> (top-right of the primary
				monitor), <strong>not</strong> on this page.
			</li>
			<li>
				Requires a <strong>debug backend</strong> or <code>MAGICIAN_DEBUG_EVENTS=1</code> on a
				release backend; otherwise the endpoint returns 403.
			</li>
			<li>
				<strong>principal</strong> / <strong>workspace</strong> must match the overlay's
				subscription scope (default <code>anonymous</code> / <code>default</code>) or the event is
				filtered out before it reaches the overlay.
			</li>
		</ul>
	</section>

	<form class="panel" on:submit|preventDefault={() => void send()}>
		<label class="field">
			<span>Kind</span>
			<select bind:value={kind}>
				{#each KINDS as k (k.value)}
					<option value={k.value}>{k.label}</option>
				{/each}
			</select>
		</label>

		<label class="field">
			<span>{textLabel}</span>
			<input type="text" bind:value={text} placeholder={textPlaceholder} />
		</label>

		{#if showHint}
			<label class="field">
				<span>Hint <em>(card subtext, optional)</em></span>
				<input type="text" bind:value={hint} placeholder="Optional supplementary hint" />
			</label>
		{/if}

		<div class="scope-row">
			<label class="field">
				<span>principal</span>
				<input type="text" bind:value={principal} placeholder="anonymous" />
			</label>
			<label class="field">
				<span>workspace</span>
				<input type="text" bind:value={workspace} placeholder="default" />
			</label>
		</div>
		<p class="scope-note">Must match the overlay's subscription scope.</p>

		<button type="submit" class="send" disabled={sending}>
			{sending ? 'Sending…' : 'Send to desktop overlay'}
		</button>
	</form>

	{#if error}
		<section class="result-card error" role="alert">
			<h2>Error</h2>
			<pre>{error}</pre>
		</section>
	{/if}

	{#if result}
		<section class="result-card ok">
			<h2>Sent</h2>
			<pre>{result}</pre>
		</section>
	{/if}

	<section class="gallery" aria-label="Notification previews">
		<header class="gallery-head">
			<h2>Live preview — all notification types</h2>
			<p>
				Rendered with the real <code>NotifyCard</code> component at the overlay's 300px width, on a
				checkered backdrop standing in for the transparent desktop overlay. Hover a card to reveal
				its <strong>×</strong>; long messages clamp to 2 lines with a <strong>Show more</strong>
				toggle that opens a scrollable view (and the full text on hover). For tweaking the look.
			</p>
		</header>

		<div class="gallery-grid">
			{#each previews as card (card.id)}
				<figure class="preview-cell">
					<figcaption class="preview-label">{LABELS[card.id] ?? card.id}</figcaption>
					<div class="preview-stage">
						<NotifyCardView
							{card}
							on:openInApp={(e) => flashPreview('Open exact prompt', e.detail)}
							on:open={(e) => flashPreview('Open', e.detail)}
							on:dismiss={onPreviewDismiss}
						/>
					</div>
				</figure>
			{/each}
		</div>

		<div class="gallery-foot">
			<button type="button" class="ghost" on:click={resetPreviews}>Reset previews</button>
			{#if previewStatus}<span class="preview-status">{previewStatus}</span>{/if}
		</div>
	</section>
</main>

<style>
	.notify-debug {
		/* Match the app-wide content column the other pages use
		   (--app-content-max). `width: 100%` is required so the page fills to that
		   cap inside the column-flex `.v5-main` instead of shrinking to content. */
		max-width: var(--app-content-max, 1320px);
		width: 100%;
		margin: 0 auto;
		padding: 32px 24px 64px;
		display: flex;
		flex-direction: column;
		gap: 24px;
	}

	.page-head .eyebrow {
		text-transform: uppercase;
		letter-spacing: 0.08em;
		font-size: 0.72rem;
		color: var(--text-muted, #8a8a8a);
		margin: 0 0 4px;
	}

	.page-head h1 {
		margin: 0 0 8px;
		font-size: 1.6rem;
	}

	.page-head .lede {
		margin: 0;
		color: var(--text-muted, #6b6b6b);
		line-height: 1.5;
	}

	.blurb {
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.25));
		border-radius: 10px;
		padding: 16px 20px;
		background: var(--bg-soft, rgba(128, 128, 128, 0.06));
	}

	.blurb ul {
		margin: 0;
		padding-left: 18px;
		display: flex;
		flex-direction: column;
		gap: 8px;
		line-height: 1.5;
	}

	.panel {
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.25));
		border-radius: 12px;
		padding: 24px;
		display: flex;
		flex-direction: column;
		gap: 16px;
		background: var(--bg-surface, transparent);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
		font-size: 0.9rem;
	}

	.field span {
		font-weight: 600;
		color: var(--text-primary, inherit);
	}

	.field em {
		font-weight: 400;
		font-style: italic;
		color: var(--text-muted, #8a8a8a);
	}

	.field select,
	.field input {
		padding: 10px 12px;
		border-radius: 8px;
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.35));
		background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
		color: inherit;
		font-size: 0.95rem;
	}

	.scope-row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 12px;
	}

	.scope-note {
		margin: -8px 0 0;
		font-size: 0.78rem;
		color: var(--text-muted, #8a8a8a);
	}

	.send {
		align-self: flex-start;
		padding: 10px 20px;
		border-radius: 8px;
		border: none;
		background: var(--accent-primary, #ff6b6b);
		color: var(--text-on-accent, #fff);
		font-size: 0.95rem;
		font-weight: 600;
		cursor: pointer;
	}

	.send:disabled {
		opacity: 0.6;
		cursor: default;
	}

	.result-card {
		border-radius: 10px;
		padding: 16px 20px;
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.25));
	}

	.result-card h2 {
		margin: 0 0 8px;
		font-size: 1rem;
	}

	.result-card pre {
		margin: 0;
		white-space: pre-wrap;
		word-break: break-word;
		font-size: 0.85rem;
	}

	.result-card.error {
		border-color: var(--color-error, #ff6b6b);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
	}

	.result-card.ok {
		border-color: var(--color-success, #00bb7f);
		background: var(--color-success-soft, rgba(0, 187, 127, 0.14));
	}

	.gallery {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.gallery-head h2 {
		margin: 0 0 4px;
		font-size: 1.1rem;
	}

	.gallery-head p {
		margin: 0;
		color: var(--text-muted, #6b6b6b);
		line-height: 1.5;
		font-size: 0.88rem;
	}

	.gallery-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, 300px);
		gap: 20px 24px;
	}

	.preview-cell {
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.preview-label {
		font-size: 0.74rem;
		font-weight: 600;
		color: var(--text-muted, #8a8a8a);
	}

	/* Checkered backdrop stands in for the transparent desktop overlay so the
	   card's own surface + shadow read as they will on screen. */
	.preview-stage {
		width: 300px;
		padding: 16px;
		border-radius: 14px;
		display: flex;
		align-items: flex-start;
		background:
			repeating-conic-gradient(rgba(128, 128, 128, 0.1) 0% 25%, transparent 0% 50%) 0 0 / 18px 18px,
			var(--bg-soft, rgba(128, 128, 128, 0.06));
	}

	.gallery-foot {
		display: flex;
		align-items: center;
		gap: 12px;
	}

	.ghost {
		padding: 8px 14px;
		border-radius: 8px;
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.35));
		background: none;
		color: inherit;
		cursor: pointer;
		font-size: 0.85rem;
	}

	.preview-status {
		font-size: 0.82rem;
		color: var(--text-muted, #8a8a8a);
	}
</style>
