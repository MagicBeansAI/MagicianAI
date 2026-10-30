import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

type Module = typeof import('./chatHarnessPreferenceStore');

/** A fresh store per test: its "chosen" flag and value load from localStorage. */
async function freshStore(): Promise<Module> {
	vi.resetModules();
	return await import('./chatHarnessPreferenceStore');
}

function roster(chat_current: string, extra: Partial<{ chat_model: string; unavailable: boolean }> = {}) {
	return {
		engines: [
			{ name: 'magician', installed: true },
			{ name: 'claude_code', installed: true, models: ['opus'] },
			{ name: 'codex', installed: true },
			{ name: 'grok', installed: false }
		],
		chat_current,
		...extra
	};
}

describe('chat harness preference', () => {
	beforeEach(() => {
		localStorage.clear();
	});

	it('sends no choice until this browser has one, so the server default applies', async () => {
		const { composerChatChoice } = await freshStore();
		expect(composerChatChoice('balanced')).toBeNull();
	});

	it('waits for model restoration while native engine choices use their own model', async () => {
		const { chatHarnessPreferenceStore, composerChatChoice } = await freshStore();
		chatHarnessPreferenceStore.select('magician');
		expect(() => composerChatChoice('balanced')).toThrow('still loading');
		chatHarnessPreferenceStore.select('claude_code', 'opus');
		expect(composerChatChoice()).toEqual({ harness_engine: 'claude_code', harness_model: 'opus' });
	});

    it('sends typed voice model/thinking and native engine choices on the model API', async () => {
        const { chatHarnessPreferenceStore, composerChatChoice } = await freshStore();
        const { chatProfileStore } = await import('./chatProfileStore');
        const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify({
            models: [{ id: 'test/luna', provider: 'openai', connection: 'openai', model: 'gpt-6-luna', label: 'Luna',
                thinking: { mode: 'effort', levels: ['none', 'medium'], default: 'medium' }, limits: null,
                capabilities: { tools: true, vision: true },
                choices: { auto: 'model/test/luna/auto', none: 'model/test/luna/none' } }],
            default_selection: { model_id: 'test/luna', thinking: 'auto' }
        }), { status: 200 }));
        try {
            await chatProfileStore.load();
            chatHarnessPreferenceStore.select('magician');
            expect(composerChatChoice('model/test/luna/none')).toEqual({ harness_engine: 'magician', model_selection: { model_id: 'test/luna', thinking: 'none' } });
            chatHarnessPreferenceStore.select('claude_code', 'opus');
            expect(composerChatChoice()).toEqual({ harness_engine: 'claude_code', harness_model: 'opus' });
        } finally { fetch.mockRestore(); }
    });

	it('follows the server until a local pick, and a later server change beats that pick', async () => {
		let module = await freshStore();
		module.chatHarnessPreferenceStore.reconcileWithRoster(roster('claude_code'));
		expect(get(module.chatHarnessPreferenceStore).engine).toBe('claude_code');

		module.chatHarnessPreferenceStore.select('codex');
		module.chatHarnessPreferenceStore.reconcileWithRoster(roster('claude_code'));
		expect(get(module.chatHarnessPreferenceStore).engine).toBe('codex');

		// The server moved while this browser was closed: the newer choice wins.
		module = await freshStore();
		module.chatHarnessPreferenceStore.reconcileWithRoster(roster('magician'));
		expect(get(module.chatHarnessPreferenceStore).engine).toBe('magician');
	});

	it('never reconciles against a stand-in roster from a failed request', async () => {
		const { chatHarnessPreferenceStore } = await freshStore();
		chatHarnessPreferenceStore.reconcileWithRoster(roster('claude_code'));
		chatHarnessPreferenceStore.select('codex');
		chatHarnessPreferenceStore.reconcileWithRoster(roster('magician', { unavailable: true }));
		expect(get(chatHarnessPreferenceStore).engine).toBe('codex');
	});

	it('falls back to Magician when the picked engine is not installed here', async () => {
		const { chatHarnessPreferenceStore } = await freshStore();
		chatHarnessPreferenceStore.reconcileWithRoster(roster('magician'));
		chatHarnessPreferenceStore.select('grok');
		chatHarnessPreferenceStore.reconcileWithRoster(roster('magician'));
		expect(get(chatHarnessPreferenceStore).engine).toBe('magician');
	});

	it('adopts a server chat engine change over a local pick', async () => {
		const { chatHarnessPreferenceStore, handleChatEngineUpdatedEnvelope } = await freshStore();
		chatHarnessPreferenceStore.select('codex');
		handleChatEngineUpdatedEnvelope({
			event_type: 'chat.engine.updated',
			payload: { chat_current: 'claude_code', chat_model: 'opus' }
		});
		expect(get(chatHarnessPreferenceStore)).toEqual({ engine: 'claude_code', model: 'opus' });
	});

	it('ignores other events and malformed payloads', async () => {
		const { chatHarnessPreferenceStore, handleChatEngineUpdatedEnvelope } = await freshStore();
		chatHarnessPreferenceStore.select('codex');
		handleChatEngineUpdatedEnvelope({ event_type: 'ui.preferences.updated', payload: { chat_current: 'pi' } });
		handleChatEngineUpdatedEnvelope({ event_type: 'chat.engine.updated', payload: { chat_current: '' } });
		handleChatEngineUpdatedEnvelope({ event_type: 'chat.engine.updated', payload: null });
		expect(get(chatHarnessPreferenceStore).engine).toBe('codex');
	});
});

