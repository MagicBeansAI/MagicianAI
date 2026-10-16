/**
 * Browser wake-word service for the Warroom ambient Orb.
 *
 * Listens continuously for the user's explicit "Hey <assistant>" invocation
 * (using the agent's configured wake spellings) with on-device STT (Vosk WASM,
 * fully local) and, on a finalized phrase-led match, fires whichever voice
 * mode is selected (Recording ⇄ Realtime). The wake phrase is config/data (not
 * a compiled keyword model), which is the only approach that supports
 * arbitrary user-chosen names — see
 * `docs/plans/2026-06-08-wake-word-in-app-voice.md`.
 *
 * Setup (one-time, see the plan doc):
 *   - `npm i vosk-browser` in `ui/unified-ui`
 *   - host a small Vosk model tarball at `static/vosk/model.tar.gz`
 *     (e.g. vosk-model-small-en-us-0.15) — overridable via `wakeModelUrlStore`.
 *
 * Desktop (Tauri) owns wake natively from device-local Orb Settings. This
 * browser engine must never configure that detector or target a chat composer.
 */
import { browser } from '$app/environment';
import { derived, get, writable } from 'svelte/store';

import { finalizedWakePhraseMatches } from './wakePhrase';
import { constrainedWakeNamesForAgent } from './voiceAddressing';

import { primaryAgent } from '$lib/stores/agentStore';
import {
	engagePushToTalk,
	isVoiceCallCaptureState,
	releasePushToTalk,
	setPushToTalkMode,
	startVoiceCall,
	voiceCallStore
} from './realtimeVoiceClient';

export type VoiceMode = 'recording' | 'realtime' | 'hands_free';
export type WakeStatus = 'off' | 'loading' | 'mic' | 'listening' | 'paused' | 'error';

// ─── Persisted preferences ──────────────────────────────────────────────────
// Wake-word-only settings remain browser-local. The Call/Dictate mode is
// backend-owned via `$lib/media/preferences` so desktop and web stay in sync.
const WAKE_ENABLED_KEY = 'magician.voice.wakeEnabled';
const WAKE_PHRASE_KEY = 'magician.voice.wakePhrase';
const WAKE_MODEL_URL_KEY = 'magician.voice.wakeModelUrl';

const DEFAULT_MODEL_URL = '/vosk/model.tar.gz';

/** Ignore repeat matches within this window (the recognizer re-emits the
 *  same partial; one spoken command must not fire twice). */
const FIRE_COOLDOWN_MS = 4000;

function readString(key: string, fallback: string): string {
	if (!browser) return fallback;
	try {
		const raw = localStorage.getItem(key);
		return raw && raw.trim() ? raw : fallback;
	} catch {
		return fallback;
	}
}

function readBool(key: string, fallback: boolean): boolean {
	if (!browser) return fallback;
	try {
		const raw = localStorage.getItem(key);
		return raw == null ? fallback : raw === 'true';
	} catch {
		return fallback;
	}
}

function persist(key: string, value: string): void {
	if (!browser) return;
	try {
		localStorage.setItem(key, value);
	} catch {
		// quota / disabled storage — non-fatal (falls back to defaults on reload)
	}
}

export const wakeEnabledStore = writable<boolean>(readBool(WAKE_ENABLED_KEY, false));
export const voiceModeStore = writable<VoiceMode>('recording');
// The desktop tray seeds its OWN push-to-talk mode from the persisted backend
// preference at startup (desktop `main.rs` + `media_rails::preferences`). The
// Tauri-webview mirror below must push to the desktop ONLY after THIS surface has
// loaded the real preference (`hydrateVoiceModePreference`). Otherwise a webview
// that never hydrates (e.g. the notify-overlay) — or the main app before its async
// preference load completes — fires the subscriber with an unhydrated compiled
// fallback. Gate every push on this flag so only backend-hydrated state reaches
// the native tray.
let voiceModeHydrated = false;
/** Optional manual override for the wake word; empty = derive from the
 *  assistant's alias (the normal case). */
export const wakePhraseStore = writable<string>(readString(WAKE_PHRASE_KEY, ''));
export const wakeModelUrlStore = writable<string>(readString(WAKE_MODEL_URL_KEY, DEFAULT_MODEL_URL));
/** Coarse status for the UI (chip color / tooltip). */
export const wakeStatusStore = writable<WakeStatus>('off');
/**
 * Assistant-name stems used to build explicit invocations: the manual override
 * if set, else every configured wake spelling. Definitions without wake
 * spellings fall back to advertised names; a missing identity fails closed with
 * no phrase. Reactive — updates as the agent definition changes.
 */
