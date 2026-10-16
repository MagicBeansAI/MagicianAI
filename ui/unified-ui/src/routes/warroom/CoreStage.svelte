<script lang="ts">
	/**
	 * CORE STAGE — VOICE. The centre of the room is the voice channel and
	 * nothing else, and THE ORB IS THE CONTROL.
	 *
	 * No buttons. Jarvis does not have buttons:
	 *   - tap the core          → open / end the channel
	 *   - hold the core / SPACE → transmit (in PTT mode)
	 *   - the whisper line under the readout carries the affordance in words
	 *
	 * Live transcription plays centre-stage under the orb, colour-coded by
	 * speaker (green = you, accent = agent), then decays — the words' permanent
	 * home is the text rail on the left, so the stage never accumulates.
	 *
	 * WHY `ensureMediaSessionStarted` IS CALLED HERE: `startVoiceCall` errors
	 * out unless a realtime media session is already registered, and the chat
	 * page registers one in its own bootstrap. This surface never did — which
	 * is exactly why OPEN CHANNEL used to fail on the deck while working in
	 * the main app.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { get } from 'svelte/store';

	import StageGraph from './StageGraph.svelte';
	import VoiceOrbCanvas from './VoiceOrbCanvas.svelte';
	import type { RunGraphState } from './runGraph';
	import { ensureMediaSessionStarted } from '$lib/media/session';
	import { startMicRecording, makeRecordedAudioFile } from '$lib/media/capture/mic';
	import type { RecordedAudio, RecordingHandle } from '$lib/media/capture/mic';
	import { transcribeAudioStreaming } from '$lib/media/stt/sttClient';
	import { runAmbientDictationTurns } from '$lib/media/ambientDictationLoop';
	import {
		createAmbientActivationGate,
		type AmbientActivationGate
	} from '$lib/media/ambientActivationGate';
	import {
		AMBIENT_DICTATION_NO_SPEECH_MS,
		AMBIENT_DICTATION_POLL_MS,
		advanceAmbientDictationGate,
		loadAmbientVoiceMode,
		persistAmbientVoiceMode,
		seedAmbientVoiceMode,
		type AmbientVoiceMode
	} from '$lib/media/ambientVoiceMode';
	import { mediaPreferencesStore, refreshMediaPreferences } from '$lib/media/preferences';
	import { mediaProvidersStore } from '$lib/media/providers';
	import {
		cancelCurrent as cancelBrowserTts,
		isBrowserTtsAvailable,
		speak as speakBrowserTts
	} from '$lib/media/tts/browserTts';
	import {
		cancelProviderSpeak,
		primeProviderTtsPlayback,
		providerSpeakBlocks
	} from '$lib/media/tts/providerTts';
	import { resolveTtsProviderChoice } from '$lib/media/tts/providerChoices';
	import { resolveSpeechBlocks, type SpeechBlock } from '$lib/media/tts/speechTags';
	import { ttsStore } from '$lib/media/tts/store';
	import {
		engagePushToTalk,
		isVoiceCallCaptureState,
		pushToTalkActive,
		pushToTalkMode,
		releasePushToTalk,
		setPushToTalkMode,
		startVoiceCall,
		stopVoiceCall,
		voiceCallStore,
		voiceMicAnalyser,
		voiceTranscriptStore
	} from '$lib/media/voice/realtimeVoiceClient';
	import {
		effectiveWakePhraseStore,
		registerWakeRecordingTrigger,
		resumeWake,
		startWakeWord,
		suspendWake,
		voiceModeStore,
		wakeEnabledStore
	} from '$lib/media/voice/wakeWord';
	import { explicitWakePhrase } from '$lib/media/voice/wakePhrase';
	import { deriveVoiceStage, stageLabel } from './voiceViz';

	export let breathePeriod = 6;
	export let lastEventAt = 0;
	/** Thread the voice channel binds to, so speech lands in this conversation. */
	export let threadId: string | null = null;
	/** The deck's living execution tree, painted behind the stage. */
	export let graph: RunGraphState | null = null;
	export let graphFocusTaskId: string | null = null;
	export let graphFront = false;
	export let onGraphFront: (front: boolean) => void = () => {};

	/** Device-local mode for the next Ambient Orb conversation. The backend
	 *  seeds it once; this browser owns every subsequent selection. */
	let mode: AmbientVoiceMode = browser
		? loadAmbientVoiceMode(window.localStorage)
		: 'hands_free';

	type AmbientDictationReply = {
		id: string;
		text: string;
		speechSegments?: SpeechBlock[];
	};

	/** The parent owns the thread/chat ledger. The Orb owns explicit turn
	 *  admission, bounded capture, response playback, and cancellation. */
	export let onDictationSend: (
		threadId: string,
		text: string
	) => Promise<AmbientDictationReply | null> =
		async () => null;

	$: call = $voiceCallStore;
	$: transcript = $voiceTranscriptStore;
	$: live = call.state === 'connected' || isVoiceCallCaptureState(call.state);
	$: busy = call.state === 'connecting' || call.state === 'closing';
	$: voiceStage = deriveVoiceStage({
		callState: call.state,
		error: call.error,
		userSpeaking: transcript.userSpeaking,
		assistantSpeaking: transcript.assistantSpeaking
	});

	// ── centre-stage transcription ───────────────────────────────────────
	// The newest turn, partial or just-finished. A finished turn fades out
	// (CSS, keyed per turn id) because its permanent home is the left rail —
	// the stage shows the words being said, not a second transcript.
	$: liveTurn = transcript.turns.length > 0 ? transcript.turns[transcript.turns.length - 1] : null;

	// ── Ambient Dictation ────────────────────────────────────────────────
	// This is the same conversation contract as iOS: explicit wake/tap → bounded
	// capture → STT → agent → TTS → wake-only arming. One iterative loop owns
	// every turn, so a long conversation does not grow the JS stack or overlap
	// microphones.
	const DICTATION_TURN_DEADLINE_MS = 180_000;
	const DICTATION_STT_DEADLINE_MS = 180_000;
	const DICTATION_PLAYBACK_DEADLINE_MS = 180_000;
	let recorder: RecordingHandle | null = null;
	let dictationActive = false;
	let dictationGeneration = 0;
	let dictating = false;
	let transcribing = false;
	let waitingForReply = false;
	let speakingReply = false;
	let awaitingActivation = false;
	let dictError: string | null = null;
	let streamText = '';
	let assistantText = '';
	let transcribeAbort: AbortController | null = null;
	let componentAlive = false;
	let activationGate: AmbientActivationGate | null = null;
	let unregisterWakeTrigger: (() => void) | null = null;

	$: wakeInvocation = explicitWakePhrase($effectiveWakePhraseStore);

	function delay(ms: number): Promise<void> {
		return new Promise((resolve) => setTimeout(resolve, ms));
	}

	function selectMode(next: AmbientVoiceMode): void {
		if (next !== mode) {
			if (dictationActive) cancelDictation();
			mode = next;
		}
		// Clicking the already-rendered default is still an explicit local
		// choice and must win a racing first backend seed.
		persistAmbientVoiceMode(browser ? window.localStorage : null, next);
		voiceModeStore.set(next === 'dictation' ? 'recording' : next);
	}

	function cancelDictation(): void {
		const cancelOwnedSpeech = speakingReply;
		dictationGeneration += 1;
		dictationActive = false;
		activationGate?.cancel();
		activationGate = null;
		if (recorder) {
			recorder.cancel();
			recorder = null;
		}
		transcribeAbort?.abort();
		transcribeAbort = null;
		if (cancelOwnedSpeech) {
			cancelBrowserTts('user');
			cancelProviderSpeak('user');
			ttsStore.setActive(null);
		}
		dictating = false;
		transcribing = false;
		waitingForReply = false;
		speakingReply = false;
		awaitingActivation = false;
		voiceMicAnalyser.set(null);
		streamText = '';
		assistantText = '';
		resumeWake();
	}

	async function waitForDictationActivation(
		gate: AmbientActivationGate,
		generation: number
	): Promise<boolean> {
		if (!dictationActive || generation !== dictationGeneration) return false;
		awaitingActivation = true;
		// Between bounded turns only the explicit wake detector owns the mic.
		// Ordinary room speech cannot reach STT or the agent from this state.
		resumeWake();
		const admitted = await gate.wait();
		if (generation === dictationGeneration) awaitingActivation = false;
		if (!admitted || !dictationActive || generation !== dictationGeneration) return false;
		// The admitted turn owns capture now; do not let its request re-trigger
		// the detector while the bounded recorder is open.
		suspendWake();
		return true;
	}

	/** Wake phrase or explicit tap admission. During TTS this is barge-in: stop
	 *  the reply, preserve the conversation, then consume the already-granted
	 *  permit as the next bounded user turn. */
	function admitDictationTurn(): boolean {
		if (mode !== 'dictation' || live || busy) return false;
		if (!dictationActive) {
			beginDictation();
			return dictationActive;
		}
		activationGate?.admit();
		if (speakingReply) {
			cancelBrowserTts('user');
			cancelProviderSpeak('user');
			ttsStore.setActive(null);
		}
		return true;
	}

	function analyserRms(analyser: AnalyserNode, buffer: Float32Array<ArrayBuffer>): number {
		analyser.getFloatTimeDomainData(buffer);
		let sum = 0;
		for (const sample of buffer) sum += sample * sample;
		return buffer.length > 0 ? Math.sqrt(sum / buffer.length) : 0;
	}

	async function waitForDictationBoundary(
		handle: RecordingHandle,
		generation: number
	): Promise<'speech_complete' | 'no_speech' | 'cancelled'> {
		const startedAt = performance.now();
		const gate = { heardSpeech: false, lastSpeechAtMs: null as number | null };
		const samples = handle.analyser
			? new Float32Array(new ArrayBuffer(handle.analyser.fftSize * Float32Array.BYTES_PER_ELEMENT))
			: null;
		while (dictationActive && generation === dictationGeneration) {
			await delay(AMBIENT_DICTATION_POLL_MS);
			if (!dictationActive || generation !== dictationGeneration) return 'cancelled';
			const elapsedMs = performance.now() - startedAt;
			// A restricted browser can permit MediaRecorder but deny WebAudio.
			// Keep Dictation functional with a finite fixed turn in that case;
			// STT remains the authority on whether speech was present.
			if (!handle.analyser || !samples) {
				if (elapsedMs >= AMBIENT_DICTATION_NO_SPEECH_MS) return 'speech_complete';
				continue;
			}
			const boundary = advanceAmbientDictationGate(
				gate,
				analyserRms(handle.analyser, samples),
				elapsedMs
			);
			if (boundary) return boundary;
		}
		return 'cancelled';
	}

	async function captureDictationTurn(generation: number): Promise<RecordedAudio | null> {
		streamText = '';
		assistantText = '';
		const handle = await startMicRecording();
		if (!dictationActive || generation !== dictationGeneration) {
			handle.cancel();
			return null;
		}
		recorder = handle;
		dictating = true;
		voiceMicAnalyser.set(handle.analyser);
		const boundary = await waitForDictationBoundary(handle, generation);
		if (recorder === handle) recorder = null;
		dictating = false;
		voiceMicAnalyser.set(null);
		if (boundary === 'cancelled' || boundary === 'no_speech') {
			handle.cancel();
			return null;
		}
		return handle.stop();
	}

	async function transcribeDictationTurn(
		audio: RecordedAudio,
		generation: number
	): Promise<string | null> {
		voiceMicAnalyser.set(null);
		if (audio.blob.size < 1024) {
			return null;
		}
		transcribing = true;
		streamText = '';
		const controller = new AbortController();
		let timedOut = false;
		const deadline = setTimeout(() => {
			timedOut = true;
			controller.abort();
		}, DICTATION_STT_DEADLINE_MS);
		transcribeAbort = controller;
		try {
			// Same engine as the chat composer (streamed deltas, pinned
			// language hint) — one STT path everywhere, nothing to drift.
			const language =
				(typeof navigator !== 'undefined' && navigator.language
					? navigator.language.split('-')[0]
					: '') || 'en';
			const result = await transcribeAudioStreaming(
				{
					audio: audio.blob,
					mimeType: audio.mimeType,
					filename: makeRecordedAudioFile(audio).name,
					language,
					signal: controller.signal
				},
				{
					onDelta: (text) => {
						if (!controller.signal.aborted) streamText = text;
					}
				}
			);
			if (controller.signal.aborted || generation !== dictationGeneration) return null;
			const transcript = (result.transcript ?? '').trim();
			streamText = transcript;
			return transcript || null;
		} catch {
			if (timedOut) throw new Error('transcription timed out');
			if (!controller.signal.aborted) throw new Error('transcription failed');
			return null;
		} finally {
			clearTimeout(deadline);
			if (transcribeAbort === controller) transcribeAbort = null;
			transcribing = false;
		}
	}

	async function withTurnDeadline<T>(work: Promise<T>): Promise<T> {
		let timer: ReturnType<typeof setTimeout> | null = null;
		try {
			return await Promise.race([
				work,
				new Promise<T>((_, reject) => {
					timer = setTimeout(
						() => reject(new Error('agent reply timed out')),
						DICTATION_TURN_DEADLINE_MS
					);
				})
			]);
		} finally {
			if (timer) clearTimeout(timer);
		}
	}

	async function speakDictationReply(
		reply: AmbientDictationReply,
		generation: number
	): Promise<void> {
		const blocks = resolveSpeechBlocks(reply.speechSegments, reply.text);
		const fallbackText = blocks.map((block) => block.text).join(' ').trim();
		if (!fallbackText || generation !== dictationGeneration) return;
		let providers = get(mediaProvidersStore);
		if (!providers.resolved) providers = await mediaProvidersStore.refresh();
		if (generation !== dictationGeneration) return;
		const tts = get(ttsStore);
		const choice = resolveTtsProviderChoice(providers, isBrowserTtsAvailable());
		if (choice.mode === 'none') return;
		ttsStore.setActive(reply.id);
		await new Promise<void>((resolve) => {
			let settled = false;
			let cancellationPoll: ReturnType<typeof setInterval> | null = null;
			let playbackDeadline: ReturnType<typeof setTimeout> | null = null;
			const finish = () => {
				if (settled) return;
				settled = true;
				if (cancellationPoll) clearInterval(cancellationPoll);
				if (playbackDeadline) clearTimeout(playbackDeadline);
				if (get(ttsStore).activeMessageId === reply.id) ttsStore.setActive(null);
				resolve();
			};
			// Some SpeechSynthesis implementations do not emit `end` after a
			// cancellation. The generation fence settles on those engines, and
			// the hard deadline prevents a broken provider stream from keeping an
			// Ambient Dictation loop alive forever.
			cancellationPoll = setInterval(() => {
				if (
					generation !== dictationGeneration
					|| get(ttsStore).activeMessageId !== reply.id
				) finish();
			}, AMBIENT_DICTATION_POLL_MS);
			playbackDeadline = setTimeout(() => {
				if (get(ttsStore).activeMessageId === reply.id) {
					cancelBrowserTts('user');
					cancelProviderSpeak('user');
				}
				finish();
			}, DICTATION_PLAYBACK_DEADLINE_MS);
			if (choice.mode === 'backend') {
				void providerSpeakBlocks(
					{
						messageId: reply.id,
						blocks,
						provider: null,
						voice: null,
						model: null,
						rate: tts.prefs.rate
					},
					finish
				).catch(finish);
				return;
			}
			speakBrowserTts(
				{
					messageId: reply.id,
					text: fallbackText,
					voiceName: tts.prefs.voiceName,
					rate: tts.prefs.rate,
					pitch: tts.prefs.pitch
				},
				finish
			);
		});
	}

	async function runDictationLoop(generation: number, sessionId: string): Promise<void> {
		const gate = activationGate;
		if (!gate) return;
		try {
			const outcome = await runAmbientDictationTurns({
				sessionId,
				isActive: () => dictationActive && generation === dictationGeneration,
				waitForActivation: () => waitForDictationActivation(gate, generation),
				capture: () => captureDictationTurn(generation),
				transcribe: (audio) => transcribeDictationTurn(audio, generation),
				send: async (pinnedSessionId, text) => {
					waitingForReply = true;
					try {
						return await withTurnDeadline(onDictationSend(pinnedSessionId, text));
					} finally {
						waitingForReply = false;
					}
				},
				speak: async (reply) => {
					assistantText = reply.text.trim();
					speakingReply = true;
					// Voice barge-in remains available, but only the finalized explicit
					// wake phrase can cross this boundary and cancel the reply.
					resumeWake();
					try {
						await speakDictationReply(reply, generation);
					} finally {
						speakingReply = false;
					}
				}
			});
			if (outcome === 'reply_unavailable') {
				throw new Error('reply queued or unavailable');
			}
		} catch (error) {
			if (generation === dictationGeneration) {
				dictError = error instanceof Error ? error.message : 'Dictation failed';
			}
		} finally {
			if (generation === dictationGeneration) {
				gate.cancel();
				if (activationGate === gate) activationGate = null;
				dictationActive = false;
				dictating = false;
				transcribing = false;
				waitingForReply = false;
				speakingReply = false;
				awaitingActivation = false;
				voiceMicAnalyser.set(null);
				resumeWake();
			}
		}
	}

	function beginDictation(): void {
		if (dictationActive || live || busy) return;
		const sessionId = threadId?.trim();
		if (!sessionId) {
			dictError = 'A conversation is still loading. Try again in a moment.';
			return;
		}
		dictError = null;
		streamText = '';
		assistantText = '';
		cancelBrowserTts('replaced');
		cancelProviderSpeak('replaced');
		ttsStore.setActive(null);
		ttsStore.markUserInteracted();
		primeProviderTtsPlayback();
		dictationGeneration += 1;
		dictationActive = true;
		activationGate = createAmbientActivationGate();
		activationGate.admit();
		suspendWake();
		void runDictationLoop(dictationGeneration, sessionId);
	}

	// ── ignored-turn visibility ──────────────────────────────────────────
	// The wire contract distinguishes semantic admission rejection from silent
	// lifecycle cleanup, so removing a partial can never invent an error toast.
	$: ignoredTurn = live ? transcript.lastIgnoredTurn : null;
	$: ignoredAt = ignoredTurn?.at ?? 0;
	$: ignoredMessage = ignoredTurn?.reason === 'address_prefix_armed'
		? 'wake phrase heard — go ahead'
		: call.addressingRequired && call.activationPhrase
			? `turn ignored — start with “${call.activationPhrase}”`
			: 'turn ignored — the channel dropped it';

	// ── the orb as control ───────────────────────────────────────────────
	let holdTimer: ReturnType<typeof setTimeout> | null = null;
	let holding = false;
	let downAtMs = 0;

	async function openOrEnd(): Promise<void> {
		if (busy || mode === 'dictation') return; // Dictation owns its turn loop.
		if (live) {
			stopVoiceCall();
			return;
		}
		try {
			await ensureMediaSessionStarted({ threadId: threadId ?? null });
		} catch {
			// startVoiceCall will surface the error state; nothing to do here.
		}
		await startVoiceCall({ threadId, mode: mode === 'hands_free' ? 'hands_free' : 'realtime' });
	}

	function corePointerDown(event: PointerEvent): void {
		downAtMs = performance.now();
		(event.currentTarget as HTMLElement)?.setPointerCapture?.(event.pointerId);
		if (mode === 'dictation' && !live) return;
		if (live && $pushToTalkMode && mode === 'realtime') {
			// Hold ≥220ms = transmit. Shorter is a tap (toggle), handled on up.
			holdTimer = setTimeout(() => {
				holding = true;
				engagePushToTalk();
			}, 220);
		}
	}

	function corePointerUp(): void {
		if (holdTimer) {
			clearTimeout(holdTimer);
			holdTimer = null;
		}
		if (holding) {
			holding = false;
			releasePushToTalk();
			return; // a hold is not a tap
		}
		if (performance.now() - downAtMs >= 600) return;
		if (mode === 'dictation') {
			if (awaitingActivation || speakingReply) admitDictationTurn();
			else if (dictationActive) cancelDictation();
			else beginDictation();
			return;
		}
		void openOrEnd();
	}

	function corePointerCancel(): void {
		if (holdTimer) {
			clearTimeout(holdTimer);
			holdTimer = null;
		}
		if (holding) {
			holding = false;
			releasePushToTalk();
		}
	}

	function coreKeydown(event: KeyboardEvent): void {
		// Enter = toggle, for keyboard users. Space is the global PTT key.
		if (event.key !== 'Enter') return;
		if (mode === 'dictation') {
			if (awaitingActivation || speakingReply) admitDictationTurn();
			else if (dictationActive) cancelDictation();
			else beginDictation();
			return;
		}
		void openOrEnd();
	}

	// ── SPACE = the transmit key (classic radio) ─────────────────────────
	function isTypingTarget(target: EventTarget | null): boolean {
		const el = target as HTMLElement | null;
		if (!el) return false;
		const tag = el.tagName;
		return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
	}

	function windowKeydown(event: KeyboardEvent): void {
		if (event.code === 'Escape' && dictationActive) {
			cancelDictation();
			return;
		}
		if (event.code !== 'Space' || event.repeat) return;
		if (isTypingTarget(event.target)) return;
		if (mode === 'dictation' && !live) {
			event.preventDefault();
			if (awaitingActivation || speakingReply) admitDictationTurn();
			else if (dictationActive) cancelDictation();
			else beginDictation();
			return;
		}
		// SPACE opens the channel too: the deck's one key does the whole
		// journey — open, transmit, or start Dictation — depending on state.
		if (!live) {
			if (!busy) {
				event.preventDefault();
				void openOrEnd();
			}
			return;
		}
		if (!$pushToTalkMode) return;
		event.preventDefault();
		engagePushToTalk();
	}

	function windowKeyup(event: KeyboardEvent): void {
		if (event.code !== 'Space') return;
		if (isTypingTarget(event.target)) return;
		if (mode === 'dictation') return;
		if (!$pushToTalkMode) return;
		releasePushToTalk();
	}

	onMount(() => {
		if (!browser) return;
		componentAlive = true;
		voiceModeStore.set(mode === 'dictation' ? 'recording' : mode);
		unregisterWakeTrigger = registerWakeRecordingTrigger(async () => admitDictationTurn());
		if (get(wakeEnabledStore)) void startWakeWord();
		window.addEventListener('keydown', windowKeydown);
		window.addEventListener('keyup', windowKeyup);
		void refreshMediaPreferences().then((preferences) => {
			if (!componentAlive) return;
			if (get(mediaPreferencesStore).error) return;
			mode = seedAmbientVoiceMode(window.localStorage, preferences.voice_mode);
			voiceModeStore.set(mode === 'dictation' ? 'recording' : mode);
		});
	});

	onDestroy(() => {
		componentAlive = false;
		if (!browser) return;
		window.removeEventListener('keydown', windowKeydown);
		window.removeEventListener('keyup', windowKeyup);
		unregisterWakeTrigger?.();
		unregisterWakeTrigger = null;
		if (holdTimer) clearTimeout(holdTimer);
		if (holding && mode !== 'dictation') releasePushToTalk();
		cancelDictation();
		if (!live) voiceMicAnalyser.set(null);
	});

	// The whisper line: the affordance in words, not chrome.
	//
	// The PTT-OFF line MUST carry the activation phrase when the backend
	// gates the call: with addressing required, unprefixed speech is dropped
	// server-side as `transcript.user.ignored` — silently. An open mic that
	// ignores everything you say reads as "voice is broken" unless the deck
	// says what the channel is waiting to hear.
	$: hint = busy
		? 'opening channel…'
		: mode === 'dictation' && !live
			? dictating
				? 'listening — pause after you finish'
				: transcribing
					? 'reading it back…'
					: waitingForReply
						? 'thinking…'
						: speakingReply
							? $wakeEnabledStore && wakeInvocation
								? `speaking — say “${wakeInvocation}” or tap to interrupt`
								: 'speaking — tap to interrupt'
							: awaitingActivation
								? $wakeEnabledStore && wakeInvocation
									? `ready — say “${wakeInvocation}” or tap for the next turn`
									: 'ready — tap for the next turn'
							: dictationActive
								? 'preparing the admitted turn…'
								: 'tap the core or press SPACE to start'
			: !live
				? 'tap the core — or press SPACE — to open the channel'
				: mode === 'hands_free'
				? 'hands-free — just speak · tap the core to end'
				: $pushToTalkActive
					? 'transmitting — release to send'
					: $pushToTalkMode
						? 'hold the core or SPACE to transmit · tap to end'
						: call.addressingRequired && call.activationPhrase
							? `open mic — start with “${call.activationPhrase}” · tap to end`
							: 'open mic — just speak · tap the core to end';
