<!--
  NotifyCard — one notify-overlay card rendered in the transparent /notify-overlay.

  Renders BOTH families of the `NotifyCard` discriminated union:
    - `kind === 'actionable'` — a HITL request. The whole card opens its concrete
      correlation in global Attention without resolving it. A close × persistently suppresses
      only this native notification id; a resurfaced request with a new id shows.
    - `kind === 'info' | 'success' | 'error'` — informational. title=`title`,
      body=`message`. A present `deepLink` makes the WHOLE card clickable (emits
      `open`); a top-right × always emits `dismiss`.

  Kind is conveyed by a colored bracket on each of the two left corners (keyed by
  `data-kind` via `--notify-accent`) — no leading dot, no type subtitle. A long
  body clamps to 2 lines with a Show more toggle that opens a scrollable view.

  Presentational only: it never touches the network, a store, or `invoke`. It
  emits Svelte events (each carrying the `card`) and lets the /notify-overlay
  route own the cards-store update, auto-dismiss timers, the deep-link
  `open_app_at` invoke, the tray-count
  `set_pending_approval_count` invoke, and the confirmation toast. The single
  network / store / invoke path stays in the route; the card is trivially
  testable in isolation.

  Look: a self-contained, compact, native-themed card. The overlay window is
  fully transparent, so the card paints its OWN surface (a clean, solid themed
  elevated surface + subtle shadow — NOT a translucent frosted veil, which over a
  transparent window read as a murky background box) rather than relying on the
  shared generative `Card`/`Button` — those rendered too tall and the iconOnly
  `Button` × showed as an empty box. The /notify-overlay route mirrors the app's
  active theme onto this separate webview's `<html data-theme>`, so the app theme
  tokens (`--bg-elevated`/`--text-…`/`--border-…`/`--accent-…`) resolve to the
  REAL active palette in both light and dark (trailing literals are last-ditch
  fallbacks only).
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { NotifyCard } from '$lib/notify/cardModel';

	export let card: NotifyCard;

	const dispatch = createEventDispatcher<{
		openInApp: NotifyCard;
		open: NotifyCard;
		dismiss: NotifyCard;
	}>();

	// Whether an informational card is the whole-surface-clickable (deepLink)
	// variant. Narrowed away from `actionable` so `deepLink` is safe to read.
	$: clickableInfo =
		card.kind !== 'actionable' && typeof card.deepLink === 'string' && card.deepLink.length > 0;

	// ── Long-message expand ───────────────────────────────────────────────────
	// The body text is clamped to 2 lines (see CSS); a longer message is
	// truncated with an ellipsis. We flag the overflow and offer a toggle that
	// opens the full text in a scrollable box — the card + overlay window grow to
	// fit via the route's ResizeObserver. A floating hover-popover would be
	// clipped by the content-hugging transparent overlay window, so inline
	// expansion is the robust "scrollable window"; the toggle also carries the
	// full text as a native `title` tooltip on hover.
	let bodyExpanded = false;
	let bodyOverflows = false;

	function clampOverflow(node: HTMLElement) {
		const measure = () => {
			// Only while collapsed — once expanded the box is scrollable, so its
			// scrollHeight > clientHeight would otherwise keep re-asserting the flag.
			if (!bodyExpanded) bodyOverflows = node.scrollHeight - node.clientHeight > 1;
		};
		let raf = requestAnimationFrame(measure);
		const ro = new ResizeObserver(() => {
			cancelAnimationFrame(raf);
			raf = requestAnimationFrame(measure);
		});
		ro.observe(node);
		return {
			destroy() {
				cancelAnimationFrame(raf);
				ro.disconnect();
			}
		};
	}

	function openInfoCard() {
		dispatch('open', card);
	}

	function handleClickableInfoKeydown(event: KeyboardEvent) {
		if (event.key !== 'Enter' && event.key !== ' ') return;
		event.preventDefault();
		openInfoCard();
	}

	function openActionableCard() {
		if (card.kind === 'actionable') dispatch('openInApp', card);
	}

	function handleActionableKeydown(event: KeyboardEvent) {
		if (event.target !== event.currentTarget) return;
		if (event.key !== 'Enter' && event.key !== ' ') return;
		event.preventDefault();
		openActionableCard();
	}
