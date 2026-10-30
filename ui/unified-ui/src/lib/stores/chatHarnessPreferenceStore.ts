import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';
import { chatProfileStore, type ChatProfileStore } from './chatProfileStore';
import { scopeIdentityStore } from './scopeIdentityStore';
import { typedChatModelRequest } from '$lib/llm/chatModelSelection';

export interface ChatHarnessPreference {
	engine: string;
	model: string;
}

/** The slice of the engine roster (`GET /plane/engines`) this store reads. */
export interface ChatHarnessRoster {
	engines: { name: string; installed: boolean; models?: string[] }[];
	chat_current: string;
	chat_model?: string;
	/** A stand-in roster from a failed request: never reconcile against it. */
	unavailable?: boolean;
}

const STORAGE_KEY = 'magician.chat.harnessPreference';
/** The server chat engine this browser last saw. A pick made here stands until
 *  the server's engine moves off it — a later server choice is the newer one,
 *  even when this browser was closed while it was made. */
const SERVER_BASIS_KEY = 'magician.chat.harnessServerBasis';
const DEFAULT_CHOICE: ChatHarnessPreference = { engine: 'magician', model: 'default' };

function parseChoice(raw: string | null): ChatHarnessPreference | null {
	try {
		const parsed = JSON.parse(raw ?? 'null');
		if (typeof parsed?.engine === 'string' && parsed.engine && typeof parsed?.model === 'string' && parsed.model) {
			return { engine: parsed.engine, model: parsed.model };
		}
	} catch { /* A stale or unavailable local setting reads as absent. */ }
	return null;
}

function readStored(key: string): ChatHarnessPreference | null {
	if (!browser) return null;
	try {
		return parseChoice(localStorage.getItem(key));
	} catch {
		return null;
	}
}

function writeStored(key: string, choice: ChatHarnessPreference): void {
	if (!browser) return;
	try { localStorage.setItem(key, JSON.stringify(choice)); }
	catch { /* The current page still keeps the choice. */ }
}

function sameChoice(a: ChatHarnessPreference | null, b: ChatHarnessPreference): boolean {
	return !!a && a.engine === b.engine && a.model === b.model;
}