export const effectiveWakePhrasesStore = derived(
	[wakePhraseStore, primaryAgent],
	([$override, $agent]) => {
		const override = ($override ?? '').trim();
		if (override) return [override];
		return constrainedWakeNamesForAgent($agent);
	}
);

/** First phrase for compact surfaces that can display only one example. */
export const effectiveWakePhraseStore = derived(
	effectiveWakePhrasesStore,
	($phrases) => $phrases[0] ?? ''
);

if (browser) {
	wakePhraseStore.subscribe((v) => persist(WAKE_PHRASE_KEY, v));
	wakeModelUrlStore.subscribe((v) => persist(WAKE_MODEL_URL_KEY, v));
	// `wakeEnabledStore` is persisted in `setWakeEnabled()` so the toggle can
	// also start/stop the engine in the same call.
}

export function hydrateVoiceModePreference(mode: unknown): void {
	// Mark hydrated BEFORE setting the store so the resulting subscriber fire is
	// allowed to push the (now-real) value to the desktop.
	voiceModeHydrated = true;
	voiceModeStore.set(
		mode === 'hands_free' ? 'hands_free' : mode === 'realtime' ? 'realtime' : 'recording'
	);
}

// ─── Recording triggers ─────────────────────────────────────────────────────
type RecordingTrigger = () => boolean | Promise<boolean>;
/** Browser push-to-talk target, injected by the active composer. */
let recordingTrigger: RecordingTrigger | null = null;
/** Wake target, injected only by an ambient Orb surface. Keeping this separate
 * makes it structurally impossible for accepted wake audio to open a composer. */
let wakeRecordingTrigger: RecordingTrigger | null = null;

/** The composer registers how browser push-to-talk starts Dictation. */
export function registerRecordingTrigger(fn: RecordingTrigger | null): () => void {
	recordingTrigger = fn;
	return () => {
		if (recordingTrigger === fn) recordingTrigger = null;
	};
}

/** An ambient Orb registers how a browser wake phrase starts its Dictation
 * rail. A composer deliberately has no access to this registration path. */
export function registerWakeRecordingTrigger(fn: RecordingTrigger | null): () => void {
	wakeRecordingTrigger = fn;
	return () => {
		if (wakeRecordingTrigger === fn) wakeRecordingTrigger = null;
	};
}

/** How to STOP an in-flight composer dictation (stop → transcribe → send).
 * Browser push-to-talk needs this explicit key-release hook; ambient wake owns
 * a separate lifecycle and registration above. */
let recordingStop: (() => void | Promise<void>) | null = null;
export function registerRecordingStop(fn: (() => void | Promise<void>) | null): void {
	recordingStop = fn;
}

// ─── Engine state ───────────────────────────────────────────────────────────
type VoskModel = { KaldiRecognizer: new (rate: number) => VoskRecognizer; terminate?: () => void };
type VoskMessage = { result?: { text?: string; partial?: string } };
type VoskRecognizer = {
	id?: string;
	on: (event: 'result' | 'partialresult', cb: (m: VoskMessage) => void) => void;
	acceptWaveform: (buffer: AudioBuffer) => boolean;
	remove?: () => void;
};

let model: VoskModel | null = null;
/** In-flight model load, so concurrent ambient starts share one download. */
let modelPromise: Promise<VoskModel> | null = null;
let recognizer: VoskRecognizer | null = null;
let audioContext: AudioContext | null = null;
let source: MediaStreamAudioSourceNode | null = null;
let processor: ScriptProcessorNode | null = null;
let sink: GainNode | null = null;
let micStream: MediaStream | null = null;
let lastFireAt = 0;
/** True while a turn/recording is capturing — we stop feeding the recognizer
 *  so the wake listener doesn't re-trigger on the user's own question or the
 *  assistant's reply. */
let suspended = false;
let starting = false;
let startGeneration = 0;

/** Pause feeding audio to the recognizer (call when a turn/recording begins). */
export function suspendWake(): void {
	suspended = true;
	if (get(wakeStatusStore) === 'listening') wakeStatusStore.set('paused');
}

