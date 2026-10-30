import { afterEach, expect, it, vi } from 'vitest';
import { transcribeAudio, transcribeAudioStreaming } from './stt/sttClient';
import { providerSpeakBlocks } from './tts/providerTts';

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/stores/scopeIdentityStore', () => ({
    scopedRequestHeaders: (extra: Record<string, string>) => new Headers({ Authorization: 'Bearer scoped-test', ...extra })
}));
vi.mock('$lib/media/session', () => ({ publishMediaEvent: vi.fn() }));
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it.each([false, true])('links %s streaming dictation to the supplied chat without changing authentication', async (streaming) => {
    const opts = { audio: new Blob(['audio'], { type: 'audio/wav' }), provider: 'openai', chatSessionId: 'chat-recorded' };
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(streaming
        ? new Response('data: {"type":"final","transcript":"hello","model":"stt"}\n\n')
        : Response.json({ transcript: 'hello', model: 'stt' }));
    const work = streaming ? transcribeAudioStreaming(opts, {}) : transcribeAudio(opts);
    opts.chatSessionId = 'another-chat';
    await work;
    const headers = new Headers(vi.mocked(fetch).mock.calls[0][1]?.headers);
    expect(headers.get('X-Chat-Session-Id')).toBe('chat-recorded');
    expect(headers.get('Authorization')).toBe('Bearer scoped-test');
});
it('links reply speech to the explicit chat and leaves standalone previews unlinked', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response('{"type":"done"}\n'));
    const request = { messageId: 'message-1', blocks: [{ text: 'Hello' }], chatSessionId: 'chat-reply' };
    await providerSpeakBlocks(request);
    const headers = new Headers(vi.mocked(fetch).mock.calls[0][1]?.headers);
    expect(headers.get('X-Chat-Session-Id')).toBe('chat-reply');
    expect(headers.get('Content-Type')).toBe('application/json');
    await providerSpeakBlocks({ messageId: 'preview', blocks: [{ text: 'Preview' }] });
    expect(new Headers(vi.mocked(fetch).mock.calls[1][1]?.headers).has('X-Chat-Session-Id')).toBe(false);
});
