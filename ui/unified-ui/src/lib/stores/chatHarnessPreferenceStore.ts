import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

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

function createChatHarnessPreferenceStore() {
	const { subscribe, set } = writable<ChatHarnessPreference>(readStored(STORAGE_KEY) ?? DEFAULT_CHOICE);
	let chosen = false;
	if (browser) {
		try { chosen = localStorage.getItem(STORAGE_KEY) !== null; }
		catch { /* Keep the preference for this page. */ }
	}
	if (browser) {
		window.addEventListener('storage', (event) => {
			if (event.key === STORAGE_KEY) {
				chosen = event.newValue !== null;
				set(parseChoice(event.newValue) ?? DEFAULT_CHOICE);
			}
		});
	}
	const select = (engine: string, model = 'default') => {
		chosen = true;
		const choice = { engine, model };
		set(choice);
		writeStored(STORAGE_KEY, choice);
	};
	/** Adopt the server's chat engine as this browser's choice and remember it
	 *  as the basis later local picks are made against. */
	const adoptServer = (engine: string, model = 'default') => {
		const server = { engine, model };
		writeStored(SERVER_BASIS_KEY, server);
		select(server.engine, server.model);
	};
	return {
		subscribe,
		select,
		adoptServer,
		/** Whether this browser holds a choice of its own. A browser without one
		 *  sends none, so the server's default applies. */
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
			const basis = readStored(SERVER_BASIS_KEY);
			if (!chosen || (basis && !sameChoice(basis, server))) {
				adoptServer(server.engine, server.model);
			} else if (!basis) {
				// A pick from before the basis existed: keep it, and start
				// tracking the server from here.
				writeStored(SERVER_BASIS_KEY, server);
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

export const chatHarnessPreferenceStore = createChatHarnessPreferenceStore();

/** The composer's chat choice as this client sends it with every turn it
 *  starts — typed, from the command palette, or spoken on a voice call — so
 *  one client never thinks with two engines. `null` when this browser has no
 *  choice of its own: it then sends none and the server's default applies.
 *  `profile` rides only for the engines that read a Magician profile. */
export function composerChatChoice(profile?: string | null): {
	engine: string;
	model: string;
	profile?: string;
} | null {
	if (!chatHarnessPreferenceStore.isChosen()) return null;
	const { engine, model } = get(chatHarnessPreferenceStore);
	return {
		engine,
		model,
		...(profile && (engine === 'magician' || engine === 'pi') ? { profile } : {})
	};
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
	chatHarnessPreferenceStore.adoptServer(engine, model);
}
