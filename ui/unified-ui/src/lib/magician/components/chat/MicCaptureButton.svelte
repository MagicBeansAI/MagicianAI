<!--
  Push-to-record mic capture button (Phase 2).

  Tap-to-start, tap-to-stop. After stopping, a tiny preview row appears
  with [Attach] [Discard] actions — honoring the "raw audio ephemeral
  unless explicitly saved" rule from the realtime-media-control-rails
  plan. By default the blob stays in memory until the action resolves it.
  When the browser-local "Keep dictation recordings" choice is enabled, the
  stopped blob is first placed in a bounded IndexedDB outbox and then uploaded
  to Audio Notes independently of chat send/cancel.

  Emits `media.capture.*` and `media.permission.*` events through the
  active realtime session.
-->
<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import {
		isMicCaptureSupported,
		makeRecordedAudioFile,
		startMicRecording,
		type RecordedAudio,
		type RecordingHandle
	} from '$lib/media/capture/mic';
	import { createPressGesture } from '$lib/media/capture/pressGesture';
	import { mediaProvidersStore } from '$lib/media/providers';
	import { publishMediaEvent, updateMediaSessionPermissions } from '$lib/media/session';
	import { mediaSessionStore } from '$lib/media/store';
	import { transcribeAudioStreaming } from '$lib/media/stt/sttClient';
	import { beginConcurrentVoiceCapture, endConcurrentVoiceCapture, settleConcurrentVoiceInput } from '$lib/media/voice/concurrentVoice';
	import { audioNoteOutbox } from '$lib/notes/audioNoteOutbox';
	import { archiveChatDictationStore } from '$lib/notes/audioNotePreferences';
	import {
		MEDIA_CAPTURE_CANCELLED,
		MEDIA_CAPTURE_COMPLETED,
		MEDIA_CAPTURE_ERROR,
		MEDIA_CAPTURE_STARTED,
		MEDIA_PERMISSION_DENIED,
		MEDIA_PERMISSION_GRANTED,
		MEDIA_TRANSCRIPT_FINAL
	} from '$lib/media/types';
	import SurfaceAudioProfileControl from '$lib/media/SurfaceAudioProfileControl.svelte';
	import VoiceOrb from '$lib/media/VoiceOrb.svelte';

	export let disabled: boolean = false;
	export let compact: boolean = false;
	export let expanded: boolean = false;

	const dispatch = createEventDispatcher<{
		capture: { file: File; durationMs: number };
		transcribe: { transcript: string; durationMs: number };
		transcribeDelta: { transcript: string };
		/** Fires whenever the mic flow becomes active (recording or holding an
		 *  unsent take) or idle — lets a host (e.g. the Voice popover) stay
		 *  mounted so an in-flight capture isn't destroyed. */
		recordingChange: boolean;
		discard: void;
	}>();

	$: sttAvailable = Boolean($mediaProvidersStore.stt || $mediaProvidersStore.stt_fallbacks?.length);
	let transcribing = false;
	// Active AbortController for the in-flight transcribe POST.
	// Discard / unmount during transcribe aborts so the resolved
	// promise can't fire `dispatch('transcribe', …)` into a destroyed
	// or otherwise stale context.
	let transcribeController: AbortController | null = null;

	let available = false;
	let recording = false;
	let recorder: RecordingHandle | null = null;
	let captureId: string | null = null;
	let pendingAudio: RecordedAudio | null = null;
	let errorMessage: string | null = null;
	let archiveError: string | null = null;
	let archiveThisCapture = false;
	let audioNoteId: string | null = null;
	let elapsedMs = 0;
	let elapsedTimer: ReturnType<typeof setInterval> | null = null;
	let recordingStartedAt = 0;

	// Signal "mic flow active" (recording or an unsent take) so a host like the
	// Voice popover stays mounted/open and never destroys an in-flight capture.
	$: dispatch('recordingChange', recording || pendingAudio !== null);

	// ── Tap vs hold (push-to-talk) ────────────────────────────────────────────
	// Tap = the long-standing start/stop toggle. Hold = push-to-talk: capture
	// runs while the button is held and finishes on release. `pressGesture`
	// owns only the discrimination (pure + unit-tested); the recorder calls stay
	// here. Taps are deliberately left to the native `click` handler so keyboard
	// activation (Enter/Space, which emits click with no pointer events) keeps
	// working unchanged — the gesture only suppresses the click that follows a
	// hold, which would otherwise re-toggle the capture just ended.
	let holdActive = false;
	let releasedBeforeCaptureStarted = false;

	const pressGesture = createPressGesture({
		onHoldStart: async () => {
			holdActive = true;
			releasedBeforeCaptureStarted = false;
			await start();
			// `start()` awaits getUserMedia; a quick release can land before the
			// stream exists, when `stopAndPreview()` would still be a no-op. Honor
			// that pending release now so the mic can never outlive the hold.
			if (releasedBeforeCaptureStarted) {
				releasedBeforeCaptureStarted = false;
				await stopAndPreview();
			}
		},
		onHoldEnd: () => {
			if (!holdActive) return;
			holdActive = false;
			if (recording) void stopAndPreview();
			else releasedBeforeCaptureStarted = true;
		},
		onTap: () => {
			/* handled by the native click handler — see the note above */
		}
	});

	function onMicPointerDown(event: PointerEvent): void {
		if (event.button !== 0) return; // primary press only
		// Capture the pointer for the whole hold. Starting a capture inserts the
		// elapsed-time readout next to this button and swaps the glyph for the
		// orb, which re-lays-out the composer row and slides the button out from
		// under a perfectly still finger. Without capture that produced a
		// pointerleave (ending the hold instantly, so the take came back "too
		// short to transcribe") and could send the pointerup to whatever element
		// had taken our place. With capture, every later pointer event for this
		// gesture is delivered here regardless of what moves.
		// Arm the gesture FIRST: capture is an enhancement, and a throw here
		// (synthetic events, unsupported pointerId) must never cost us the hold.
		pressGesture.down();
		try {
			if (event.currentTarget instanceof HTMLElement) {
				event.currentTarget.setPointerCapture?.(event.pointerId);
			}
		} catch {
			/* no capture available — the gesture still works, just less robustly */
		}
	}

	function releaseMicPointer(event: PointerEvent): void {
		try {
			if (
				event.currentTarget instanceof HTMLElement &&
				event.currentTarget.hasPointerCapture?.(event.pointerId)
			) {
				event.currentTarget.releasePointerCapture(event.pointerId);
			}
		} catch {
			/* nothing captured — releasing is best-effort */
		}
	}

	function onMicPointerUp(event: PointerEvent): void {
		releaseMicPointer(event);
		pressGesture.up();
	}

	function onMicPointerCancel(event: PointerEvent): void {
		releaseMicPointer(event);
		pressGesture.cancel();
	}
	function onMicClick(): void {
		// Drop the synthetic click the browser emits when a hold is released.
		if (pressGesture.shouldSuppressClick()) return;
		void (recording ? stopAndPreview() : start());
	}

	onMount(() => {
		available = isMicCaptureSupported();
		if (!available) {
			console.warn('[MicCaptureButton] mic capture unavailable:', {
				mediaDevices: typeof navigator !== 'undefined' ? typeof navigator.mediaDevices : 'no-navigator',
				getUserMedia:
					typeof navigator !== 'undefined' && navigator.mediaDevices
						? typeof navigator.mediaDevices.getUserMedia
						: 'n/a',
				MediaRecorder: typeof window !== 'undefined' ? typeof window.MediaRecorder : 'no-window'
			});
		}
	});

	onDestroy(() => {
		if (recorder || transcribeController || pendingAudio) { endConcurrentVoiceCapture(); settleConcurrentVoiceInput(); }
		stopTimer();
		pressGesture.dispose();
		if (recorder) {
			recorder.cancel();
			recorder = null;
		}
		if (transcribeController) {
			transcribeController.abort();
			transcribeController = null;
		}
		// A retained recording must never be stranded forever in the
		// transcript-wait state merely because its composer unmounted.
		void finalizeAudioNote(null);
	});

	function startTimer(): void {
		stopTimer();
		recordingStartedAt = Date.now();
		elapsedMs = 0;
		elapsedTimer = setInterval(() => {
			elapsedMs = Date.now() - recordingStartedAt;
		}, 250);
	}

	function stopTimer(): void {
		if (elapsedTimer !== null) {
			clearInterval(elapsedTimer);
			elapsedTimer = null;
		}
	}

	async function patchPermission(state: 'granted' | 'denied'): Promise<void> {
		const current = $mediaSessionStore.session;
		if (!current) return;
		await updateMediaSessionPermissions({
			...current.permissions,
			mic: state
		});
	}

	async function start(): Promise<void> {
		if (!available || disabled || recording || pendingAudio !== null) return;
		beginConcurrentVoiceCapture();
		errorMessage = null;
		archiveError = null;
		archiveThisCapture = $archiveChatDictationStore;
		audioNoteId = null;
		captureId = `mic-${Date.now()}`;
		void publishMediaEvent(MEDIA_CAPTURE_STARTED, {
			capture_id: captureId,
			channel: 'mic'
		});
		try {
			recorder = await startMicRecording();
			recording = true;
			startTimer();
			void publishMediaEvent(MEDIA_PERMISSION_GRANTED, {
				channel: 'mic'
			});
			void patchPermission('granted');
		} catch (error) {
			const reason = error instanceof Error ? error.message : 'unknown';
			endConcurrentVoiceCapture();
			settleConcurrentVoiceInput();
			errorMessage = reason === 'mic_capture_unsupported' ? 'Mic capture is not supported on this device.' : 'Microphone permission denied.';
			void publishMediaEvent(reason === 'mic_capture_unsupported' ? MEDIA_CAPTURE_ERROR : MEDIA_PERMISSION_DENIED, {
				capture_id: captureId,
				channel: 'mic',
				reason
			});
			void patchPermission('denied');
			recording = false;
			captureId = null;
			recorder = null;
			stopTimer();
		}
	}

	export async function beginRecording(): Promise<boolean> {
		available = isMicCaptureSupported();
		await start();
		return recording;
	}

	/** Stop an in-flight recording and run the normal stop → transcribe → send
	 *  flow. Used by push-to-talk on key-release (the orb's own button calls the
	 *  same `stopAndPreview` internally). No-op when not recording. */
	export async function stopRecording(): Promise<void> {
		await stopAndPreview();
	}

	async function stopAndPreview(): Promise<void> {
		if (!recording || !recorder) return;
		recording = false;
		endConcurrentVoiceCapture();
		stopTimer();
		try {
			const result = await recorder.stop();
			recorder = null;
			pendingAudio = result;
			if (archiveThisCapture) {
				try {
					audioNoteId = await audioNoteOutbox.stage({
						audio: result.blob,
						originalFilename: makeRecordedAudioFile(result).name,
						mimeType: result.mimeType,
						durationMs: result.durationMs,
						sourceSurface: 'web_chat_dictation'
					});
				} catch (error) {
					archiveError = error instanceof Error
						? error.message
						: 'This dictation could not be staged in Audio Notes.';
				}
			}
			void publishMediaEvent(MEDIA_CAPTURE_COMPLETED, {
				capture_id: captureId,
				channel: 'mic',
				mime_type: result.mimeType,
				duration_ms: result.durationMs,
				bytes: result.blob.size
			});
			// Conversational flow: if the backend advertises an STT path,
			// transcribe immediately on stop so the user doesn't have
			// to tap a second "Transcribe" button. On success the
			// transcript fires through the normal `transcribe` event
			// and the composer auto-sends after a short cooldown. On
			// empty / error we fall back to the manual preview row.
			if (sttAvailable) {
				await transcribe();
			}
		} catch (error) {
			recorder = null;
			pendingAudio = null;
			errorMessage = error instanceof Error ? error.message : 'Recording failed.';
			void publishMediaEvent(MEDIA_CAPTURE_ERROR, {
				capture_id: captureId,
				channel: 'mic',
				reason: errorMessage
			});
		}
	}

	async function finalizeAudioNote(transcript: string | null): Promise<void> {
		const noteId = audioNoteId;
		if (!noteId) return;
		audioNoteId = null;
		try {
			await audioNoteOutbox.finalize(noteId, transcript);
		} catch (error) {
			archiveError = error instanceof Error
				? error.message
				: 'This dictation remains pending in Audio Notes.';
		}
	}

	function discard(): void {
		endConcurrentVoiceCapture();
		settleConcurrentVoiceInput();
		const id = captureId;
		// Abort any in-flight transcribe so its `.then()` callback
		// can't dispatch a stale transcript after the user explicitly
		// chose to throw the recording away.
		if (transcribeController) {
			transcribeController.abort();
			transcribeController = null;
		}
		transcribing = false;
		void finalizeAudioNote(null);
		pendingAudio = null;
		captureId = null;
		errorMessage = null;
		if (id) {
			void publishMediaEvent(MEDIA_CAPTURE_CANCELLED, {
				capture_id: id,
				channel: 'mic',
				reason: 'user_discarded'
			});
		}
		dispatch('discard');
	}

	function attach(): void {
		if (!pendingAudio) return;
		settleConcurrentVoiceInput();
		const file = makeRecordedAudioFile(pendingAudio);
		const durationMs = pendingAudio.durationMs;
		const id = captureId;
		void finalizeAudioNote(null);
		pendingAudio = null;
		captureId = null;
		dispatch('capture', { file, durationMs });
		if (id) {
			void publishMediaEvent(MEDIA_CAPTURE_COMPLETED, {
				capture_id: id,
				channel: 'mic',
				attached: true,
				duration_ms: durationMs
			});
		}
	}

	async function transcribe(): Promise<void> {
		if (!pendingAudio || transcribing) return;
		const id = captureId;
		const audio = pendingAudio;
		transcribing = true;
		errorMessage = null;
		// eslint-disable-next-line no-console
		console.log('[mic-capture] transcribing', {
			bytes: audio.blob.size,
			durationMs: audio.durationMs,
			mimeType: audio.mimeType
		});
		if (audio.blob.size < 1024) {
			errorMessage = 'Recording is too short to transcribe.';
			transcribing = false;
			return;
		}
		const controller = new AbortController();
		transcribeController = controller;
		try {
			// Streaming variant — OpenAI's `gpt-transcribe` returns
			// `transcript.text.delta` events as it processes the upload.
			// We forward each delta into the composer via the
			// `transcribeDelta` event so the user sees text materialise
			// instead of staring at "Transcribing…" for 1–3 seconds.
			// The auto-send countdown waits for the final event below.
			// Pin the language hint so Whisper / gpt-transcribe
			// doesn't auto-detect. Auto-detect on short / low-SNR
			// clips frequently mis-identifies to Chinese (a known
			// Whisper behavior on near-silent or breath-only input),
			// producing garbage transcripts in Chinese characters.
			// `navigator.language` is the browser's locale (e.g.
			// "en-US"); take the two-letter prefix and fall back to
			// "en" if absent.
			const browserLanguage =
				(typeof navigator !== 'undefined' && navigator.language
					? navigator.language.split('-')[0]
					: '') || 'en';
			const result = await transcribeAudioStreaming(
				{
					audio: audio.blob,
					mimeType: audio.mimeType,
					filename: makeRecordedAudioFile(audio).name,
					messageId: id ?? undefined,
					language: browserLanguage,
					signal: controller.signal
				},
				{
					onDelta: (text) => {
						if (controller.signal.aborted) return;
						dispatch('transcribeDelta', { transcript: text });
					}
				}
			);
			// Discard / destroy ran during the upload — drop the
			// result on the floor instead of firing dispatch into a
			// stale context. AbortError throw path is handled in catch.
			if (controller.signal.aborted) return;
			// eslint-disable-next-line no-console
			console.log('[mic-capture] transcribe result', result);
			const transcript = (result.transcript ?? '').trim();
			if (transcript.length === 0) {
				settleConcurrentVoiceInput();
				// Whisper / gpt-transcribe returns `""` when the
				// audio has no detected speech. Keep the preview so
				// the user can retry rather than silently swallowing
				// the recording.
				errorMessage =
					'No speech detected. Try recording closer to the mic or for longer.';
				void publishMediaEvent(MEDIA_CAPTURE_ERROR, {
					capture_id: id,
					channel: 'mic',
					reason: 'empty_transcript',
					model: result.model
				});
				return;
			}
			void publishMediaEvent(MEDIA_TRANSCRIPT_FINAL, {
				capture_id: id,
				channel: 'mic',
				transcript,
				model: result.model,
				duration_ms: audio.durationMs
			});
			dispatch('transcribe', { transcript, durationMs: audio.durationMs });
			await finalizeAudioNote(transcript);
			pendingAudio = null;
			captureId = null;
		} catch (error) {
			// AbortError from user-triggered discard/destroy is the
			// expected cancellation path — silently ignore rather
			// than flashing a confusing "Transcription failed" toast.
			if (
				error instanceof DOMException
				&& (error.name === 'AbortError' || controller.signal.aborted)
			) {
				return;
			}
			settleConcurrentVoiceInput();
			errorMessage = error instanceof Error ? error.message : 'Transcription failed.';
			// eslint-disable-next-line no-console
			console.error('[mic-capture] transcribe failed', error);
			void publishMediaEvent(MEDIA_CAPTURE_ERROR, {
				capture_id: id,
				channel: 'mic',
				reason: errorMessage,
				phase: 'transcribe'
			});
		} finally {
			transcribing = false;
			if (transcribeController === controller) {
				transcribeController = null;
			}
		}
	}

	function formatDuration(ms: number): string {
		const totalSeconds = Math.max(0, Math.floor(ms / 1000));
		const minutes = Math.floor(totalSeconds / 60);
		const seconds = totalSeconds % 60;
		return `${minutes}:${seconds.toString().padStart(2, '0')}`;
	}

