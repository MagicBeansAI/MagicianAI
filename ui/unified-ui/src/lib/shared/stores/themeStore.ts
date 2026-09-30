import { writable, derived } from 'svelte/store';
import { browser } from '$app/environment';

import { appendCurrentScopeQuery, getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
import { ensureThemeFonts } from '$lib/shared/themeFonts';

export const VALID_THEMES = [
  'longhand',
  'longhand-dark',
  'soft-machine-dark',
  'soft-machine',
  'arcane-terminal',
  'arcane-terminal-light',
  'retro-16bit',
  'retro-16bit-light',
  'mario-8bit',
  'mario-8bit-dark',
  'risograph',
  'risograph-dark',
  'mixtape',
  'mixtape-dark',
  'mono',
  'mono-dark',
  'cartoon',
  'cartoon-dark',
  'bubbly',
  'bubbly-dark',
  // Jarvis — dark glass HUD with cyan accents inspired by Iron Man's
  // assistant; companion light theme is a high-key version of the same
  // palette. Pure aesthetic skin (info architecture identical to other
  // themes) — no new components, no new IA.
  'jarvis',
  'jarvis-light'
] as const;

export type Theme = (typeof VALID_THEMES)[number];

export const DEFAULT_THEME: Theme = 'longhand';
const THEME_STORAGE_KEY = 'magican-theme';
const LEGACY_THEME_STORAGE_KEY = 'magician-theme';

interface UiPreferencesResponse {
  theme?: unknown;
  saved?: unknown;
}

function isTheme(value: string | null | undefined): value is Theme {
  return value !== null && VALID_THEMES.includes(value as Theme);
}

function normalizeTheme(value: unknown): Theme {
  return typeof value === 'string' && isTheme(value) ? value : DEFAULT_THEME;
}

function readLocalTheme(): Theme {
  const storedTheme = localStorage.getItem(THEME_STORAGE_KEY);
  const legacyTheme = localStorage.getItem(LEGACY_THEME_STORAGE_KEY);
  const attrTheme = document.documentElement.getAttribute('data-theme');
  return isTheme(storedTheme)
    ? storedTheme
    : isTheme(legacyTheme)
      ? legacyTheme
      : isTheme(attrTheme)
        ? attrTheme
        : DEFAULT_THEME;
}

// Theme-sync contract: the resolved CSS variables published to the Tauri
// desktop shell so its separate Settings/setup webviews (which carry none of
// this theme CSS) can mirror the active theme. Keep in lockstep with the
// desktop frontend's theme consumer.
const THEME_SYNC_TOKENS = [
  '--bg-base', '--bg-surface', '--bg-elevated', '--bg-card', '--bg-soft',
  '--text-primary', '--text-secondary', '--text-muted',
  '--accent-primary', '--accent-primary-hover', '--accent-primary-soft',
  '--border-soft', '--border-default',
  '--color-error', '--color-error-soft', '--color-success', '--color-warning',
  '--font-primary', '--font-display', '--font-brand', '--font-mono'
];

/**
 * Publish the active theme's resolved token values to the Tauri broker
 * (`set_app_theme`), so every desktop window tracks the theme live. No-op
 * outside the desktop shell; best-effort (a failure just leaves the desktop on
 * its last theme).
 */
async function reportThemeToDesktop(theme: Theme): Promise<void> {
  if (!browser || !('__TAURI_INTERNALS__' in window)) return;
  try {
    const styles = getComputedStyle(document.documentElement);
    const tokens: Record<string, string> = {};
    for (const name of THEME_SYNC_TOKENS) {
      const value = styles.getPropertyValue(name).trim();
      if (value) tokens[name] = value;
    }
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('set_app_theme', { name: theme, tokens });
  } catch {
    /* desktop not reachable — it keeps its last theme */
  }
}

function createThemeStore() {
  const { subscribe, set } = writable<Theme>(DEFAULT_THEME);
  let initialized = false;
  let currentTheme: Theme = DEFAULT_THEME;
  let backendRefresh: Promise<Theme> | null = null;
  let preferenceVersion = 0;

  function persistLocalTheme(theme: Theme): void {
    try {
      localStorage.setItem(THEME_STORAGE_KEY, theme);
      localStorage.removeItem(LEGACY_THEME_STORAGE_KEY);
    } catch {
      /* localStorage unavailable — keep in-memory state */
    }
  }

  function syncTheme(theme: Theme): void {
    if (document.documentElement.getAttribute('data-theme') !== theme) {
      document.documentElement.setAttribute('data-theme', theme);
    }
    // The shell only ships the families the default themes resolve to; a
    // costume theme fetches its own faces the first time it is applied. This
    // is the single choke point every theme change passes through, which is
    // why the hook lives here rather than in the switcher — a theme restored
    // from localStorage or pushed from the backend needs its fonts just as
    // much as one picked from a menu.
    ensureThemeFonts(theme);
    persistLocalTheme(theme);
    currentTheme = theme;
    set(theme);
    // Mirror to the desktop shell's other windows (Settings/setup). The
    // attribute is set above, so getComputedStyle now resolves the new theme.
    void reportThemeToDesktop(theme);
  }

  async function refreshFromBackend(): Promise<Theme> {
    if (!browser) return currentTheme;
    if ((window as any).__MAGICIAN_MISSING__) return currentTheme;
    if (backendRefresh) return backendRefresh;
    const requestVersion = preferenceVersion;
    backendRefresh = (async () => {
      try {
        const params = appendCurrentScopeQuery();
        const response = await fetch(`/api/magician/v2/ui/preferences?${params.toString()}`);
        if (!response.ok) throw new Error(`ui preferences fetch ${response.status}`);
        const preferences = await response.json() as UiPreferencesResponse;
        if (preferences.saved !== true) {
          const localTheme = currentTheme;
          if (requestVersion === preferenceVersion) {
            syncTheme(localTheme);
            void saveToBackend(localTheme);
          }
          return localTheme;
        }
        const theme = normalizeTheme(preferences.theme);
        if (requestVersion === preferenceVersion) {
          syncTheme(theme);
        }
        return theme;
      } catch {
        return currentTheme;
      } finally {
        backendRefresh = null;
      }
    })();
    return backendRefresh;
  }

  async function saveToBackend(theme: Theme): Promise<void> {
    if ((window as any).__MAGICIAN_MISSING__) return;
    const requestVersion = ++preferenceVersion;
    try {
      const response = await fetch('/api/magician/v2/ui/preferences', {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ theme })
      });
      if (!response.ok) throw new Error(`ui preferences save ${response.status}`);
      const preferences = await response.json() as UiPreferencesResponse;
      if (requestVersion === preferenceVersion) {
        syncTheme(normalizeTheme(preferences.theme));
      }
    } catch {
      // Keep the optimistic local theme. The next successful save/focus refresh
      // will converge this surface with the backend again.
    }
  }

  return {
    subscribe,
    // `syncWithBackend: false` applies the local theme and installs the
    // desktop-broker observer, but never asks the backend what the theme
    // should be. It exists for surfaces that pin their own theme and have no
    // backend to ask — the marketing landing is both: it ships one opinionated
    // theme, and on the public static host `/api/magician/**` does not exist at
    // all, so the round-trip is meaningless by construction rather than merely
    // unavailable. `refreshFromBackend` already bails on
    // `__MAGICIAN_MISSING__`, but that flag is set by the landing's own
    // capability probe, which resolves AFTER layout mount — the guard is
    // correct and loses the race, so the request goes out anyway and logs a
    // console error on every load and again on every window focus.
    init: (options?: { syncWithBackend?: boolean }) => {
      if (!browser) return;
      const syncWithBackend = options?.syncWithBackend ?? true;

      syncTheme(readLocalTheme());

      if (initialized) return;
      initialized = true;
      if (syncWithBackend) void refreshFromBackend();

      new MutationObserver((mutations) => {
        mutations.forEach((mutation) => {
          if (mutation.type !== 'attributes' || mutation.attributeName !== 'data-theme') return;

          const nextTheme = document.documentElement.getAttribute('data-theme');
          if (!isTheme(nextTheme)) return;
          // Self-write guard: ignore a mutation that doesn't actually change the
          // active theme (an echoed broker event re-applying the same value, or our
          // own write). Without it, every no-op `data-theme` write re-publishes to
          // the Tauri broker and can sustain a theme-flicker feedback loop.
          if (nextTheme === currentTheme) return;

          persistLocalTheme(nextTheme);
          currentTheme = nextTheme;
          set(nextTheme);
          void reportThemeToDesktop(nextTheme);
        });
      }).observe(document.documentElement, { attributes: true });

      if (syncWithBackend) {
        window.addEventListener('focus', () => {
          void refreshFromBackend();
        });
        document.addEventListener('visibilitychange', () => {
          if (document.visibilityState === 'visible') {
            void refreshFromBackend();
          }
        });
      }
    },
    setTheme: (theme: Theme) => {
      if (!browser) return;
      syncTheme(theme);
      void saveToBackend(theme);
    },
    applyRemoteTheme: (theme: Theme) => {
      if (!browser) return;
      preferenceVersion += 1;
      syncTheme(theme);
    }
  };
}

export const themeStore = createThemeStore();

export function handleUiPreferencesUpdatedEnvelope(envelope: {
  event_type: string;
  principal?: string | null;
  workspace?: string | null;
  payload: unknown;
}): void {
  if (envelope.event_type !== 'ui.preferences.updated') return;
  const scope = getCurrentScopeIdentity();
  if (
    envelope.principal
    && envelope.workspace
    && (envelope.principal !== scope.principal || envelope.workspace !== scope.workspace)
  ) {
    return;
  }
  const payload = envelope.payload && typeof envelope.payload === 'object'
    ? envelope.payload as Record<string, unknown>
    : null;
  const preferences = payload?.preferences && typeof payload.preferences === 'object'
    ? payload.preferences as { theme?: unknown }
    : null;
  if (!preferences) return;
  themeStore.applyRemoteTheme(normalizeTheme(preferences.theme));
}

export const isRetro16Bit = derived(themeStore, ($theme) =>
  $theme === 'retro-16bit' || $theme === 'retro-16bit-light'
);

export const isRetro16BitDark = derived(themeStore, ($theme) =>
  $theme === 'retro-16bit'
);

export const isRetro16BitLight = derived(themeStore, ($theme) =>
  $theme === 'retro-16bit-light'
);
