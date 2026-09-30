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

	it('hands every surface the composer choice, with a profile only where it is read', async () => {
		const { chatHarnessPreferenceStore, composerChatChoice } = await freshStore();
		chatHarnessPreferenceStore.select('magician');
		expect(composerChatChoice('balanced')).toEqual({
			engine: 'magician',
			model: 'default',
			profile: 'balanced'
		});
		chatHarnessPreferenceStore.select('claude_code', 'opus');
		expect(composerChatChoice('balanced')).toEqual({ engine: 'claude_code', model: 'opus' });
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