describe('independent chat composers', () => {
	it('keeps each scoped session choice through switching and other composers', async () => {
		localStorage.clear();
		const { createChatHarnessPreferenceStore } = await freshStore();
		const first = createChatHarnessPreferenceStore();
		const second = createChatHarnessPreferenceStore();
		first.activateSession('owner/workspace/chat-A');
		first.select('claude_code', 'opus');
		second.activateSession('owner/workspace/chat-B');
		second.select('codex', 'gpt-6.1-sol');
		expect(get(first)).toEqual({engine:'claude_code',model:'opus'});
		first.activateSession('owner/workspace/chat-B');
		expect(get(first)).toEqual(get(second));
		first.activateSession('owner/workspace/chat-A');
		expect(get(first)).toEqual({engine:'claude_code',model:'opus'});
	});
	it('keeps an existing session pin when Settings changes the default', async () => {
		localStorage.clear();
		const { createChatHarnessPreferenceStore } = await freshStore();
		const store = createChatHarnessPreferenceStore();
		store.activateSession('chat-A'); store.select('codex');
		store.updateDefault('claude_code', 'opus');
		store.reconcileWithRoster(roster('claude_code', {chat_model:'opus'}));
		expect(get(store).engine).toBe('codex');
		store.activateSession('new-chat');
		expect(get(store)).toEqual({engine:'claude_code',model:'opus'});
	});
});

