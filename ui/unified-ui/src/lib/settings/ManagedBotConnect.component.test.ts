import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchCall } from '../../test/browser';
import ManagedBotConnect from './ManagedBotConnect.svelte';
import { botMethods, parseBotState, parseProjectAccess } from './managedBot';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

type Reply = Record<string, unknown>;
function state(status: string, pending: Reply | null = null, detail?: string): Reply {
	return { bot: 'telegram-self', status, detail, pending };
}

/**
 * Routes `bot/{action}` to queued replies per action (the last one repeats) and records
 * request bodies. A `state` poll without replies of its own answers the latest state, as the engine does.
 */
function botRoutes(replies: Record<string, Reply[]>) {
	const bodies: Array<{ action: string; body: Record<string, unknown> }> = [];
	let last: Reply = state('unknown');
	const mock = installFetchMock([
		{
			method: 'POST',
			match: /\/api\/magician\/v2\/auth\/browser-signin\/bot\/(start|input|state|cancel)$/,
			handle: (call: MockFetchCall) => {
				const action = call.url.split('/').pop() as string;
				bodies.push({ action, body: JSON.parse(String(call.init?.body ?? '{}')) });
				const queue = replies[action] ?? [];
				const next = queue.length > 1 ? queue.shift() : queue[0];
				if (next) last = next;
				return jsonResponse(next ?? last);
			}
		}
	]);
	return { bodies, ...mock };
}

describe('managedBot helpers', () => {
	it('keeps known methods in order and infers them for rows without methods', () => {
		expect(botMethods({ bot: 'whatsapp', methods: ['qr', 'pairing_code', 'fax'] })).toEqual(['qr', 'pairing_code']);
		expect(botMethods({ bot: 'gmail-work' })).toEqual(['login_url']);
		expect(botMethods({ bot: 'telegram-self', methods: [] })).toEqual(['code', 'qr']);
		expect(botMethods({ bot: 'something-else' })).toEqual([]);
	});

	it('drops unknown pending kinds, unsafe QR URLs and unknown statuses', () => {
		expect(parseBotState('x', { status: 'from_the_future', pending: { kind: 'nope' } })).toEqual({ bot: 'x', status: 'unknown', detail: undefined, pending: null });
		const qr = parseBotState('x', { status: 'authenticating', pending: { kind: 'qr', qr_data_url: 'javascript:alert(1)' } });
		expect(qr.pending?.qr_data_url).toBeUndefined();
		const svg = parseBotState('x', { status: 'authenticating', pending: { kind: 'qr', qr_data_url: 'data:image/svg+xml;base64,PHN2Zy8+' } });
		expect(svg.pending?.qr_data_url).toBeUndefined();
		const png = parseBotState('x', { status: 'authenticating', pending: { kind: 'qr', qr_data_url: 'data:image/png;base64,iVBORw0KGgo=' } });
		expect(png.pending?.qr_data_url).toBe('data:image/png;base64,iVBORw0KGgo=');
	});

	it('normalizes a project-access report and keeps only an https console link', () => {
		expect(parseProjectAccess('gmail-work', { status: 'from_the_future', can_grant: true, console_url: 'javascript:alert(1)', commands: ['gcloud x', 3] })).toEqual({
			bot: 'gmail-work', project_id: undefined, email: undefined, status: 'unknown', missing_roles: [], detail: undefined,
			grant_account: undefined, commands: ['gcloud x'], console_url: undefined, can_grant: false
		});
		const missing = parseProjectAccess('gmail-work', { status: 'missing', can_grant: true, missing_roles: ['roles/pubsub.editor'], console_url: 'https://console.cloud.google.com/iam-admin/iam?project=p' });
		expect(missing.can_grant).toBe(true);
		expect(missing.missing_roles).toEqual(['roles/pubsub.editor']);
		expect(missing.console_url).toBe('https://console.cloud.google.com/iam-admin/iam?project=p');
		// Nothing to grant once access is in place.
		expect(parseProjectAccess('gmail-work', { status: 'granted', can_grant: true }).can_grant).toBe(false);
	});
});

