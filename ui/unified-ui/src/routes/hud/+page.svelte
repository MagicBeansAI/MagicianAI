<!--
  /hud — HUD aesthetic shell around the reusable <ChatPanel/>.

  Two visual states:
   • COLLAPSED (default on every summon) — no transcript at all. The HUD is
     just the composer: a command bar.
   • EXPANDED — the transcript renders flat and readable. Entered either by
     the toolbar toggle or automatically when a send's reply is going to
     stream into this surface (see `handleInlineSend`).

  `@brainstorm` / `@tutor` / `@copilot` never expand: each renders its result
  on its own surface, so the HUD dismisses instead.

  Dismiss resets the state, so the next summon always starts collapsed.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import { installScopedApiFetch } from '$lib/stores/scopeIdentityStore';
	import { ensureMediaSessionStarted } from '$lib/media/session';
	import ChatPanel from '$lib/magician/chat/ChatPanel.svelte';
	import HistoryDrawer from '$lib/shell/HistoryDrawer.svelte';
	import {
		historyDrawerOpen,
		historyDrawerThreadFilter,
		historyDrawerInitialTab
	} from '$lib/shell/shellState';
	import ConfirmationModalHost from '$lib/magician/components/ConfirmationModalHost.svelte';
	import ThemeSwitcher from '$lib/shared/components/ThemeSwitcher.svelte';

	let isTauri = $state(false);
	let expanded = $state(false);
	/** The HUD's ChatPanel, so a summon can put the caret in the command bar. */
	/** The HUD's ChatPanel. Method surface the HUD actually uses — the
	 *  component exports more (attachFiles is invoked from the stage drop
	 *  handler, currentChatSessionId from attach-screen). */
	let chatPanel = $state<{
		focusComposer?: () => void;
		attachFiles?: (files: File[]) => void;
		currentChatSessionId?: () => string | null;
	} | null>(null);
	let unlistenFocus: (() => void) | null = null;
	let unlistenScreenAsk: (() => void) | null = null;

	// Screen capture-and-ask (⇧⌥S): the tray captured the display, staged it
	// as a chat attachment on today's `screens` session, and summoned this
	// HUD. Bind ChatPanel to that thread and surface the staged chip so the
	// user just types (or dictates) the question. Two delivery paths: the
	// `screen-ask-capture` event (HUD already loaded) and the
	// `take_screen_ask_capture` pull on mount (HUD freshly created — the
	// emit fired before our listeners existed).
	interface ScreenAskAttachment {
		attachment_id: string;
		stored_name: string;
		mime_type: string;
		size_bytes: number;
	}
	interface ScreenAskCapture {
		capture_id: string;
		mode: string;
		thread_id: string;
		session_id: string;
		session_title: string;
		attachments: ScreenAskAttachment[];
		source_app?: string | null;
		source_window_title?: string | null;
	}
	let screenCapture = $state<ScreenAskCapture | null>(null);
	// Suffix for the staged-chip label when the capture carries provenance:
	// "screen capture — Safari" instead of anonymous pixels. Window title is
	// transported but not shown (too long/noisy for a chip).
	const screenSourceSuffix = $derived(
		screenCapture?.source_app ? ` — ${screenCapture.source_app}` : ''
	);
	const screenSeeds = $derived(
		screenCapture
			? screenCapture.attachments.map((attachment, index) => ({
					session_id: screenCapture!.session_id,
					attachment_id: attachment.attachment_id,
					filename: attachment.stored_name,
					mime_type: attachment.mime_type,
					size: attachment.size_bytes,
					server_registered_capture: true,
					label: attachment.mime_type.startsWith('video/')
						? 'screen clip'
						: screenCapture!.mode === 'clip'
							? `clip frame ${index + 1}`
							: screenCapture!.mode === 'region'
								? `screen selection${screenSourceSuffix}`
								: `screen capture${screenSourceSuffix}`
				}))
			: null
	);

	// `/hud` lives at the root, OUTSIDE the `(app)/` route group, so it
	// doesn't inherit `(app)/+layout.svelte`'s setup. The most important
	// piece of that setup is `installScopedApiFetch()` — it monkey-
	// patches the global `fetch` to attach the workspace-bound bearer
	// headers to every `/api/magician/*` request. Without it the
	// backend treats all calls as scope-less and returns empty data
	// (no chat history, no profiles, no tasks). Re-installing here so
	// the HUD's ChatPanel can actually reach the magician backend.
	if (browser) {
		installScopedApiFetch();
		// Register HUD as a media surface so the VoiceCallButton
		// (gates on `mediaSession?.capabilities.mic`) and the TTS /
		// capture / realtime-voice rails know about us. Normally
		// done by `(app)/+layout.svelte`; `/hud` is outside that
		// group, so without this the realtime-call icon renders
		// with a slash overlay ("unavailable").
		void ensureMediaSessionStarted({});
	}

	// Mascot anchoring — ask the macOS presence host (a separate Swift
	// process) to glide the orb just below the panel. On dismiss it goes back
	// to its docked parking spot.
	//
	// `glide_mascot_to` takes SCREEN coordinates. This used to derive them
	// from `window.innerHeight` alone, which was only correct while the
	// webview filled the monitor. Now that the HUD is a small centred window,
	// viewport coordinates are window-relative, so the window's own screen
	// origin has to be added — otherwise the orb glides to a point measured
	// from the wrong corner.
	async function glideMascotToComposer() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const { getCurrentWindow, currentMonitor } = await import('@tauri-apps/api/window');
			const win = getCurrentWindow();
			// Pre-warm mounts this page while the window is still hidden; gliding
			// the orb to an invisible composer would park it at a phantom spot at
			// idle. The `overlay-focus-input` event re-glides on every real show.
			if (!(await win.isVisible())) return;
			const [origin, scaleFactor, monitor] = await Promise.all([
				win.outerPosition(),
				win.scaleFactor(),
				currentMonitor()
			]);
			const originLogical = origin.toLogical(scaleFactor);
			// Just below the panel, nudged left so the ~80px-wide orb reads as
			// centred under it.
			const cssX = originLogical.x + window.innerWidth / 2 - 40;
			const cssY = originLogical.y + window.innerHeight + 24;
			// AppKit's origin is bottom-left; CSS is top-left.
			const screenHeight = monitor
				? monitor.size.toLogical(monitor.scaleFactor).height
				: window.screen.height;
			await invoke('glide_mascot_to', { x: cssX, y: screenHeight - cssY });
		} catch (err) {
			console.warn('[hud] glide_mascot_to failed:', err);
		}
	}

	async function dockMascot() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('dock_mascot');
		} catch (err) {
			console.warn('[hud] dock_mascot failed:', err);
		}
	}

	// "Attach what I was looking at" (one tap, explicit): the HUD dips, the
	// previous frontmost window is captured through the screen-ask lane, and
	// the HUD returns with the shot staged as a chip. Not sending = discard.
	// The capture stages into the panel's CURRENT session (when bound) so the
	// thread never rebinds mid-conversation — rebinding would clear any
	// chips already staged here.
	let attachBusy = $state(false);

	async function attachScreenContext(): Promise<void> {
		if (attachBusy) return;
		attachBusy = true;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const sessionId = chatPanel?.currentChatSessionId?.() ?? null;
			await invoke('hud_attach_screen_context', { sessionId });
		} catch (err) {
			console.warn('[hud] attach screen context failed:', err);
		} finally {
			attachBusy = false;
		}
	}

	onMount(() => {
		isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
		// Window blur — fires when the Tauri HUD window loses OS
		// focus (Cmd+Z hide, user clicks outside the window, switches
		// apps, etc.). Blur whatever element is focused inside the
		// webview so the HUD starts fresh on next show.
		window.addEventListener('blur', handleWindowBlur);
		// First open, both in Tauri and in a plain browser. The
		// `overlay-focus-input` listener below is registered asynchronously and
		// can miss the emission that opened this very window, so the first
		// summon is focused here rather than relying on the event.
		chatPanel?.focusComposer?.();
		if (!isTauri) return;

		// Defer one frame so the WebView has laid out at its real size before
		// the first measurement and the first mascot glide.
		requestAnimationFrame(() => {
			void glideMascotToComposer();
		});

		// Tauri keeps the HUD WebView mounted across hide/show — so
		// onMount only fires once, on the first open. To re-anchor
		// the mascot on subsequent opens, listen for the
		// `overlay-focus-input` event Tauri emits in
		// `show_overlay_window`. Each emission means "the user just
		// reopened the HUD" — re-glide the orb and reset to floating.
		void (async () => {
			const { listen } = await import('@tauri-apps/api/event');
			const { invoke } = await import('@tauri-apps/api/core');
			unlistenFocus = await listen('overlay-focus-input', () => {
				// Attach-screen re-summons the HUD mid-conversation: this is
				// not a fresh summon, so the transcript stays exactly as it
				// was — only re-anchor the mascot and refocus the composer.
				if (attachBusy) {
					requestAnimationFrame(() => {
						void glideMascotToComposer();
						chatPanel?.focusComposer?.();
					});
					return;
				}
				expanded = false;
				requestAnimationFrame(() => {
					void glideMascotToComposer();
					// The event is named for exactly this and did not do it:
					// hiding blurs the focused element on purpose, so without
					// this the command bar reopens focused on nothing.
					chatPanel?.focusComposer?.();
				});
			});
			unlistenScreenAsk = await listen<ScreenAskCapture>('screen-ask-capture', (event) => {
				screenCapture = event.payload;
			});
			// Pull any capture staged BEFORE our listener mounted (the chord
			// that created this very window). Consume-once on the tray side.
			try {
				const pending = await invoke<ScreenAskCapture | null>('take_screen_ask_capture');
				if (pending) screenCapture = pending;
			} catch (err) {
				console.warn('[hud] take_screen_ask_capture failed:', err);
			}
		})();
	});

	onDestroy(() => {
		unlistenFocus?.();
		unlistenScreenAsk?.();
		window.removeEventListener('blur', handleWindowBlur);
	});

	function handleWindowBlur() {
		const focused = document.activeElement as HTMLElement | null;
		if (focused && focused !== document.body && typeof focused.blur === 'function') {
			focused.blur();
		}
		// Reset to floating so the next show starts cleanly.
		// Preserve scroll position — do NOT pin to bottom (the user
		// explicitly asked for this; they want to see exactly where
		// they left off when reopening).
		expanded = false;
	}

	async function dismiss() {
		if (!isTauri) return;
		// Reset so re-open starts in the compact floating view.
		expanded = false;
		// Send the mascot back to its docked parking position. Don't
		// await — the hide_overlay below shouldn't block on it.
		void dockMascot();
		const { invoke } = await import('@tauri-apps/api/core');
		await invoke('hide_overlay');
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			event.preventDefault();
			// Two-stage ESC:
			//   1. If HUD is expanded, first press returns it to
			//      floating (and blurs the composer if it was focused).
			//   2. If HUD is already floating, ESC dismisses the
			//      whole HUD window.
			// Cmd+Z continues to be the single-press dismiss /
			// toggle via the Tauri global shortcut.
			if (expanded) {
				expanded = false;
				const focused = document.activeElement as HTMLElement | null;
				if (focused && focused.closest('.composer-shell, .composer-wrap')) {
					focused.blur();
				}
				return;
			}
			void dismiss();
		}
	}

	function toggleExpanded(event: MouseEvent) {
		event.stopPropagation();
		expanded = !expanded;
	}

	function handleFireAndForgetSend() {
		void dismiss();
	}

	// A send whose reply streams into this surface needs a transcript to land
	// in — expand so the user is not left staring at an unchanged command bar.
	// `@brainstorm` / `@tutor` / `@copilot` never reach here: they each own a
	// surface elsewhere and take the fire-and-forget dismiss above instead.
	function handleInlineSend() {
		expanded = true;
	}

	// Click on the transparent stage — i.e. outside the panel — dismisses.
	// A plain ancestor test is enough now that `.hud` is a real bounded box;
	// the old coordinate-comparison handler existed because `.chat-main` was
	// full-width and fixed, so `event.target` reported the page for clicks in
	// the gutters the user perceived as "inside the panel".
	//
	// Popups are children of the panel, so `closest` covers them too — no
	// per-popup allowlist to keep in sync.
	function handleStageClick(event: MouseEvent) {
		const target = event.target as HTMLElement | null;
		if (target?.closest('.hud, .drawer, .drawer-shade, .confirmation-modal')) return;
		void dismiss();
	}

	// The stage is a whole-window DROP zone: a file dropped slightly outside
	// the panel must land in the composer, not navigate the webview away to a
	// file:// URL (the WKWebView default for an unhandled drop). Composer
	// drops stop propagation, so this only fires for genuine misses.
	function handleStageDragOver(event: DragEvent) {
		if (!event.dataTransfer?.types.includes('Files')) return;
		event.preventDefault();
	}

	function handleStageDrop(event: DragEvent) {
		if (!event.dataTransfer?.files?.length) return;
		event.preventDefault();
		const target = event.target as HTMLElement | null;
		if (target?.closest('.hud, .drawer, .drawer-shade, .confirmation-modal')) return;
		chatPanel?.attachFiles?.(Array.from(event.dataTransfer.files));
	}