describe('session model stores and voice admission', () => {
	it('keeps typed model choices local to each composer and restores them on return', async () => {
		localStorage.clear();
		const { createChatProfileStore } = await import('./chatProfileStore');
		const a=createChatProfileStore(), b=createChatProfileStore();
		const fetchMock=vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(JSON.stringify({
			models:[{id:'test/model',provider:'openai',connection:'openai',model:'model',label:'Model',
				thinking:{mode:'effort',levels:['none','high'],default:'high'},limits:null,
				capabilities:{tools:true,vision:true},choices:{auto:'model/auto',none:'model/none'}}],
			default_selection:{model_id:'test/model',thinking:'auto'}
		})));
		try {
			a.activateSession('chat-A'); b.activateSession('chat-B');
			await a.load();
			// A real fetch creates an independent response body per request.
			fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
				models:get(a).models,default_selection:{model_id:'test/model',thinking:'auto'}
			})));
			await b.load();
			a.selectModel({model_id:'test/model',thinking:'none'});
			expect(get(b).selected).toBe('model/auto');
			a.activateSession('chat-B'); expect(get(a).selected).toBe('model/auto');
			a.activateSession('chat-A'); expect(get(a).selected).toBe('model/none');
		} finally { a.destroy(); b.destroy(); fetchMock.mockRestore(); }
	});
	it('inherits the browser-wide model pick in a chat without its own, without pinning it', async () => {
		localStorage.clear();
		const { createChatProfileStore } = await import('./chatProfileStore');
		const catalog = () => new Response(JSON.stringify({
			models:[{id:'test/model',provider:'openai',connection:'openai',model:'model',label:'Model',
				thinking:{mode:'effort',levels:['none','high'],default:'high'},limits:null,
				capabilities:{tools:true,vision:true},choices:{auto:'model/auto',none:'model/none'}}],
			default_selection:{model_id:'test/model',thinking:'auto'}
		}));
		const fetchMock=vi.spyOn(globalThis,'fetch').mockImplementation(async()=>catalog());
		// The pick this browser made before chats had keys of their own.
		localStorage.setItem('magician.chat.modelSelection', JSON.stringify({model_id:'test/model',thinking:'none'}));
		const store=createChatProfileStore();
		try {
			store.activateSession('chat-A');
			await store.load();
			expect(get(store).selected).toBe('model/none');
			expect(localStorage.getItem('magician.chat.modelSelection.chat-A')).toBeNull();
			// It keeps following the browser default as that changes.
			localStorage.setItem('magician.chat.modelSelection', JSON.stringify({model_id:'test/model',thinking:'auto'}));
			store.activateSession('chat-B'); store.activateSession('chat-A');
			expect(get(store).selected).toBe('model/auto');
			// An explicit pick in the chat pins it there only.
			store.selectModel({model_id:'test/model',thinking:'none'});
			expect(JSON.parse(localStorage.getItem('magician.chat.modelSelection.chat-A')!)).toEqual({model_id:'test/model',thinking:'none'});
			expect(JSON.parse(localStorage.getItem('magician.chat.modelSelection')!)).toEqual({model_id:'test/model',thinking:'auto'});
			store.activateSession('chat-B');
			expect(get(store).selected).toBe('model/auto');
			store.activateSession('chat-A');
			expect(get(store).selected).toBe('model/none');
		} finally { store.destroy(); fetchMock.mockRestore(); }
	});
	it('reads the initiating composer for voice without changing browser defaults', async () => {
		const { registerChatComposerChoice, composerChatChoice } = await freshStore();
		const first=registerChatComposerChoice('thread-A',()=>({harness_engine:'codex',harness_model:'original'}));
		const other=registerChatComposerChoice('thread-B',()=>({harness_engine:'claude_code',harness_model:'opus'}));
		try {
			expect(composerChatChoice(null,'thread-A')).toEqual({harness_engine:'codex',harness_model:'original'});
			expect(composerChatChoice(null,'thread-B')).toEqual({harness_engine:'claude_code',harness_model:'opus'});
		} finally { first(); other(); }
	});
});

