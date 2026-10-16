import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	enrollAmbientBrowser,
	fetchAmbientStatus,
	fetchRecentMeetings,
	fetchRecentWatchSessions,
	fetchUpcomingMeetings,
	decideWhatsAppRing,
	fetchWhatsAppRing,
	notifyWhatsAppBrowserBar,
	readWhatsAppBrowserBar,
	writeWhatsAppBrowserBar,
	whatsAppCallActions,
	whatsAppEngineChoices,
	whatsAppMouthCorrection,
	notifyWhatsAppTab,
	whatsAppRingSummary,
	whatsAppSessionBadge,
	whatsAppShowsOnActiveList,
	joinMeeting,
	retargetScreenObservation,
	saveAmbientConfig,
	setMeetingPaused,
	startMeetingListen,
	startScreenObservation,
	stopMeetingSession,
	stopScreenObservation
} from './api';

describe('observe API', () => {
	it('normalizes recent and upcoming listing payloads', async () => {
		installFetchMock([
			{
				match: '/meetings/upcoming',
				handle: () =>
					jsonResponse({
						events: [{ event_id: 'event-1', title: 'Review', live_now: true }],
						errors: [{ account: 'personal', error: 'calendar unavailable' }]
					})
			},
			{
				match: '/meetings',
				handle: () => jsonResponse({ recent: [{ thread_id: 'thread-1' }] })
			}
		]);

		expect(await fetchRecentMeetings()).toEqual([{ thread_id: 'thread-1' }]);
		expect(await fetchUpcomingMeetings(true)).toEqual({
			events: [{ event_id: 'event-1', title: 'Review', live_now: true }],
			errors: [{ account: 'personal', error: 'calendar unavailable' }]
		});
	});

	it('sends complete listen and join requests', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/meetings/listen', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/meetings/join', handle: () => jsonResponse({}) }
		]);

		await startMeetingListen({ title: 'Planning', url: null, mic: true });
		await joinMeeting({ url: 'https://meet.example/abc', title: null });
		await joinMeeting({
			url: 'https://meet.example/abc',
			title: null,
			realtime_profile: 'voice_realtime_grok'
		});

		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			title: 'Planning',
			url: null,
			mic: true
		});
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			url: 'https://meet.example/abc',
			title: null
		});
		expect(JSON.parse(String(calls[2]?.init?.body))).toEqual({
			url: 'https://meet.example/abc',
			title: null,
			realtime_profile: 'voice_realtime_grok'
		});
	});

	it('reads and posts the WhatsApp call ring', async () => {
		const { calls } = installFetchMock([
			{
				match: '/meetings/whatsapp-ring',
				handle: () =>
					jsonResponse({
						phase: 'incoming',
						profile: 'voice_realtime_openai_backend',
						profiles: [{ id: 'voice_realtime_openai_backend', label: 'GPT Realtime 2' }],
						url: 'https://web.whatsapp.com/',
						title: 'WhatsApp call',
						session_id: null,
						error: null,
						starting: false,
						leaving: false
					})
			}
		]);

		expect((await fetchWhatsAppRing())?.phase).toBe('incoming');
		await decideWhatsAppRing({
			decision: 'approved',
			profile: 'voice_realtime_gpt_live_1'
		});
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			decision: 'approved',
			profile: 'voice_realtime_gpt_live_1'
		});
	});

	it('lists the composer realtime models and keeps an unavailable engine disabled', () => {
		const choices = whatsAppEngineChoices(
			[
				{
					profile_id: 'voice_realtime_default',
					label: 'GPT Realtime 2.1 Mini (Default)',
					available: true,
					mode: 'assistant'
				},
				{
					profile_id: 'voice_realtime_grok',
					label: 'Grok Voice',
					available: false,
					unavailable_reason: 'XAI_API_KEY is not set',
					mode: 'assistant'
				},
				{
					profile_id: 'voice_realtime_gemini_translate_en',
					label: 'Gemini translate',
					available: true,
					mode: 'translation'
				}
			],
			[{ id: 'voice_realtime_openai_backend', label: 'GPT Realtime 2', available: true }],
			'voice_realtime_kept'
		);
		expect(choices.map((choice) => choice.id)).toEqual([
			'voice_realtime_default',
			'voice_realtime_grok',
			'voice_realtime_kept'
		]);
		expect(choices[1]).toMatchObject({
			available: false,
			unavailable_reason: 'XAI_API_KEY is not set'
		});
	});

	it('keeps the browser WebRTC engine off the WhatsApp mouth list', () => {
		const choices = whatsAppEngineChoices(
			[
				{
					profile_id: 'voice_realtime_default',
					label: 'GPT Realtime 2.1 Mini (Default)',
					available: true,
					mode: 'assistant',
					topology: 'direct_peer_to_peer'
				},
				{
					profile_id: 'voice_realtime_openai_backend',
					label: 'GPT Realtime 2.1',
					available: true,
					mode: 'assistant',
					topology: 'backend_proxied'
				}
			],
			[{ id: 'voice_realtime_default', label: 'GPT Realtime 2.1 Mini (Default)' }],
			'voice_realtime_default',
			{ mouthsOnly: true }
		);
		expect(choices.map((choice) => choice.id)).toEqual(['voice_realtime_openai_backend']);
		expect(whatsAppMouthCorrection('voice_realtime_default', 'idle')).toBe(
			'voice_realtime_openai_backend'
		);
		expect(whatsAppMouthCorrection('voice_realtime_default', 'approved')).toBeNull();
		expect(whatsAppMouthCorrection('voice_realtime_grok', 'idle')).toBeNull();
	});

	it('lists a WhatsApp call on the active deck before a meeting session exists', () => {
		expect(whatsAppShowsOnActiveList({ phase: 'idle', surface: 'none' }, [])).toBe(false);
		expect(whatsAppShowsOnActiveList({ phase: 'incoming', surface: 'ringing' }, [])).toBe(true);
		expect(whatsAppShowsOnActiveList({ phase: 'ongoing', surface: 'ongoing' }, [])).toBe(true);
		expect(
			whatsAppShowsOnActiveList({ phase: 'approved', surface: 'ringing' }, [
				{ url: 'https://web.whatsapp.com/' }
			])
		).toBe(false);
		expect(whatsAppShowsOnActiveList({ phase: 'approved', surface: 'ringing' }, [])).toBe(true);
		expect(whatsAppShowsOnActiveList({ phase: 'failed', surface: 'none' }, [])).toBe(false);
		expect(whatsAppSessionBadge('incoming', 'Magician')).toBe('Incoming WhatsApp call');
		expect(whatsAppSessionBadge('approved', 'Magician')).toBe('Joined as Magician');
	});

	it('uses the ring list until the composer catalog loads', () => {
		expect(
			whatsAppEngineChoices(
				[],
				[{ id: 'voice_realtime_gemini_38_live', label: 'Gemini 3.8 Live' }],
				'voice_realtime_gemini_38_live'
			).map((choice) => choice.id)
		).toEqual(['voice_realtime_gemini_38_live']);
	});

	it('keeps the WhatsApp browser buttons until Observe hides them', () => {
		const memory = new Map<string, string>();
		const storage = {
			getItem: (key: string) => memory.get(key) ?? null,
			setItem: (key: string, value: string) => {
				memory.set(key, value);
			}
		};
		expect(readWhatsAppBrowserBar(storage)).toBe(true);
		writeWhatsAppBrowserBar(false, storage);
		expect(readWhatsAppBrowserBar(storage)).toBe(false);
		writeWhatsAppBrowserBar(true, storage);
		expect(readWhatsAppBrowserBar(storage)).toBe(true);

		const seen: Array<{ enabled?: boolean }> = [];
		const host = {
			dispatchEvent(event: CustomEvent<{ enabled?: boolean }>) {
				seen.push(event.detail);
				return true;
			}
		};
		const previous = (globalThis as { window?: unknown }).window;
		(globalThis as { window?: unknown }).window = host;
		notifyWhatsAppBrowserBar(false);
		(globalThis as { window?: unknown }).window = previous;
		expect(seen).toEqual([{ enabled: false }]);
	});

	it('tells the WhatsApp tab when Observe joins or leaves', () => {
		const seen: Array<{ decision?: string; profile?: string }> = [];
		const host = {
			dispatchEvent(event: CustomEvent<{ decision?: string; profile?: string }>) {
				seen.push(event.detail);
				return true;
			}
		};
		const previous = (globalThis as { window?: unknown }).window;
		(globalThis as { window?: unknown }).window = host;
		notifyWhatsAppTab('join', 'voice_realtime_openai_backend');
		notifyWhatsAppTab('leave');
		(globalThis as { window?: unknown }).window = previous;
		expect(seen).toEqual([
			{ decision: 'join', profile: 'voice_realtime_openai_backend' },
			{ decision: 'leave', profile: '' }
		]);
	});

	it('shows Join only while a WhatsApp call is ringing', () => {
		expect(whatsAppCallActions('idle')).toEqual([]);
		expect(whatsAppCallActions('cancelled')).toEqual([]);
		expect(whatsAppCallActions('incoming', 'none')).toEqual(['join']);
		expect(whatsAppCallActions('incoming', 'ringing')).toEqual(['join', 'listen', 'cancel']);
		expect(whatsAppCallActions('ongoing', 'ongoing')).toEqual(['listen', 'cancel']);
		expect(whatsAppCallActions('approved')).toEqual(['leave']);
		expect(whatsAppCallActions('listening')).toEqual(['stop']);
		expect(whatsAppCallActions('failed', 'ringing')).toEqual(['join', 'listen']);
		expect(whatsAppCallActions('failed', 'ongoing')).toEqual(['listen']);
		expect(whatsAppRingSummary({ phase: 'idle', caller: null, surface: 'none', starting: false }, false)).toMatch(
			/Waiting for a WhatsApp call/
		);
		expect(
			whatsAppRingSummary(
				{ phase: 'incoming', caller: 'Ada', surface: 'ringing', starting: false },
				false
			)
		).toMatch(/Ada is calling/);
		expect(
			whatsAppRingSummary(
				{ phase: 'ongoing', caller: 'Ada', surface: 'ongoing', starting: false },
				false
			)
		).toMatch(/Ada is on a WhatsApp call/);
		expect(
			whatsAppRingSummary(
				{ phase: 'cancelled', caller: 'Ada', surface: 'ringing', starting: false },
				false
			)
		).toBe('Cancelling the WhatsApp call.');
	});

	it('treats a missing WhatsApp ring route as not ready', async () => {
		installFetchMock([
			{
				match: '/meetings/whatsapp-ring',
				handle: () => jsonResponse({ error: 'not found' }, { status: 404 })
			}
		]);
		expect(await fetchWhatsAppRing()).toBeNull();
		await expect(decideWhatsAppRing({ decision: 'approved' })).rejects.toThrow(
			/does not handle WhatsApp calls/
		);
	});

	it('encodes meeting control paths and propagates backend errors', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/pause', handle: () => jsonResponse({}) },
			{
				method: 'POST',
				match: '/stop',
				handle: () => jsonResponse({ error: 'capture already ended' }, { status: 409 })
			}
		]);

		await setMeetingPaused('session/with slash', false);
		expect(calls[0]?.url).toContain('/session%2Fwith%20slash/pause');
		await expect(stopMeetingSession('session-1')).rejects.toThrow('capture already ended');
	});

	it('caps recent screen sessions and treats malformed lists as empty', async () => {
		const sessions = Array.from({ length: 24 }, (_, index) => ({ id: `watch-${index}` }));
		installFetchMock([
			{ match: '/chat/sessions', handle: () => jsonResponse({ sessions }) }
		]);

		expect(await fetchRecentWatchSessions()).toHaveLength(20);
	});

	it('sends start, retarget, and stop screen-observation commands', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/observe/start', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/observe/retarget', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/observe/stop', handle: () => jsonResponse({}) }
		]);

		await startScreenObservation({
			purpose: 'Capture decisions',
			mode: 'watch',
			watch_for: 'deployment complete',
			deep_observation: true,
			audio: 'system'
		});
		await retargetScreenObservation({ watch_for: 'tests pass' });
		await stopScreenObservation();

		expect(JSON.parse(String(calls[0]?.init?.body))).toMatchObject({
			mode: 'watch',
			watch_for: 'deployment complete',
			audio: 'system'
		});
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({ watch_for: 'tests pass' });
		expect(calls[2]?.method).toBe('POST');
	});

	it('round-trips ambient status, consent, and pairing', async () => {
		const { calls } = installFetchMock([
			{
				match: '/ambient/status',
				handle: () =>
					jsonResponse({
						enabled: true,
						denylist: ['bank.example'],
						total_signals: 4,
						total_rejected: 1,
						last_signal_at: null,
						paired: true
					})
			},
			{ method: 'PUT', match: '/ambient/config', handle: () => jsonResponse({}) },
			{
				method: 'POST',
				match: '/ambient/enroll',
				handle: () => jsonResponse({ token: 'collector-token' })
			}
		]);

		expect(await fetchAmbientStatus()).toMatchObject({ enabled: true, paired: true });
		await saveAmbientConfig(false, ['bank.example']);
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			enabled: false,
			denylist: ['bank.example']
		});
		expect(await enrollAmbientBrowser()).toBe('collector-token');
	});

	it('surfaces malformed or failed mutations instead of reporting success', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/meetings/join',
				handle: () => jsonResponse({}, { status: 503 })
			}
		]);

		await expect(
			joinMeeting({ url: 'https://meet.example/abc', title: null })
		).rejects.toThrow('HTTP 503');
	});
});