export function createChatHarnessPreferenceStore() {
	let sessionKey: string | null = null;
	const preferenceKey = () => sessionKey ? `${STORAGE_KEY}.${sessionKey}` : STORAGE_KEY;
	const basisKey = () => sessionKey ? `${SERVER_BASIS_KEY}.${sessionKey}` : SERVER_BASIS_KEY;
	const { subscribe, set } = writable<ChatHarnessPreference>(readStored(STORAGE_KEY) ?? DEFAULT_CHOICE);
	let chosen = false;
	// A chat with no pick of its own shows (and sends) the browser-wide pick.
	// That inherited pick counts as chosen — reconciling must not overwrite it
	// with the server engine — but it keeps following the browser default.
	let inheritsDefault = false;
	if (browser) {
		try { chosen = localStorage.getItem(STORAGE_KEY) !== null; }
		catch { /* Keep the preference for this page. */ }
	}
	let removeStorageListener = () => {};
	if (browser) {
		const storageListener = (event: StorageEvent) => {
			if (event.key === preferenceKey()) {
				chosen = event.newValue !== null;
				inheritsDefault = false;
				set(parseChoice(event.newValue) ?? DEFAULT_CHOICE);
			} else if (inheritsDefault && event.key === STORAGE_KEY) {
				followDefault(parseChoice(event.newValue));
			}
		};
        window.addEventListener('storage', storageListener);
        removeStorageListener = () => window.removeEventListener('storage', storageListener);
	}
	/** Show the browser-wide pick in a chat without a pick of its own. */
	const followDefault = (choice: ChatHarnessPreference | null) => {
		chosen = !!choice;
		set(choice ?? DEFAULT_CHOICE);
	};
	const select = (engine: string, model = 'default') => {
		chosen = true;
		inheritsDefault = false;
		const choice = { engine, model };
		set(choice);
		writeStored(preferenceKey(), choice);
	};
	/** Adopt the server's chat engine as this browser's choice and remember it
	 *  as the basis later local picks are made against. */
	const adoptServer = (engine: string, model = 'default') => {
		const server = { engine, model };
		writeStored(basisKey(), server);
		select(server.engine, server.model);
	};
	return {
		subscribe,
        destroy: () => removeStorageListener(),
		select,
		adoptServer,
		updateDefault: (engine: string, model = 'default') => {
			writeStored(STORAGE_KEY, { engine, model });
			if (!sessionKey) adoptServer(engine, model);
			else if (!chosen || inheritsDefault) {
				inheritsDefault = true;
				followDefault({ engine, model });
			}
		},
		/** The browser-wide pick changed in this page (`/engine` for this
		 *  browser): a chat that inherits it shows the new one. */
		defaultChanged: (choice: ChatHarnessPreference) => {
			if (!sessionKey) followDefault(choice);
			else if (!chosen || inheritsDefault) {
				inheritsDefault = true;
				followDefault(choice);
			}
		},
		/** A composer choice belongs to its scoped chat. Defaults remain available
		 * for new chats; changing one session never rewrites another's choice. */
		activateSession: (key: string | null) => {
			if (sessionKey === key) return;
			sessionKey = key;
			const saved = readStored(preferenceKey());
			const inherited = sessionKey && !saved ? readStored(STORAGE_KEY) : null;
			// The composer shows the browser-wide pick for a chat without its
			// own, so that pick is the one sent — and not replaced by the
			// server engine on the next roster reconcile.
			inheritsDefault = !!sessionKey && !saved;
			chosen = !!(saved ?? inherited);
			set(saved ?? inherited ?? DEFAULT_CHOICE);
		},
		/** Whether this composer has a pick to send: the chat's own, or the
		 *  browser-wide one it inherits. Without either it sends none, so the
		 *  server's default applies. */
		isChosen: () => chosen,
		/** Reconcile with the roster: follow the server when this browser has no
		 *  pick or the server's engine changed since the pick was made, then fall
		 *  back to what is installed here. Every surface calls this — the app
		 *  shell at start, the chat panel, Settings — so no surface sends an
		 *  engine this machine cannot run. */
		reconcileWithRoster: (roster: ChatHarnessRoster) => {
			// A failed roster request says nothing about the server's engine or
			// what is installed: keep the pick until a real answer arrives.
			if (roster.unavailable) return;
			const server = { engine: roster.chat_current, model: roster.chat_model ?? 'default' };
			const basis = readStored(basisKey());
			if (inheritsDefault) {
				// A chat without its own pick tracks the browser default (which
				// the app shell reconciles under the unscoped basis); it is never
				// pinned to the server engine here. With no browser pick either,
				// it shows the server engine and sends none.
				const current = readStored(STORAGE_KEY);
				if (current) followDefault(current);
				else set(server);
			} else if (!chosen || (!sessionKey && basis && !sameChoice(basis, server))) {
				adoptServer(server.engine, server.model);
			} else if (!basis) {
				// A pick from before the basis existed: keep it, and start
				// tracking the server from here.
				writeStored(basisKey(), server);
			}
			const selected = get({ subscribe });
			const available = roster.engines.find(
				(engine) => engine.name === selected.engine && engine.installed
			);
			if (!available && selected.engine !== 'magician') {
				select('magician');
			} else if (
				available
				&& selected.model !== 'default'
				&& !available.models?.includes(selected.model)
			) {
				select(selected.engine);
			}
		}
	};
}

export type ChatHarnessPreferenceStore = ReturnType<typeof createChatHarnessPreferenceStore>;

export const chatHarnessPreferenceStore = createChatHarnessPreferenceStore();

/** A mounted chat composer's own (per-chat) stores. */
export interface ChatComposerStores {
	harness: ChatHarnessPreferenceStore;
	profiles: ChatProfileStore;
}
interface OpenComposer extends ChatComposerStores {
	sessionId: string | null;
}
const openComposers: OpenComposer[] = [];
const sharedComposer: ChatComposerStores = { harness: chatHarnessPreferenceStore, profiles: chatProfileStore };