describe('inherited browser picks', () => {
	beforeEach(() => localStorage.clear());

	it('treats the browser pick a chat inherits as chosen, so opening it never flips the engine', async () => {
		const { createChatHarnessPreferenceStore } = await freshStore();
		const browserWide = createChatHarnessPreferenceStore();
		browserWide.select('codex');
		const panel = createChatHarnessPreferenceStore();
		panel.activateSession('owner/workspace/chat-A');
		expect(get(panel).engine).toBe('codex');
		expect(panel.isChosen()).toBe(true);
		// Opening the chat (or the /engine menu) reconciles with the roster.
		panel.reconcileWithRoster(roster('magician'));
		expect(get(panel).engine).toBe('codex');
		expect(localStorage.getItem('magician.chat.harnessPreference.owner/workspace/chat-A')).toBeNull();
	});

	it('keeps an inheriting chat on the browser default as it changes, and a pinned chat on its own', async () => {
		const { createChatHarnessPreferenceStore } = await freshStore();
		const inheriting = createChatHarnessPreferenceStore();
		const pinned = createChatHarnessPreferenceStore();
		inheriting.activateSession('chat-A');
		pinned.activateSession('chat-B'); pinned.select('codex');
		inheriting.updateDefault('claude_code', 'opus');
		pinned.updateDefault('claude_code', 'opus');
		expect(get(inheriting)).toEqual({ engine: 'claude_code', model: 'opus' });
		expect(inheriting.isChosen()).toBe(true);
		expect(get(pinned).engine).toBe('codex');
		inheriting.defaultChanged({ engine: 'codex', model: 'default' });
		expect(get(inheriting).engine).toBe('codex');
		// Its own pick ends the inheritance.
		inheriting.select('claude_code', 'opus');
		inheriting.defaultChanged({ engine: 'magician', model: 'default' });
		expect(get(inheriting).engine).toBe('claude_code');
	});

	it('shows the server engine in a chat with no pick anywhere without pinning it', async () => {
		const { createChatHarnessPreferenceStore } = await freshStore();
		const panel = createChatHarnessPreferenceStore();
		panel.activateSession('chat-A');
		panel.reconcileWithRoster(roster('claude_code'));
		expect(get(panel).engine).toBe('claude_code');
		expect(panel.isChosen()).toBe(false);
		expect(localStorage.getItem('magician.chat.harnessPreference.chat-A')).toBeNull();
	});
});

describe('open chat panels', () => {
	beforeEach(() => localStorage.clear());

	it('routes sends, server engine updates and browser-pick changes to the panel showing the chat', async () => {
		const module = await freshStore();
		const { createChatProfileStore } = await import('./chatProfileStore');
		const harness = module.createChatHarnessPreferenceStore();
		const profiles = createChatProfileStore();
		const registration = module.registerChatComposer({ harness, profiles });
		try {
			harness.activateSession('owner/workspace/chat-A');
			registration.setSession('chat-A');
			expect(module.chatComposerFor('chat-A').harness).toBe(harness);
			expect(module.chatComposerFor('chat-B').harness).toBe(module.chatHarnessPreferenceStore);
			expect(module.chatComposerFor(null).profiles).not.toBe(profiles);

			module.handleChatEngineUpdatedEnvelope({
				event_type: 'chat.engine.updated',
				payload: { chat_current: 'claude_code', chat_model: 'opus' }
			});
			expect(get(harness)).toEqual({ engine: 'claude_code', model: 'opus' });
			expect(get(module.chatHarnessPreferenceStore)).toEqual({ engine: 'claude_code', model: 'opus' });

			module.notifyChatHarnessDefaultChanged({ engine: 'codex', model: 'default' });
			expect(get(harness).engine).toBe('codex');
		} finally {
			registration.unregister();
			harness.destroy(); profiles.destroy();
		}
		expect(module.chatComposerFor('chat-A').harness).toBe(module.chatHarnessPreferenceStore);
	});

	it('loads the browser-wide catalog on demand instead of failing as still loading', async () => {
		const { createChatProfileStore } = await import('./chatProfileStore');
		const store = createChatProfileStore();
		const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response(JSON.stringify({
			models: [{ id: 'test/model', provider: 'openai', connection: 'openai', model: 'model', label: 'Model',
				thinking: { mode: 'effort', levels: ['none', 'high'], default: 'high' }, limits: null,
				capabilities: { tools: true, vision: true }, choices: { auto: 'model/auto', none: 'model/none' } }],
			default_selection: { model_id: 'test/model', thinking: 'auto' }
		})));
		try {
			expect(() => store.requestSelection(null, 'magician')).toThrow('still loading');
			// Concurrent callers share one request; a loaded catalog is not refetched.
			await Promise.all([store.ensureLoaded('magician'), store.ensureLoaded('magician')]);
			await store.ensureLoaded('magician');
			expect(fetchMock).toHaveBeenCalledTimes(1);
			expect(store.requestSelection(null, 'magician')).toMatchObject({ harness_engine: 'magician' });
		} finally { store.destroy(); fetchMock.mockRestore(); }
	});
});
