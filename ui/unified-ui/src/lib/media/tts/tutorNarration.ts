/**
 * Tutor narration is part of the tutor timeline, not generic chat auto-speak.
 *
 * It intentionally ignores the `autoSpeak` preference while still using the
 * selected TTS provider/voice/rate. It acquires a temporary tutor audio-focus
 * lock so normal chat/desktop assistant playback is suppressed without muting
 * the tutor itself.
 */

import { browser } from '$app/environment';
import { get } from 'svelte/store';

import { refreshMediaPreferences } from '$lib/media/preferences';
import { ensureMediaSessionStarted } from '$lib/media/session';
import { mediaProvidersStore } from '$lib/media/providers';
import { isBrowserTtsAvailable, speak as speakBrowserTts, cancelCurrent as cancelBrowserTts } from './browserTts';
import { resolveTtsProviderChoice } from './providerChoices';
import { cancelProviderSpeak, providerSpeakBlocks } from './providerTts';
import { ttsStore } from './store';
import type { SpeechBlock } from './speechTags';
import { acquireTutorAudioFocus } from './tutorAudioFocus';

export type TutorNarrationStatus = 'completed' | 'cancelled' | 'error' | 'skipped';

export interface TutorNarrationOptions {
	stepId: string;
	text: string;
	onStart?: () => void;
}

let narrationGeneration = 0;
let activeFocusRelease: (() => Promise<void>) | null = null;

function releaseActiveTutorFocus(): void {
	const release = activeFocusRelease;
	activeFocusRelease = null;
	if (release) void release();
}

export function cancelTutorNarration(reason: 'user' | 'replaced' = 'replaced'): void {
	narrationGeneration += 1;
	cancelProviderSpeak(reason);
	cancelBrowserTts(reason);
	releaseActiveTutorFocus();
	ttsStore.setActive(null);
}

export async function speakTutorNarration(
	opts: TutorNarrationOptions
): Promise<TutorNarrationStatus> {
	if (!browser) return 'skipped';
	const text = opts.text.trim();
	if (!text) return 'skipped';

	const generation = (narrationGeneration += 1);
	const releaseFocus = await acquireTutorAudioFocus();
	if (generation !== narrationGeneration) {
		await releaseFocus();
		return 'cancelled';
	}
	const previousRelease = activeFocusRelease;
	activeFocusRelease = releaseFocus;
	if (previousRelease) void previousRelease();
	cancelProviderSpeak('replaced');
	cancelBrowserTts('replaced');
	await refreshMediaPreferences().catch(() => null);
	await ensureMediaSessionStarted({ displayLabel: 'Tutor overlay' }).catch(() => null);
	const providers = await mediaProvidersStore.refresh();
	const state = get(ttsStore);
	const browserOk = isBrowserTtsAvailable();
	const ttsSelection = resolveTtsProviderChoice(providers, browserOk);
	if (ttsSelection.mode === 'none') {
		if (activeFocusRelease === releaseFocus) activeFocusRelease = null;
		await releaseFocus();
		return 'skipped';
	}

	const messageId = `tutor-step:${opts.stepId}`;
	const blocks: SpeechBlock[] = [{ text }];
	ttsStore.setActive(messageId);
	const finish = (status: TutorNarrationStatus): TutorNarrationStatus => {
		if (generation === narrationGeneration && activeFocusRelease === releaseFocus) {
			activeFocusRelease = null;
			ttsStore.setActive(null);
		}
		void releaseFocus();
		return status;
	};

	if (ttsSelection.mode === 'backend') {
		return new Promise<TutorNarrationStatus>((resolve) => {
			void providerSpeakBlocks(
				{
					messageId,
					blocks,
					provider: null,
					voice: null,
					model: null,
					rate: state.prefs.rate,
					onStart: opts.onStart
				},
				(status) => resolve(finish(status))
			).catch(() => resolve(finish('error')));
		});
	}

	return new Promise<TutorNarrationStatus>((resolve) => {
		speakBrowserTts(
			{
				messageId,
				text,
				voiceName: state.prefs.voiceName,
				rate: state.prefs.rate,
				pitch: state.prefs.pitch,
				onStart: opts.onStart
			},
			(status) => resolve(finish(status))
		);
	});
}