/** Resume feeding the recognizer (call when the turn/recording ends). */
export function resumeWake(): void {
	if (suspended) {
		// The previous finalized invocation has already handed microphone
		// ownership away. A lifecycle re-arm starts a genuinely new command
		// boundary, so an immediate barge-in must not inherit its cooldown.
		lastFireAt = 0;
	}
	suspended = false;
	if (audioContext && get(wakeStatusStore) === 'paused') wakeStatusStore.set('listening');
}

/** Enable/disable hands-free wake; starts or stops the engine accordingly. */
export function setWakeEnabled(enabled: boolean): void {
	wakeEnabledStore.set(enabled);
	persist(WAKE_ENABLED_KEY, String(enabled));
	if (enabled) void startWakeWord();
	else void stopWakeWord();
}

/** Legacy Cache Storage bucket for the model tarball — superseded by
 *  vosk's own persisted-extracted-model store (see
 *  `resetLegacyModelStorage`); deleted during the one-time reset. */
const LEGACY_MODEL_CACHE_NAME = 'vosk-wake-model-v1';

/** Hard ceiling on model init: `createModel()` on a corrupt archive HANGS
 *  (never settles — verified live: an HTML page where the tarball
 *  belongs hangs the worker silently), so an un-timed await reads as
 *  "loading forever" with no recovery path. Generous: a clean profile
 *  inits in ~8s, but a busy renderer competing with dev tooling can
 *  legitimately take several times that. */
const MODEL_INIT_TIMEOUT_MS = 120_000;

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
	return new Promise<T>((resolve, reject) => {
		const t = setTimeout(() => reject(new Error(`${what} timed out after ${ms / 1000}s`)), ms);
		p.then(
			(v) => {
				clearTimeout(t);
				resolve(v);
			},
			(e) => {
				clearTimeout(t);
				reject(e);
			}
		);
	});
}

/** Emscripten IDBFS database — its name IS the worker's mount point.
 *  vosk-browser persists the EXTRACTED model here, keyed by a directory
 *  derived from the model URL, and `syncfs(true)` at mount loads the
 *  WHOLE store into worker memory before anything else runs. */
const VOSK_IDBFS_DB = '/vosk';
const WAKE_FS_VERSION_KEY = 'magician.voice.wakeFsVersion';
const WAKE_FS_VERSION = '2';

/** `indexedDB.deleteDatabase` waits forever when another connection holds
 *  the DB open (a stranded init worker does) — bound it and move on; a
 *  blocked deletion completes once the holder dies. */
function deleteDatabaseBounded(name: string, ms = 5_000): Promise<void> {
	return new Promise((resolve) => {
		let settled = false;
		const finish = (): void => {
			if (!settled) {
				settled = true;
				resolve();
			}
		};
		try {
			const req = indexedDB.deleteDatabase(name);
			req.onsuccess = finish;
			req.onerror = finish;
			req.onblocked = finish;
		} catch {
			finish();
		}
		setTimeout(finish, ms);
	});
}

/** One-time storage reset (v2). The old loader fed `createModel` a
 *  PER-PAGE-LOAD blob URL, and vosk keys its persisted filesystem by a
 *  directory derived from that URL — so every page load deposited
 *  another full extracted model copy (~100 MB) into the `/vosk`
 *  IndexedDB. The mount-time `syncfs` loads ALL copies into worker
 *  memory, so init time and memory grew without bound (observed live:
 *  gigabytes resident, >120s init, reads as "loading forever"). Nuke
 *  the store once; the stable URL key keeps it single-copy from here
 *  on. Also drops the legacy Cache Storage tarball — vosk's own
 *  extracted-model persistence replaces it (and skips the untar). */
async function resetLegacyModelStorage(): Promise<void> {
	try {
		if (localStorage.getItem(WAKE_FS_VERSION_KEY) === WAKE_FS_VERSION) return;
	} catch {
		/* storage unavailable — still attempt the cleanup below */
	}
	console.info('[wake] one-time model-storage reset (clearing accumulated per-load model copies)');
	await deleteDatabaseBounded(VOSK_IDBFS_DB);
	try {
		if (typeof caches !== 'undefined') await caches.delete(LEGACY_MODEL_CACHE_NAME);
	} catch {
		/* ignore */
	}
	try {
		localStorage.setItem(WAKE_FS_VERSION_KEY, WAKE_FS_VERSION);
	} catch {
		/* ignore */
	}
}

