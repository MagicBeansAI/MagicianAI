<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import Setup from "./lib/Setup.svelte";
  import Settings from "./lib/Settings.svelte";
  import OrbSurface from "./orb/OrbSurface.svelte";
  import { startContextualAssistWebviewTracker } from "./lib/contextualAssistWebviewTracker";

  // App theme, brokered by Tauri (`theme.rs`). The unified-ui publishes the
  // active theme's resolved tokens (it's a separate origin, so no shared
  // localStorage); this window mirrors them so Settings/setup match the app
  // theme live. See docs/components/desktop + themeStore.ts.
  interface AppThemeSnapshot {
    name: string;
    tokens: Record<string, string>;
  }

  function applyAppTheme(snapshot: AppThemeSnapshot | null): void {
    if (!snapshot) return;
    const root = document.documentElement;
    if (snapshot.name) root.setAttribute("data-theme", snapshot.name);
    for (const [key, value] of Object.entries(snapshot.tokens ?? {})) {
      if (key.startsWith("--") && value) root.style.setProperty(key, value);
    }
  }

  let unlistenTheme: UnlistenFn | null = null;

  // After the unified-ui consolidation (2026-05-22), this tray-owned
  // Svelte app renders Setup, Settings, and the process-owned notch Orb. Unified-ui-backed
  // native surfaces such as Quick Automate (`/overlay`) and bounded
  // Attention load directly from the Vite dev server via
  // `WebviewUrl::External`, not through this entrypoint. General app routes
  // open in the user's browser.
  // See `docs/plans/2026-05-22-tauri-unified-ui-consolidation.md`.

  interface StatusResponse {
    container_running: boolean;
    container_status: string;
    runtime_name: string;
    image: string;
    health: string;
    needs_setup: boolean;
    runtime_managed: boolean;
  }

  let view = $state<"loading" | "setup" | "settings" | "orb">(
    window.location.pathname === "/orb" ? "orb" : "loading",
  );
  let status = $state<StatusResponse | null>(null);
  let error = $state<string>("");
  let stopContextualAssistTracker: (() => void) | null = null;

  onMount(async () => {
    const path = window.location.pathname;
    if (path === "/orb") {
      view = "orb";
      return;
    }
    view = path === "/setup" ? "setup" : "settings";
    stopContextualAssistTracker = startContextualAssistWebviewTracker();

    // Mirror the app theme: apply the current snapshot, then track live changes.
    try {
      applyAppTheme(await invoke<AppThemeSnapshot>("get_app_theme"));
      unlistenTheme = await listen<AppThemeSnapshot>("app-theme-changed", (event) =>
        applyAppTheme(event.payload),
      );
    } catch {
      /* not in a themed shell — fall back to the static stylesheet */
    }

    try {
      const result = await invoke<StatusResponse>("get_status");
      status = result;
    } catch (e) {
      error = String(e);
    }
  });

  onDestroy(() => {
    stopContextualAssistTracker?.();
    unlistenTheme?.();
  });

  async function onSetupComplete(capabilitiesCompleted = false) {
    await invoke("finish_setup", { capabilitiesCompleted });
  }
</script>

{#if view === "loading"}
  <div class="loading-screen">
    <div class="spinner"></div>
    <p>Loading...</p>
    {#if error}
      <p class="error-text">{error}</p>
    {/if}
  </div>
{:else if view === "setup"}
  <Setup onComplete={onSetupComplete} />
{:else if view === "settings"}
  <Settings {status} />
{:else if view === "orb"}
  <OrbSurface />
{/if}

<style>
  .loading-screen {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 100vh;
    gap: 16px;
  }

  .spinner {
    width: 32px;
    height: 32px;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .error-text {
    color: var(--error);
    font-size: 13px;
  }
</style>