</script>

{#if card.kind === 'actionable'}
	<div
		class="notify-card notify-card--actionable notify-card--clickable"
		data-kind="actionable"
		role="button"
		tabindex="0"
		aria-label={`Open notification: ${card.prompt}`}
		on:click={openActionableCard}
		on:keydown={handleActionableKeydown}
	>
		<div class="notify-body">
			<p class="notify-title" title={card.prompt}>{card.prompt}</p>
			{#if card.hint}
				<p class="notify-text" class:notify-text--expanded={bodyExpanded} use:clampOverflow>
					{card.hint}
				</p>
			{/if}
			{#if card.hint && bodyOverflows}
				<div class="notify-footer">
					<button
						type="button"
						class="notify-more"
						aria-expanded={bodyExpanded}
						title={bodyExpanded ? 'Show less' : card.hint}
						on:click|stopPropagation={() => (bodyExpanded = !bodyExpanded)}
					>
						{bodyExpanded ? 'Show less' : 'Show more'}
						<svg
							class="notify-more-chev"
							class:flip={bodyExpanded}
							viewBox="0 0 12 12"
							width="9"
							height="9"
							aria-hidden="true"
						>
							<path
								d="M2.5 4.5 L6 8 L9.5 4.5"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-linecap="round"
								stroke-linejoin="round"
								fill="none"
							/>
						</svg>
					</button>
				</div>
			{/if}
		</div>
		<button
			type="button"
			class="notify-close"
			aria-label="Dismiss notification"
			title="Dismiss this notification"
			on:click|stopPropagation={() => dispatch('dismiss', card)}
		>
			<svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true" focusable="false">
				<path
					d="M1 1 L11 11 M11 1 L1 11"
					stroke="currentColor"
					stroke-width="1.6"
					stroke-linecap="round"
					fill="none"
				/>
			</svg>
		</button>
	</div>
{:else}
	<!-- Informational card — info / success / error. Whole-card click opens a
	     deepLink (when present); the top-right × always dismisses. -->
	{#if clickableInfo}
		<div
			class="notify-card notify-card--info notify-card--clickable"
			data-kind={card.kind}
			role="button"
			tabindex="0"
			on:click={openInfoCard}
			on:keydown={handleClickableInfoKeydown}
		>
			<div class="notify-body">
				<p class="notify-title" title={card.title}>{card.title}</p>
				{#if card.message}
					<p class="notify-text" class:notify-text--expanded={bodyExpanded} use:clampOverflow>
						{card.message}
					</p>
					{#if bodyOverflows}
						<button
							type="button"
							class="notify-more"
							aria-expanded={bodyExpanded}
							title={bodyExpanded ? 'Show less' : card.message}
							on:click|stopPropagation={() => (bodyExpanded = !bodyExpanded)}
						>
							{bodyExpanded ? 'Show less' : 'Show more'}
							<svg
								class="notify-more-chev"
								class:flip={bodyExpanded}
								viewBox="0 0 12 12"
								width="9"
								height="9"
								aria-hidden="true"
							>
								<path
									d="M2.5 4.5 L6 8 L9.5 4.5"
									stroke="currentColor"
									stroke-width="1.5"
									stroke-linecap="round"
									stroke-linejoin="round"
									fill="none"
								/>
							</svg>
						</button>
					{/if}
				{/if}
			</div>
			<button
				type="button"
				class="notify-close"
				aria-label="Dismiss"
				title="Dismiss"
				on:click|stopPropagation={() => dispatch('dismiss', card)}
			>
				<svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true" focusable="false">
					<path
						d="M1 1 L11 11 M11 1 L1 11"
						stroke="currentColor"
						stroke-width="1.6"
						stroke-linecap="round"
						fill="none"
					/>
				</svg>
			</button>
		</div>
	{:else}
		<div class="notify-card notify-card--info" data-kind={card.kind}>
			<div class="notify-body">
				<p class="notify-title" title={card.title}>{card.title}</p>
				{#if card.message}
					<p class="notify-text" class:notify-text--expanded={bodyExpanded} use:clampOverflow>
						{card.message}
					</p>
					{#if bodyOverflows}
						<button
							type="button"
							class="notify-more"
							aria-expanded={bodyExpanded}
							title={bodyExpanded ? 'Show less' : card.message}
							on:click|stopPropagation={() => (bodyExpanded = !bodyExpanded)}
						>
							{bodyExpanded ? 'Show less' : 'Show more'}
							<svg
								class="notify-more-chev"
								class:flip={bodyExpanded}
								viewBox="0 0 12 12"
								width="9"
								height="9"
								aria-hidden="true"
							>
								<path
									d="M2.5 4.5 L6 8 L9.5 4.5"
									stroke="currentColor"
									stroke-width="1.5"
									stroke-linecap="round"
									stroke-linejoin="round"
									fill="none"
								/>
							</svg>
						</button>
					{/if}
				{/if}
			</div>
			<button
				type="button"
				class="notify-close"
				aria-label="Dismiss"
				title="Dismiss"
				on:click|stopPropagation={() => dispatch('dismiss', card)}
			>
				<svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true" focusable="false">
					<path
						d="M1 1 L11 11 M11 1 L1 11"
						stroke="currentColor"
						stroke-width="1.6"
						stroke-linecap="round"
						fill="none"
					/>
				</svg>
			</button>
		</div>
	{/if}
{/if}

<style>
	/* ── Themed native surface ────────────────────────────────────────────────
	   The overlay window is fully transparent + chrome-less, so each card paints
	   its OWN surface and nothing else should be visible. The /notify-overlay
	   route mirrors the app's active theme onto this webview's
	   `<html data-theme="…">`, so the app's theme tokens below resolve to the REAL
	   active palette in both light and dark — no rgba veil, no prefers-color-scheme
	   hack needed.

	   A clean, SOLID themed surface (the app's elevated card color) reads as a
	   native themed card. We deliberately DON'T use a translucent veil + backdrop
	   blur: over a fully transparent window that produced the murky "light dark
	   background" box behind the cards instead of a crisp surface. A subtle themed
	   elevation shadow gives depth instead. (The trailing literal in each var() is
	   only a last-ditch fallback for a context that somehow carries no theme.) */
	.notify-card {
		position: relative;
		display: flex;
		align-items: flex-start;
		gap: 8px;
		padding: 10px 12px;
		border-radius: 12px;
		overflow: hidden;
		background: var(--bg-elevated, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		box-shadow: var(--shadow-lg, 0 12px 32px rgba(0, 0, 0, 0.18));
		color: var(--text-primary, #1d1d1f);
		font-family: inherit;
	}

	/* Two colored brackets tracing the top-left + bottom-left corners (kind-keyed)
	   instead of a full-height bar. The straight bar left a card-background gap
	   where it met the rounded corners; brackets that follow the corner curve
	   read as a deliberate accent and hug the edge cleanly. Pseudo-elements, so
	   no extra markup; both read the kind accent via `--notify-accent`. The
	   bracket radius sits just inside the card's 12px corner. `pointer-events:
	   none` keeps them clear of the whole-card click target (deepLink info cards). */
	.notify-card::before,
	.notify-card::after {
		content: '';
		position: absolute;
		left: 5px;
		width: 13px;
		height: 13px;
		border-left: 2.5px solid var(--notify-accent, var(--color-info, #4d9de0));
		pointer-events: none;
	}
	.notify-card::before {
		top: 5px;
		border-top: 2.5px solid var(--notify-accent, var(--color-info, #4d9de0));
		border-top-left-radius: 8px;
	}
	.notify-card::after {
		bottom: 5px;
		border-bottom: 2.5px solid var(--notify-accent, var(--color-info, #4d9de0));
		border-bottom-left-radius: 8px;
	}

	.notify-body {
		flex: 1 1 auto;
		min-width: 0;
		padding-left: 2px;
	}

	.notify-title {
		margin: 0;
		font-size: 12px;
		font-weight: 600;
		line-height: 1.25;
		color: var(--text-primary, #1d1d1f);
		/* Max ~2 lines, ellipsis. */
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.notify-text {
		margin: 3px 0 0;
		font-size: 11.5px;
		line-height: 1.35;
		color: var(--text-secondary, rgba(0, 0, 0, 0.62));
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	/* Expanded: drop the 2-line clamp and show the full message in a scrollable
	   box. The card (and the overlay window, via the route's ResizeObserver) grow
	   to fit, capped here so a huge message scrolls instead of filling the screen. */
	.notify-text--expanded {
		display: block;
		-webkit-line-clamp: unset;
		line-clamp: unset;
		max-height: 168px;
		overflow-y: auto;
		overscroll-behavior: contain;
		scrollbar-width: thin;
	}

	/* Expand/collapse toggle — only rendered when the body text overflows its
	   2-line clamp. Click opens/closes the scrollable full text; it also carries
	   the full text as a native hover tooltip (`title`). */
	.notify-more {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		margin-top: 3px;
		padding: 1px 5px;
		border: none;
		background: none;
		border-radius: 5px;
		font: inherit;
		font-size: 11px;
		font-weight: 600;
		color: var(--text-muted, rgba(0, 0, 0, 0.5));
		cursor: pointer;
		transition:
			color 120ms ease,
			background 120ms ease;
	}

	.notify-more:hover {
		color: var(--text-primary, #1d1d1f);
		background: color-mix(in srgb, var(--text-muted, #888) 12%, transparent);
	}

	.notify-more-chev {
		transition: transform 160ms ease;
	}

	.notify-more-chev.flip {
		transform: rotate(180deg);
	}

	/* ── Kind accent colors ───────────────────────────────────────────────────
	   A custom property the accent bar + dot both read, set per data-kind. */
	.notify-card[data-kind='error'] {
		--notify-accent: var(--color-error, #ff453a);
	}
	.notify-card[data-kind='success'] {
		--notify-accent: var(--color-success, #30d158);
	}
	.notify-card[data-kind='info'] {
		--notify-accent: var(--color-info, #4d9de0);
	}
	.notify-card[data-kind='actionable'] {
		--notify-accent: var(--color-warning, #ff9f0a);
	}

	/* ── Top-right close × (purpose-built, NOT the generative Button) ─────────
	   18px circular hit area, EQUAL 7px inset from top + right so it reads
	   centered in the corner (5px sat in the rounded-corner curve and looked
	   off). Fades in on card hover, out otherwise — macOS style. A crisp
	   inline-SVG X (renders reliably regardless of font), centered via flex +
	   line-height:0. Actionable dismissal is locally persisted by correlation id. */
	.notify-close {
		position: absolute;
		top: 7px;
		right: 7px;
		display: flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 18px;
		padding: 0;
		margin: 0;
		border: none;
		border-radius: 50%;
		background: var(--text-muted, rgba(0, 0, 0, 0.32));
		color: var(--bg-elevated, #fff);
		line-height: 0;
		opacity: 0;
		cursor: pointer;
		transition:
			opacity 160ms ease,
			background 160ms ease;
	}

	/* Fade in on card hover (and on keyboard focus); fade back out otherwise.
	   Full opacity + a touch darker on direct hover. */
	.notify-card:hover .notify-close,
	.notify-close:focus-visible {
		opacity: 0.9;
	}

	.notify-close:hover {
		opacity: 1;
		background: var(--text-secondary, rgba(0, 0, 0, 0.55));
	}

	.notify-close svg {
		display: block;
	}

	/* No prefers-color-scheme override: the route applies the app's real theme to
	   this window's <html data-theme>, so every token above already resolves to
	   the correct light/dark palette. A media-query veil would fight that. */

	/* ── Whole-card clickable (info w/ deepLink) ──────────────────────────────*/
	.notify-card--clickable {
		cursor: pointer;
	}

	.notify-card--clickable:hover {
		background: var(--bg-card, var(--bg-elevated, #ffffff));
		border-color: var(--border-default, rgba(0, 0, 0, 0.14));
	}

	.notify-card--clickable:focus-visible {
		outline: 2px solid var(--notify-accent, #4d9de0);
		outline-offset: 1px;
	}

	/* ── Expanded text control ────────────────────────────────────────────────*/
	.notify-footer {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px;
		margin-top: 6px;
	}

	.notify-footer .notify-more {
		margin-top: 0;
	}

</style>