/** One-shot environment probe logged before the first init. vosk needs
 *  BOTH blob-URL workers and WebAssembly; a browser/extension/policy
 *  denying either makes `createModel()` hang silently (the worker dies
 *  with an unhandled error the main thread never hears). When init
 *  times out in the field, this line says which primitive to blame. */
let envProbed = false;
async function probeWakeEnv(): Promise<void> {
	if (envProbed) return;
	envProbed = true;
	try {
		const okWasm = await WebAssembly.compile(
			new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])
		).then(
			() => true,
			() => false
		);
		const okWorker = await new Promise<boolean>((resolve) => {
			try {
				const u = URL.createObjectURL(
					new Blob(['postMessage(1)'], { type: 'application/javascript' })
				);
				const w = new Worker(u);
				const t = setTimeout(() => {
					w.terminate();
					URL.revokeObjectURL(u);
					resolve(false);
				}, 3000);
				w.onmessage = () => {
					clearTimeout(t);
					w.terminate();
					URL.revokeObjectURL(u);
					resolve(true);
				};
				w.onerror = () => {
					clearTimeout(t);
					w.terminate();
					URL.revokeObjectURL(u);
					resolve(false);
				};
			} catch {
				resolve(false);
			}
		});
		console.info(
			`[wake] env probe: wasm=${okWasm ? 'ok' : 'BLOCKED'} blobWorker=${okWorker ? 'ok' : 'BLOCKED'}`
		);
	} catch (err) {
		console.warn('[wake] env probe failed', err);
	}
}

/** Download + initialize the Vosk model once; subsequent calls reuse it. */
function loadModel(): Promise<VoskModel> {
	if (model) return Promise.resolve(model);
	if (!modelPromise) {
		modelPromise = (async () => {
			await probeWakeEnv();
			await resetLegacyModelStorage();
			const { createModel } = await import('vosk-browser');
			// The DIRECT, STABLE URL — never a blob URL. vosk keys its
			// persisted extracted-model directory by this string, so a
			// stable key means ONE copy ever, and repeat inits skip both
			// the download and the untar (`extracted.ok` check in the
			// worker). A per-load blob URL here is what bloated the
			// store unboundedly (see resetLegacyModelStorage).
			const directUrl = get(wakeModelUrlStore);
			const t0 = performance.now();
			try {
				const m = (await withTimeout(
					createModel(directUrl),
					MODEL_INIT_TIMEOUT_MS,
					'wake model init'
				)) as unknown as VoskModel;
				model = m;
				console.info(`[wake] model ready in ${Math.round(performance.now() - t0) / 1000}s`);
				return m;
			} catch (err) {
				console.error(
					`[wake] model init failed (url: ${directUrl} — check the file exists and the persisted override in localStorage 'magician.voice.wakeModelUrl'):`,
					err
				);
				throw err;
			}
		})().catch((err) => {
			modelPromise = null; // allow a retry on the next attempt
			throw err;
		});
	}
	return modelPromise;
}