/** A chat panel registers its private stores so the paths that do not hold
 *  them — sends from the chat bubble, command palette or war room, `/engine`,
 *  and `chat.engine.updated` — reach the composer the person is looking at.
 *  `setSession` follows the chat the panel shows. */
export function registerChatComposer(stores: ChatComposerStores): {
	setSession: (sessionId: string | null) => void;
	unregister: () => void;
} {
	const entry: OpenComposer = { ...stores, sessionId: null };
	openComposers.push(entry);
	return {
		setSession: (sessionId) => { entry.sessionId = sessionId; },
		unregister: () => {
			const index = openComposers.indexOf(entry);
			if (index >= 0) openComposers.splice(index, 1);
		}
	};
}

/** The stores a send into `sessionId` should read: the most recently mounted
 *  panel showing that chat, otherwise the browser-wide stores. */
export function chatComposerFor(sessionId: string | null | undefined): ChatComposerStores {
	if (sessionId) {
		for (let index = openComposers.length - 1; index >= 0; index -= 1) {
			if (openComposers[index].sessionId === sessionId) return openComposers[index];
		}
	}
	return sharedComposer;
}

/** The browser-wide pick changed in this page: every open composer whose
 *  chat inherits it shows the new one; chats with their own pick keep it. */
export function notifyChatHarnessDefaultChanged(choice: ChatHarnessPreference): void {
	for (const composer of openComposers) composer.harness.defaultChanged(choice);
}

type ComposerChoiceReader = () => Record<string, unknown> | null;
const composers = new Map<string, ComposerChoiceReader>();
function composerKey(thread: string): string {
    const scope = get(scopeIdentityStore);
    return JSON.stringify([scope.principal, scope.workspace, thread]);
}
/** Voice takes the initiating composer's choice once, without sharing mutable
 * per-chat stores. An older unmount cannot unregister a replacement composer. */
export function registerChatComposerChoice(thread: string, reader: ComposerChoiceReader): () => void {
    const key = composerKey(thread);
    composers.set(key, reader);
    return () => { if (composers.get(key) === reader) composers.delete(key); };
}

/** Voice and typed turns preserve the same engine/model/thinking choice. */
export function composerChatChoice(profile?: string | null, thread?: string): Record<string, unknown> | null {
    const reader = thread ? composers.get(composerKey(thread)) : undefined;
    if (reader) return reader();
    if (!chatHarnessPreferenceStore.isChosen()) return null;
    const { engine, model } = get(chatHarnessPreferenceStore);
    const state = get(chatProfileStore);
    if (['magician', 'pi'].includes(engine) && (state.isLoading || state.selectionError)) {
        throw new Error(state.selectionError ?? 'Model choices are still loading.');
    }
    return typedChatModelRequest(state.models, state.profiles, profile, engine, model);
}

/** `chat.engine.updated`: the server's chat engine changed (Settings' "default
 *  for all clients", a config reload). The newest explicit choice wins, so every
 *  open composer adopts it; a later pick in one browser overrides it there. */
export function handleChatEngineUpdatedEnvelope(envelope: {
	event_type: string;
	payload: unknown;
}): void {
	if (envelope.event_type !== 'chat.engine.updated') return;
	const payload =
		envelope.payload && typeof envelope.payload === 'object'
			? (envelope.payload as Record<string, unknown>)
			: null;
	const engine = typeof payload?.chat_current === 'string' ? payload.chat_current.trim() : '';
	if (!engine) return;
	const model =
		typeof payload?.chat_model === 'string' && payload.chat_model.trim()
			? payload.chat_model.trim()
			: 'default';
	// Global Settings updates change the default; existing session picks stay
	// pinned, and open chats that inherit the default follow it.
	chatHarnessPreferenceStore.updateDefault(engine, model);
	for (const composer of openComposers) composer.harness.updateDefault(engine, model);
}
