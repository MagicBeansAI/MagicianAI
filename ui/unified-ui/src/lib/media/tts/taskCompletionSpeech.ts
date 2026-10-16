/**
 * Off-call TTS read-out of a completed task's result (Part 4).
 *
 * When a voice-started task finishes and no live voice call is running, the
 * synthesizer-authored `speech_tts` line on the terminal `task_status_update`
 * card is read aloud — IF auto-speak is unmuted. If the tab isn't focused we
 * surface a desktop notification instead and speak once the tab regains focus.
 *
 * A LIVE voice call already speaks the (terser) `speech_live` line over the
 * voice channel, so this path explicitly no-ops while a call is active to avoid
 * double-speaking.
 *
 * Invoked from the chat store's live WebSocket path
 * (`handleChatMessageReceived`) for each newly-appended message — so history
 * replay (which doesn't flow through the WS handler) never re-speaks old
 * completions, and each completed card arrives exactly once.
 */
import { get } from 'svelte/store';
import { browser } from '$app/environment';

import { isBrowserTtsAvailable, speak } from './browserTts';
import { mediaProvidersStore } from '$lib/media/providers';
import { providerSpeakBlocks } from './providerTts';
import { resolveTtsProviderChoice, type ResolvedTtsProviderChoice } from './providerChoices';
import { ttsStore, type TtsState } from './store';
import { isTutorAudioFocusActive } from './tutorAudioFocus';
import { voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';

/** Minimal shape we read off a chat message — kept local to avoid a circular
 *  import with `chatStore` (which imports this module). */
interface TaskCompletionMessageLike {
	id: string;
	content?: {
		type?: string;
		status?: string;
		task_id?: string;
		execution_id?: string;
		speech_tts?: string;
	} | null;
}

/** A live voice call is in any non-idle, non-error state (connecting through
 *  closing). While one runs it speaks `speech_live` itself, so off-call TTS
 *  stays silent. */
function isVoiceCallActive(): boolean {
	const { state } = get(voiceCallStore);
	return state !== 'idle' && state !== 'error';
}

interface CompletionTtsSelection {
	state: TtsState;
	selection: ResolvedTtsProviderChoice;
}

/** Resolve the configured off-call TTS path. Auto-speak remains opt-in and
 *  gesture-gated; the backend owns provider/model selection and browser speech
 *  is used only when backend TTS is unavailable. */
function resolveCompletionTts(): CompletionTtsSelection | null {
	if (isTutorAudioFocusActive()) return null;
	const state = get(ttsStore);
	if (!state.prefs.autoSpeak || !state.userInteracted) return null;
	const selection = resolveTtsProviderChoice(get(mediaProvidersStore), isBrowserTtsAvailable());
	if (selection.mode === 'none') return null;
	return { state, selection };
}

let pendingRefocusSpeech: { messageId: string; text: string } | null = null;
let visibilityListenerBound = false;
const spokenCompletionKeys = new Set<string>();
const MAX_SPOKEN_COMPLETION_KEYS = 512;

function claimCompletionSpeech(key: string): boolean {
	if (spokenCompletionKeys.has(key)) return false;
	spokenCompletionKeys.add(key);
	if (spokenCompletionKeys.size > MAX_SPOKEN_COMPLETION_KEYS) {
		const oldest = spokenCompletionKeys.values().next().value;
		if (typeof oldest === 'string') spokenCompletionKeys.delete(oldest);
	}
	return true;
}

/** Test seam for the module-local exactly-once cache. */
export function resetTaskCompletionSpeechDedupeForTests(): void {
	spokenCompletionKeys.clear();
	pendingRefocusSpeech = null;
}

function bindVisibilityFlush(): void {
	if (visibilityListenerBound || !browser || typeof document === 'undefined') return;
	visibilityListenerBound = true;
	document.addEventListener('visibilitychange', () => {
		if (document.hidden) return;
		const queued = pendingRefocusSpeech;
		pendingRefocusSpeech = null;
		if (!queued) return;
		// Re-check the gates at refocus time — a call may have started, or the
		// user may have muted auto-speak, since the notification fired.
		if (isVoiceCallActive()) return;
		speakTaskText(queued.messageId, queued.text);
	});
}

/** Best-effort desktop notification via the web Notification API (also surfaces
 *  natively inside the Tauri webview). No-ops without permission. */
function notifyDesktop(title: string, body: string): void {
	if (!browser || typeof Notification === 'undefined') return;
	const show = () => {
		try {
			// eslint-disable-next-line no-new
			new Notification(title, { body });
		} catch {
			/* notification construction can throw on some platforms — ignore */
		}
	};
	if (Notification.permission === 'granted') {
		show();
	} else if (Notification.permission !== 'denied') {
		Notification.requestPermission()
			.then((permission) => {
				if (permission === 'granted') show();
			})
			.catch(() => {
				/* permission prompt unavailable — ignore */
			});
	}
}

function speakTaskText(messageId: string, text: string): void {
	const resolved = resolveCompletionTts();
	if (!resolved) return;
	const { state, selection } = resolved;
	ttsStore.setActive(messageId);
	const onEnd = (status: 'completed' | 'cancelled' | 'error') => {
		if (status !== 'cancelled') {
			ttsStore.setActive(null);
		}
	};
	if (selection.mode === 'backend') {
		void providerSpeakBlocks(
			{
				messageId,
				blocks: [{ text }],
				provider: null,
				voice: null,
				model: null,
				rate: state.prefs.rate
			},
			onEnd
		);
		return;
	}
	speak(
		{
			messageId,
			text,
			voiceName: state.prefs.voiceName,
			rate: state.prefs.rate,
			pitch: state.prefs.pitch
		},
		onEnd
	);
}

/**
 * If `message` is a freshly-arrived completed task card carrying a spoken
 * summary, read it aloud (or notify + queue if the tab is hidden). Safe to call
 * for every incoming message — it filters to the relevant ones.
 */
export function maybeSpeakTaskCompletion(message: TaskCompletionMessageLike): void {
	if (!browser) return;
	const content = message.content;
	if (!content || content.type !== 'task_status_update' || content.status !== 'completed') {
		return;
	}
	const text = content.speech_tts?.trim();
	if (!text) return;
	if (isVoiceCallActive() || !resolveCompletionTts()) return;

	const messageId = `task-tts-${content.task_id ?? message.id}`;
	const completionKey = content.task_id && content.execution_id
		? `${content.task_id}\u001f${content.execution_id}`
		: message.id;
	if (!claimCompletionSpeech(completionKey)) return;
	if (typeof document !== 'undefined' && document.hidden) {
		notifyDesktop('Task complete', text);
		pendingRefocusSpeech = { messageId, text };
		bindVisibilityFlush();
		return;
	}
	speakTaskText(messageId, text);
}
