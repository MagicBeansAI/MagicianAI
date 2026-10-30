import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import BrowserSignInButton from './BrowserSignInButton.svelte';

const timedFetch = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/fetch', () => ({ timedFetch }));

afterEach(() => { cleanup(); scopeIdentityStore.reset(); vi.restoreAllMocks(); timedFetch.mockReset(); });

function popup() {
	const value = { closed: false, opener: null, close: vi.fn(), location: { replace: vi.fn() } };
	vi.spyOn(window, 'open').mockReturnValue(value as unknown as Window);
	return value;
}

function launchResponse(value: unknown): Response {
	const response = new Response(JSON.stringify(value));
	Object.defineProperty(response, 'url', { value: 'https://computer.example/api/magician/v2/auth/browser-signin/launch' });
	return response;
}

it('revokes an unclaimed login when its originating card disappears before launch returns', async () => {
	scopeIdentityStore.observe('owner', 'first');
	const viewer = popup();
	let finish!: (response: Response) => void;
	timedFetch.mockReturnValueOnce(new Promise<Response>((resolve) => { finish = resolve; }))
		.mockResolvedValue(new Response(JSON.stringify({ closed: true })));
	const mounted = render(BrowserSignInButton, { skill: 'example', placement: 'remote' });
	await userEvent.click(screen.getByRole('button', { name: /Connect on this device/ }));
	await waitFor(() => expect(timedFetch).toHaveBeenCalledTimes(1));
	mounted.unmount();
	const ticket = `bsi_${'a'.repeat(64)}`;
	finish(launchResponse({ mode: 'computer', ticket, viewer_path: '/api/magician/v2/auth/browser-signin/view' }));
	await waitFor(() => expect(timedFetch).toHaveBeenCalledTimes(2));
	expect(viewer.location.replace).not.toHaveBeenCalled();
	expect(viewer.close).toHaveBeenCalled();
	const [url, request] = timedFetch.mock.calls[1];
	expect(String(url)).toBe('https://computer.example/api/magician/v2/auth/browser-signin/close');
	expect(request.headers['X-Magician-Signin']).toBe(ticket);
});

it('revokes a late ticket after switching to another workspace', async () => {
	scopeIdentityStore.observe('owner', 'first');
	const viewer = popup();
	let finish!: (response: Response) => void;
	timedFetch.mockReturnValueOnce(new Promise<Response>((resolve) => { finish = resolve; }))
		.mockResolvedValue(new Response(JSON.stringify({ closed: true })));
	render(BrowserSignInButton, { skill: 'example', placement: 'remote' });
	await userEvent.click(screen.getByRole('button', { name: /Connect on this device/ }));
	scopeIdentityStore.observe('owner', 'second');
	finish(launchResponse({ mode: 'computer', ticket: `bsi_${'b'.repeat(64)}`, viewer_path: '/api/magician/v2/auth/browser-signin/view' }));
	await waitFor(() => expect(timedFetch).toHaveBeenCalledTimes(2));
	expect(viewer.location.replace).not.toHaveBeenCalled();
	expect(await screen.findByRole('alert')).toHaveTextContent('Workspace changed');
});

it('lets a successfully handed-off sign-in window outlive the card', async () => {
	scopeIdentityStore.observe('owner', 'first');
	const viewer = popup();
	timedFetch.mockResolvedValueOnce(launchResponse({ mode: 'direct', authorization_url: 'https://provider.example/authorize' }));
	const mounted = render(BrowserSignInButton, { skill: 'example', placement: 'remote' });
	await userEvent.click(screen.getByRole('button', { name: /Connect on this device/ }));
	await waitFor(() => expect(viewer.location.replace).toHaveBeenCalledWith('https://provider.example/authorize'));
	mounted.unmount();
	expect(viewer.close).not.toHaveBeenCalled();
});

it('does not launch an account shown for another workspace', async () => {
	scopeIdentityStore.observe('owner', 'second');
	const open = vi.spyOn(window, 'open');
	render(BrowserSignInButton, { skill: 'example', placement: 'remote', expectedScope: { principal: 'owner', workspace: 'first' } });
	await userEvent.click(screen.getByRole('button', { name: /Connect on this device/ }));
	expect(timedFetch).not.toHaveBeenCalled();
	expect(open).not.toHaveBeenCalled();
});

it('offers one button on the Magician computer itself, opening the sign-in there', async () => {
	scopeIdentityStore.observe('owner', 'first');
	const open = vi.spyOn(window, 'open');
	timedFetch.mockResolvedValueOnce(launchResponse({ mode: 'computer_opened' }));
	render(BrowserSignInButton, { skill: 'example', placement: 'engine-host' });
	expect(screen.queryByRole('button', { name: 'Open on computer' })).toBeNull();
	await userEvent.click(screen.getByRole('button', { name: /Connect account/ }));
	await waitFor(() => expect(timedFetch).toHaveBeenCalledTimes(1));
	expect(JSON.parse(timedFetch.mock.calls[0][1].body).open_on_computer).toBe(true);
	expect(open).not.toHaveBeenCalled();
	expect(await screen.findByRole('status')).toHaveTextContent('Waiting for the service to confirm sign-in');
});

it('shows a provider registration failure when opening sign-in on the computer', async () => {
	scopeIdentityStore.observe('owner', 'first');
	const onLaunched = vi.fn();
	const message = 'The service could not register Magican for sign-in. No sign-in window was opened.';
	timedFetch.mockResolvedValueOnce(new Response(JSON.stringify({ error: 'signin_registration_failed', message }), { status: 502 }));
	render(BrowserSignInButton, { skill: 'zepto-mcp', placement: 'engine-host', onLaunched });
	await userEvent.click(screen.getByRole('button', { name: /Connect account/ }));
	expect(await screen.findByRole('alert')).toHaveTextContent(message);
	expect(onLaunched).not.toHaveBeenCalled();
});