</script>

{#if !available}
	<!-- Mic capture API not available in this surface (no
	     navigator.mediaDevices / MediaRecorder, or no NSMicrophone
	     plist entry on macOS Tauri). Render an unavailable state
	     so the button slot still occupies its place in the
	     composer — matches the VoiceCallButton's "unavailable with
	     slash" affordance. */ -->
	<button
		type="button"
		class="mic-capture__btn mic-capture__btn--compact mic-capture__btn--unavailable"
		disabled
		title="Mic capture unavailable on this surface"
		aria-label="Mic capture unavailable"
	>
		<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
			<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
			<path d="M19 10v2a7 7 0 0 1-14 0v-2" />
			<line x1="12" y1="19" x2="12" y2="23" />
			<line x1="8" y1="23" x2="16" y2="23" />
			<line x1="4" y1="4" x2="20" y2="20" stroke-width="2" />
		</svg>
	</button>
{:else}
	<div class="mic-capture" class:mic-capture--expanded={expanded}>
		{#if expanded && sttAvailable && !pendingAudio}
			<SurfaceAudioProfileControl
				surface="dictation"
				compact={true}
				showStages={true}
				disabled={transcribing}
			/>
		{/if}
		<button
			type="button"
			class="mic-capture__btn"
			class:mic-capture__btn--compact={compact}
			class:mic-capture__btn--recording={recording}
			class:mic-capture__btn--armed={pendingAudio !== null}
			on:pointerdown={onMicPointerDown}
			on:pointerup={onMicPointerUp}
			on:pointercancel={onMicPointerCancel}
			on:click|preventDefault={onMicClick}
			disabled={disabled || pendingAudio !== null}
			title={recording
				? 'Stop recording'
				: 'Tap to record · hold to talk (release to send)'}
			aria-label={recording ? 'Stop recording voice note' : 'Record a voice note — tap to start, or hold to talk'}
		>
			{#if recording}
				<!-- Live audio-reactive orb while capturing. The orb scales +
				     glows with the mic input via the `AnalyserNode` exposed
				     on the recording handle — visible confirmation that we're
				     actually hearing the user, plus a "tap to stop" target. -->
				<VoiceOrb state="capturing" analyser={recorder?.analyser ?? null} size={compact ? 22 : 26} />
			{:else}
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
					<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
					<path d="M19 10v2a7 7 0 0 1-14 0v-2" />
					<line x1="12" y1="19" x2="12" y2="23" />
					<line x1="8" y1="23" x2="16" y2="23" />
				</svg>
			{/if}
			{#if expanded}
				<span class="mic-capture__btn-label">
					{recording
						? `Stop recording ${formatDuration(elapsedMs)}`
						: pendingAudio
							? 'Recording ready'
							: 'Record voice note'}
				</span>
			{/if}
		</button>
		{#if recording && !expanded}
			<span class="mic-capture__elapsed" aria-live="polite">{formatDuration(elapsedMs)}</span>
		{/if}
		{#if recording && archiveThisCapture}
			<span class="mic-capture__saving" aria-live="polite">Saving to Audio Notes</span>
		{/if}
		{#if pendingAudio}
			<div class="mic-capture__preview" role="group" aria-label="Voice note preview">
				<span class="mic-capture__duration">Recorded {formatDuration(pendingAudio.durationMs)}</span>
				{#if sttAvailable}
					<button
						type="button"
						class="mic-capture__action mic-capture__action--primary"
						disabled={transcribing}
						on:click|preventDefault={transcribe}
					>
						{transcribing ? 'Transcribing…' : 'Transcribe'}
					</button>
					<button
						type="button"
						class="mic-capture__action"
						disabled={transcribing}
						on:click|preventDefault={attach}
					>
						Attach audio
					</button>
				{:else}
					<button
						type="button"
						class="mic-capture__action mic-capture__action--primary"
						on:click|preventDefault={attach}
					>
						Attach
					</button>
				{/if}
				<button
					type="button"
					class="mic-capture__action"
					disabled={transcribing}
					on:click|preventDefault={discard}
				>
					Discard
				</button>
			</div>
		{/if}
		{#if errorMessage}
			<span class="mic-capture__error" role="alert">{errorMessage}</span>
		{/if}
		{#if archiveError}
			<span class="mic-capture__error" role="alert">{archiveError}</span>
		{/if}
	</div>
{/if}

<style>
	.mic-capture {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
	}
	.mic-capture--expanded {
		width: 100%;
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: 10px;
	}
	.mic-capture__btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border-radius: 8px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		background: transparent;
		color: var(--theme-color-foreground-muted, #6b7280);
		cursor: pointer;
		transition:
			color 120ms ease,
			border-color 120ms ease,
			background-color 120ms ease;
	}
	.mic-capture__btn--compact {
		width: 28px;
		height: 28px;
	}
	.mic-capture--expanded .mic-capture__btn {
		width: 100%;
		height: 44px;
		gap: 10px;
		justify-content: center;
		border-radius: 12px;
		background: var(--theme-color-surface, rgba(255, 255, 255, 0.55));
	}
	.mic-capture__btn-label {
		font-size: 0.9rem;
		font-weight: 650;
	}
	.mic-capture__btn:hover:not(:disabled) {
		color: var(--theme-color-foreground, #111827);
		border-color: var(--theme-color-foreground-muted, #6b7280);
	}
	.mic-capture__btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.mic-capture__btn--recording {
		color: var(--theme-color-danger, #dc2626);
		border-color: var(--theme-color-danger, #dc2626);
		background: color-mix(in srgb, var(--theme-color-danger, #dc2626) 14%, transparent);
	}
	.mic-capture__btn--armed {
		color: var(--theme-color-accent, #2563eb);
		border-color: var(--theme-color-accent, #2563eb);
	}
	/* Slash-icon unavailable state (matches VoiceCallButton's
	   `--unavailable` affordance). The mic API doesn't exist on
	   this surface — render the icon with a slash through it so
	   the user sees the slot but knows recording is offline. */
	.mic-capture__btn--unavailable {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.mic-capture__elapsed {
		font-variant-numeric: tabular-nums;
		font-size: 0.78rem;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.mic-capture__saving {
		font-size: 0.72rem;
		font-weight: 650;
		color: var(--theme-color-accent, #2563eb);
		white-space: nowrap;
	}
	.mic-capture__preview {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.25rem 0.5rem;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background: var(--theme-color-surface, rgba(255, 255, 255, 0.5));
	}
	.mic-capture--expanded .mic-capture__preview {
		width: 100%;
		box-sizing: border-box;
		display: flex;
		flex-wrap: wrap;
		align-items: stretch;
		gap: 8px;
		padding: 10px;
		border-radius: 12px;
		background: color-mix(in srgb, var(--theme-color-accent, #2563eb) 7%, var(--theme-color-surface, rgba(255, 255, 255, 0.7)));
	}
	.mic-capture__duration {
		font-variant-numeric: tabular-nums;
		font-size: 0.78rem;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.mic-capture--expanded .mic-capture__duration {
		flex: 1 0 100%;
		font-size: 0.82rem;
	}
	.mic-capture__action {
		padding: 0.15rem 0.55rem;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		background: transparent;
		color: var(--theme-color-foreground, #111827);
		font-size: 0.78rem;
		font-weight: 500;
		border-radius: 4px;
		cursor: pointer;
		transition: background-color 120ms ease, border-color 120ms ease;
	}
	.mic-capture--expanded .mic-capture__action {
		flex: 1 1 120px;
		min-height: 38px;
		padding: 0.4rem 0.65rem;
		border-radius: 9px;
		white-space: nowrap;
	}
	.mic-capture__action:hover {
		border-color: var(--theme-color-foreground-muted, #6b7280);
	}
	.mic-capture__action--primary {
		background: var(--theme-color-accent, #2563eb);
		border-color: var(--theme-color-accent, #2563eb);
		color: var(--theme-color-on-accent, #fff);
	}
	.mic-capture__action--primary:hover {
		filter: brightness(1.05);
	}
	.mic-capture__error {
		font-size: 0.75rem;
		color: var(--theme-color-danger, #dc2626);
	}
	.mic-capture--expanded .mic-capture__error {
		display: block;
		width: 100%;
		box-sizing: border-box;
		padding: 9px 10px;
		border-radius: 10px;
		background: color-mix(in srgb, var(--theme-color-danger, #dc2626) 10%, transparent);
		font-size: 0.82rem;
	}
</style>
