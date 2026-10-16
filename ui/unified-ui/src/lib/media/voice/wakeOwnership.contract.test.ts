import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = (path: string) => readFileSync(join(process.cwd(), path), 'utf8');
const composer = source('src/lib/magician/components/chat/VoiceControl.svelte');
const wakeService = source('src/lib/media/voice/wakeWord.ts');
const ambientOrb = source('src/routes/warroom/CoreStage.svelte');

describe('wake-word surface ownership', () => {
	it('does not expose or preload wake from the chat composer', () => {
		expect(composer).not.toContain('class="wake-toggle"');
		expect(composer).not.toContain('setWakeEnabled');
		expect(composer).not.toContain('preloadWakeWord');
		expect(composer).not.toContain('startWakeWord');
	});

	it('keeps wake dictation registration separate from composer push-to-talk', () => {
		expect(wakeService).toContain('let recordingTrigger: RecordingTrigger | null = null;');
		expect(wakeService).toContain('let wakeRecordingTrigger: RecordingTrigger | null = null;');
		expect(wakeService).toContain('await wakeRecordingTrigger()');
		expect(ambientOrb).toContain('registerWakeRecordingTrigger');
		expect(composer).toContain('registerRecordingTrigger');
	});

	it('has no webview bridge capable of controlling native Orb wake', () => {
		expect(wakeService).not.toContain("invoke('set_native_wake_enabled'");
		expect(wakeService).not.toContain("invoke('set_wake_phrase'");
		expect(wakeService).not.toContain("listen('native-wake'");
	});

	it('matches every configured wake spelling instead of silently selecting one', () => {
		expect(wakeService).toContain('export const effectiveWakePhrasesStore = derived(');
		expect(wakeService).toContain('phrases.some((phrase) => finalizedWakePhraseMatches');
		expect(wakeService).not.toContain('constrainedWakeNamesForAgent($agent)[0]');
	});

	it('uses ready language instead of engineering lifecycle terms in public wake copy', () => {
		expect(ambientOrb).toContain('ready — say');
		expect(ambientOrb).toContain('ready — tap for the next turn');
		expect(ambientOrb).toContain('Ambient Dictation ready for your wake phrase');
		expect(ambientOrb).toContain("? 'READY'");
		expect(ambientOrb).not.toContain('armed — say');
		expect(ambientOrb).not.toContain('armed — tap');
		expect(ambientOrb).not.toContain('WAKE ARMED');
	});
});