</script>

<svelte:window on:keydown={handleKeydown} />

<!--
  The panel IS the window. There is no backdrop any more: the Tauri window is
  sized to this element, so clicking outside it lands on another app, blurs the
  window, and `WindowEvent::Focused(false)` hides the HUD. That retires the old
  coordinate-comparison dismiss handler and its maintenance hazard, where every
  new popup had to be registered or clicking it dismissed the HUD.
-->
<!--
  The window is a generous TRANSPARENT STAGE; this panel is a smaller themed box
  centred in it. Popups render into the transparent area around the panel, which
  is the whole reason the stage is bigger than the panel — a window cannot paint
  outside itself, so a window shrink-wrapped to the panel clips every dropdown.

  Clicking the transparent area dismisses. That is a plain ancestor test now:
  the panel is a real bounded element, unlike the old full-width fixed
  `.chat-main` that forced coordinate comparison.
-->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="hud-stage" onclick={handleStageClick} ondragover={handleStageDragOver} ondrop={handleStageDrop} role="presentation">
<div class="hud" class:hud--active={expanded}>
	<ChatPanel
		bind:this={chatPanel}
		threadId={screenCapture?.thread_id ?? null}
		seedStagedAttachments={screenSeeds}
		fireAndForget="tutor"
		hud
		on:fire-and-forget-send={handleFireAndForgetSend}
		on:inline-send={handleInlineSend}
	>
		<!-- HUD chrome, pinned right of the docked ContextPill inside the
		     composer's dock row. Previously a free-floating `.hud-actions`
		     cluster positioned at `left: calc(50% + 226px)` — an offset
		     measured against the floating pill, which no longer exists. -->
		<svelte:fragment slot="composer-dock">
			{#if isTauri}
				<button
					type="button"
					class="hud-icon-toggle"
					title={attachBusy ? 'Capturing…' : 'Attach what I was looking at'}
					aria-label={attachBusy ? 'Capturing screen context' : 'Attach screen context'}
					disabled={attachBusy}
					onclick={() => void attachScreenContext()}
				>
					{#if attachBusy}
						<span aria-hidden="true">⋯</span>
					{:else}
						<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
							<path
								d="M4 8h2.8l1.4-2h7.6l1.4 2H20v10H4z"
								fill="none"
								stroke="currentColor"
								stroke-width="1.6"
								stroke-linejoin="round"
							/>
							<circle cx="12" cy="13" r="3.2" fill="none" stroke="currentColor" stroke-width="1.6" />
						</svg>
					{/if}
				</button>
			{/if}
			<div class="hud-theme">
				<ThemeSwitcher iconOnly />
			</div>

			<button
				type="button"
				class="hud-icon-toggle"
				title={expanded ? 'Return to floating HUD' : 'Expand HUD'}
				aria-label={expanded ? 'Return to floating HUD' : 'Expand HUD'}
				aria-pressed={expanded}
				onclick={toggleExpanded}
			>
				{#if expanded}
					<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<path d="M6 3v3H3" />
						<path d="M10 3v3h3" />
						<path d="M6 13v-3H3" />
						<path d="M10 13v-3h3" />
					</svg>
				{:else}
					<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<path d="M6 3H3v3" />
						<path d="M10 3h3v3" />
						<path d="M3 10v3h3" />
						<path d="M13 10v3h-3" />
					</svg>
				{/if}
			</button>
		</svelte:fragment>
	</ChatPanel>

</div>
</div>

<!--
  HistoryDrawer normally lives in (app)/+layout.svelte. /hud is outside
  that group, so without mounting it here ContextPill's "Open history"
  button has nowhere to render and the user has no UI to unarchive
  sessions (the sidebar is hidden in HUD).
  ConfirmationModalHost is needed because HistoryDrawer's destructive
  actions (delete) dispatch through it.
-->
<HistoryDrawer
	bind:open={$historyDrawerOpen}
	threadFilter={$historyDrawerThreadFilter}
	initialTab={$historyDrawerInitialTab}
/>
<ConfirmationModalHost />

<style>
	/* Tauri window is transparent — html/body must let it through. */
	:global(html),
	:global(body) {
		margin: 0;
		padding: 0;
		overflow: hidden;
		background: transparent !important;
	}
	:global(body::before),
	:global(body::after) {
		content: none !important;
		display: none !important;
	}

	/* Transparent stage filling the window. Centres the panel and gives popups
	   room to render outside it — the whole point of the window being larger
	   than the panel. */
	.hud-stage {
		position: fixed;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		background: transparent;
	}

	/* The themed panel. The window is transparent, so the rounded corners and
	   drop shadow are real — an opaque `decorations(false)` window would have
	   hard square corners. */
	.hud {
		width: 820px;
		max-width: calc(100% - 32px);
		max-height: calc(100% - 32px);
		background: var(--bg-base, #f3ead6);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.10));
		border-radius: 14px;
		box-shadow:
			0 12px 40px -8px rgba(0, 0, 0, 0.35),
			0 2px 8px -2px rgba(0, 0, 0, 0.18);
		/* Visible, so a dropdown can spill into the stage around the panel.
		   `border-radius` still clips the panel's own background and border;
		   the transcript clips itself via `overflow-y: auto`. */
		overflow: visible;
		display: flex;
		flex-direction: column;
	}

	/* The panel IS the composer's container. Without this the composer paints
	   its own border, background, radius and shadow inside the panel's, and
	   you see a box inside a box. */
	.hud :global(.composer-shell) {
		background: transparent !important;
		border: 0 !important;
		border-radius: 0 !important;
		box-shadow: none !important;
		backdrop-filter: none !important;
		-webkit-backdrop-filter: none !important;
		width: 100% !important;
		max-width: none !important;
	}

	/* ─── The HUD is never the phone composer ───
	   `FloatingComposer` collapses to a touch layout under
	   `@media (max-width: 767px)`: `Do|Plan` hidden, 40px targets, the profile
	   chip reduced to a bare icon. The window width IS the webview width, so a
	   narrow HUD window silently rendered the mobile composer.

	   The window is now 820px, which clears the breakpoint — but relying on
	   that alone is fragile: it re-breaks the moment anyone changes the width,
	   and it is wrong before a desktop rebuild lands. The HUD is a
	   desktop-shaped surface at any size, so re-assert the desktop layout
	   unconditionally.

	   Specificity: the mobile rules are `.composer-tools .seg` (0,2,0); these
	   are `.hud :global(...)` = (0,3,0) and win on their own, with
	   `!important` as belt-and-braces inside the media query. */
	.hud :global(.composer-tools .seg) {
		display: inline-flex !important;
	}
	.hud :global(.composer-tools .profile .pp-badge),
	.hud :global(.composer-tools .profile .pp-model),
	.hud :global(.composer-tools .profile .pp-chevron) {
		display: inline !important;
	}
	.hud :global(.composer-tools .profile) {
		width: auto !important;
		height: auto !important;
		min-width: 0 !important;
		padding: 2px 6px !important;
		justify-content: flex-start !important;
		gap: 4px !important;
	}
	.hud :global(.composer-tools .profile .pp-icon) {
		width: 10px !important;
		height: 10px !important;
	}
	.hud :global(.composer-tools .mic-capture__btn),
	.hud :global(.composer-tools .capture-btn),
	.hud :global(.composer-tools .voice-call-btn),
	.hud :global(.composer-tools .auto-speak-toggle button),
	.hud :global(.composer-tools .auto-speak-toggle__btn),
	.hud :global(.composer-tools .icon-btn) {
		width: 20px !important;
		height: 20px !important;
	}

	/* The panel IS the composer's container. Without this the composer paints
	   its own border, background, radius and shadow inside the panel's, and
	   you see a box inside a box. */
	.hud :global(.composer-shell) {
		background: transparent !important;
		border: 0 !important;
		border-radius: 0 !important;
		box-shadow: none !important;
		backdrop-filter: none !important;
		-webkit-backdrop-filter: none !important;
		width: 100% !important;
		max-width: none !important;
	}

	/* ─── Hide chrome ChatPanel inherits from /chat ───
	   ChatPanel is a clone of /chat — it brings sidebar / workbench /
	   modals along. The HUD suppresses them so only the transcript +
	   composer reads. */
	.hud :global(.chat-sidebar),
	.hud :global(.workbench-column),
	.hud :global(.execution-panel-overlay),
	.hud :global(.chat-plan-sheet),
	.hud :global(.dev-workbench-launcher),
	.hud :global(.todo-sidebar) {
		display: none !important;
	}

	/* Page wrapper transparent, single-column layout. */
	/* Normal flow inside the panel — the window is sized to the panel, so
	   nothing here may claim viewport height. */
	.hud :global(.chat-page) {
		background: transparent !important;
		display: block !important;
		grid-template-columns: 1fr !important;
		padding: 0 !important;
		max-width: none !important;
		height: auto;
		overflow: visible !important;
	}

	/* ─── Floating state defaults (chat-main + messages + composer) ─── */
	/* The chat-main pane is fixed-positioned so we can shrink it to the
	   top half while floating and expand to full screen on demand. */
	.hud :global(.chat-main) {
		position: static !important;
		top: auto;
		bottom: auto;
		transform: none;
		height: auto;
		max-width: none;
		margin: 0;
		background: transparent !important;
		display: flex;
		flex-direction: column;
		/* Must stay visible: `.chat-main` is `overflow: hidden` in ChatPanel,
		   which would clip the docked chrome's popups. The panel's own
		   `overflow: hidden` does the rounded-corner clipping instead. */
		overflow: visible !important;
	}


	/* ─── Transcript visibility ───
	   COLLAPSED renders no transcript at all: the HUD is the composer.
	   EXPANDED renders it flat and unmasked for reading.

	   The previous floating state rendered it tilted (rotateX 15deg) and
	   masked to 5% alpha at the top — unreadable by design, ~56vh of window,
	   and the reason backdrop-dismiss needed a coordinate hack. Neither state
	   wants that now, so the whole 3D treatment is gone: the mask ramp, the
	   Z-inversion, the `perspective` scene root and its `preserve-3d`
	   propagation. */
	.hud :global(.chat-messages-area) {
		display: none;
	}

	.hud--active :global(.chat-messages-area) {
		display: block;
		flex: 1 1 auto;
		min-height: 0;
		overflow-y: auto;
		background: transparent !important;
		padding-top: 12px !important;
		padding-bottom: 4px !important;
	}

	/* Composer floats at vertical center while floating. The
	   FloatingComposer is normally position:fixed at the bottom; we
	   override it to top:50% / translateY(-50%) so it reads as
	   "expectant" — the cursor is right where the user's eye lands. */
	.hud :global(.composer-wrap) {
		/* STATIC positioning — composer-wrap becomes a normal flex
		   child of `.chat-main` (which is `display: flex;
		   flex-direction: column`). With chat-messages-area set to
		   `flex: 1`, the composer sits naturally below it. */
		position: static !important;
		top: auto !important;
		bottom: auto !important;
		left: auto !important;
		right: auto !important;
		pointer-events: auto;
		z-index: auto;
		flex: 0 0 auto;
	}
	/* Chrome-stripping lives in the single `.composer-shell` rule near the top
	   of this block. This one used to paint `--bg-elevated`, an accent border
	   and a shadow — correct when the composer floated alone over the desktop,
	   but inside the themed panel it drew a second visible box around the
	   input. */
	.hud :global(.composer-shell) {
		pointer-events: auto;
	}
	/* The composer-wrap is normally an invisible flex container, but
	   defensive override in case anything inherits a border/shadow. */
	.hud :global(.composer-wrap) {
		box-shadow: none !important;
		background: transparent !important;
	}
	/* The chat-input-area is the LEGACY shell's composer wrapper.
	   Even though /hud renders the v5 FloatingComposer, the chat-page
	   markup still has the chat-input-area div (just hidden via
	   conditional logic) — if it sneaks through, ensure it doesn't
	   paint anything. */
	.hud :global(.chat-input-area),
	.hud :global(.chat-input-container) {
		background: transparent !important;
		border: none !important;
		box-shadow: none !important;
	}

	/* Bubbles — keep their theme-driven palette (chat-bubble-primary
	   uses `--accent-primary`, chat-bubble-neutral uses `--bg-card`),
	   just dial down opacity in HUD context so they read as glass
	   over the desktop. `color-mix` preserves the theme color (e.g.
	   the brown of longhand's --accent-primary) but mixes with
	   transparent for the alpha. Without this every bubble was 100%
	   opaque (browser /chat behavior is right for a solid page, but
	   over a transparent HUD it feels too dense). */
	/* No backdrop-filter on chat bubbles. A backdrop-filter spawns a
	   compositing layer for the bubble, and the parent's `mask-image`
	   gradient (applied on `.chat-messages-area`) does not always cleanly
	   fade the contents of a child compositing layer. Without the blur the
	   bubble is a plain solid `background-color` element, so the mask fades
	   it cleanly to transparent. The HUD's overall glass feel is carried by
	   the composer's blur and the transparent window over the desktop. */
	/* Headers above bubbles ("You 14:10", "Assistant 14:10") — force
	   sharp rendering. No filters can leak from neighboring elements.
	   `will-change: transform` on the parent forces a composite layer
	   that bypasses any sibling backdrop-filter influence. */
	.hud :global(.chat-header) {
		filter: none !important;
		backdrop-filter: none !important;
		-webkit-backdrop-filter: none !important;
		text-rendering: geometricPrecision;
		-webkit-font-smoothing: antialiased;
		position: relative;
		z-index: 1;
	}
	/* Bubble bg at 98% solid — essentially opaque for crisp text
	   and bubble distinctness at the bottom of the chat (where the
	   mask is fully opaque). The mask gradient still tapers the
	   final rendered alpha smoothly up the chat: at full mask the
	   bubble sits at 98% (solid feel); at mid-mask it drops below
	   50% effective alpha (silhouette becomes faint enough not to
	   read as a discrete shape). */
	.hud :global(.chat-page .chat-bubble-neutral) {
		background-color: var(--bg-card, #ffffff) !important;
	}
	.hud :global(.chat-page .chat-bubble-primary) {
		background-color: var(--accent-primary, #ff6b6b) !important;
	}
	.hud :global(.chat-action-card-wrap > div),
	.hud :global(.chat-plan-bubble),
	.hud :global(.chat-turn-typing__bubble),
	.hud :global(.chat-executed-alert),
	.hud :global(.chat-rich-result-card),
	.hud :global(.chat-status-alert),
	.hud :global(.chat-escalation-card),
	.hud :global(.chat-escalation-resolved-card) {
		background-color: var(--bg-card, #ffffff) !important;
	}

	/* ─── Bubble + pill shadow cleanup ───
	   No drop shadows on bubbles. The mask gradient fades the bubble
	   itself but the shadow below extends into a less-faded region
	   of the mask — so the shadow persists as a "residual border" at
	   the top of the chat box even when the bubble is mostly faded.
	   Solid bg + colored border are enough visual definition in HUD
	   context; the HUD's own composition gives the layering.
	   The 1px chat-bubble-neutral ring (`0 0 0 1px var(--border-soft)`)
	   is also stripped — it was the rounded outline visible around
	   faded bubbles. */
	.hud :global(.chat-bubble),
	.hud :global(.chat-bubble-neutral),
	.hud :global(.chat-bubble-primary),
	.hud :global(.chat-action-card-wrap > div),
	.hud :global(.chat-plan-bubble),
	.hud :global(.chat-turn-typing__bubble),
	.hud :global(.chat-executed-alert),
	.hud :global(.chat-rich-result-card),
	.hud :global(.chat-status-alert),
	.hud :global(.chat-escalation-card),
	.hud :global(.chat-escalation-resolved-card),
	.hud :global(.request-activity) {
		box-shadow: none !important;
		border: none !important;
	}

	/* Higher-specificity stomp on the bubble ring. ChatPanel sets
	   `.chat-page :global(.chat-bubble-neutral) { box-shadow: 0 0 0 1px var(--border-soft); }`
	   which ties at specificity (0,2,0) with the rule above and — depending
	   on stylesheet order in the bundle — can win when neither has
	   !important on box-shadow. Chaining `.chat-page` lifts our rule to
	   (0,3,0), guaranteeing it wins regardless of declaration order. */
	.hud :global(.chat-page .chat-bubble),
	.hud :global(.chat-page .chat-bubble-neutral),
	.hud :global(.chat-page .chat-bubble-primary),
	.hud :global(.chat-page .chat-bubble-attachment),
	.hud :global(.chat-page .chat-plan-bubble),
	.hud :global(.chat-page .chat-turn-typing__bubble) {
		box-shadow: none !important;
		border: none !important;
		outline: none !important;
	}


	/* Status pills, badges, headers, timestamps — anywhere a small
	   pill-shaped element sits above a bubble. Strip any
	   shadow that could halo against the transparent HUD. */
	.hud :global(.chat-status-pill),
	.hud :global(.chat-msg-time),
	.hud :global(.chat-header),
	.hud :global(.chat-executed-label),
	.hud :global(.chat-rich-result-label),
	.hud :global(.chat-status-label),
	.hud :global(.chat-escalation-title),
	.hud :global(.chat-escalation-resolved-label),
	.hud :global(.chat-status-live) {
		box-shadow: none !important;
	}


	/* ─── EXPANDED state ─── */
	/* In the new two-row layout, the chat-main and composer always
	   occupy the same screen rows regardless of focus state — no
	   "composer drops down" animation because composer is already
	   at the bottom. The 3D push + glass backdrop still fire on
	   expansion for visual feedback, but no layout reflow needed. */
	.hud--active :global(.chat-main) {
		/* Bounded so a long transcript scrolls inside the panel rather than
		   growing it past the stage. The panel's own `max-height` on `.hud`
		   is the outer guard; this keeps the transcript the part that
		   scrolls. */
		max-height: 520px;
	}

	/* ─── HUD controls (docked right of the ContextPill) ─── */
	/* `align-items: center` on every level of the wrapper chain. The dock row
	   centres its own children, but ThemeSwitcher nests its button inside a
	   `.theme-switcher` div — an un-centred intermediate wrapper lets that
	   button sit at a different height from the bare `.hud-icon-toggle`
	   beside it, which is what made the two icons look misaligned. */
	.hud-theme {
		display: inline-flex;
		align-items: center;
		/* Never squeezed by a long session title — the pill flexes, these do
		   not. (`.hud-icon-toggle` carries the same.) */
		flex-shrink: 0;
	}

	.hud-theme :global(.theme-switcher) {
		display: inline-flex;
		align-items: center;
		line-height: 0;
	}

	/* Popups overlay normally and spill into the transparent stage around the
	   panel. Earlier iterations forced them in flow, or measured them to grow
	   the window — both were working around a window shrink-wrapped to the
	   panel, which the stage removes. */
	.hud :global(.theme-dropdown),
	.hud :global(.more-menu) {
		max-height: 320px;
		overflow-y: auto;
	}

</style>
