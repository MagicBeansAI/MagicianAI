<script lang="ts">
  // app.css already imported by parent layout
  import { browser } from '$app/environment';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import ToastNotifications from '$lib/shared/components/ToastNotifications.svelte';
  import MediaPermissionToasts from '$lib/media/MediaPermissionToasts.svelte';
  import VoiceCallOverlay from '$lib/media/voice/VoiceCallOverlay.svelte';
  import { startConcurrentVoiceMonitor } from '$lib/media/voice/concurrentVoice';
  import { appState } from '$lib/shared/stores/app';
  import { clearAllMuijState } from '$lib/stores/muijStore';
  import {
    bindPrestoRouteAgentCycle,
    mountPrestoRoute,
    unmountPrestoRoute
  } from '$lib/magician/presto/state/routeLifecycle';
  import {
    AttentionCenter,
    attentionCenterState
  } from '$lib/attention';
  import { floatingChromeHidden } from '$lib/stores/floatingChromeStore';
  import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
  import ConfirmationModalHost from '$lib/magician/components/ConfirmationModalHost.svelte';
  import EventsConsole from '$lib/magician/components/EventsConsole.svelte';
  import { toggleEventsConsole } from '$lib/stores/eventsConsoleStore';
  import { get } from 'svelte/store';
  import { onDestroy, onMount } from 'svelte';
  import { taskStore } from '$lib/stores/taskStore';
  import {
    installScopedApiFetch,
    refreshScopeSession,
    scopeIdentityStore
  } from '$lib/stores/scopeIdentityStore';
  import { chatStore } from '$lib/stores/chatStore';
  import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';
  import { fetchEngineAvailability } from '$lib/plane/terminalGrants';
  import { disconnectMediaSession, ensureMediaSessionStarted } from '$lib/media/session';
  import { refreshMediaPreferences } from '$lib/media/preferences';
  import { ttsStore } from '$lib/media/tts/store';
  import ChatBubble from './ChatBubble.svelte';
  import { isFrozen, loadFreezeStatus } from '$lib/stores/resourceAuthorityStore';
  import AtmosphereLayer from '$lib/shell/AtmosphereLayer.svelte';
  import TopBar from '$lib/shell/TopBar.svelte';
  import CommandPalette from '$lib/shell/CommandPalette.svelte';
  import HistoryDrawer from '$lib/shell/HistoryDrawer.svelte';
  import {
    OVERLAY_IDS,
    focusedOverlay,
    mountEscapeHandler
  } from '$lib/shell/overlayCoordinator';
  import {
    commandPaletteOpen,
    historyDrawerOpen,
    historyDrawerThreadFilter,
    historyDrawerInitialTab,
    openHistoryDrawer
  } from '$lib/shell/shellState';
  import NetworkStatusBanner from '$lib/shell/NetworkStatusBanner.svelte';
  import { audioNoteOutbox } from '$lib/notes/audioNoteOutbox';

  // The v5 ("modern") shell is the only shell. The legacy shell + its
  // `magican-shell` switch were removed; this layout always renders v5.

  function openPalette(): void {
    commandPaletteOpen.set(true);
  }

  function openHistory(): void {
    openHistoryDrawer({ threadFilter: null, initialTab: 'sessions' });
  }

  // ────────────────────────────────────────────────────────────────────
  // Global keyboard shortcuts (v5 shell only)
  //
  // Bindings, mirroring the kbd hints rendered by `CommandPalette.svelte`.
  // All non-⌘ shortcuts are leader-prefixed so single letters typed in
  // the chat composer never accidentally fire a navigation.
  //
  //   ⌘K / ^K     Open the command palette
  //   N T         New task
  //   G D         Go to Today          (/today)
  //   G C         Go to Chat           (/chat)
  //   G T         Go to Tasks          (/tasks)
	//   G P         Go to Apps           (/apps)
  //   G ;         Go to Settings       (/settings)
  //   G V         Go to VibeDev        (/vibe)
  //   G O         Go to Crew operations (/crew)
  //   G W         Go to Warroom        (/warroom)
  //   G X         Open Debug           (/debug)
  //   G S         Open SOTA tests      (/debug?mode=sota-tests)
  //
  // Shortcuts are silently ignored when (a) modifier keys are held
  // (don't conflict with browser/OS combos), (b) the focus is in a form
  // field or contenteditable element, or (c) the command palette is
  // open. The palette's own ⌘K binding still
  // works via the modifier-key path above the form-focus gate.
  // ────────────────────────────────────────────────────────────────────
  type ShortcutHandler = () => void;
  const LEADER_SHORTCUTS: Record<string, Record<string, ShortcutHandler>> = {
    g: {
      d: () => goto('/today'),
      c: () => goto('/chat'),
      t: () => goto('/tasks'),
		p: () => goto('/apps'),
      ';': () => goto('/settings'),
      v: () => goto('/vibe'),
      o: () => goto('/crew'),
      w: () => goto('/warroom'),
      x: () => goto('/debug'),
      s: () => goto('/debug?mode=sota-tests'),
      // `g j` — jot a quick chat. Toggles the small floating chat
      // overlay so a thought can be sent without leaving the current
      // page. Esc closes; the same chord toggles it shut.
      j: () => chatStore.toggleBubble()
    },
    n: {
      t: () => goto('/tasks?compose=1')
    }
  };
  const LEADER_TIMEOUT_MS = 1500;

  // Tracks the active leader prefix (e.g. 'g' or 'n'). null when no
  // leader is currently waiting for a follow-up key.
  let leaderActive: string | null = null;
  let leaderTimeout: ReturnType<typeof setTimeout> | null = null;

  function clearLeader(): void {
    leaderActive = null;
    if (leaderTimeout) {
      clearTimeout(leaderTimeout);
      leaderTimeout = null;
    }
  }

  function isFormFocused(): boolean {
    if (!browser) return false;
    const el = document.activeElement as HTMLElement | null;
    if (!el) return false;
    const tag = el.tagName?.toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select') return true;
    if (el.isContentEditable) return true;
    return false;
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    const attentionOwnsFocus =
      get(focusedOverlay)?.id === OVERLAY_IDS.attentionCenter
      || get(focusedOverlay)?.id === OVERLAY_IDS.attentionPrompt
      || get(focusedOverlay)?.id === OVERLAY_IDS.attentionChannelChild;
    if (attentionOwnsFocus) {
      const key = event.key.toLowerCase();
      if ((event.metaKey || event.ctrlKey) && (key === 'k' || key === 'j' || key === 'e')) {
        event.preventDefault();
      }
      clearLeader();
      return;
    }

    // The bounded native Attention window is a single-purpose surface. Do not
    // expose general shell shortcuts that could navigate this WebView away from
    // the canonical Attention page.
    if (nativeAttentionWindow) {
      clearLeader();
      return;
    }

    // ⌘K / ^K — open palette. Always wins, even from form fields.
    const isCmdK = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k';
    if (isCmdK) {
      event.preventDefault();
      commandPaletteOpen.set(true);
      clearLeader();
      return;
    }

    // ⌘J / ^J — toggle the quick-chat overlay. Wins over form fields
    // so the chord works regardless of which input is focused; the
    // chat textarea itself owns Enter/Esc once open. Browser default
    // (downloads in some browsers) is intentionally suppressed.
    const isCmdJ = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'j';
    if (isCmdJ) {
      event.preventDefault();
      chatStore.toggleBubble();
      clearLeader();
      return;
    }

    // ⌘E / ^E — toggle the floating Events Console (draggable +
    // resizable overlay mounting the same `<EventStreamCard />` as
    // the `/events` route). Same shortcut family as ⌘K (palette) and
    // ⌘J (quick chat). Wins over form fields so operators can summon
    // the live tail from anywhere.
    //
    // ⌘⇧E / ^⇧E — full-page nav to `/events` for operators who want
    // the route view (deep-linking, full viewport real estate).
    //
    // `!event.altKey` is load-bearing: without it, `⌘⌥E` (or any
    // alt/option combo that the user might press as part of a system
    // shortcut on macOS, e.g. typed-character variants) would fall
    // through and toggle the console. Restricting to "exactly cmd OR
    // ctrl, optionally + shift" keeps the binding to the documented
    // chord and avoids surprise toggles on adjacent system bindings.
    const isCmdE =
      (event.metaKey || event.ctrlKey) &&
      !event.altKey &&
      event.key.toLowerCase() === 'e';
    if (isCmdE && event.shiftKey) {
      event.preventDefault();
      void goto('/events');
      clearLeader();
      return;
    }
    if (isCmdE) {
      event.preventDefault();
      toggleEventsConsole();
      clearLeader();
      return;
    }

    // Don't fire leader shortcuts when modifier keys are held, when
    // typing in a form field, or while the palette is open.
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (isFormFocused()) return;
    if (get(commandPaletteOpen)) return;

    const key = event.key.toLowerCase();

    // Second key of a leader-prefixed shortcut (e.g. the `t` of `N T`).
    if (leaderActive) {
      const handler = LEADER_SHORTCUTS[leaderActive]?.[key];
      clearLeader();
      if (handler) {
        event.preventDefault();
        handler();
      }
      return;
    }

    // Enter leader mode on a registered prefix key. Bounded by
    // LEADER_TIMEOUT_MS so a press-and-walk-away doesn't permanently
    // swallow the next keystroke.
    if (LEADER_SHORTCUTS[key]) {
      event.preventDefault();
      leaderActive = key;
      leaderTimeout = setTimeout(clearLeader, LEADER_TIMEOUT_MS);
    }
  }

  if (browser) {
    installScopedApiFetch();
  }

  // View Transitions opt-in moved to the ROOT layout (routes/+layout.svelte)
  // so the landing → app handoff is wrapped too. Registering here as well
  // would double-wrap every in-app navigation.

  onMount(() => {
    const stopVoiceRequests = nativeAttentionWindow ? () => {} : startConcurrentVoiceMonitor();
    // Auth gate, stage two: the load-time gate already redirected surfaces
    // with NO bearer; this one verifies the bearer is LIVE (a stale or
    // revoked token passes presence). `refreshScopeSession` resolves null
    // for both a 401 and no-token — one code path — and awaits native
    // hydration first. A network failure is not an auth verdict: the shell
    // renders and per-route error states speak for themselves.
    void refreshScopeSession()
      .then((session) => {
        if (session) {
          // Every surface sends the composer's chat engine (palette, bubble,
          // war room, voice calls), not only the chat panel: reconcile it with
          // the server and this machine's installed engines once, here.
          void fetchEngineAvailability()
            .then((roster) => chatHarnessPreferenceStore.reconcileWithRoster(roster))
            .catch(() => {});
        }
        if (!session && $page.url.pathname !== '/login') {
          const target = encodeURIComponent(
            $page.url.pathname + $page.url.search
          );
          void goto(`/login?redirectTo=${target}`);
        }
      })
      .catch(() => {});

    window.addEventListener('keydown', handleGlobalKeydown);
    const markTtsInteraction = () => ttsStore.markUserInteracted();
    window.addEventListener('pointerdown', markTtsInteraction, { once: true, passive: true });
    window.addEventListener('keydown', markTtsInteraction, { once: true });

    // Mount the OverlayCoordinator's capture-phase Esc handler so any v5
    // primitive (palette / drawer / future modals) closes from a single
    // global keystroke. Cheap: idempotent, no-op when no overlay is open.
    const releaseEscape = mountEscapeHandler();

    // The bounded native Attention window deliberately avoids booting the
    // general shell's task/media/background rails. It needs scoped API access,
    // the canonical Attention route, and the Attention globals—nothing more.
    if (!nativeAttentionWindow) {
      // Eagerly load tasks so sidebar task counts are available on all pages.
      taskStore.loadTasks().catch((err: unknown) => {
        console.warn('[app-layout] Eager task load failed:', err);
      });

      // Load Resource Authority freeze status so the global banner shows everywhere.
      loadFreezeStatus().catch(() => {});

      // Backend-owned media preferences hydrate the composer and observe surfaces.
      // Web Settings is the editor for those shared choices.
      refreshMediaPreferences().catch(() => {});

      // Recover and drain private dictation recordings staged before a tab was
      // closed or the network disappeared. Failures stay visible in /notes and
      // must never prevent the general shell from mounting.
      void audioNoteOutbox.start().catch((error) => {
        console.warn('[app-layout] Audio Notes outbox unavailable:', error);
      });

      // Realtime media rails (Phase 0): register this surface so TTS /
      // capture / pointer / future provider events all flow through one
      // session model. Best-effort — the rest of the shell renders even
      // if registration fails or the user opts out.
      void ensureMediaSessionStarted({});
    }

    return () => {
      stopVoiceRequests();
      window.removeEventListener('keydown', handleGlobalKeydown);
      window.removeEventListener('pointerdown', markTtsInteraction);
      window.removeEventListener('keydown', markTtsInteraction);
      clearLeader();
      releaseEscape();
      audioNoteOutbox.stop();
      if (!nativeAttentionWindow) void disconnectMediaSession();
    };
  });

  let state = get(appState);
  const unsubscribe = appState.subscribe((v) => (state = v));

  // The native Attention window renders the canonical /attention page while
  // suppressing only the general app chrome and background rails.
  $: isAboutRoute = $page.url.pathname === '/about' || $page.url.pathname === '/about/';
  $: nativeAttentionWindow = $page.url.searchParams.get('native_attention') === '1';
  $: showSidebar = !isAboutRoute && !nativeAttentionWindow;

  // Global freeze status — shown on ALL pages
  $: systemFrozen = $isFrozen;

  let mountedPath = '';
  let lastMuijScopeKey = '';

  function decodeSafe(value: string): string {
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }

  function extractAgentId(pathname: string): string | null {
    const crewMatch = /^\/crew\/([^/]+)(?:\/|$)/.exec(pathname);
    if (crewMatch && crewMatch[1] && crewMatch[1] !== 'new') {
      return decodeSafe(crewMatch[1]);
    }
    return null;
  }

  function extractCycleId(search: URLSearchParams): string | null {
    return search.get('cycle_id') || search.get('cycle');
  }

  $: if (browser) {
    const muijScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
    if (muijScopeKey !== lastMuijScopeKey) {
      lastMuijScopeKey = muijScopeKey;
      clearAllMuijState();
    }
  }

  $: if (browser && !nativeAttentionWindow) {
    const currentPath = $page.url.pathname;
    if (currentPath !== mountedPath) {
      if (mountedPath) {
        unmountPrestoRoute(mountedPath);
      }
      mountPrestoRoute(currentPath);
      mountedPath = currentPath;
    }
    bindPrestoRouteAgentCycle(
      currentPath,
      extractAgentId(currentPath),
      extractCycleId($page.url.searchParams)
    );
  }

  onDestroy(() => {
    unsubscribe();
    if (mountedPath) {
      unmountPrestoRoute(mountedPath);
      mountedPath = '';
    }
    clearAllMuijState();
  });
