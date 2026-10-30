import { describe, expect, it } from 'vitest';
import { composerDraftKey, createComposerDraft } from './composerDraft';

function storage(): Storage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => { values.set(key, value); },
		removeItem: (key) => { values.delete(key); },
		clear: () => values.clear(), key: (i) => [...values.keys()][i] ?? null,
		get length() { return values.size; }
	};
}

describe('composer text recovery', () => {
	it('restores after remount and removes a sent or explicitly cleared draft', () => {
		const saved = storage();
		const first = createComposerDraft(() => saved);
		expect(first.sync('chat-a', '')).toBe('');
		first.sync('chat-a', 'Line one\nLine two');
		const reloaded = createComposerDraft(() => saved);
		expect(reloaded.sync('chat-a', '')).toBe('Line one\nLine two');
		reloaded.sync('chat-a', '');
		expect(createComposerDraft(() => saved).sync('chat-a', '')).toBe('');
	});

	it('keeps separate chats and owner/workspace identities isolated', () => {
		const saved = storage();
		const drafts = createComposerDraft(() => saved);
		const a = composerDraftKey(['owner', 'default', 'general', 'chat-a']);
		const b = composerDraftKey(['owner', 'default', 'general', 'chat-b']);
		const other = composerDraftKey(['other', 'default', 'general', 'chat-a']);
		drafts.sync(a, '');
		drafts.sync(a, 'Draft A');
		expect(drafts.sync(b, 'Draft A')).toBe('');
		drafts.sync(b, 'Draft B');
		expect(drafts.sync(a, 'Draft B')).toBe('Draft A');
		expect(drafts.sync(other, 'Draft A')).toBe('');
		expect(drafts.sync(b, '')).toBe('Draft B');
	});

	it('keeps editing usable when browser storage is unavailable', () => {
		const drafts = createComposerDraft(() => { throw new Error('denied'); });
		drafts.sync('chat-a', '');
		expect(drafts.sync('chat-a', 'Keep typing')).toBe('Keep typing');
	});

	it('preserves text typed before the initial chat finishes loading', () => {
		const saved = storage();
		expect(createComposerDraft(() => saved).sync('chat-a', 'Typed during load')).toBe('Typed during load');
		expect(createComposerDraft(() => saved).sync('chat-a', '')).toBe('Typed during load');
	});

	it('recovers a reload during loading and transfers text only to the same owner and thread', () => {
		const saved = storage();
		const pending = composerDraftKey(['owner', 'thread-a', 'loading']);
		const resolved = composerDraftKey(['owner', 'thread-a', 'session-1']);
		const first = createComposerDraft(() => saved);
		first.sync(pending, '', pending);
		first.sync(pending, 'Typed before session loaded', pending);
		const reloaded = createComposerDraft(() => saved);
		expect(reloaded.sync(pending, '', pending)).toBe('Typed before session loaded');
		expect(reloaded.sync(resolved, 'Typed before session loaded', pending)).toBe('Typed before session loaded');
		expect(saved.getItem(pending)).toBeNull();
		expect(createComposerDraft(() => saved).sync(resolved, '')).toBe('Typed before session loaded');
		const otherPending = composerDraftKey(['other-owner', 'thread-a', 'loading']);
		expect(reloaded.sync(otherPending, 'Typed before session loaded', otherPending)).toBe('');
	});
});
