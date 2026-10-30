import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import * as notifications from '$lib/shared/stores/notifications';
import HistoryDrawer from './HistoryDrawer.svelte';
import ChatHistoryRail from './ChatHistoryRail.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
afterEach(() => { cleanup(); scopeIdentityStore.reset(); localStorage.clear(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it.each(['drawer', 'rail'])('shows pending deletion and reports failure in the %s', async surface => {
	localStorage.setItem('chat_principal', 'anonymous');
	localStorage.setItem('chat_channel_address', 'browser-test');
	let finish!: (response: Response) => void;
	const pending = new Promise<Response>(resolve => { finish = resolve; });
	const error = vi.spyOn(notifications, 'showError');
	vi.stubGlobal('fetch', vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
		if (init?.method === 'DELETE') return pending;
		if (String(input).includes('/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
		return Response.json({ sessions: [{ id: 'delete-test', title: 'Pending deletion', status: 'active',
			ui_thread_id: 'general', updated_at: Date.now() }], total: 1 });
	}));
	if (surface === 'drawer') render(HistoryDrawer, { open: true });
	else render(ChatHistoryRail, { expanded: true });
	await screen.findByText('Pending deletion');
	const button = screen.getByRole('button', { name: 'Delete session' });
	await fireEvent.click(button);
	expect(await screen.findByText('Deleting…')).toBeInTheDocument();
	expect(button).toBeDisabled();
	expect(screen.getByText('Pending deletion').closest('.item')).toHaveAttribute('aria-busy', 'true');
	finish(Response.json({ error: 'Cleanup failed; please retry' }, { status: 500 }));
	await waitFor(() => expect(error).toHaveBeenCalledWith('Cleanup failed; please retry'));
	expect(screen.queryByText('Deleting…')).toBeNull();
	expect(button).toBeEnabled();
	expect(screen.getByText('Pending deletion')).toBeInTheDocument();
});