export async function startWakeWord(): Promise<void> {
	if (!browser || audioContext || starting) return;
	// In the desktop app the native Rust listener (Vosk) owns wake detection — it
	// keeps working when the window is backgrounded, where this in-page listener
	// would be throttled/suspended. Desktop wake is configured and rendered by
	// the Orb, so this browser service neither mirrors nor claims its status.
	if ('__TAURI_INTERNALS__' in window) {
		wakeStatusStore.set('off');
		return;
	}
	const generation = ++startGeneration;
	starting = true;
	if (!model) wakeStatusStore.set('loading');
	try {
		// Memoized for the lifetime of the browser ambient surface; only the mic
		// and audio graph are set up per start.
		const ready = await loadModel();
		if (generation !== startGeneration) return;

		// Distinct status while we wait on the PERMISSION PROMPT: if the user
		// never answers it (or the browser quiet-UI swallows it), getUserMedia
		// stays pending FOREVER — without this the UI reads "loading" with no
		// way to tell model work from a prompt nobody noticed.
		wakeStatusStore.set('mic');
		const stream = await navigator.mediaDevices.getUserMedia({
			audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }
		});
		if (generation !== startGeneration) {
			stream.getTracks().forEach((track) => track.stop());
			return;
		}
		micStream = stream;

		audioContext = new AudioContext();
		recognizer = new ready.KaldiRecognizer(audioContext.sampleRate);
		recognizer.on('result', (m) => handleText(m?.result?.text, true));
		// Partials are useful for diagnostics and UI previews, never authority to
		// open a microphone. A closed-vocabulary decoder can transiently project
		// its phrase while unrelated room speech is still being decoded.
		recognizer.on('partialresult', () => {});

		source = audioContext.createMediaStreamSource(micStream);
		// ScriptProcessor is deprecated but universally supported and far simpler
		// than hosting an AudioWorklet; its `inputBuffer` is an AudioBuffer that
		// vosk's `acceptWaveform` takes directly. Routed through a muted gain node
		// so the mic is never echoed to the speakers.
		processor = audioContext.createScriptProcessor(4096, 1, 1);
		processor.onaudioprocess = (e) => {
			if (suspended || !recognizer) return;
			try {
				recognizer.acceptWaveform(e.inputBuffer);
			} catch {
				// transient resampling hiccup — drop the frame
			}
		};
		sink = audioContext.createGain();
		sink.gain.value = 0;
		source.connect(processor);
		processor.connect(sink);
		sink.connect(audioContext.destination);

		wakeStatusStore.set(suspended ? 'paused' : 'listening');
	} catch (err) {
		if (generation !== startGeneration) return;
		console.error('[wake] failed to start', err);
		wakeStatusStore.set('error');
		await stopWakeWord(true);
	} finally {
		if (generation === startGeneration) starting = false;
	}
}

export async function stopWakeWord(keepErrorStatus = false): Promise<void> {
	startGeneration += 1;
	starting = false;
	// Desktop: there's no in-page audio graph to tear down (native owns wake).
	if ('__TAURI_INTERNALS__' in window) {
		if (!keepErrorStatus) wakeStatusStore.set('off');
		return;
	}
	try {
		if (processor) processor.onaudioprocess = null;
		processor?.disconnect();
		sink?.disconnect();
		source?.disconnect();
	} catch {
		/* ignore teardown races */
	}
	try {
		micStream?.getTracks().forEach((t) => t.stop());
	} catch {
		/* ignore */
	}
	try {
		recognizer?.remove?.();
	} catch {
		/* ignore */
	}
	try {
		await audioContext?.close();
	} catch {
		/* ignore */
	}
	// Keep the loaded `model` warm so re-enabling is instant — only the mic +
	// audio graph are torn down here. Call `disposeWakeModel()` to free it.
	processor = sink = source = recognizer = micStream = audioContext = null;
	if (!keepErrorStatus) wakeStatusStore.set('off');
}

/** Fully release the warm model (frees memory). Rarely needed — wake keeps the
 *  model loaded across on/off toggles so re-enabling is instant. */
export function disposeWakeModel(): void {
	try {
		model?.terminate?.();
	} catch {
		/* ignore */
	}
	model = null;
	modelPromise = null;
}

function handleText(text: string | undefined, finalized: boolean): void {
	if (!text) return;
	const phrases = get(effectiveWakePhrasesStore);
	if (!phrases.some((phrase) => finalizedWakePhraseMatches(finalized, text, phrase))) return;
	const now = Date.now();
	if (now - lastFireAt < FIRE_COOLDOWN_MS) return;
	lastFireAt = now;
	void fireWake();
}

async function fireWake(): Promise<void> {
	const mode = get(voiceModeStore);
	let recordingOwnsWake = false;
	// Don't capture the wake utterance itself into the turn.
	suspendWake();
	try {
		if (mode === 'realtime' || mode === 'hands_free') {
			const state = get(voiceCallStore)?.state;
			const live = isVoiceCallCaptureState(state);
			// Wake starts the selected call mode. Vendor Realtime starts in PTT;
			// named Hands-free starts continuous capture with backend-owned VAD.
			if (!live) {
				setPushToTalkMode(mode === 'realtime');
				await startVoiceCall({ mode });
			}
		} else if (wakeRecordingTrigger) {
			recordingOwnsWake = await wakeRecordingTrigger();
		}
	} catch (err) {
		console.error('[wake] trigger failed', err);
	} finally {
		// Realtime: the call's own lifecycle re-enables wake when it ends (see the
		// voiceCallStore subscription below). Recording: the admitted surface calls
		// resumeWake() at its real lifecycle boundary; only a failed handoff uses a
		// timer backstop so wake cannot remain stuck paused.
		// A successfully admitted Dictation surface owns the paused detector until
		// its bounded capture finishes. The former unconditional 1.5s resume could
		// re-enable wake in the middle of a long recording, letting background
		// speech race the active turn. Only failed/unhandled activation needs the
		// backstop; successful owners resume at their actual lifecycle boundary.
		if (!recordingOwnsWake) {
			setTimeout(() => {
				if (get(voiceCallStore)?.state === 'idle') resumeWake();
			}, 1500);
		}
	}
}

