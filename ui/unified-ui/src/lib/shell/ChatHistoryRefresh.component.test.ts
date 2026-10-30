import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { chatStore } from '$lib/stores/chatStore';
import HistoryDrawer from './HistoryDrawer.svelte';
import ChatHistoryRail from './ChatHistoryRail.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each(['drawer', 'rail'])('refreshes %s timestamps and order for a background chat without blanking the list', async surface => {
	const sessions = [
		{ id: 'front', title: 'Front chat', updated_at: Date.now() - 60000, status: 'active', ui_thread_id: 'research' },
		{ id: 'background', title: 'Background chat', updated_at: Date.now() - 120000, status: 'active', ui_thread_id: 'research' }
	];
	let finish: ((response: Response) => void) | undefined;
	let changed = false;
	const fetchMock = vi.fn(async (input: string | URL | Request) => {
		if (String(input).includes('/chat/activity')) return Response.json({ working_session_ids: [] });
		if (changed) return new Promise<Response>(resolve => { finish = resolve; });
		return Response.json({ sessions, total: 2 });
	});
	vi.stubGlobal('fetch', fetchMock);
	const { container } = surface === 'drawer'
		? render(HistoryDrawer, { open: true, threadFilter: 'research' })
		: render(ChatHistoryRail, { expanded: true, threadFilter: 'research' });
	await screen.findByText('Background chat');
	const titles = () => [...container.querySelectorAll('.item .title')].map(el => el.textContent);
	expect(titles()).toEqual(['Front chat', 'Background chat']);
	const oldTimestamp = screen.getByText('Background chat').closest('.item')!.querySelector('.ts')!.textContent;
	changed = true;
	chatStore.handleChatMessageReceived('background', { id: 'new-background-message', session_id: 'background',
		direction: 'assistant', created_at: Date.now(), content: { type: 'text', text: 'Background work finished' } });
	await waitFor(() => expect(finish).toBeDefined());
	expect(titles()).toEqual(['Front chat', 'Background chat']);
	expect(container.querySelector('.skeleton-list')).toBeNull();
	finish!(Response.json({ sessions: [{ ...sessions[1], updated_at: Date.now() }, sessions[0]], total: 2 }));
	await waitFor(() => expect(titles()).toEqual(['Background chat', 'Front chat']));
	expect(screen.getByText('Background chat').closest('.item')!.querySelector('.ts')!.textContent).not.toBe(oldTimestamp);
	const requests = fetchMock.mock.calls.map(([input]) => new URL(String(input), 'http://localhost'))
		.filter(url => url.pathname.endsWith('/chat/sessions'));
	expect(requests).toHaveLength(2);
	expect(requests.every(url => url.searchParams.get('ui_thread_id') === 'research')).toBe(true);
});
