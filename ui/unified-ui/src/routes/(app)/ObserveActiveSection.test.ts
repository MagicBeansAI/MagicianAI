import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(join(process.cwd(), 'src/routes/(app)/observe/+page.svelte'), 'utf8');

describe('Observe Active section placement', () => {
	it('promotes live captures above the panes and the audio profiles', () => {
		const promoted = source.search(/\{#if anyActive\}\s*\{@render activeCaptures\(true\)\}/);
		const tabs = source.indexOf('class="observe-tabs"');
		const audioControls = source.indexOf('<section class="audio-surface-bar"');
		expect(promoted).toBeGreaterThanOrEqual(0);
		expect(tabs).toBeGreaterThan(promoted);
		expect(audioControls).toBeGreaterThan(promoted);
	});

	it('keeps the idle capture render for loading and the last summary, without an empty card', () => {
		const resting = source.search(/\{#if !anyActive\}\s*\{@render activeCaptures\(false\)\}/);
		expect(resting).toBeGreaterThanOrEqual(0);
		expect(source).not.toContain('No active captures');
		expect(source).toContain('Loading active captures');
		expect(source).toContain('Last observation — summary');
	});

	it('splits the console into Now, Sources, Audio, and Notes', () => {
		expect(source).toContain("type ObservePane = 'now' | 'sources' | 'audio' | 'notes'");
		expect(source).toContain('hidden={pane !== \'now\'}');
		expect(source).toContain('hidden={pane !== \'sources\'}');
		expect(source).toContain('hidden={pane !== \'audio\'}');
		expect(source).toContain('hidden={pane !== \'notes\'}');
		expect(source).toContain('id="observe-upcoming"');
		expect(source).toContain('id="observe-recent"');
		expect(source).toContain('Listen to meeting');
		expect(source).toContain('Mail &amp; chat');
		expect(source).toContain('Observe calendar');
		expect(source).toContain('Observe tabs');
		expect(source).toContain('Save denylist');
		expect(source).toContain('Use for verification codes');
	});

	it('uses one canonical Active implementation with a promoted compact variant', () => {
		expect(source.match(/{#snippet activeCaptures\(promoted: boolean\)}/g)).toHaveLength(1);
		expect(source).toContain('class:observe-active-section--promoted={promoted}');
		expect(source).toContain('.observe-active-section--promoted .session-card');
		expect(source).toContain("'head actions'");
		expect(source).toContain("'title actions'");
		expect(source).toContain("'sources actions'");
		expect(source).toContain('.observe-active-section--promoted .session-head .status');
		expect(source).toContain('white-space: nowrap');
		expect(source).toContain("s.status.trim().toLowerCase() !== 'listening'");
		expect(source).toContain('s.thread_id.trim() !== activeSessionLabel(s).trim()');
		expect(source).toContain('.session-head > .status-badge');
	});

	it('replaces duplicate Upcoming actions for a matching active session', () => {
		expect(source).toContain('matchActiveSessionsToUpcoming(upcoming, active)');
		expect(source).toContain('upcomingTitleBySessionId.get(session.session_id)');
		expect(source).toContain('{#if activeSession}');
		expect(source).toContain("'Listening now'");
		expect(source).toMatch(/\{:else\}[\s\S]*?listenToEvent\(ev\)[\s\S]*?joinEvent\(ev\)/);
	});

	it('persists a uniquely inferred calendar identity before starting a manual listener', () => {
		expect(source).toContain(
			'resolveMeetingListenMetadata(listenTitle, listenUrl, upcoming)'
		);
		expect(source).toContain('title: metadata.title');
		expect(source).toContain('url: metadata.url');
	});
});
