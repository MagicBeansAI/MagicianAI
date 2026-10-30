import { writable, get } from 'svelte/store';
import { browser } from '$app/environment';
import { timedFetch } from '$lib/shared/fetch';
import { scopeIdentityStore, scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
import { routeForSelection, selectionForRoute, type ModelChoice, type ModelSelection } from '$lib/llm/modelCatalog';
import { migrateLegacyChatSelection } from '$lib/llm/legacySelections';
import { chatModelRows, typedChatModelRequest } from '$lib/llm/chatModelSelection';

// Model/thinking is authoritative. Read the old key once for migration.
const SELECTED_PROFILE_STORAGE_KEY = 'magician.chat.selectedProfile';
const SELECTED_MODEL_STORAGE_KEY = 'magician.chat.modelSelection';
export const UNAVAILABLE_MODEL_SELECTION = '__unavailable_model_selection__';

export function decodeSavedModelSelection(raw: string | null): ModelSelection | null {
    if (!raw) return null;
    try {
        const value = JSON.parse(raw);
        return value && typeof value.model_id === 'string' && value.model_id.trim()
            && typeof value.thinking === 'string' && value.thinking.trim()
            ? { model_id: value.model_id, thinking: value.thinking } : null;
    } catch { return null; }
}


export interface ChatProfile {
    name: string;
    provider: string;
    model: string;
    is_default: boolean;
    supports_user_image_inputs: boolean;
    model_selection?: ModelSelection | null;
    model_choice?: ModelChoice | null;

}

export interface ChatProfileWarning {
    profile_name: string;
    message: string;
}

export function resolveSelectedChatProfile(
    profiles: readonly ChatProfile[],
    currentSelected: string | null | undefined,
    models: readonly ModelChoice[] = profiles.flatMap(profile => profile.model_choice ? [profile.model_choice] : []),
    savedSelection: ModelSelection | null = null
): string | null {
    const previous = profiles.find(profile => profile.name === currentSelected);
    const selection = savedSelection ?? previous?.model_selection ?? migrateLegacyChatSelection(currentSelected);
    if (selection) {
        const route = routeForSelection(models, selection);
        if (route && profiles.some(profile => profile.name === route)) return route;
        return UNAVAILABLE_MODEL_SELECTION;
    }
    if (currentSelected) return currentSelected; // Never silently reset a saved choice.
    const fallback = profiles.find(profile => profile.is_default) ?? profiles[0];
    if (!fallback) return null;
    return fallback.model_selection
        ? resolveSelectedChatProfile(profiles, fallback.name, models, fallback.model_selection)
        : fallback.name;
}

export function createChatProfileStore() {
    const scopeKey = () => {
        const scope = get(scopeIdentityStore);
        return JSON.stringify([scope.principal, scope.workspace, getCurrentScopeCredentialRevision()]);
    };
    let activeSessionKey: string | null = null;
    const selectionKey = (base: string) => activeSessionKey ? `${base}.${activeSessionKey}` : base;
    /** Whether the open chat saved a model pick of its own. A chat without one
     *  inherits the browser-wide pick (the unscoped keys) and keeps following
     *  it; only an explicit pick in the chat pins it. */
    function sessionOwnsSelection(): boolean {
        if (!activeSessionKey || !browser) return false;
        try {
            return localStorage.getItem(selectionKey(SELECTED_MODEL_STORAGE_KEY)) !== null
                || localStorage.getItem(selectionKey(SELECTED_PROFILE_STORAGE_KEY)) !== null;
        } catch { return false; }
    }
    const inheritsBrowserSelection = () => !!activeSessionKey && !sessionOwnsSelection();
    /** The key a saved pick is read from: the chat's own, else the browser's. */
    const readKey = (base: string) => inheritsBrowserSelection() ? base : selectionKey(base);
function loadPersistedModelSelection(): ModelSelection | null {
    if (!browser) return null;
    try { return decodeSavedModelSelection(localStorage.getItem(readKey(SELECTED_MODEL_STORAGE_KEY))); }
    catch { return null; }
}

function invalidPersistedModelSelection(): boolean {
    if (!browser) return false;
    try {
        const raw = localStorage.getItem(readKey(SELECTED_MODEL_STORAGE_KEY));
        return raw !== null && decodeSavedModelSelection(raw) === null;
    } catch { return false; }
}

function unavailableSelectionMessage(profiles: readonly ChatProfile[], selected: string | null): string | null {
    return selected && !profiles.some(profile => profile.name === selected)
        ? 'Your saved model choice is unavailable. Choose a model to continue.' : null;
}

function loadPersistedSelection(): string | null {
    if (!browser) return null;
    try {
        const raw = localStorage.getItem(readKey(SELECTED_PROFILE_STORAGE_KEY));
        return raw && raw.trim() ? raw : null;
    } catch {
        return null;
    }
}

function persistSelection(name: string | null, models: readonly ModelChoice[], profiles: readonly ChatProfile[]): void {
    if (!browser) return;
    try {
        if (name) {
            const selection = selectionForRoute(models, profiles, name);
            if (!selection) return; // Retain unresolved/older preferences until migration can resolve them.
            localStorage.setItem(selectionKey(SELECTED_MODEL_STORAGE_KEY), JSON.stringify(selection));
            localStorage.removeItem(selectionKey(SELECTED_PROFILE_STORAGE_KEY));
        } else {
            localStorage.removeItem(selectionKey(SELECTED_PROFILE_STORAGE_KEY));
            localStorage.removeItem(selectionKey(SELECTED_MODEL_STORAGE_KEY));
        }
    } catch {
        // ignore — quota exceeded / disabled storage. Behaviour falls
        // back to "default profile on refresh" which is the pre-v0.6.x
        // baseline, so nothing is broken if persistence fails.
    }
}


    let loadGeneration = 0;
    // The engine whose catalog last finished loading in this store, and the
    // load in flight. Senders without a picker of their own (chat bubble,
    // command palette, war room) await `ensureLoaded` instead of failing with
    // "still loading" when no composer has loaded this store yet.
    let catalogEngine: string | null = null;
    let catalogScope: string | null = null;
    let inflight: { engine: string; scope: string; promise: Promise<void> } | null = null;
    // Hydrate the store with the persisted selection up front so the
    // chat page renders the right profile chip on first paint instead
    // of flashing the default and then swapping after `load()` lands.
    const { subscribe, set, update } = writable<{
        profiles: ChatProfile[];
        models: ModelChoice[];
        warnings: ChatProfileWarning[];
        selected: string | null;
        selectionError: string | null;
        isLoading: boolean;
        engine: string;
    }>({
        profiles: [],
        models: [],
        warnings: [],
        selected: loadPersistedSelection(),
        selectionError: null,
        isLoading: true,
        engine: 'magician',
    });

    async function loadCatalog(engine: string, scope: string): Promise<void> {
        const generation = ++loadGeneration;
        update(s => ({ ...s, isLoading: true }));
        try {
            // Prefer the live store value (which carries any selection
            // the user made in this session); fall back to the
            // persisted localStorage value when the store is fresh
            // (initial load after a hard refresh).
            const res = await timedFetch(`/api/magician/v2/chat/models?engine=${encodeURIComponent(engine)}`, {
                headers: scopedRequestHeaders({ Accept: 'application/json' })
            });
            if (!res.ok) throw new Error(`Failed: ${res.status}`);
            const data = await res.json();
            if (generation !== loadGeneration || scope !== scopeKey()) return;
            const models: ModelChoice[] = data.models ?? [];
            const profiles: ChatProfile[] = chatModelRows(models, data.default_selection ?? null);
            const warnings: ChatProfileWarning[] = data.warnings ?? [];
            let selected = invalidPersistedModelSelection() ? UNAVAILABLE_MODEL_SELECTION : resolveSelectedChatProfile(
                profiles, get({ subscribe }).selected ?? loadPersistedSelection(), models, loadPersistedModelSelection()
            );
            if (!data.default_selection && !get({ subscribe }).selected && !loadPersistedSelection() && !loadPersistedModelSelection()) selected = null;
            // Migrate known aliases to typed choices. Unavailable choices
            // retain their saved data and require an explicit new choice.
            const selectionError = unavailableSelectionMessage(profiles, selected)
                ?? (!selected ? 'Choose a model to continue.' : null);
            // An inherited pick is not copied into the chat's own key: that
            // would pin the chat and stop it following the browser default.
            if (!selectionError && !inheritsBrowserSelection()) persistSelection(selected, models, profiles);
            catalogEngine = engine;
            catalogScope = scope;
            set({ profiles, models, warnings, selected, selectionError, isLoading: false, engine });
        } catch (err) {
            if (generation !== loadGeneration || scope !== scopeKey()) return;
            catalogScope = null;
            console.error('[chatProfileStore] load failed:', err);
            update(s => ({ ...s, isLoading: false,
                selectionError: 'Model choices could not be loaded. Reconnect and try again.' }));
        }
    }

    function load(engine = 'magician'): Promise<void> {
        if (!['magician', 'pi'].includes(engine)) return Promise.resolve();
        const scope = scopeKey();
        const promise = loadCatalog(engine, scope);
        inflight = { engine, scope, promise };
        void promise.finally(() => { if (inflight?.promise === promise) inflight = null; });
        return promise;
    }

    let removeStorageListener = () => {};
    if (browser) {
        const storageListener = (event: StorageEvent) => {
            const watched = [SELECTED_PROFILE_STORAGE_KEY, SELECTED_MODEL_STORAGE_KEY]
                .flatMap(base => [selectionKey(base), readKey(base)]);
            if (!event.key || !watched.includes(event.key)) return;
            update(s => {
                const selected = invalidPersistedModelSelection() ? UNAVAILABLE_MODEL_SELECTION
                    : s.profiles.length > 0
                        ? resolveSelectedChatProfile(s.profiles, loadPersistedSelection(), s.models, loadPersistedModelSelection())
                        : loadPersistedSelection();
                return { ...s, selected, selectionError: unavailableSelectionMessage(s.profiles, selected) };
            });
        };
        window.addEventListener('storage', storageListener);
        removeStorageListener = () => window.removeEventListener('storage', storageListener);
    }

    return {
        subscribe,
        destroy: () => { loadGeneration += 1; removeStorageListener(); },
        activateSession: (key: string | null) => {
            if (activeSessionKey === key) return;
            activeSessionKey = key;
            // Invalidate an old chat's in-flight catalog response.
            loadGeneration += 1;
            update(s => {
                const selected = invalidPersistedModelSelection() ? UNAVAILABLE_MODEL_SELECTION
                    : resolveSelectedChatProfile(s.profiles, loadPersistedSelection(), s.models, loadPersistedModelSelection());
                return { ...s, selected, isLoading: false,
                    selectionError: unavailableSelectionMessage(s.profiles, selected) };
            });
        },
        load,
        /** Load the catalog for `engine` unless it is already here; join a load
         *  in flight. A failed load leaves `selectionError` for the caller. */
        ensureLoaded: async (engine = 'magician'): Promise<void> => {
            if (!['magician', 'pi'].includes(engine)) return;
            // Two passes: a chat switch can invalidate the load we joined.
            for (let attempt = 0; attempt < 2; attempt += 1) {
                const scope = scopeKey();
                if (catalogEngine === engine && catalogScope === scope && !get({ subscribe }).isLoading) return;
                await (inflight?.engine === engine && inflight.scope === scope ? inflight.promise : load(engine));
                if (scope === scopeKey() && get({ subscribe }).selectionError && catalogScope !== scope) return;
            }
        },
        select: (name: string) => {
            const state = get({ subscribe });
            if (!state.profiles.some(profile => profile.name === name)) return;
            const selected = resolveSelectedChatProfile(state.profiles, name, state.models);
            persistSelection(selected, state.models, state.profiles);
            update(s => ({ ...s, selected, selectionError: null }));
        },
        selectModel: (selection: ModelSelection) => {
            const route = routeForSelection(get({ subscribe }).models, selection);
            if (!route) return;
            const state = get({ subscribe });
            persistSelection(route, state.models, state.profiles);
            update(s => ({ ...s, selected: route, selectionError: null }));
        },
        getSelected: (): string | null => {
            return get({ subscribe }).selected;
        },
        requestSelection: (route: string | null | undefined, engine: string, nativeModel = 'default'): Record<string, unknown> => {
            const state = get({ subscribe });
            if (['magician', 'pi'].includes(engine) && (state.selectionError || state.isLoading)) {
                throw new Error(state.selectionError ?? 'Model choices are still loading. Try again in a moment.');
            }
            if (['magician', 'pi'].includes(engine) && catalogScope !== scopeKey()) {
                throw new Error('Model choices need refreshing for this workspace. Reconnect and try again.');
            }
            return typedChatModelRequest(state.models, state.profiles, route ?? state.selected, engine, nativeModel);
        },
    };
}

export type ChatProfileStore = ReturnType<typeof createChatProfileStore>;

export const chatProfileStore = createChatProfileStore();