</script>

<ToastNotifications />
<NetworkStatusBanner />
{#if !nativeAttentionWindow}
  <MediaPermissionToasts />
  <VoiceCallOverlay />
{/if}

<!-- Global freeze banner — shown on ALL pages when system is frozen -->
{#if systemFrozen && !nativeAttentionWindow}
  <div
    class="ra-global-freeze-banner"
    inert={$attentionCenterState.open}
    aria-hidden={$attentionCenterState.open ? 'true' : undefined}
  >
    <span class="ra-global-freeze-text">SYSTEM FROZEN &mdash; All spending halted</span>
    <a href="/budget" class="ra-global-freeze-link">Manage</a>
  </div>
{/if}

<!-- Layout — the v5 ("modern") shell is the only shell. `data-shell-mode="v5"`
     is now matched by nothing: its only reader was `ExecutionPanel.svelte`'s
     global `[data-shell-mode='v5']` selectors, and that panel is gone. Left in
     place rather than removed in the same commit as the panel — it is a
     one-attribute hook that a shell-mode question may want again, and pulling
     it is a layout change, not part of retiring a panel. -->
<div
  class="magician-layout"
  class:with-sidebar={showSidebar}
  class:native-attention-window={nativeAttentionWindow}
  data-shell-mode="v5"
  inert={$attentionCenterState.open}
  aria-hidden={$attentionCenterState.open ? 'true' : undefined}
>
  {#if nativeAttentionWindow}
    <main class="native-attention-pane">
      <slot />
    </main>
  {:else if showSidebar}
    <!-- Atmosphere layer — fixed-position decorative ground rendered once
         behind the (app) shell. Standalone pane (/about) opts out. -->
    <AtmosphereLayer />

    <!-- v5 shell: thin top bar + full-canvas main + on-demand palette/drawer. -->
    <div class="layout-v5">
      <!-- v5 mounts BackendHealthIndicator inside <TopBar /> in compact
           mode (next to the brand) so it stays pinned in the topbar
           instead of scrolling away with .v5-main's overflow. -->
      <TopBar
        on:open-palette={openPalette}
        on:open-history={openHistory}
      />
      <main class="presto-main-pane v5-main">
        <slot />
      </main>
    </div>
  {:else}
    <!-- Standalone content (e.g., about page) — same in both shells. -->
    <div class="standalone-pane">
      <slot />
    </div>
  {/if}
</div>

<!-- Always-mounted globals (both shells) -->
<AttentionCenter />
<AttentionPromptModal />
<ConfirmationModalHost />
{#if !nativeAttentionWindow}
  <EventsConsole />
{/if}
{#if !isAboutRoute && !nativeAttentionWindow}
  <div class="floating-chrome" class:floating-chrome--hidden={$floatingChromeHidden}>
    <ChatBubble />
  </div>
{/if}

<!-- Shell overlays. Coordinator-arbitrated; Esc / backdrop close. -->
{#if showSidebar}
  <CommandPalette bind:open={$commandPaletteOpen} />
  <HistoryDrawer
    bind:open={$historyDrawerOpen}
    threadFilter={$historyDrawerThreadFilter}
    initialTab={$historyDrawerInitialTab}
  />
{/if}

<!-- Leader-key indicator. Visible while waiting for the second key of a
     leader-prefixed shortcut; auto-hides when the binding fires or the
     timeout expires. -->
{#if leaderActive && !nativeAttentionWindow}
  <div class="leader-indicator" role="status" aria-live="polite">
    <kbd>{leaderActive}</kbd>
    <span>… {Object.keys(LEADER_SHORTCUTS[leaderActive] ?? {}).join(' / ')}</span>
  </div>
{/if}

<style>
  .magician-layout.native-attention-window {
    background: var(--bg-base, #ffffff);
  }

  .native-attention-pane {
    min-height: 100vh;
    min-height: 100dvh;
    overflow-y: auto;
  }

  /* Floating chat chrome yields to
     immersive surfaces like the /square game. Opacity on the wrapper is
     safe for the fixed-position children (no containing-block change). */
  .floating-chrome {
    transition: opacity 0.35s ease;
  }
  .floating-chrome--hidden {
    opacity: 0;
    pointer-events: none;
  }

  /* Resource Authority global freeze banner */
  .ra-global-freeze-banner {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 1rem;
    background: var(--color-error-soft);
    border-bottom: 2px solid var(--color-error);
    padding: 0.5rem 1rem;
    position: sticky;
    top: 0;
    z-index: 999;
  }

  .ra-global-freeze-text {
    font-family: var(--font-primary);
    font-size: 0.8125rem;
    font-weight: 700;
    color: var(--color-error);
  }

  .ra-global-freeze-link {
    font-family: var(--font-primary);
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--color-error);
    text-decoration: underline;
  }

  .magician-layout {
    min-height: 100vh;
    /* Matches .layout-v5's dvh fix — without this, the wrapper
       overshoots the visible viewport on mobile (100vh includes the
       address bar area) and the body scrolls, dragging the sticky
       topbar away with it. */
    min-height: 100dvh;
    background: var(--bg-base, #ffffff);
    color: var(--text-primary, #2d2a26);
    font-family: var(--font-primary);
    transition: background 0.3s ease, color 0.3s ease;
    overflow-x: hidden;
  }

  /* v5 shell — top bar + full-canvas main. No left rail. */
  .layout-v5 {
    /* The shell's one vertical constant, published as a token because pages
       size themselves against it. A full-pane hero (e.g. /square) must be
       viewport-minus-topbar on its FIRST paint or the main pane overflows by
       exactly this much and the page arrives already scrolled; a page cannot
       wait for JS to measure the bar to find that out. TopBar reads it too, so
       the two can never drift apart. */
    --v5-topbar-h: 48px;
    display: flex;
    flex-direction: column;
    height: 100vh;
    height: 100dvh;
    overflow: hidden;
  }

  .layout-v5 .v5-main {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    position: relative;
    overflow-y: auto;
    overflow-x: hidden;
    /* Reserve the scrollbar gutter at all times so the centred 1320px
       column doesn't shift horizontally when content fills enough to
       trigger overflow. Was producing a brief width-flash on refresh. */
    scrollbar-gutter: stable;
  }

  .presto-main-pane {
    scrollbar-gutter: stable;
  }

  .presto-main-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    position: relative;
    overflow-y: auto;
    overflow-x: hidden;
  }

  .standalone-pane {
    min-height: 100vh;
  }

  /* Ensure full height layout */
  :global(html, body) {
    height: auto;
    min-height: 100%;
    margin: 0;
    padding: 0;
  }

  :global(.presto-gaui-page) {
    flex: 1;
    width: 100%;
    max-width: var(--app-content-max, 1320px);
    margin: 0 auto;
    box-sizing: border-box;
    padding: 1.35rem 1.45rem 5rem;
    /* Transparent so the AtmosphereLayer (paper grid + stipple) shows through
       inside the centred 1280-1320px column. Without this the column painted
       over the atmosphere and the page read as a solid slab. Per-content
       surfaces (cards, chat bubbles, etc.) bring their own --bg-card so
       readability is preserved. */
    background: transparent;
    color: var(--text-primary, #2d2a26);
  }

  :global(.presto-gaui-page .muij-container) {
    width: 100%;
    gap: 0.9rem;
  }

  :global(.presto-gaui-page .muij-card),
  :global(.presto-gaui-page .muij-form),
  :global(.presto-gaui-page .muij-entitygrid),
  :global(.presto-gaui-page .muij-actionbus),
  :global(.presto-gaui-page .muij-tabs),
  :global(.presto-gaui-page .muij-codeblock) {
    border: 1px solid var(--border-soft, #ebe7e0);
    border-radius: 14px;
    background: var(--bg-card, #ffffff);
    box-shadow: var(--shadow-sm, 0 1px 2px 0 rgba(0, 0, 0, 0.05));
    margin-bottom: 1.5rem;
  }

  :global(.home-page.presto-gaui-page) {
    display: flex;
    flex-direction: column;
  }

  :global(.home-page.presto-gaui-page .muij-container) {
    max-width: var(--app-content-max, 1320px);
    margin: 0 auto;
  }

  :global(.home-page.presto-gaui-page .presto-home-urgent-container) {
    padding: 0.9rem 1rem 0.75rem;
  }

  :global(.home-page.presto-gaui-page .presto-home-urgent-container > .muij-card-title) {
    font-size: 1.5rem;
    font-weight: 700;
    line-height: 1.2;
    color: var(--text-primary, #2d2a26);
    letter-spacing: -0.01em;
  }

  :global(.home-page.presto-gaui-page .presto-home-urgent-subsection) {
    border: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
    border-radius: 0 !important;
    padding: 0.35rem 0 0.45rem !important;
  }

  :global(.home-page.presto-gaui-page .presto-home-urgent-subsection + .presto-home-urgent-subsection) {
    margin-top: 0.45rem;
    padding-top: 0.75rem !important;
    border-top: 1px solid var(--border-soft, #ebe7e0) !important;
  }

  :global(.home-page.presto-gaui-page .presto-home-urgent-subsection .muij-card-title) {
    font-size: 0.94rem;
    font-weight: 600;
    line-height: 1.25;
    margin-bottom: 0.45rem;
    color: var(--text-muted);
  }

  :global(.presto-gaui-page .presto-queue-empty-state.muij-empty-state) {
    border: 0;
    background: transparent;
    border-radius: 18px;
    box-shadow: none;
    padding: 2rem 1rem 2.35rem;
    gap: 0.8rem;
  }

  :global(.presto-gaui-page .presto-queue-empty-state .muij-empty-icon) {
    font-size: 2.15rem;
    line-height: 1;
    color: var(--accent-primary, #e85d5d);
  }

  /* === SPELLS PAGE === */
  :global(.presto-spells-page .presto-spells-compose-card) {
    width: 100% !important;
    max-width: none !important;
    margin-bottom: 1rem;
  }

  :global(.presto-spells-page .presto-spells-inline-compose) {
    width: 100% !important;
  }

  :global(.presto-spells-page .presto-spells-inline-compose .muij-form) {
    flex: 1 !important;
    width: 100% !important;
  }

  :global(.presto-spells-page .presto-spells-compose-toggle-btn) {
    width: 100%;
  }

  :global(.presto-spells-page .presto-spells-compose-card:not(.presto-spells-compose-collapsed) .presto-spells-compose-toggle-btn) {
    margin-bottom: 0.75rem;
  }

  :global(.presto-spells-page .presto-spells-compose-collapsed) {
    padding: 0.5rem !important;
  }

  /* Inline tag editor — strip Form chrome, force single-row layout */
  :global(.presto-task-inline-tag-editor) {
    display: inline-flex !important;
    align-items: center !important;
    gap: 0.25rem !important;
  }
  :global(.presto-task-inline-tag-editor .muij-renderer-item) {
    display: contents !important;
  }
  :global(.presto-task-inline-tag-editor form) {
    display: inline-flex !important;
    flex-direction: row !important;
    align-items: center !important;
    gap: 0.35rem !important;
    margin: 0 !important;
    border: none !important;
    padding: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
    border-radius: 0 !important;
  }
  :global(.presto-task-inline-tag-editor .muij-form-body) {
    display: contents !important;
  }
  :global(.presto-task-inline-tag-editor .muij-form-generated-fields) {
    display: contents !important;
  }
  :global(.presto-task-inline-tag-editor .muij-form-field) {
    display: contents !important;
  }
  :global(.presto-task-inline-tag-editor .muij-form-field-input) {
    width: 5rem !important;
    height: auto !important;
    padding: 2px 6px !important;
    font-size: 0.75rem !important;
    line-height: 1.4 !important;
    box-shadow: none !important;
  }
  :global(.presto-task-inline-tag-editor button[type="submit"]) {
    padding: 2px 8px !important;
    font-size: 0.75rem !important;
    line-height: 1.4 !important;
    height: auto !important;
    min-height: 0 !important;
    box-shadow: none !important;
  }

  /* Inline schedule editor — single-row compact form matching the tag editor pattern */
  :global(.presto-task-inline-schedule-editor) {
    display: inline-flex !important;
    align-items: center !important;
    gap: 0.25rem !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-renderer-item) {
    display: contents !important;
  }
  :global(.presto-task-inline-schedule-editor form) {
    display: inline-flex !important;
    flex-direction: row !important;
    align-items: center !important;
    gap: 0.35rem !important;
    margin: 0 !important;
    border: none !important;
    padding: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
    border-radius: 0 !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-form-body) {
    display: contents !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-form-generated-fields) {
    display: inline-flex !important;
    flex-direction: row !important;
    gap: 0.25rem !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-form-field) {
    display: contents !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-form-field-input) {
    width: 6rem !important;
    height: auto !important;
    padding: 2px 6px !important;
    font-size: 0.75rem !important;
    line-height: 1.4 !important;
    box-shadow: none !important;
  }
  :global(.presto-task-inline-schedule-editor .muij-form-field-input[id*="timezone"]) {
    width: 3.5rem !important;
  }
  :global(.presto-task-inline-schedule-editor button[type="submit"]) {
    padding: 2px 8px !important;
    font-size: 0.75rem !important;
    line-height: 1.4 !important;
    height: auto !important;
    min-height: 0 !important;
    box-shadow: none !important;
  }

  /* Retro overrides for spells page */
  :global([data-theme^="retro-16bit"] .presto-spells-page .presto-spells-compose-card) {
    border-radius: 0 !important;
    border: 2px solid var(--text-primary) !important;
    box-shadow: 8px 8px 0 var(--text-muted) !important;
  }

  /* === VEIL / DEBUG PAGE THEME ADAPTATIONS === */
  :global(.debug-page.presto-gaui-page .veil-header-card .veil-header-title-text) {
    font-size: 1.95rem;
    font-weight: 700;
    line-height: 1.08;
    letter-spacing: -0.025em;
    color: var(--accent-primary, #e85d5d);
  }

  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .veil-header-card .veil-header-title-text) {
    font-family: var(--font-mono) !important;
    text-transform: uppercase !important;
    color: var(--text-primary) !important;
  }

  :global(.debug-page.presto-gaui-page .muij-card.veil-header-card) {
    border: 0;
    box-shadow: none;
    background: transparent;
    padding: 0.1rem 0 0.1rem;
  }

  :global(.debug-page.presto-gaui-page .muij-card.veil-mode-shell-card) {
    border-radius: var(--radius-lg, 22px);
    border: 1px solid var(--border-soft, #ece7e0);
    background: var(--bg-card, #fff);
    box-shadow: var(--shadow-sm, 0 1px 2px rgba(30, 20, 10, 0.05));
    padding: 1.05rem 1.15rem 1.15rem;
  }

  :global(.debug-page.presto-gaui-page .veil-mode-tabs) {
    width: 100%;
    padding-bottom: 0.95rem;
    border-bottom: 1px solid var(--border-soft, #efebe5);
    margin-bottom: 0.95rem;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-pill-btn) {
    border-radius: var(--radius-full, 999px);
    border: 1px solid var(--border-soft, #e3ded6);
    background: var(--bg-soft, #f8f6f2);
    color: var(--text-secondary, #5e5952);
    font-size: 0.74rem;
    font-weight: 600;
    padding: 0.42rem 0.82rem;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-pill-btn.is-active) {
    border-color: transparent;
    background: var(--accent-primary, #e85d5d);
    color: var(--text-on-accent, #fff);
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-env-btn) {
    border-radius: var(--radius-sm, 8px);
    border: 1px solid var(--border-soft, #ece7e0);
    background: var(--bg-soft, #faf8f5);
    color: var(--text-secondary, #666157);
    font-size: 0.74rem;
    font-weight: 600;
    padding: 0.34rem 0.56rem;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-env-btn.is-active) {
    border-color: transparent;
    background: var(--accent-primary, #e85d5d);
    color: var(--text-on-accent, #fff);
  }

  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form-field-label) {
    font-size: 0.78rem;
    color: var(--text-secondary, #504b44);
    font-weight: 600;
  }

  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form-field-input) {
    border-radius: var(--radius-md, 14px);
    border-color: var(--border-soft, #e7e2db);
    background: var(--bg-card, #fff);
    font-size: 0.8rem;
    padding: 0.54rem 0.78rem;
  }

  :global(.debug-page.presto-gaui-page .veil-direct-action-select-row .muij-select-input) {
    border-radius: var(--radius-md, 10px);
    border: 1px solid var(--border-soft, #e2ddd5);
    background: var(--bg-card, #fff);
    font-size: 0.8rem;
    min-height: 2.25rem;
    padding: 0.38rem 0.66rem;
    max-width: 100% !important;
    width: 100% !important;
    box-sizing: border-box;
    text-overflow: ellipsis;
    overflow: hidden;
    white-space: nowrap;
  }
  :global(.debug-page.presto-gaui-page .veil-direct-action-select-row .muij-select) {
    flex: 0 0 18rem;
    max-width: 18rem;
    min-width: 0;
  }
  /* Direct Actions skill-action runner: constrain every form input
     (text/number/textarea/select) to the card width so long parameter
     descriptions / enum option labels can't push the panel sideways.
     Native <select> popups still auto-size to longest <option> text,
     but the closed form is now bounded. */
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form) {
    max-width: 100%;
    width: 100%;
    box-sizing: border-box;
  }
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form-generated-fields) {
    max-width: 100%;
  }
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form-field) {
    min-width: 0;
    max-width: 100%;
  }
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-form-field-input) {
    width: 100% !important;
    max-width: 100% !important;
    box-sizing: border-box;
    text-overflow: ellipsis;
  }
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card textarea.muij-form-field-input) {
    text-overflow: clip;
    word-break: break-all;
    white-space: pre-wrap;
    resize: vertical;
  }
  :global(.debug-page.presto-gaui-page .veil-mode-shell-card .muij-text) {
    max-width: 100%;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-runtime-tab-btn) {
    border: 0 !important;
    border-bottom: 2px solid transparent !important;
    border-radius: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
    color: var(--text-muted, #8b857b) !important;
    font-size: 0.78rem !important;
    font-weight: 500 !important;
    min-height: 0 !important;
    padding: 0.32rem 0.18rem 0.42rem !important;
    white-space: nowrap;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-runtime-tab-btn.is-active) {
    color: var(--accent-primary, #e85d5d) !important;
    border-bottom-color: var(--accent-primary, #e85d5d) !important;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-sota-group-btn) {
    border-radius: var(--radius-full, 999px);
    border: 1px solid var(--border-soft, #e8e2da);
    background: var(--bg-soft, #f9f6f2);
    color: var(--text-secondary, #635d56);
    font-size: 0.74rem;
    font-weight: 500;
    padding: 0.32rem 0.72rem;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-sota-group-btn.is-active) {
    border-color: transparent;
    background: var(--accent-primary, #e85d5d);
    color: var(--text-on-accent, #fff);
  }

  :global(.debug-page.presto-gaui-page .veil-sota-config-row) {
    width: 100%;
    margin: 0.2rem 0 1.5rem;
    align-items: flex-start;
    --veil-sota-control-height: 2.25rem;
    --veil-sota-control-gap: 0.45rem;
  }

  :global(.debug-page.presto-gaui-page .veil-sota-max-iterations-wrap .muij-number-input),
  :global(.debug-page.presto-gaui-page .veil-sota-test-box-wrap .muij-textarea-input) {
    border-radius: var(--radius-md, 10px);
    border: 1px solid var(--border-soft, #e2ddd5);
    background: var(--bg-card, #fff);
    color: var(--text-primary, #4d4740);
  }

  :global(.debug-page.presto-gaui-page .veil-sota-test-box-wrap) {
    flex: 1 1 auto;
    min-width: 0;
  }

  :global(.debug-page.presto-gaui-page .veil-sota-fixture-row) {
    border: 1px solid var(--border-soft, #b4d3be);
    border-radius: var(--radius-full, 999px);
    background: var(--bg-soft, #edf5ef);
    padding: 0.36rem 0.58rem 0.36rem 0.7rem;
    margin: 0.82rem 0 1.05rem;
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-sota-fixture-refresh:hover:not(:disabled)) {
    background: var(--bg-surface);
    border-color: var(--border-soft);
    color: var(--text-primary, #2d2a26);
  }

  :global(.debug-page.presto-gaui-page .veil-sota-groups-row) {
    margin-top: 0.1rem;
    margin-bottom: 1.5rem;
  }

  :global(.debug-page.presto-gaui-page .muij-card.veil-sota-test-card) {
    border: 1px solid var(--border-soft, #e7e1d9);
    border-radius: var(--radius-lg, 22px);
    background: var(--bg-card, #fff);
    padding: 0.75rem 0.85rem 0.78rem;
  }

  :global(.debug-page.presto-gaui-page .veil-sota-test-phase-chip.muij-tag) {
    border: 1px solid var(--border-soft, #e5bbb5);
    border-radius: var(--radius-sm, 8px);
    background: var(--bg-soft, #f9eceb);
    color: var(--text-primary, #cf746a);
  }

  :global(.debug-page.presto-gaui-page .muij-button.veil-sota-test-action-btn) {
    border-radius: var(--radius-sm, 8px);
    background: var(--accent-primary, #e85d5d);
    color: var(--text-on-accent, #fff);
  }

  /* === RETRO OVERRIDES FOR VEIL === */
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-card),
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-button),
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-form-field-input),
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-select-input) {
    border-radius: 0 !important;
    font-family: var(--font-mono) !important;
    text-transform: uppercase !important;
  }

  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-button.veil-pill-btn.is-active),
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-button.veil-env-btn.is-active),
  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .muij-button.veil-sota-group-btn.is-active) {
    background: var(--text-primary) !important;
    color: var(--bg-base) !important;
    box-shadow: 4px 4px 0 var(--text-muted) !important;
  }

  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .veil-sota-fixture-row) {
    border-radius: 0;
    border: 1px dashed var(--text-primary);
    background: var(--bg-base);
  }

  :global([data-theme^="retro-16bit"] .debug-page.presto-gaui-page .veil-runtime-tab-btn.is-active) {
    border-bottom: 2px solid var(--text-primary) !important;
    color: var(--text-primary) !important;
  }

  :global(.debug-page.presto-gaui-page .muij-card.veil-debug-threads-card) {
    margin: 1.5rem 0 1.5rem;
  }

  :global(.debug-page.presto-gaui-page .veil-runtime-tabs) {
    margin: 0.15rem 0 0.55rem;
    padding-bottom: 0.08rem;
    border-bottom: 1px solid var(--border-soft, #ebe6de);
    align-items: flex-end;
    flex-wrap: nowrap;
    overflow-x: auto;
  }

  @media (max-width: 768px) {
    :global(.presto-gaui-page) {
      padding: 1rem;
    }

    :global(.legend-route.presto-gaui-page) {
      padding: 4rem 1rem 2rem;
    }

    :global(.briefing-page.presto-gaui-page .muij-container),
    :global(.home-page.presto-gaui-page .muij-container) {
      max-width: none;
    }

    :global(.debug-page.presto-gaui-page .veil-sota-test-grid > .muij-renderer-item) {
      flex: 1 1 100%;
      max-width: none;
    }
  }

  /* Leader-key indicator (toast at the bottom of the viewport while in
     `g`-leader mode). Theme-aware via existing tokens. */
  .leader-indicator {
    position: fixed;
    bottom: 1rem;
    left: 50%;
    transform: translateX(-50%);
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.85rem;
    border-radius: 999px;
    background: var(--bg-elevated, #ffffff);
    color: var(--text-muted, #64748b);
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
    box-shadow: var(--shadow-md);
    font-size: 0.78rem;
    font-family: var(--font-display);
    z-index: 9999;
    pointer-events: none;
  }
  .leader-indicator kbd {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    padding: 0.05rem 0.35rem;
    border-radius: 4px;
    background: var(--bg-soft, rgba(0, 0, 0, 0.06));
    color: var(--text-primary, #0f172a);
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
  }
</style>
