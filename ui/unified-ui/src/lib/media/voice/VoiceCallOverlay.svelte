<script lang="ts">
	/**
	 * Live overlay shown whenever a realtime voice call is active.
	 *
	 * Renders:
	 *  - the VoiceOrb in capturing state (mic-reactive)
	 *  - a "speaking" / "listening" status pill
	 *  - the recent turn history (user + assistant transcripts)
	 *  - an end-call button
	 *
	 * Mounted globally in (app)/+layout.svelte so the overlay follows
	 * the user across `/chat`, `/t/[name]`, `/tasks`, etc. — once a
	 * call is up the user can navigate around without losing voice.
	 */
	import { browser } from '$app/environment';
	import { onDestroy, onMount, tick } from 'svelte';
	import { page } from '$app/stores';
	import VoiceOrb from '$lib/media/VoiceOrb.svelte';
	import {
		clearVoiceTranscript,
		engagePushToTalk,
		pushToTalkActive,
		pushToTalkMode,
		releasePushToTalk,
		sessionCapMsStore,
		setPushToTalkMode,
		stopVoiceCall,
		voiceCallStore,
		voiceMicAnalyser,
		voiceTranscriptStore
	} from '$lib/media/voice/realtimeVoiceClient';
	import { primaryAgent } from '$lib/stores/agentStore';
	import { agentDisplayName } from '$lib/presentationIdentity';

	$: call = $voiceCallStore;
	$: transcript = $voiceTranscriptStore;

	// Routes that own the inline DesktopVoiceCenterStage above the ledger.
	// On those pages the floating overlay
	// would double-up — suppress so the inline surface owns the
	// presentation. Off-chat routes (/today, /tasks, /settings, …)
	// keep the overlay as the only "you're on a call" indicator.
	$: pathname = $page.url.pathname;
	$: inlineVoiceSurfaceClaimed =
		pathname === '/chat'
		|| pathname.startsWith('/t/')
		|| pathname.startsWith('/chat/');
	// Overlay stays open during reconnect/rotate so the user sees the
	// recovery status pill instead of an abrupt close. The transport
	// is mid-flight in both states — closing the overlay would tear
	// the audio context down and orphan the new peer connection.
	$: visible =
		!inlineVoiceSurfaceClaimed
		&& (
			call.state === 'connected'
			|| call.state === 'connecting'
			|| call.state === 'reconnecting'
			|| call.state === 'rotating'
		);
	$: turns = transcript.turns;
	$: assistantName = agentDisplayName($primaryAgent);

	// ── Session timer ────────────────────────────────────────────────
	// Denominator comes from the backend descriptor's
	// `max_session_duration_secs`, surfaced via `sessionCapMsStore`.
	// Different models/providers report different caps; we render
	// whatever the backend told us. `null` = uncapped session
	// (rotation is purely watermark-driven); we just show elapsed.
	let now = Date.now();
	let tickTimer: ReturnType<typeof setInterval> | null = null;
	$: sessionCapMs = $sessionCapMsStore;
	$: elapsedMs = call.connectedAt !== null ? Math.max(0, now - call.connectedAt) : 0;
	$: timerLabel =
		call.connectedAt !== null
			? sessionCapMs !== null
				? `${fmtClock(elapsedMs)} / ${fmtClock(sessionCapMs)}`
				: fmtClock(elapsedMs)
			: '';

	function fmtClock(ms: number): string {
		const total = Math.floor(ms / 1000);
		const m = Math.floor(total / 60);
		const s = total % 60;
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	let scrollerEl: HTMLDivElement | null = null;
	let lastTurnCount = 0;
	let lastTextLen = 0;

	// Auto-scroll captions when new content arrives. Tracks both
	// turn-count change (new turn appended) AND last-turn text length
	// change (streaming assistant transcript growing) so the bottom
	// stays in view as words tick in.
	$: if (scrollerEl && turns.length > 0) {
		const newCount = turns.length;
		const newLen = turns[turns.length - 1]?.text.length ?? 0;
		if (newCount !== lastTurnCount || newLen !== lastTextLen) {
			lastTurnCount = newCount;
			lastTextLen = newLen;
			void tick().then(() => {
				if (scrollerEl) scrollerEl.scrollTop = scrollerEl.scrollHeight;
			});
		}
	}

	function statusLabel(
		state: typeof call.state,
		userSpeaking: boolean,
		assistantSpeaking: boolean,
		assistantWorking: boolean,
		ptt: boolean,
		pttActive: boolean
	): string {
		if (state === 'connecting') return 'Connecting…';
		if (state === 'reconnecting') return 'Reconnecting…';
		if (state === 'rotating') return 'Refreshing session…';
		if (state === 'closing') return 'Ending call…';
		if (state === 'error') return 'Voice error';
		if (assistantSpeaking) return 'Speaking…';
		if (userSpeaking) return 'Hearing you…';
		if (ptt && pttActive) return 'Hearing you… (release to send)';
		// Between "let me check…" and the answer the line is silent; say so
		// rather than inviting the next question.
		if (assistantWorking) return 'Working…';
		if (ptt) return 'Hold to talk';
		if (call.addressingRequired && call.activationPhrase) {
			return `Listening — start with “${call.activationPhrase}”`;
		}
		return 'Listening';
	}

	$: ptt = $pushToTalkMode;
	$: pttActive = $pushToTalkActive;
	$: status = statusLabel(
		call.state,
		transcript.userSpeaking,
		transcript.assistantSpeaking,
		transcript.assistantWorking,
		ptt,
		pttActive
	);

	// Hold-to-talk pointer handling with capture so the user can drag
	// off the button without releasing — common when shifting grip on
	// mobile. Touch + mouse both go through Pointer Events.
	let pttBtnEl: HTMLButtonElement | null = null;

	function pttPress(event: PointerEvent): void {
		if (call.state !== 'connected') return;
		event.preventDefault();
		(event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
		engagePushToTalk();
	}

	function pttRelease(event: PointerEvent): void {
		if ((event.currentTarget as HTMLElement).hasPointerCapture?.(event.pointerId)) {
			(event.currentTarget as HTMLElement).releasePointerCapture?.(event.pointerId);
		}
		releasePushToTalk();
	}

	// Spacebar = hold-to-talk on desktop. Ignored while typing in an
	// input/textarea/contenteditable so chat composer typing isn't hijacked.
	function isFormField(el: EventTarget | null): boolean {
		if (!(el instanceof HTMLElement)) return false;
		const tag = el.tagName.toLowerCase();
		return tag === 'input' || tag === 'textarea' || tag === 'select' || el.isContentEditable;
	}

	let spaceHeld = false;
	function onKeydown(event: KeyboardEvent): void {
		if (event.key !== ' ' || !ptt || call.state !== 'connected') return;
		if (isFormField(event.target)) return;
		if (event.repeat || spaceHeld) {
			event.preventDefault();
			return;
		}
		event.preventDefault();
		spaceHeld = true;
		engagePushToTalk();
	}

	function onKeyup(event: KeyboardEvent): void {
		if (event.key !== ' ' || !spaceHeld) return;
		event.preventDefault();
		spaceHeld = false;
		releasePushToTalk();
	}

	onMount(() => {
		if (!browser) return;
		window.addEventListener('keydown', onKeydown);
		window.addEventListener('keyup', onKeyup);
		// 1 s tick is enough resolution for an MM:SS clock + the
		// 2-min warning window without burning frames.
		tickTimer = setInterval(() => (now = Date.now()), 1000);
	});

	onDestroy(() => {
		// onDestroy fires during Svelte 5 SSR cleanup too — guard so
		// `window` lookups don't crash the server render.
		if (!browser) return;
		window.removeEventListener('keydown', onKeydown);
		window.removeEventListener('keyup', onKeyup);
		if (tickTimer !== null) clearInterval(tickTimer);
		if (spaceHeld) {
			spaceHeld = false;
			releasePushToTalk();
		}
	});

	function toggleMode(): void {
		// Applies LIVE: `setPushToTalkMode` rebuilds turn detection + re-arms/mutes
		// the mic on the active call (no reconnect) and updates the shared store
		// the buttons read.
		setPushToTalkMode(!ptt);
	}
</script>

{#if visible}
	<aside class="vc-overlay" aria-label="Voice call">
		<header class="vc-overlay__head">
			<div class="vc-overlay__orb">
				<VoiceOrb
					state={call.state === 'connecting'
							|| call.state === 'reconnecting'
							|| call.state === 'rotating'
						? 'idle'
						: transcript.assistantSpeaking
							? 'playing'
							: 'capturing'}
					analyser={transcript.assistantSpeaking ? null : $voiceMicAnalyser}
					size={32}
				/>
			</div>
			<div class="vc-overlay__meta">
				<span class="vc-overlay__title">
					Voice call
					{#if timerLabel}
						<span class="vc-overlay__timer">
							{timerLabel}
						</span>
					{/if}
				</span>
				<span class="vc-overlay__status" aria-live="polite">
					{status}{call.model ? ` · ${call.model}` : ''}
				</span>
			</div>
			<button
				type="button"
				class="vc-overlay__end"
				on:click={stopVoiceCall}
				aria-label="End voice call"
				title="End voice call"
			>
				<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<line x1="18" y1="6" x2="6" y2="18" />
					<line x1="6" y1="6" x2="18" y2="18" />
				</svg>
			</button>
		</header>
		{#if call.boundary && call.boundary.audience === 'untrusted' && call.state === 'connected'}
			<!-- Shown only for an untrusted call, and deliberately not actionable:
			     there is no control here to raise the boundary, because a room
			     cannot ask to be trusted. -->
			<div class="vc-overlay__boundary" aria-live="polite">
				Shared room · {assistantName} is answering as the outward agent, without access to your private context
			</div>
		{/if}
		{#if call.activationPhrase && call.state === 'connected'}
			<div class="vc-overlay__address" aria-live="polite">
				Start with “{call.activationPhrase}”
			</div>
		{/if}

		{#if turns.length > 0}
			<div class="vc-overlay__captions" bind:this={scrollerEl}>
				{#each turns as turn (turn.id)}
					<div class="vc-turn vc-turn--{turn.speaker}" class:vc-turn--streaming={!turn.done}>
						<span class="vc-turn__role">
							{turn.speaker === 'user' ? 'You' : assistantName}
						</span>
						<span class="vc-turn__text">
							{turn.text}{#if !turn.done && turn.speaker === 'assistant'}<span class="vc-turn__cursor" aria-hidden="true">▍</span>{/if}
						</span>
					</div>
				{/each}
			</div>
		{:else}
			<div class="vc-overlay__hint">
				{call.state === 'connecting'
					? 'Setting up voice…'
					: call.state === 'reconnecting'
						? 'Reconnecting your voice session…'
						: call.state === 'rotating'
							? 'Refreshing the voice session — hang on a moment.'
							: ptt
								? 'Hold the button (or space) and talk.'
								: 'Start talking. Captions will appear here.'}
			</div>
		{/if}

		{#if ptt}
			<div class="vc-overlay__ptt-wrap">
				<button
					bind:this={pttBtnEl}
					type="button"
					class="vc-overlay__ptt"
					class:vc-overlay__ptt--active={pttActive}
					on:pointerdown={pttPress}
					on:pointerup={pttRelease}
					on:pointercancel={pttRelease}
					on:contextmenu|preventDefault
					disabled={call.state !== 'connected'}
					aria-pressed={pttActive}
					aria-label={pttActive ? 'Release to send' : 'Push to talk'}
					title="Hold to talk — release to send (or hold space)"
				>
					<VoiceOrb
						state={pttActive ? 'capturing' : 'idle'}
						analyser={pttActive ? $voiceMicAnalyser : null}
						size={36}
					/>
					<span class="vc-overlay__ptt-label">
						{pttActive ? 'Release to send' : 'Hold to talk'}
					</span>
				</button>
			</div>
		{/if}

		<footer class="vc-overlay__foot">
			<button
				type="button"
				class="vc-overlay__mode"
				on:click={toggleMode}
				title="Toggle push-to-talk vs hands-free. Applies immediately."
			>
				{ptt ? 'Push-to-talk' : 'Hands-free'}
			</button>
			{#if turns.length > 0}
				<button type="button" class="vc-overlay__clear" on:click={clearVoiceTranscript}>
					Clear transcript
				</button>
			{/if}
		</footer>
	</aside>
{/if}

<style>
	.vc-overlay {
		position: fixed;
		bottom: calc(env(safe-area-inset-bottom, 0px) + 16px);
		right: 16px;
		width: min(360px, calc(100vw - 32px));
		max-height: min(60vh, 480px);
		display: flex;
		flex-direction: column;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 14px;
		box-shadow: 0 12px 36px rgba(0, 0, 0, 0.18);
		z-index: 95;
		overflow: hidden;
		font-family: var(--font-primary);
		color: var(--text-primary, #2d3436);
	}

	.vc-overlay__head {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 12px 12px 12px 14px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.vc-overlay__boundary {
		padding: 7px 14px;
		font-size: 11px;
		font-weight: 550;
		color: var(--text-muted, #6b7280);
		background: var(--surface-subtle, rgba(0, 0, 0, 0.04));
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.vc-overlay__address {
		padding: 7px 14px;
		font-size: 11px;
		font-weight: 550;
		color: var(--text-muted, #6b7280);
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.08));
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.vc-overlay__orb {
		display: inline-flex;
		flex: 0 0 auto;
	}

	.vc-overlay__meta {
		flex: 1 1 auto;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.vc-overlay__title {
		font-size: 13px;
		font-weight: 700;
		letter-spacing: 0.02em;
		display: inline-flex;
		align-items: baseline;
		gap: 8px;
	}

	.vc-overlay__timer {
		font-family: var(--font-mono, 'JetBrains Mono', monospace);
		font-size: 10.5px;
		font-weight: 500;
		color: var(--text-muted, #6b7280);
		letter-spacing: 0.02em;
	}

	.vc-overlay__status {
		font-size: 11px;
		color: var(--text-muted, #6b7280);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.vc-overlay__end {
		flex: 0 0 auto;
		width: 32px;
		height: 32px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		background: transparent;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 50%;
		color: var(--color-error, #c0392b);
		cursor: pointer;
	}

	.vc-overlay__end:hover {
		background: color-mix(in srgb, var(--color-error, #c0392b) 12%, transparent);
	}

	.vc-overlay__captions {
		flex: 1 1 auto;
		min-height: 0;
		overflow-y: auto;
		padding: 10px 14px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.vc-turn {
		display: flex;
		flex-direction: column;
		gap: 2px;
		font-size: 13px;
		line-height: 1.4;
	}

	.vc-turn__role {
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-muted, #6b7280);
	}

	.vc-turn--user .vc-turn__role {
		color: var(--accent-primary, #c2502a);
	}

	.vc-turn__text {
		color: var(--text-primary, #2d3436);
		word-wrap: break-word;
		overflow-wrap: anywhere;
	}

	.vc-turn__cursor {
		display: inline-block;
		margin-left: 2px;
		color: var(--accent-primary, #c2502a);
		animation: vc-blink 1s steps(2) infinite;
	}

	.vc-overlay__ptt-wrap {
		flex: 0 0 auto;
		display: flex;
		justify-content: center;
		padding: 10px 14px 14px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.vc-overlay__ptt {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		padding: 8px 16px 8px 10px;
		min-height: 52px;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 999px;
		color: var(--text-primary, #2d3436);
		font: 600 13px var(--font-primary);
		cursor: pointer;
		user-select: none;
		-webkit-user-select: none;
		touch-action: none;
		box-shadow: 0 2px 6px rgba(0, 0, 0, 0.08);
		transition: transform 80ms ease, background 100ms ease, border-color 100ms ease;
	}

	.vc-overlay__ptt:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.vc-overlay__ptt--active {
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 14%, var(--bg-card, #fff));
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
		transform: scale(1.02);
	}

	.vc-overlay__ptt-label {
		white-space: nowrap;
	}

	.vc-overlay__foot {
		flex: 0 0 auto;
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 6px;
		padding: 6px 10px 8px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.vc-overlay__mode,
	.vc-overlay__clear {
		padding: 4px 10px;
		font-size: 11px;
		font-weight: 500;
		color: var(--text-muted, #6b7280);
		background: transparent;
		border: 1px solid transparent;
		border-radius: 999px;
		cursor: pointer;
	}

	.vc-overlay__mode:hover,
	.vc-overlay__clear:hover {
		color: var(--text-primary, #2d3436);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.vc-overlay__hint {
		padding: 18px 14px 20px;
		font-size: 12px;
		color: var(--text-muted, #6b7280);
		text-align: center;
	}

	@keyframes vc-blink {
		from { opacity: 1; }
		to   { opacity: 0; }
	}

	@media (max-width: 767px) {
		.vc-overlay {
			right: 8px;
			left: 8px;
			width: auto;
			bottom: calc(env(safe-area-inset-bottom, 0px) + 12px);
			max-height: 50vh;
		}
	}
</style>
