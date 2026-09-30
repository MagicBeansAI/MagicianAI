<script lang="ts">
	/**
	 * Unified voice control for the composer: a "Voice" trigger whose popover
	 * groups the Hands-free/Call/Dictate mode switch, STT provider, and
	 * read-aloud. Wake-word ownership belongs exclusively to the desktop Orb and
	 * is configured in Orb Settings, never from a chat composer.
	 *
	 * The ACTION for the selected mode is split by where it belongs. Dictate's
	 * MicCaptureButton is rendered next to the trigger on the composer row (tap to
	 * record, hold to talk) because dictation is reached constantly and a popover
	 * made it three interactions deep; the call modes keep VoiceCallButton inside
	 * the popover. The mic is rendered ONLY in Dictate mode — a live STT mic on the
	 * row while Call/Hands-free is selected would contradict the chosen mode.
	 *
	 * Because the mic no longer lives inside the popover, the popover no longer
	 * pins itself open through a take.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import { primaryAgent } from '$lib/stores/agentStore';
	import {
		isVoiceCallCaptureState,
		voiceCallStore
	} from '$lib/media/voice/realtimeVoiceClient';
	import { activationNamesForAgent } from '$lib/media/voice/voiceAddressing';
	import {
		installPushToTalkHotkey,
		registerRecordingStop,
		registerRecordingTrigger,
		voiceModeStore,
		type VoiceMode
	} from '$lib/media/voice/wakeWord';
	import { mediaPreferencesStore, saveMediaPreferences } from '$lib/media/preferences';
	import { mediaProvidersStore, realtimeVoiceProfileStore } from '$lib/media/providers';
	import SurfaceAudioProfileControl from '$lib/media/SurfaceAudioProfileControl.svelte';
	import AutoSpeakToggle from './AutoSpeakToggle.svelte';
	import MicCaptureButton from './MicCaptureButton.svelte';
	import BrowserVoiceControls from '$lib/media/tts/BrowserVoiceControls.svelte';
	import VoiceCallButton from './VoiceCallButton.svelte';
	import { startScreenLockMonitoring } from '$lib/media/voice/screenLock';
	import {
		archiveChatDictationStore,
		setArchiveChatDictation
	} from '$lib/notes/audioNotePreferences';

	export let disabled = false;
	export let isReadOnly = false;
	export let isUploading = false;
	export let isSending = false;
	export let showChevron = true;

	const dispatch = createEventDispatcher<{
		micCapture: { file: File; durationMs: number };
		micTranscribe: { transcript: string; durationMs: number };
		micTranscribeDelta: { transcript: string };
	}>();

	type MicRecorderHandle = {
		beginRecording: () => Promise<boolean>;
		stopRecording: () => Promise<void>;
	};

	let open = false;
	let recordingActive = false; // keep the popover open while dictating
	let micRecorder: MicRecorderHandle | null = null;
	let rootEl: HTMLDivElement | null = null;
	let uninstallPtt: (() => void) | null = null;
	let unregisterRecordingTrigger: (() => void) | null = null;

	$: mode = $voiceModeStore;
	$: requirePrefix = $mediaPreferencesStore.preferences.require_voice_prefix;
	$: callLive = isVoiceCallCaptureState($voiceCallStore.state);
	// Before a call, mirror the server's aliases → name → wake-spellings
	// assembly. Once connected, show the exact frozen phrase set returned by the
	// backend so the UI can never advertise a stale or locally inferred gate.
	$: configuredAddressingPhrases = activationNamesForAgent($primaryAgent).map(
		(name) => `Hey ${name}`
	);
	$: addressingPhrases =
		callLive && $voiceCallStore.addressingRequired
			? $voiceCallStore.activationPhrases
			: configuredAddressingPhrases;
	$: addressingPhraseLabel =
		addressingPhrases.map((p) => `"${p}"`).join(' or ') || 'a configured "Hey …" phrase';
	$: realtimeProfiles = $mediaProvidersStore.realtime_voice_profiles ?? [];
	$: selectedRealtimeProfile = realtimeProfiles.find(
		(profile) => profile.profile_id === $realtimeVoiceProfileStore
	);
	$: selectedRealtimeMode = selectedRealtimeProfile?.mode ?? 'assistant';
	$: defaultRealtimeProfile = realtimeProfiles.find(
		(profile) => profile.profile_id === $mediaProvidersStore.realtime_voice_default_profile
	);
	$: if (
		$realtimeVoiceProfileStore &&
		$mediaProvidersStore.resolved &&
		!realtimeProfiles.some(
			(profile) => profile.profile_id === $realtimeVoiceProfileStore && profile.available
		)
	) {
		realtimeVoiceProfileStore.set(null);
	}
	// The backend leaves a call ungated when it has no assistant name to listen
	// for, so wanting the gate is not the same as having it. Only warn once a
	// call is actually live and the backend has reported back.
	$: gateRequestedButNotApplied =
		requirePrefix && callLive && $voiceCallStore.addressingRequired === false;

	onMount(() => {
		// Adopt an already-granted Chromium Idle Detection permission without
		// prompting. The pointer-down seam below requests it only from an explicit
		// Voice interaction, as required by the browser permission contract.
		void startScreenLockMonitoring(false);
		// Browser push-to-talk triggers Dictation through this composer because it
		// owns the recorder. Native wake is Orb-only and never enters this seam.
		unregisterRecordingTrigger = registerRecordingTrigger(async () => {
			if (!micRecorder) return false;
			return micRecorder.beginRecording();
		});
		// Push-to-talk releases mid-hold need an explicit stop (the recorder
		// otherwise auto-stops on its own VAD/timer).
		registerRecordingStop(async () => {
			await micRecorder?.stopRecording();
		});
		// Mount the Left-Option push-to-talk hold while a chat composer is on
		// screen (ref-counted in the wake module, so multiple composers share one
		// listener).
		uninstallPtt = installPushToTalkHotkey();
	});
	onDestroy(() => {
		unregisterRecordingTrigger?.();
		unregisterRecordingTrigger = null;
		registerRecordingStop(null);
		uninstallPtt?.();
		uninstallPtt = null;
	});

	// The popover no longer owns the recorder (the mic sits on the composer row),
	// so closing it mid-take is safe — it can't destroy an in-flight capture.
	function toggleOpen(): void {
		open = !open;
	}
	function prepareScreenLockMonitoring(): void {
		void startScreenLockMonitoring(true);
	}
	function close(): void {
		open = false;
	}
	function onWindowPointerDown(event: PointerEvent): void {
		if (rootEl?.contains(event.target as Node)) prepareScreenLockMonitoring();
	}
	function onWindowClick(event: MouseEvent): void {
		if (open && rootEl && !rootEl.contains(event.target as Node)) close();
	}
	function onWindowKey(event: KeyboardEvent): void {
		if (open && event.key === 'Escape') close();
		if (
			rootEl?.contains(event.target as Node) &&
			(event.key === 'Enter' || event.key === ' ')
		) {
			prepareScreenLockMonitoring();
		}
	}
	function pick(next: VoiceMode): void {
		void saveMediaPreferences({ voice_mode: next }).catch((error) => {
			console.warn('Failed to save voice mode preference:', error);
		});
	}
	function toggleVoicePrefix(): void {
		void saveMediaPreferences({ require_voice_prefix: !requirePrefix }).catch((error) => {
			console.warn('Failed to save voice prefix preference:', error);
		});
	}
	function toggleArchiveChatDictation(): void {
		setArchiveChatDictation(!$archiveChatDictationStore);
	}

	function onRecordingChange(active: boolean): void {
		if (!active && recordingActive) {
			open = false; // take finished (sent/discarded) — collapse the popover
		}
		recordingActive = active;
	}
	function onMicCapture(detail: { file: File; durationMs: number }): void {
		dispatch('micCapture', detail);
	}
	function onMicTranscribe(detail: { transcript: string; durationMs: number }): void {
		dispatch('micTranscribe', detail);
	}

	$: triggerTitle = callLive
		? 'Voice — call in progress'
		: 'Voice — call, dictate, or hands-free';
</script>

<svelte:window
	on:pointerdown={onWindowPointerDown}
	on:click={onWindowClick}
	on:keydown={onWindowKey}
/>

<div class="voice" role="group" aria-label="Voice controls" bind:this={rootEl}>
	<!-- Split button: the LEFT half is the action for the selected mode, the
	     RIGHT half opens the menu that changes that selection. One control whose
	     primary half always does the thing the user already chose, so the action
	     is one click and never contradicts the mode:
	       Dictate    → the mic (tap to record, hold to talk)
	       Call       → start/end the realtime call
	       Hands-free → start/end the local cascade
	     The action lives here rather than in the popover because it is reached
	     constantly and a popover made it three interactions deep. The mic is
	     mounted only in Dictate mode — it is the only mode whose push-to-talk
	     hold records through `micRecorder`, and the mode
	     switch is disabled mid-take so this can never unmount a live recording. -->
	<div class="voice-split" class:voice-split--open={open}>
		{#if mode === 'recording'}
			<MicCaptureButton
				bind:this={micRecorder}
				compact={true}
				disabled={isUploading || isReadOnly || disabled}
				on:recordingChange={(e) => onRecordingChange(e.detail)}
				on:capture={(e) => onMicCapture(e.detail)}
				on:transcribe={(e) => onMicTranscribe(e.detail)}
				on:transcribeDelta={(e) => dispatch('micTranscribeDelta', e.detail)}
			/>
		{:else}
			<VoiceCallButton
				{mode}
				compact={true}
				disabled={isReadOnly || disabled}
				chatTurnActive={isSending}
			/>
		{/if}

		<!-- The options half renders only with its chevron: without it the
		     button is empty (VibeDev showed a blank button beside the mic). -->
		{#if showChevron}
		<button
		type="button"
		class="voice-trigger"
		class:on={open || callLive}
		aria-haspopup="dialog"
		aria-expanded={open}
		aria-label="Voice options — change mode and speech settings"
		title={triggerTitle}
		on:click={toggleOpen}
	>
		<!-- No mode glyph here: the action half to the left already IS the
		     selected mode (mic / phone / hands-free), so repeating it would read
		     as two controls for one thing. This half is purely "change what that
		     button does", i.e. the caret of a split button. -->
			<svg
				class="chev"
				width="12"
				height="12"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2.6"
				stroke-linecap="round"
				stroke-linejoin="round"
				aria-hidden="true"
			>
				<polyline points="6 9 12 15 18 9" />
			</svg>
		</button>
		{/if}
	</div>

	{#if open}
		<div class="voice-pop" role="dialog" aria-label="Voice controls">
			<div class="pop-row">
				<span class="seg" role="tablist" aria-label="Voice mode">
					<button
						type="button"
						class:active={mode === 'hands_free'}
						role="tab"
						aria-selected={mode === 'hands_free'}
						disabled={recordingActive}
						on:click={() => pick('hands_free')}>Hands-free</button
					>
					<button
						type="button"
						class:active={mode === 'realtime'}
						role="tab"
						aria-selected={mode === 'realtime'}
						disabled={recordingActive}
						on:click={() => pick('realtime')}>Call</button
					>
					<button
						type="button"
						class:active={mode === 'recording'}
						role="tab"
						aria-selected={mode === 'recording'}
						disabled={recordingActive}
						on:click={() => pick('recording')}>Dictate</button
					>
				</span>

			</div>

			{#if mode === 'realtime'}
				<label class="profile-row">
					<span>Realtime model</span>
					<select
						value={$realtimeVoiceProfileStore ?? ''}
						disabled={callLive}
						on:change={(event) =>
							realtimeVoiceProfileStore.set(
								(event.currentTarget as HTMLSelectElement).value || null
							)}
					>
						<option value="">{defaultRealtimeProfile?.label ?? 'GPT Realtime (default)'}</option>
						{#each realtimeProfiles.filter((profile) => profile.profile_id !== $mediaProvidersStore.realtime_voice_default_profile) as profile}
							<option
								value={profile.profile_id}
								disabled={!profile.available}
								title={profile.unavailable_reason ?? ''}
							>
								{profile.label}{profile.available ? '' : ' (unavailable)'}
							</option>
						{/each}
					</select>
				</label>
			{/if}

			<!-- Describes what the split button's action half now does; the action
			     itself lives out on the composer row, so this row is text only. -->
			<div class="pop-row action-row">
				{#if mode === 'realtime'}
					<span class="hint">{selectedRealtimeMode === 'translation'
						? 'The button starts continuous spoken translation.'
						: 'The button starts a live voice call.'}</span>
				{:else if mode === 'hands_free'}
					<span class="hint">The button starts hands-free — local VAD, STT, and TTS.</span>
				{:else}
					<span class="hint"
						>The button dictates: tap to record, hold to talk. The Left-Control +
						Left-Option hold dictates too; Orb wake never targets this composer.</span
					>
				{/if}
			</div>

			<div class="pop-row guided-flow-row">
				<span class="hint"
					>Voice flows: “Tutor screen…”, “Tutor Quick blackboard…”, or “App Copilot…”</span
				>
			</div>

			{#if mode === 'recording'}
				<div class="archive-setting">
					<label class="prefix-row" title="Save each completed chat dictation as private audio and Markdown in the configured Notes provider.">
						<input
							type="checkbox"
							checked={$archiveChatDictationStore}
							disabled={recordingActive}
							on:change={toggleArchiveChatDictation}
						/>
						<span class="prefix-label">Keep dictation recordings</span>
					</label>
					<div class="archive-detail">
						<span>Off by default. Saved recordings survive message cancellation.</span>
						<a href="/notes">Open Audio Notes</a>
					</div>
				</div>
			{/if}

			{#if (mode === 'realtime' && selectedRealtimeMode !== 'translation') || mode === 'hands_free'}
				<label
					class="prefix-row"
					title={`When enabled, room speech is ignored unless it starts with ${addressingPhraseLabel}. Applies from the NEXT call — the gate is fixed when a call starts.`}
				>
					<input
						type="checkbox"
						checked={requirePrefix}
						disabled={callLive}
						on:change={toggleVoicePrefix}
					/>
					<span class="prefix-label">Require {addressingPhraseLabel}</span>
				</label>
				{#if gateRequestedButNotApplied}
					<!-- Don't let the checkbox imply a gate the backend didn't apply: it
					     leaves a call ungated when it has no assistant name to listen
					     for, and silently ignoring that reads as "the setting is
					     broken". -->
					<p class="prefix-warning" role="status">
						This call isn’t gated — the assistant reported no activation phrase, so all
						room speech is being heard.
					</p>
				{:else if requirePrefix && callLive}
					<p
						class="prefix-note"
						role="status"
						title={`Gated — say ${addressingPhraseLabel} to address it.`}
					>
						Gated — say {addressingPhraseLabel} to address it.
					</p>
				{/if}
			{/if}

			<div class="pop-row settings-row">
				{#if mode !== 'realtime'}
					<SurfaceAudioProfileControl
						surface={mode === 'hands_free' ? 'hands_free' : 'dictation'}
						compact={true}
						showStages={true}
					/>
				{/if}
				<AutoSpeakToggle compact={true} />
			</div>

			<div class="pop-row browser-voice-row">
				<BrowserVoiceControls />
			</div>
		</div>
	{/if}
</div>

<style>
	.voice {
		position: relative;
		display: inline-flex;
	}

	/* Split button: mode action (left) + menu caret (right) read as ONE control.
	   The halves keep their own hit targets and focus rings; only the outer
	   corners are rounded, and a hairline divider separates them. The action half
	   is a child component (MicCaptureButton / VoiceCallButton) so its own button
	   is reached with :global. */
	.voice-split {
		display: inline-flex;
		align-items: stretch;
		gap: 0;
	}
	/* One geometry for every half. The action components size themselves for
	   standalone use (28px box, and the call glyph 14px vs the mic's 16px) while
	   the caret was 24px tall, so the halves sat at different heights with
	   mismatched icon weights. Pin both axes here — this is the only place the
	   three are siblings, so it can't regress their standalone sizing. */
	.voice-split :global(.mic-capture__btn),
	.voice-split :global(.voice-call-btn),
	.voice-split .voice-trigger {
		height: var(--voice-split-h, 28px);
		box-sizing: border-box;
	}
	.voice-split :global(.mic-capture__btn),
	.voice-split :global(.voice-call-btn) {
		width: var(--voice-split-h, 28px);
		border-top-right-radius: 0;
		border-bottom-right-radius: 0;
	}
	/* Equal optical weight across mic / phone / headset. Scoped to the action
	   half's own glyph so the caret chevron and the recording orb keep theirs. */
	.voice-split :global(.mic-capture__btn > svg),
	.voice-split :global(.voice-call-btn > svg) {
		width: 15px;
		height: 15px;
	}
	.voice-split .voice-trigger {
		border-top-left-radius: 0;
		border-bottom-left-radius: 0;
		/* Collapse the doubled border between the halves. */
		margin-left: -1px;
		padding: 0 5px;
	}
	/* Float the running-time readout ABOVE the button instead of letting it sit
	   inline. Inline, it is inserted into this row the instant capture starts,
	   which widens the split button and slides it out from under a stationary
	   finger mid-hold — jarring on its own, and it used to end the hold outright.
	   (FloatingComposer carries the same overlay treatment under
	   `.voice-slot--mic`, a wrapper it stopped rendering when the voice controls
	   were consolidated, so that rule no longer reaches this button.) */
	.voice-split :global(.mic-capture) {
		position: relative;
	}
	.voice-split :global(.mic-capture__elapsed) {
		position: absolute;
		bottom: calc(100% + 6px);
		left: 50%;
		transform: translateX(-50%);
		padding: 3px 8px;
		font-size: 0.68rem;
		font-variant-numeric: tabular-nums;
		color: var(--text-on-accent, #fff);
		background: var(--color-error, #dc2626);
		border-radius: var(--radius-full, 999px);
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.18));
		white-space: nowrap;
		pointer-events: none;
	}
	/* Raise whichever half is interacted with so its full border is visible over
	   the collapsed seam. */
	.voice-split :global(.mic-capture__btn:hover),
	.voice-split :global(.mic-capture__btn:focus-visible),
	.voice-split :global(.voice-call-btn:hover),
	.voice-split :global(.voice-call-btn:focus-visible),
	.voice-split .voice-trigger:hover,
	.voice-split .voice-trigger:focus-visible {
		position: relative;
		z-index: 1;
	}

	.voice-trigger {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		height: 24px;
		padding: 0 7px;
		border-radius: var(--radius-sm, 8px);
		border: 1px solid var(--border-soft, var(--theme-color-border, rgba(0, 0, 0, 0.12)));
		background: color-mix(
			in srgb,
			var(--bg-elevated, var(--theme-color-surface, #fff)) 82%,
			transparent
		);
		color: var(--text-secondary, var(--theme-color-foreground-muted, #6b7280));
		cursor: pointer;
		transition:
			color 0.12s ease,
			border-color 0.12s ease;
	}
	.voice-trigger:hover {
		color: var(--text-primary, var(--theme-color-foreground, #111827));
	}
	.voice-trigger.on {
		color: var(--text-primary, var(--theme-color-foreground, #111827));
		border-color: color-mix(
			in srgb,
			var(--accent-primary, #c2502a) 55%,
			var(--border-soft, transparent)
		);
	}
	/* 9px at 60% opacity was barely visible on the split button; the caret is
	   that half's only content, so it has to read as a control. */
	.voice-trigger .chev {
		flex: none;
		opacity: 0.85;
	}
	.voice-trigger:hover .chev,
	.voice-trigger.on .chev {
		opacity: 1;
	}

	.voice-pop {
		position: absolute;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 60;
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-width: 320px;
		/* Bounded on purpose: without a ceiling the popover simply GROWS to fit
		   its widest line, so an agent with several aliases stretched it far past
		   the composer instead of ellipsizing the phrase list. The cap is what
		   makes `.prefix-label`'s ellipsis reachable. */
		max-width: min(360px, calc(100vw - 24px));
		padding: 12px;
		border-radius: var(--radius-md, 10px);
		border: 1px solid var(--border-soft, var(--theme-color-border, rgba(0, 0, 0, 0.12)));
		background: var(--bg-elevated, var(--theme-color-surface, #fff));
		box-shadow: var(--shadow-popover, 0 10px 30px rgba(0, 0, 0, 0.18));
	}

	.pop-row {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.profile-row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		align-items: center;
		gap: 10px;
		font-size: 11px;
		color: var(--text-secondary, var(--theme-color-foreground-muted, #6b7280));
	}
	.profile-row select {
		min-width: 0;
		width: 100%;
		padding: 6px 8px;
		border: 1px solid var(--border-soft, var(--theme-color-border, rgba(0, 0, 0, 0.12)));
		border-radius: var(--radius-sm, 8px);
		background: var(--bg-surface, var(--theme-color-surface, #fff));
		color: var(--text-primary, var(--theme-color-foreground, #111827));
		font: inherit;
	}
	.action-row {
		justify-content: flex-start;
	}
	.guided-flow-row {
		line-height: 1.35;
	}
	.archive-setting {
		display: grid;
		gap: 5px;
		padding: 8px;
		border: 1px solid var(--border-soft, var(--theme-color-border, rgba(0, 0, 0, 0.12)));
		border-radius: var(--radius-sm, 8px);
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 6%, transparent);
	}
	.archive-detail {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 10px;
		font-size: 10.5px;
		line-height: 1.35;
		color: var(--text-muted, #7f8794);
	}
	.archive-detail a {
		flex: none;
		color: var(--accent-primary, #c2502a);
		font-weight: 650;
		text-decoration: none;
	}
	.settings-row {
		align-items: stretch;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		width: 100%;
	}
	.prefix-row {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
		font-size: 11px;
		color: var(--text-secondary, var(--theme-color-foreground-muted, #6b7280));
		cursor: pointer;
	}
	.prefix-note,
	.prefix-warning {
		margin: 2px 0 0 22px;
		font-size: 10.5px;
		line-height: 1.35;
	}
	/* Same one-line rule as the label: the phrase list is unbounded, and the
	   title carries the full text. The warning is allowed to wrap — it is prose
	   the user must actually read, not a phrase list. */
	.prefix-note {
		color: var(--text-secondary, var(--theme-color-foreground-muted, #6b7280));
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	/* The gate was asked for but not applied — say so plainly rather than letting
	   the ticked checkbox imply ambient speech is being ignored. */
	.prefix-warning {
		color: var(--color-warning, #b45309);
	}
	/* The alias list grows with every alias the agent answers to, so the label
	   must never wrap the popover open — it ellipsizes and the row's `title`
	   carries the full sentence. */
	.prefix-label {
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	/* A native checkbox only ever borrowed the accent colour — everything else
	   (box, radius, check) stayed the platform's, which read as unthemed next to
	   the popover's own controls. Drawn here in the popover's language: the same
	   border token as the trigger, and the `.seg button.active`
	   inversion (text-primary fill, bg-base mark) for the checked state. */
	.prefix-row input[type='checkbox'] {
		appearance: none;
		-webkit-appearance: none;
		position: relative;
		flex: none;
		width: 14px;
		height: 14px;
		margin: 0;
		border: 1px solid var(--border-soft, var(--theme-color-border, rgba(0, 0, 0, 0.22)));
		border-radius: 4px;
		background: var(--bg-base, var(--theme-color-surface, #fff));
		cursor: pointer;
		transition:
			background 0.12s ease,
			border-color 0.12s ease;
	}
	.prefix-row input[type='checkbox']:hover:not(:disabled) {
		border-color: var(--text-secondary, var(--theme-color-foreground-muted, #6b7280));
	}
	.prefix-row input[type='checkbox']:checked {
		background: var(--text-primary, #1a1a1a);
		border-color: var(--text-primary, #1a1a1a);
	}
	/* Checkmark drawn from borders so it inherits theme tokens (an SVG data URI
	   could not follow `--bg-base` across themes). */
	.prefix-row input[type='checkbox']:checked::after {
		content: '';
		position: absolute;
		left: 4px;
		top: 1px;
		width: 3px;
		height: 7px;
		border: solid var(--bg-base, #fff);
		border-width: 0 1.6px 1.6px 0;
		transform: rotate(45deg);
	}
	.prefix-row input[type='checkbox']:focus-visible {
		outline: 2px solid var(--accent-primary, #c2502a);
		outline-offset: 1px;
	}
	.prefix-row:has(input:disabled) {
		opacity: 0.58;
		cursor: not-allowed;
	}

	.hint {
		font-size: 11px;
		color: var(--text-muted, var(--theme-color-foreground-muted, #6b7280));
	}

	/* Segmented Call/Dictate — mirrors the composer's Do/Plan switch. */
	.seg {
		display: inline-flex;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border-radius: 6px;
		padding: 2px;
	}
	.seg button {
		padding: 3px 10px 2px;
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 4px;
		cursor: pointer;
		transition:
			background 0.12s ease,
			color 0.12s ease;
	}
	.seg button.active {
		background: var(--text-primary, #1a1a1a);
		color: var(--bg-base, #fff);
	}

</style>
