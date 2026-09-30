<script lang="ts">
  import '../app.css';
  import { page } from '$app/stores';
  import { browser } from '$app/environment';
  import { onNavigate } from '$app/navigation';
  import QuotaBanner from '$lib/shared/components/QuotaBanner.svelte';
  import MobileAppGate from '$lib/shell/MobileAppGate.svelte';
  import StartupGate from '$lib/shell/StartupGate.svelte';
  import { isMarketingPath } from '$lib/shell/mobileAccess';
  import { appState } from '$lib/shared/stores/app';
  import { themeStore } from '$lib/shared/stores/themeStore';
  import { startContextualAssistWebviewTracker } from '$lib/contextualAssistWebviewTracker';
  import { installScopedApiFetch } from '$lib/stores/scopeIdentityStore';
  import { get } from 'svelte/store';
  import { onDestroy } from 'svelte';

  let state = get(appState);
  const unsubscribe = appState.subscribe((v) => (state = v));
  let stopContextualAssistTracker: (() => void) | null = null;

  // Install before child components initialize so every Magician API request,
  // including overlay-only routes, shares the bearer transport and Tauri
  // hydration gate.
  if (browser) installScopedApiFetch();

  // ── View Transitions API opt-in ────────────────────────────────
  // Wraps every SvelteKit navigation in document.startViewTransition()
  // so route changes cross-fade per the ::view-transition-* CSS in
  // app.css. Lives in the ROOT layout (not the (app) group) so the
  // landing → app handoff gets one too — that's where the
  // `command-ask` morph (landing input → chat composer) fires.
  // Browsers without support skip the opt-in path.
  onNavigate((nav) => {
    if (!browser || !('startViewTransition' in document)) return;
    return new Promise((resolve) => {
      // @ts-ignore — startViewTransition is not yet in TS DOM lib
      document.startViewTransition(async () => {
        resolve();
        await nav.complete;
      });
    });
  });

  function initializeRuntimeUi(): void {
    // The notify-overlay is a pure THEME MIRROR webview: it applies the brokered
    // theme via its own `app-theme-changed` listener (startThemeMirror) and must
    // NOT also run themeStore.init(), whose MutationObserver re-publishes every
    // `data-theme` write back to the Tauri broker. Running both closes a feedback
    // loop (broker emit → applyAppTheme setAttribute → MutationObserver →
    // reportThemeToDesktop → broker emit → …) that flickers the theme many times a
    // second. Mirror surfaces listen, never publish.
    // The marketing landing pins one opinionated theme (its selector was
    // retired 2026-08-17) and, on the public static host, has no
    // `/api/magician/**` to ask — so it initialises the theme locally and
    // never round-trips. Without this it fetches UI preferences on every load
    // and again on every window focus, and logs a console error each time.
    const pathname = get(page).url.pathname;
    // The theme specimen sheet (/dev/theme-gallery) is skipped for the same
    // reason: each plate is an iframe that puts ITS theme on <html>, and that
    // must never be republished as the user's choice.
    if (!pathname.startsWith('/notify-overlay') && !pathname.startsWith('/dev/theme-gallery')) {
      themeStore.init({ syncWithBackend: !isMarketingPath(pathname) });
    }
    stopContextualAssistTracker = startContextualAssistWebviewTracker();
  }

  onDestroy(() => {
    stopContextualAssistTracker?.();
    unsubscribe();
  });
</script>

<StartupGate enabled={!isMarketingPath($page.url.pathname) && !$page.url.pathname.startsWith('/notify-overlay')} on:ready={initializeRuntimeUi}>
<MobileAppGate>
  <!-- Flash Messages -->
  <QuotaBanner
    visible={state.quotaExceeded}
    message={state.quotaMessage ?? 'You have reached your current quota.'}
    used={state.quotaUsed}
    limit={state.quotaLimit}
    retryAfterSec={state.retryAfterSec}
    queued={state.queued}
    perUserUsed={state.userUsed}
    perUserLimit={state.userLimit}
    queuedRequestId={state.queuedRequestId}
  />

  <slot />
</MobileAppGate>
</StartupGate>
