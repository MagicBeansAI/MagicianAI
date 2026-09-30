import { writable, get } from 'svelte/store';
import { browser } from '$app/environment';
import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

// Persisted across refreshes so the user's chosen profile sticks
// between sessions instead of snapping back to the configured default
// every reload. Stored as the profile *name*; we still validate it
// against the freshly-loaded `profiles` list in `load()` and fall back
// to the default when the saved name is no longer available (e.g. the
// profile was removed from `magician-config.yaml` or renamed).
const SELECTED_PROFILE_STORAGE_KEY = 'magician.chat.selectedProfile';

function loadPersistedSelection(): string | null {
    if (!browser) return null;
    try {
        const raw = localStorage.getItem(SELECTED_PROFILE_STORAGE_KEY);
        return raw && raw.trim() ? raw : null;
    } catch {
        return null;
    }
}

function persistSelection(name: string | null): void {
    if (!browser) return;
    try {
        if (name) {
            localStorage.setItem(SELECTED_PROFILE_STORAGE_KEY, name);
        } else {
            localStorage.removeItem(SELECTED_PROFILE_STORAGE_KEY);
        }
    } catch {
        // ignore — quota exceeded / disabled storage. Behaviour falls
        // back to "default profile on refresh" which is the pre-v0.6.x
        // baseline, so nothing is broken if persistence fails.
    }
}

export interface ChatProfile {
    name: string;
    provider: string;
    model: string;
    is_default: boolean;
    supports_user_image_inputs: boolean;
    /**
     * True when this entry is an adaptive composite profile —
     * a no-thinking fast variant that self-escalates to a thinking
     * variant when the LLM calls `request_thinking_mode`. The UI
     * surfaces these in their own group above standard profiles so
     * users can pick "auto-escalating" vs a fixed standard profile.
     */
    is_adaptive?: boolean;
    /** Free-form description from the adaptive composite spec. */
    adaptive_description?: string | null;
    /**
     * Optional tier label from the adaptive composite spec (e.g.
     * `instant` / `normal` / `advanced` / `frontier`). Rendered as a small chip in
     * the chat profile picker alongside the `Adaptive` badge so users
     * pick a size / capability point without seeing raw model names.
     * Null for standard profiles and adaptive composites that haven't
     * opted into tiering.
     */
    adaptive_tier?: string | null;
}

export interface ChatProfileWarning {
    profile_name: string;
    message: string;
}

export function resolveSelectedChatProfile(
    profiles: readonly ChatProfile[],
    currentSelected: string | null | undefined
): string | null {
    if (currentSelected && profiles.some(profile => profile.name === currentSelected)) {
        return currentSelected;
    }
    return profiles.find(profile => profile.is_default)?.name ?? profiles[0]?.name ?? null;
}

function createChatProfileStore() {
    // Hydrate the store with the persisted selection up front so the
    // chat page renders the right profile chip on first paint instead
    // of flashing the default and then swapping after `load()` lands.
    const { subscribe, set, update } = writable<{
        profiles: ChatProfile[];
        warnings: ChatProfileWarning[];
        selected: string | null;
        isLoading: boolean;
    }>({
        profiles: [],
        warnings: [],
        selected: loadPersistedSelection(),
        isLoading: false,
    });

    if (browser) {
        window.addEventListener('storage', (event) => {
            if (event.key !== SELECTED_PROFILE_STORAGE_KEY) return;
            update(s => ({
                ...s,
                selected: s.profiles.length > 0
                    ? resolveSelectedChatProfile(s.profiles, event.newValue)
                    : event.newValue
            }));
        });
    }

    return {
        subscribe,
        load: async () => {
            update(s => ({ ...s, isLoading: true }));
            try {
                // Prefer the live store value (which carries any selection
                // the user made in this session); fall back to the
                // persisted localStorage value when the store is fresh
                // (initial load after a hard refresh).
                const res = await timedFetch('/api/magician/v2/chat/profiles', {
                    headers: scopedRequestHeaders({ Accept: 'application/json' })
                });
                if (!res.ok) throw new Error(`Failed: ${res.status}`);
                const data = await res.json();
                const profiles: ChatProfile[] = data.profiles ?? [];
                const warnings: ChatProfileWarning[] = data.warnings ?? [];
                const selected = resolveSelectedChatProfile(
                    profiles, get({ subscribe }).selected ?? loadPersistedSelection()
                );
                // Re-persist whatever we ended up with — clears stale
                // localStorage entries when the saved name vanished
                // from the catalog (renamed / removed profile), and is
                // a no-op when it's still valid.
                persistSelection(selected);
                set({ profiles, warnings, selected, isLoading: false });
            } catch (err) {
                console.error('[chatProfileStore] load failed:', err);
                update(s => ({ ...s, isLoading: false }));
            }
        },
        select: (name: string) => {
            persistSelection(name);
            update(s => ({ ...s, selected: name }));
        },
        getSelected: (): string | null => {
            return get({ subscribe }).selected;
        },
    };
}

export const chatProfileStore = createChatProfileStore();