describe('ManagedBotConnect', () => {
	it('runs Telegram phone → code → two-step password and never keeps the password', async () => {
		const { bodies } = botRoutes({
			start: [state('authenticating', { kind: 'code', label: 'Telegram code' }, 'Enter the code Telegram sent you')],
			input: [state('authenticating', { kind: 'password', label: 'Two-step password' }), state('connected')]
		});
		const onDone = vi.fn();
		render(ManagedBotConnect, { bot: 'telegram-self', methods: ['code', 'qr'], onDone });

		expect(screen.getByLabelText('Code')).toBeChecked();
		await userEvent.type(screen.getByLabelText('Phone number'), '+91 98765 43210');
		await userEvent.click(screen.getByRole('button', { name: 'Continue' }));

		await userEvent.type(await screen.findByLabelText('Telegram code'), '12345');
		expect(screen.getByText('Enter the code Telegram sent you')).toBeInTheDocument();
		await userEvent.click(screen.getByRole('button', { name: 'Send code' }));

		const password = await screen.findByLabelText('Two-step password');
		expect(password).toHaveAttribute('type', 'password');
		await userEvent.type(password, 'hunter2');
		await userEvent.click(screen.getByRole('button', { name: 'Send' }));

		expect(await screen.findByText('Connected.')).toBeInTheDocument();
		expect(bodies.filter((entry) => entry.action !== 'state')).toEqual([
			{ action: 'start', body: { bot: 'telegram-self', method: 'code', phone: '+91 98765 43210' } },
			{ action: 'input', body: { bot: 'telegram-self', step: 'code', value: '12345' } },
			{ action: 'input', body: { bot: 'telegram-self', step: 'password', value: 'hunter2' } }
		]);
		expect(document.body.innerHTML).not.toContain('hunter2');
		await waitFor(() => expect(onDone).toHaveBeenCalledWith(true), { timeout: 3_000 });
	});

	it('shows the WhatsApp pairing code with a copy button and the linking steps', async () => {
		botRoutes({ start: [state('authenticating', { kind: 'pairing_code', pairing_code: 'ABCD1234' })] });
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(ManagedBotConnect, { bot: 'whatsapp', methods: ['pairing_code', 'qr'] });

		expect(screen.getByLabelText('Link with phone number')).toBeChecked();
		expect(screen.getByLabelText('Scan QR')).toBeInTheDocument();
		await userEvent.type(screen.getByLabelText('Phone number'), '+15551234567');
		await userEvent.click(screen.getByRole('button', { name: 'Continue' }));

		expect(await screen.findByLabelText('Pairing code')).toHaveTextContent('ABCD1234');
		expect(screen.getByText(/Link with phone number instead/)).toBeInTheDocument();
		await userEvent.click(screen.getByRole('button', { name: 'Copy' }));
		expect(writeText).toHaveBeenCalledWith('ABCD1234');
		expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
	});

	it('starts a QR login without asking for a phone number', async () => {
		const { bodies } = botRoutes({ start: [state('authenticating', { kind: 'qr', qr_data_url: 'data:image/png;base64,iVBORw0KGgo=' })] });
		render(ManagedBotConnect, { bot: 'whatsapp', methods: ['pairing_code', 'qr'] });
		await userEvent.click(screen.getByLabelText('Scan QR'));
		expect(screen.queryByLabelText('Phone number')).not.toBeInTheDocument();
		await userEvent.click(screen.getByRole('button', { name: 'Continue' }));
		expect(await screen.findByAltText('QR code to scan')).toHaveAttribute('src', 'data:image/png;base64,iVBORw0KGgo=');
		expect(screen.getByText(/Scan from another device, or use the code option/)).toBeInTheDocument();
		expect(bodies[0]).toEqual({ action: 'start', body: { bot: 'whatsapp', method: 'qr' } });
	});

	it('hands a Google sign-in ticket to the computer-mode viewer', async () => {
		const ticket = `bsi_${'c'.repeat(64)}`;
		const { bodies } = botRoutes({
			start: [{ bot: 'gmail-work', status: 'authenticating', pending: { kind: 'login_url', sign_in: { mode: 'computer', ticket, viewer_path: '/api/magician/v2/auth/browser-signin/view', expires_in: 300 } } }]
		});
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		render(ManagedBotConnect, { bot: 'gmail-work', methods: ['login_url'] });

		// A single method that needs no phone starts at once.
		await userEvent.click(await screen.findByRole('button', { name: 'Open sign-in ↗' }));
		expect(bodies[0]).toEqual({ action: 'start', body: { bot: 'gmail-work', method: 'login_url' } });
		const [url] = open.mock.calls[0];
		expect(new URL(String(url)).pathname).toBe('/api/magician/v2/auth/browser-signin/view');
		expect(new URL(String(url)).hash).toBe(`#${ticket}`);
		expect(await screen.findByText('Finish signing in in the new window.')).toBeInTheDocument();
	});

	it('restarts a Google sign-in on the computer and keeps checking', async () => {
		const ticket = `bsi_${'d'.repeat(64)}`;
		const { bodies } = botRoutes({
			start: [
				{ bot: 'gmail-work', status: 'authenticating', pending: { kind: 'login_url', sign_in: { mode: 'computer', ticket, viewer_path: '/api/magician/v2/auth/browser-signin/view' } } },
				{ bot: 'gmail-work', status: 'authenticating', pending: { kind: 'login_url', sign_in: { mode: 'computer_opened' } } }
			]
		});
		render(ManagedBotConnect, { bot: 'gmail-work', methods: ['login_url'] });

		await userEvent.click(await screen.findByRole('button', { name: 'Open on computer' }));
		expect(await screen.findByText('Finish the sign-in on your computer — this updates automatically.')).toBeInTheDocument();
		expect(bodies[1]).toEqual({ action: 'start', body: { bot: 'gmail-work', method: 'login_url', open_on_computer: true } });
		expect(screen.queryByRole('button', { name: 'Open sign-in ↗' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
	});

	it('polls state every 2 s while authenticating and stops on another status', async () => {
		// Plain promise responses keep this test on fake timers without stream internals.
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		const replies: Record<string, Reply[]> = {
			start: [{ bot: 'gmail-work', status: 'authenticating', pending: null }],
			state: [
				{ bot: 'gmail-work', status: 'authenticating', pending: null },
				{ bot: 'gmail-work', status: 'needs_sign_in', detail: 'Signed in as a different account', pending: null }
			]
		};
		const actions: string[] = [];
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
			const action = String(input).split('/').pop() as string;
			actions.push(action);
			const queue = replies[action];
			const body = queue.length > 1 ? queue.shift() : queue[0];
			return { ok: true, status: 200, json: async () => body } as unknown as Response;
		}));
		render(ManagedBotConnect, { bot: 'gmail-work', methods: ['login_url'] });
		await vi.advanceTimersByTimeAsync(0);
		expect(actions).toEqual(['start']);
		await vi.advanceTimersByTimeAsync(2_000);
		expect(actions).toEqual(['start', 'state']);
		await vi.advanceTimersByTimeAsync(2_000);
		expect(actions).toEqual(['start', 'state', 'state']);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(actions).toEqual(['start', 'state', 'state']);
		await tick();
		expect(screen.getByText('Signed in as a different account')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
	});

	it('cancels a running flow on the engine', async () => {
		const { bodies } = botRoutes({
			start: [state('authenticating', { kind: 'qr', qr_data_url: 'data:image/png;base64,iVBORw0KGgo=' })],
			cancel: [state('needs_sign_in')]
		});
		const onDone = vi.fn();
		render(ManagedBotConnect, { bot: 'telegram-self', methods: ['qr'], onDone });
		await screen.findByAltText('QR code to scan');
		await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(onDone).toHaveBeenCalledWith(false));
		expect(bodies.map((entry) => entry.action)).toEqual(['start', 'cancel']);
	});
});
