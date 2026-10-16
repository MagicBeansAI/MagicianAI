import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { installFetchMock, jsonResponse } from '../../test/browser';
import { mediaPreferencesStore } from './preferences';
import VoicePersonaPicker from './VoicePersonaPicker.svelte';

beforeEach(() => mediaPreferencesStore.reset());
afterEach(cleanup);

describe('VoicePersonaPicker', () => {
	it('saves and resets only the selected TTS binding', async () => {
		const { calls } = installFetchMock([{
			method: 'PUT', match: '/media/preferences', handle: ({ init }) =>
				jsonResponse({ tts_voices: JSON.parse(String(init?.body)).tts_voices })
		}]);
		const user = userEvent.setup();
		render(VoicePersonaPicker, { bindingId: 'bulbul-v4', label: 'Speaking voice', defaultVoice: 'shubh_enhi_ads',
			choices: [{ id: 'shubh_enhi_ads', label: 'Shubh' }, { id: 'aparna_hi_customer', label: 'Aparna' }] });
		const select = screen.getByRole('combobox', { name: 'Speaking voice' });
		await user.click(select);
		await user.click(screen.getByRole('option', { name: 'Aparna' }));
		await waitFor(() => expect(select).toHaveTextContent('Aparna'));
		expect(JSON.parse(String(calls[0].init?.body))).toMatchObject({ tts_voices: { 'bulbul-v4': 'aparna_hi_customer' } });
		await user.click(select);
		await user.click(screen.getByRole('option', { name: /Provider default/ }));
		await waitFor(() => expect(select).toHaveTextContent('Provider default'));
		expect(JSON.parse(String(calls[1].init?.body))).toMatchObject({ tts_voices: { 'bulbul-v4': '' } });
	});

	it('uses realtime preferences and explains when the voice takes effect', async () => {
		const { calls } = installFetchMock([{
			method: 'PUT', match: '/media/preferences', handle: () => jsonResponse({ realtime_voices: { grok: 'ara' } })
		}]);
		const user = userEvent.setup();
		render(VoicePersonaPicker, { kind: 'realtime', bindingId: 'grok', choices: [{ id: 'ara', label: 'Ara' }] });
		await user.click(screen.getByRole('combobox'));
		await user.click(screen.getByRole('option', { name: 'Ara' }));
		await waitFor(() => expect(calls).toHaveLength(1));
		expect(JSON.parse(String(calls[0].init?.body))).toMatchObject({ realtime_voices: { grok: 'ara' } });
		expect(screen.getByText('Applies to your next call.')).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Preview voice' })).toBeNull();
	});

	it('filters a large catalog without losing the selected value', async () => {
		const user = userEvent.setup();
		render(VoicePersonaPicker, { bindingId: 'kokoro', choices: Array.from({ length: 30 }, (_, i) => ({ id: `voice-${i}`, label: `Voice ${i}` })) });
		expect(screen.queryByRole('listbox')).toBeNull();
		await user.click(screen.getByRole('combobox'));
		await user.type(screen.getByRole('searchbox'), 'Voice 29');
		expect(screen.getAllByRole('option')).toHaveLength(1);
		expect(screen.getByRole('option', { name: 'Voice 29' })).toBeTruthy();
	});

	it('does not present fallback audio as a preview of the selected provider', async () => {
		const { calls } = installFetchMock([{
			method: 'POST', match: '/media/tts/synthesize', handle: () =>
				new Response('audio', { headers: { 'Content-Type': 'audio/wav', 'X-Tts-Fallback': 'true' } })
		}]);
		const user = userEvent.setup();
		render(VoicePersonaPicker, { bindingId: 'grok', defaultVoice: 'ara', choices: [{ id: 'ara', label: 'Ara' }] });
		await user.click(screen.getByRole('button', { name: 'Preview voice' }));
		expect(await screen.findByRole('alert')).toHaveTextContent('This provider is unavailable');
		expect(JSON.parse(String(calls[0].init?.body))).toMatchObject({ provider: 'grok', voice: 'ara' });
	});
});