// While a realtime call is live, keep the wake listener suspended (the call
// owns the mic + turns); resume when it returns to idle — but only after a
// cooldown so the tail of the just-ended conversation (the user's "bye", the
// assistant's trailing audio) doesn't immediately re-trigger wake and spin the
// call straight back up. The cooldown is cancelled if a new call starts first.
const WAKE_RESUME_COOLDOWN_MS = 2500;
let wakeResumeTimer: ReturnType<typeof setTimeout> | null = null;
if (browser) {
	let wasLive = false;
	voiceCallStore.subscribe((s) => {
		const live = isVoiceCallCaptureState(s.state);
		if (live && !wasLive) {
			if (wakeResumeTimer) {
				clearTimeout(wakeResumeTimer);
				wakeResumeTimer = null;
			}
			suspendWake();
			// Browser wake and live voice must never retain two microphone
			// streams. Release Vosk's capture graph while the call owns audio;
			// Tauri has no in-page graph, so its native wake listener stays paused.
			if (!('__TAURI_INTERNALS__' in window) && get(wakeEnabledStore)) {
				void stopWakeWord().then(() => {
					if (isVoiceCallCaptureState(get(voiceCallStore).state)) {
						wakeStatusStore.set('paused');
					}
				});
			}
		}
		if (!live && wasLive) {
			if (wakeResumeTimer) clearTimeout(wakeResumeTimer);
			wakeResumeTimer = setTimeout(() => {
				wakeResumeTimer = null;
				if (isVoiceCallCaptureState(get(voiceCallStore).state)) return;
				if (!get(wakeEnabledStore)) return;
				// Clear the recognizer feed gate before rebuilding the browser
				// graph; otherwise the new graph starts successfully but remains
				// permanently paused after the call.
				resumeWake();
				if (!('__TAURI_INTERNALS__' in window)) void startWakeWord();
			}, WAKE_RESUME_COOLDOWN_MS);
		}
		wasLive = live;
	});
}

// ─── Push-to-talk: hold Left Control + Left Option ───────────────────────────
//
// One chord, mode-aware: while held it drives a LIVE push-to-talk turn (when the
// universal switch is `realtime`) or a DICTATION take (when it is `recording`).
// Press starts/engages; release commits/stops. The mode is read once at press
// time and remembered so a mid-hold switch can't strand the release on the wrong
// path.
//
// A two-key chord (not a bare modifier) is used so it never shadows ⌥-key typing
// in the composer. On the desktop the native CGEventTap (`voice_gesture.rs`) owns
// the same chord; the in-page listener below is disabled inside the Tauri webview
// so the two never double-trigger.

let pttHeldMode: VoiceMode | null = null;

export async function pushToTalkPress(): Promise<void> {
	if (pttHeldMode) return; // already holding
	const mode = get(voiceModeStore);
	pttHeldMode = mode;
	suspendWake(); // the hold owns the mic; don't let wake re-trigger on it
	try {
		if (mode === 'realtime' || mode === 'hands_free') {
			const state = get(voiceCallStore)?.state;
			const live = isVoiceCallCaptureState(state);
			if (!live) {
				// Cold Realtime starts in PTT. Hands-free starts continuously and
				// deliberately ignores the synthetic engage/release boundary.
				setPushToTalkMode(mode === 'realtime');
				await startVoiceCall({ mode });
			}
			if (mode === 'realtime') engagePushToTalk();
		} else if (recordingTrigger) {
			await recordingTrigger();
		}
	} catch (err) {
		console.error('[ptt] press failed', err);
		pttHeldMode = null;
	}
}

export async function pushToTalkRelease(): Promise<void> {
	const mode = pttHeldMode;
	if (!mode) return; // release without a matching press
	pttHeldMode = null;
	try {
		if (mode === 'realtime') {
			releasePushToTalk();
		} else if (recordingStop) {
			await recordingStop();
		}
	} catch (err) {
		console.error('[ptt] release failed', err);
	}
}