</script>

<section class="stage" aria-label="Voice stage">
	{#if graph && graphFocusTaskId}
		<!-- The graph exists ONLY for a selected task: no ambient web behind
		     the idle stage. Deselecting unmounts it entirely. -->
		<StageGraph
			{graph}
			focusTaskId={graphFocusTaskId}
			front={graphFront}
			onExit={() => onGraphFront(false)}
		/>
	{/if}
	<div class="corner tl" aria-hidden="true"></div>
	<div class="corner tr" aria-hidden="true"></div>
	<div class="corner bl" aria-hidden="true"></div>
	<div class="corner br" aria-hidden="true"></div>

	<div
		class="core"
		class:core--holding={holding || $pushToTalkActive}
		style:--breathe="{breathePeriod}s"
		style:--spin="{breathePeriod * 7}s"
		role="button"
		tabindex="0"
		aria-pressed={live || dictationActive}
		aria-label={dictationActive
			? awaitingActivation
				? 'Ambient Dictation ready for your wake phrase — tap to admit the next turn'
				: speakingReply
					? 'Assistant speaking — tap to interrupt and admit the next turn'
					: 'Ambient Dictation active — tap to end'
			: live
				? 'Voice channel open — tap to end'
				: 'Tap to start the selected voice mode'}
		on:pointerdown={corePointerDown}
		on:pointerup={corePointerUp}
		on:pointercancel={corePointerCancel}
		on:keydown={coreKeydown}
	>
		<svg viewBox="0 0 240 240" class="rings" role="img" aria-label="Event-rate tick ring">
			<!-- tick ring: spin period derives from live event rate -->
			<g class="ticks">
				<circle cx="120" cy="120" r="70" />
			</g>
		</svg>
		{#if transcribing}
			<!-- Processing ring: a fast dashed sweep while the STT reads the
			     recording back. Removed from the DOM the moment it ends. -->
			<svg viewBox="0 0 240 240" class="proc" aria-hidden="true">
				<circle cx="120" cy="120" r="86" />
			</svg>
		{/if}
		<!-- The living centre: real mic amplitude, framed by the tick ring. -->
		<div class="orb-slot">
			<VoiceOrbCanvas
				overrideStage={dictating
					? 'you'
					: transcribing || waitingForReply
						? 'connecting'
						: speakingReply
							? 'agent'
							: dictationActive
								? 'listening'
								: null}
			/>
		</div>
		{#key lastEventAt}
			{#if lastEventAt > 0}<span class="flash" aria-hidden="true"></span>{/if}
		{/key}
	</div>

	<div class="core-label">
		<span
			class="voice-stage"
			data-stage={dictating
				? 'you'
				: transcribing || waitingForReply
					? 'connecting'
					: speakingReply
						? 'agent'
						: dictationActive
							? 'listening'
							: voiceStage}
			>{dictating
				? 'DICTATING'
				: transcribing
					? 'TRANSCRIBING'
					: waitingForReply
						? 'THINKING'
						: speakingReply
							? 'AGENT SPEAKING'
							: awaitingActivation
								? 'READY'
							: dictationActive
								? 'LISTENING'
								: stageLabel(voiceStage)}</span>
		{#if call.error}
			<span class="voice-sub voice-sub--bad">{call.error}</span>
		{:else if dictError}
			<span class="voice-sub voice-sub--bad">{dictError}</span>
		{/if}
	</div>

	<!-- Live transcription: the words being said, speaker-coded, decaying.
	     Reserved height so the orb never jumps when speech starts. -->
	<div class="caption" aria-live="polite">
		{#if assistantText && (waitingForReply || speakingReply || dictationActive)}
			<p class="caption-text" data-speaker="assistant">{assistantText}</p>
		{:else if transcribing && streamText}
			<p class="caption-text caption-text--stream" data-speaker="user">{streamText}</p>
		{:else if streamText && dictationActive}
			<p class="caption-text" data-speaker="user">{streamText}</p>
		{:else if live && liveTurn && liveTurn.text.trim()}
			<!-- `live` gate: the transcript store keeps turns after a call
			     ends, and without the gate the caption replayed the LAST
			     LLM TURN of an old call whenever another branch collapsed —
			     seen after cancelling a dictation send. The caption is "the
			     words being said"; a closed channel is saying nothing. -->
			{#key liveTurn.id}
				<p class="caption-text" data-speaker={liveTurn.speaker} data-done={liveTurn.done}>
					{liveTurn.text}
				</p>
			{/key}
		{/if}
	</div>

	{#if ignoredAt > 0}
		{#key ignoredAt}
			<p class="ignored" role="status">
				{ignoredMessage}
			</p>
		{/key}
	{/if}

	<div class="hint-stack">
		{#if !live && !busy && !dictationActive}
			<!-- Mode picker, in words: the active mode is lit, the others are
			     text links. The support line sits BELOW the options — it
			     describes the selected mode, so it reads as a caption, not a
			     sibling. -->
			<span class="modes" role="radiogroup" aria-label="Voice mode">
				<button
					class="whisper"
					data-on={mode === 'dictation'}
					role="radio"
					aria-checked={mode === 'dictation'}
					on:click|stopPropagation={() => selectMode('dictation')}
				>DICTATION</button>
				<span class="sep" aria-hidden="true">·</span>
				<button
					class="whisper"
					data-on={mode === 'hands_free'}
					role="radio"
					aria-checked={mode === 'hands_free'}
					on:click|stopPropagation={() => selectMode('hands_free')}
				>HANDS-FREE</button>
				<span class="sep" aria-hidden="true">·</span>
				<button
					class="whisper"
					data-on={mode === 'realtime'}
					role="radio"
					aria-checked={mode === 'realtime'}
					on:click|stopPropagation={() => selectMode('realtime')}
				>LIVE</button>
			</span>
		{/if}
		<p class="hint">
			{hint}
			{#if graphFocusTaskId && !graphFront}
			<button
				class="whisper"
				on:click|stopPropagation={() => onGraphFront(true)}
			>VIEW RUN GRAPH ⌕</button>
		{/if}
		{#if mode === 'realtime' && (live || !$pushToTalkMode)}
				<button
					class="whisper"
					on:click|stopPropagation={() => setPushToTalkMode(!$pushToTalkMode)}
					aria-pressed={$pushToTalkMode}
				>PTT {$pushToTalkMode ? 'ON' : 'OFF'}</button>
			{/if}
		</p>
	</div>
</section>

<style>
	.stage {
		grid-area: stage;
		/* Above the deck's scanline film — see `.scan` in +page.svelte. */
		z-index: 1;
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 14px;
		padding: 18px 26px 14px;
		min-height: 0;
		min-width: 0;
	}

	/* HUD corner brackets */
	.corner {
		position: absolute;
		width: 26px;
		height: 26px;
		border: 1px solid color-mix(in srgb, var(--deck-glow) 55%, transparent);
	}
	.corner.tl { top: 14px; left: 16px; border-right: none; border-bottom: none; }
	.corner.tr { top: 14px; right: 16px; border-left: none; border-bottom: none; }
	.corner.bl { bottom: 12px; left: 16px; border-right: none; border-top: none; }
	.corner.br { bottom: 12px; right: 16px; border-left: none; border-top: none; }

	/* Voice is the deck's primary function, so the assembly is sized to lead
	   the composition. It is also the CONTROL: cursor and focus ring say so. */
	.core {
		position: relative;
		width: 320px;
		height: 320px;
		flex: none;
		cursor: pointer;
		border-radius: 50%;
		touch-action: none;
		-webkit-tap-highlight-color: transparent;
	}
	.core:focus-visible {
		outline: 1px solid var(--deck-glow);
		outline-offset: 8px;
	}
	.core--holding {
		/* Transmitting: the whole assembly leans in. Cheap (transform only). */
		transform: scale(1.02);
	}

	.rings { position: absolute; inset: 0; width: 100%; height: 100%; }

	.ticks circle {
		fill: none;
		stroke: color-mix(in srgb, var(--deck-glow) 35%, transparent);
		stroke-width: 6;
		stroke-dasharray: 1 10;
	}
	/* The tick ring spins, and the PERIOD IS A READOUT: `--spin` derives from
	   the live event-rate EMA (idle ~42s per revolution, saturated ~12.6s), so
	   the ring visibly quickens when the system gets busy. That satisfies the
	   deck's law — it is an instrument, not decoration.

	   `will-change: transform` promotes the group to its own compositor layer
	   so each frame composites rather than re-rasterising the dashed stroke.
	   Reduced motion is handled centrally in `+page.svelte`, which clears every
	   animation inside `.deck`. */
	.ticks {
		transform-origin: 120px 120px;
		will-change: transform;
		animation: tick-spin var(--spin, 42s) linear infinite;
	}
	@keyframes tick-spin {
		to { transform: rotate(360deg); }
	}

	/* The orb occupies the ring assembly's inner void (r=70 of a 240 box), so
	   the tick ring frames it rather than collides with it. */
	.orb-slot {
		position: absolute;
		left: 50%;
		top: 50%;
		width: 56%;
		height: 56%;
		transform: translate(-50%, -50%);
		pointer-events: none;
	}

	/* Processing sweep while the STT reads the recording back. */
	.proc {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		pointer-events: none;
	}
	.proc circle {
		fill: none;
		stroke: var(--sev-warn, var(--color-warning));
		stroke-width: 1.5;
		stroke-dasharray: 40 500;
		stroke-linecap: round;
		transform-origin: 120px 120px;
		animation: proc-sweep 1.1s linear infinite;
	}
	@keyframes proc-sweep {
		to { transform: rotate(360deg); }
	}

	/* White flash on a real uplink frame. */
	.flash {
		position: absolute;
		inset: 38%;
		border-radius: 50%;
		background: radial-gradient(circle, color-mix(in srgb, var(--deck-text) 30%, transparent), transparent 70%);
		animation: flash 700ms ease-out forwards;
		pointer-events: none;
	}
	@keyframes flash { from { opacity: 1; transform: scale(0.85); } to { opacity: 0; transform: scale(1.12); } }

	/* IN NORMAL FLOW, as a SIBLING of the core. Twice this label lived inside
	   an absolutely-positioned or fixed-size box and twice it landed on top of
	   whatever followed it. Flow layout cannot overlap. */
	.core-label {
		text-align: center;
		white-space: nowrap;
		flex: none;
	}

	.voice-stage {
		display: block;
		font: 700 15px/1.1 var(--font-display);
		letter-spacing: 0.3em;
		color: var(--deck-glow);
	}
	.voice-stage[data-stage='offline'] { color: var(--deck-dim); }
	.voice-stage[data-stage='connecting'] { color: var(--sev-warn, var(--color-warning)); }
	.voice-stage[data-stage='you'] { color: var(--sev-ok, var(--color-success)); }
	.voice-stage[data-stage='error'] { color: var(--sev-err, var(--color-error)); }

	.voice-sub {
		display: block;
		margin-top: 4px;
		font: 400 9px/1.2 var(--font-data);
		letter-spacing: 0.06em;
		color: var(--deck-dim);
	}
	.voice-sub--bad { color: var(--sev-err, var(--color-error)); }

	/* ── live caption ──────────────────────────────────────────────────── */
	.caption {
		min-height: 64px;
		max-width: min(620px, 92%);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		flex: none;
	}

	.caption-text {
		margin: 0;
		text-align: center;
		font: 400 15px/1.5 var(--font-body);
		color: var(--deck-text);
		display: -webkit-box;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
		animation: caption-in 200ms ease-out;
	}
	/* Speaker colour code — the same code the orb wears. */
	.caption-text[data-speaker='user'] {
		color: color-mix(in srgb, var(--sev-ok) 75%, var(--deck-text));
	}
	.caption-text[data-speaker='assistant'] {
		color: color-mix(in srgb, var(--deck-glow) 70%, var(--deck-text));
	}
	/* A finished turn decays: its permanent home is the left rail. The delay
	   holds it readable for a beat before it lets go. */
	.caption-text[data-done='true'] {
		animation: caption-out 1.6s ease 900ms forwards;
	}
	@keyframes caption-in {
		from { opacity: 0; transform: translateY(5px); }
	}
	@keyframes caption-out {
		to { opacity: 0; transform: translateY(-8px); }
	}

	/* Ignored-turn flash: appears, holds long enough to read, lets go. */
	.ignored {
		margin: 0;
		font: 500 11px/1.3 var(--font-data);
		letter-spacing: 0.08em;
		color: var(--sev-warn, var(--color-warning));
		animation: ignored-decay 5s ease 2.2s forwards;
	}
	@keyframes ignored-decay {
		to { opacity: 0; }
	}

	/* ── the whisper line: affordances in words, not chrome ────────────── */
	.hint-stack {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 8px;
		flex: none;
	}

	.hint {
		margin: 0;
		font: 400 10px/1.4 var(--font-data);
		letter-spacing: 0.12em;
		color: var(--deck-dim);
		text-align: center;
		flex: none;
	}

	.whisper {
		background: none;
		border: none;
		padding: 0;
		margin-left: 10px;
		cursor: pointer;
		font: 600 10px/1 var(--font-data);
		letter-spacing: 0.14em;
		color: color-mix(in srgb, var(--deck-glow) 70%, var(--deck-dim));
	}
	.whisper:hover { color: var(--deck-glow); text-decoration: underline; }
	.whisper[data-on='true'] { color: var(--deck-glow); }
	.whisper[data-on='false'] { color: var(--deck-dim); }

	.modes { display: inline-flex; align-items: center; gap: 2px; }
	.sep { color: var(--deck-dim); margin: 0 2px; }
</style>