// Ref-counted global Left-Control+Left-Option chord listener. Mounted by the chat
// composer's voice controls (so push-to-talk is live exactly where chat input
// is), and only one set of listeners is installed regardless of how many
// composers mount.
//
// Both keys must be held: it engages when the second lands and releases when
// either lifts. Engagement is deferred a short window and cancelled if a third
// (non-chord) key arrives, so a Ctrl+Option+<key> shortcut (e.g. VoiceOver)
// doesn't trip it. The delay is imperceptible for a hold (you keep holding for
// the whole utterance).
const PTT_HOLD_DELAY_MS = 120;
let pttHotkeyRefs = 0;
let pttPendingTimer: ReturnType<typeof setTimeout> | null = null;
let leftOptionDown = false;
let controlDown = false;
function clearPttPending(): void {
	if (pttPendingTimer) {
		clearTimeout(pttPendingTimer);
		pttPendingTimer = null;
	}
}
function isPttChordKey(code: string): boolean {
	// Left Control + Left Option specifically (not the right-hand keys).
	return code === 'AltLeft' || code === 'ControlLeft';
}
function onPttKeyDown(event: KeyboardEvent): void {
	const code = event.code;
	if (!isPttChordKey(code)) {
		// A third key while engagement is pending = a Ctrl+Option+<key> shortcut,
		// not a hold — cancel.
		if (pttPendingTimer) clearPttPending();
		return;
	}
	if (code === 'AltLeft') leftOptionDown = true;
	else controlDown = true;
	if (event.metaKey) {
		clearPttPending(); // Cmd in the mix → a different chord
		return;
	}
	if (!(leftOptionDown && controlDown)) return; // need both halves of the chord
	if (pttHeldMode || pttPendingTimer) return;
	pttPendingTimer = setTimeout(() => {
		pttPendingTimer = null;
		void pushToTalkPress();
	}, PTT_HOLD_DELAY_MS);
}
function onPttKeyUp(event: KeyboardEvent): void {
	const code = event.code;
	if (code === 'AltLeft') leftOptionDown = false;
	else if (code === 'ControlLeft') controlDown = false;
	else return;
	// A half of the chord lifted → the hold is broken.
	if (pttPendingTimer) {
		clearPttPending(); // released before the hold window elapsed → never engaged
		return;
	}
	void pushToTalkRelease();
}
function onPttBlur(): void {
	// Window lost focus mid-hold — the key-up may never arrive; release as a
	// safety so the mic/recorder doesn't stay stuck open.
	leftOptionDown = false;
	controlDown = false;
	clearPttPending();
	void pushToTalkRelease();
}

export function installPushToTalkHotkey(): () => void {
	if (!browser) return () => {};
	// The desktop owns this chord natively (CGEventTap in `voice_gesture.rs`), so
	// don't also run an in-page listener inside the Tauri webview.
	if ('__TAURI_INTERNALS__' in window) return () => {};
	pttHotkeyRefs += 1;
	if (pttHotkeyRefs === 1) {
		window.addEventListener('keydown', onPttKeyDown);
		window.addEventListener('keyup', onPttKeyUp);
		window.addEventListener('blur', onPttBlur);
	}
	return () => {
		pttHotkeyRefs = Math.max(0, pttHotkeyRefs - 1);
		if (pttHotkeyRefs === 0) {
			window.removeEventListener('keydown', onPttKeyDown);
			window.removeEventListener('keyup', onPttKeyUp);
			window.removeEventListener('blur', onPttBlur);
		}
	};
}

// Mirror the universal Call/Dictate switch to the desktop so its native
// Left-Control+Left-Option chord dispatches to the same mode. No-op in a plain browser.
if (browser && '__TAURI_INTERNALS__' in window) {
	voiceModeStore.subscribe((mode) => {
		// Don't clobber the desktop's own startup seed with an unhydrated default —
		// only mirror once this surface has loaded the real preference.
		if (!voiceModeHydrated) return;
		void (async () => {
			try {
				const { invoke } = await import('@tauri-apps/api/core');
				await invoke('set_ptt_mode', { mode });
			} catch {
				// desktop bridge unavailable (web-only build / command missing) — ignore
			}
		})();
	});
}
