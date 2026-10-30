import { cleanup, render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchCall } from '../../test/browser';
import ServiceAccountsPanel from './ServiceAccountsPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function accountsRoute(accounts: unknown[]) {
	return {
		method: 'POST',
		match: /\/api\/magician\/v2\/auth\/browser-signin\/accounts$/,
		handle: () => jsonResponse({ accounts })
	};
}

const GOOGLE_ROW = { name: 'gmail-work', label: 'Google Workspace (work)', kind: 'managed_bot', bot: 'gmail-work', profile: 'work', methods: ['login_url'], status: 'connected', project_access: true };
const COMMANDS = [
	'gcloud projects add-iam-policy-binding demo-project --member=user:me@example.com --role=roles/serviceusage.serviceUsageConsumer',
	'gcloud projects add-iam-policy-binding demo-project --member=user:me@example.com --role=roles/pubsub.editor'
];
function accessReport(status: string, extra: Record<string, unknown> = {}) {
	return { bot: 'gmail-work', project_id: 'demo-project', email: 'me@example.com', status, detail: `Access ${status}`, console_url: 'https://console.cloud.google.com/iam-admin/iam?project=demo-project', can_grant: false, ...extra };
}

/** Accounts plus `bot/project-access[/grant]`, answering queued replies (the last one repeats) and recording bodies. */
function projectAccessRoutes(accounts: unknown[], replies: { check?: unknown[]; grant?: unknown[] }) {
	const bodies: Array<{ path: string; body: Record<string, unknown> }> = [];
	const mock = installFetchMock([
		accountsRoute(accounts),
		{
			method: 'POST',
			match: /\/api\/magician\/v2\/auth\/browser-signin\/bot\/project-access(\/grant)?$/,
			handle: (call: MockFetchCall) => {
				const path = call.url.endsWith('/grant') ? 'grant' : 'check';
				bodies.push({ path, body: JSON.parse(String(call.init?.body ?? '{}')) });
				const queue = replies[path] ?? [];
				const next = queue.length > 1 ? queue.shift() : queue[0];
				return next instanceof Response ? next : jsonResponse(next ?? {});
			}
		}
	]);
	return { bodies, ...mock };
}

describe('ServiceAccountsPanel', () => {
	it('starts collapsed and expands to show account status, detail and actions', async () => {
		installFetchMock([
			accountsRoute([
				{ name: 'metabase-mcp', label: 'Provider account', status: 'connected', detail: 'Account login is active' },
				{ name: 'swiggy-mcp', label: 'Provider account — instamart', status: 'needs_sign_in', detail: 'Account login is required', endpoint_alias: 'instamart' }
			])
		]);
		render(ServiceAccountsPanel);

		const rows = await screen.findAllByTestId('service-account');
		expect(rows).toHaveLength(2);
		expect(screen.getByText('Service accounts').closest('details')).not.toHaveAttribute('open');
		expect(screen.getByRole('button', { name: 'Reconnect ↗' })).not.toBeVisible();
		await userEvent.click(screen.getByText('Service accounts'));

		const connected = within(rows[0]);
		expect(connected.getByText('metabase-mcp')).toBeInTheDocument();
		expect(connected.getByText('Connected')).toHaveClass('status-connected');
		expect(connected.getByText('Account login is active')).toBeInTheDocument();
		expect(connected.getByRole('button', { name: 'Reconnect ↗' })).toBeInTheDocument();

		const needsSignIn = within(rows[1]);
		expect(needsSignIn.getByText('Needs sign-in')).toBeInTheDocument();
		expect(needsSignIn.getByText('Provider account — instamart')).toBeInTheDocument();
		expect(needsSignIn.getByRole('button', { name: 'Connect ↗' })).toBeInTheDocument();
	});

	it('treats rows from an older engine without a status as unknown', async () => {
		installFetchMock([accountsRoute([{ name: 'higgsfield', label: 'Higgsfield account' }])]);
		render(ServiceAccountsPanel);

		const row = within((await screen.findAllByTestId('service-account'))[0]);
		await userEvent.click(screen.getByText('Service accounts'));
		expect(row.getByText('Unknown')).toBeInTheDocument();
		expect(row.getByRole('button', { name: 'Connect ↗' })).toBeInTheDocument();
	});

	it('expands when Settings search matches it, while allowing the user to collapse it', async () => {
		installFetchMock([accountsRoute([{ name: 'metabase-mcp', label: 'Provider account' }])]);
		const panel = render(ServiceAccountsPanel);
		await screen.findByTestId('service-account');
		await panel.rerender({ expandedBySearch: true });
		expect(screen.getByRole('button', { name: 'Connect ↗' })).toBeVisible();
		await userEvent.click(screen.getByText('Service accounts'));
		expect(screen.getByText('Service accounts').closest('details')).not.toHaveAttribute('open');
	});

	it('shows an unavailable managed bot greyed with its detail and no Connect', async () => {
		installFetchMock([
			accountsRoute([
				{ name: 'telegram-self', label: 'Telegram', kind: 'managed_bot', bot: 'telegram-self', methods: ['code', 'qr'], status: 'unavailable', detail: 'Set TGCLI_API_ID and TGCLI_API_HASH' }
			])
		]);
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));

		const row = within((await screen.findAllByTestId('service-account'))[0]);
		expect(row.getByText('Unavailable')).toHaveClass('status-unavailable');
		expect(row.getByText('Set TGCLI_API_ID and TGCLI_API_HASH')).toBeInTheDocument();
		expect(row.queryByRole('button')).not.toBeInTheDocument();
	});

	it('opens the managed-bot connect panel instead of a browser sign-in', async () => {
		installFetchMock([
			accountsRoute([
				{ name: 'gmail-work', label: 'Google Workspace (work)', kind: 'managed_bot', bot: 'gmail-work', profile: 'work', methods: ['login_url'], status: 'connected' },
				{ name: 'whatsapp', label: 'WhatsApp', kind: 'managed_bot', bot: 'whatsapp', methods: ['pairing_code', 'qr'], status: 'needs_sign_in' }
			])
		]);
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));

		const rows = await screen.findAllByTestId('service-account');
		expect(rows).toHaveLength(2);
		expect(within(rows[0]).getByRole('button', { name: 'Reconnect' })).toBeInTheDocument();
		expect(within(rows[0]).queryByRole('button', { name: 'Open on computer' })).not.toBeInTheDocument();

		await userEvent.click(within(rows[1]).getByRole('button', { name: 'Connect' }));
		const panel = within(rows[1]).getByTestId('managed-bot-connect');
		expect(within(panel).getByLabelText('Link with phone number')).toBeChecked();
		expect(within(panel).getByLabelText('Phone number')).toBeInTheDocument();
		// One flow at a time.
		expect(within(rows[0]).getByRole('button', { name: 'Reconnect' })).toBeDisabled();
	});

	it('checks Google Cloud project access only after the panel opens, and shows a quiet OK', async () => {
		const { bodies } = projectAccessRoutes([GOOGLE_ROW], { check: [accessReport('granted')] });
		render(ServiceAccountsPanel);
		await screen.findAllByTestId('service-account');
		expect(bodies).toEqual([]);

		await userEvent.click(screen.getByText('Service accounts'));
		expect(await screen.findByText('Cloud project access OK')).toBeInTheDocument();
		expect(bodies).toEqual([{ path: 'check', body: { bot: 'gmail-work' } }]);
		expect(screen.queryByRole('button', { name: 'Grant access' })).not.toBeInTheDocument();
	});

	it('skips the check for rows without project_access or not connected', async () => {
		const { bodies } = projectAccessRoutes([
			{ ...GOOGLE_ROW, project_access: undefined },
			{ ...GOOGLE_ROW, name: 'gmail-home', bot: 'gmail-home', status: 'needs_sign_in' }
		], { check: [accessReport('granted')] });
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));
		expect(await screen.findAllByTestId('service-account')).toHaveLength(2);
		expect(screen.queryByTestId('project-access')).not.toBeInTheDocument();
		expect(bodies).toEqual([]);
	});

	it('grants missing roles with the computer gcloud account and shows the result', async () => {
		const { bodies } = projectAccessRoutes([GOOGLE_ROW], {
			check: [accessReport('missing', { missing_roles: ['roles/pubsub.editor'], detail: 'me@example.com is missing roles/pubsub.editor on demo-project.', grant_account: 'owner@example.com', commands: COMMANDS, can_grant: true })],
			grant: [accessReport('granted')]
		});
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));

		const row = within((await screen.findAllByTestId('service-account'))[0]);
		expect(await row.findByText('me@example.com is missing roles/pubsub.editor on demo-project.')).toBeInTheDocument();
		expect(row.getByText('Uses owner@example.com on the computer.')).toBeInTheDocument();
		// A grant needs no manual commands.
		expect(row.queryByLabelText('Commands to grant access')).not.toBeInTheDocument();
		await userEvent.click(row.getByRole('button', { name: 'Grant access' }));
		expect(await row.findByText('Cloud project access OK')).toBeInTheDocument();
		expect(bodies).toEqual([
			{ path: 'check', body: { bot: 'gmail-work' } },
			{ path: 'grant', body: { bot: 'gmail-work' } }
		]);
	});

	it('shows copyable commands and the console link when the computer cannot grant, and re-checks fresh', async () => {
		const { bodies } = projectAccessRoutes([GOOGLE_ROW], {
			check: [
				accessReport('missing', { missing_roles: ['roles/pubsub.editor'], detail: 'Ask the project owner to add the roles.', commands: COMMANDS, console_url: 'javascript:alert(1)' }),
				accessReport('missing', { detail: 'Still missing roles/pubsub.editor.', commands: COMMANDS })
			]
		});
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));

		const row = within((await screen.findAllByTestId('service-account'))[0]);
		expect(await row.findByText('Ask the project owner to add the roles.')).toBeInTheDocument();
		expect(row.queryByRole('button', { name: 'Grant access' })).not.toBeInTheDocument();
		expect(row.getByLabelText('Commands to grant access')).toHaveTextContent('roles/serviceusage.serviceUsageConsumer');
		// An unsafe console link is dropped.
		expect(row.queryByRole('link', { name: 'Open Cloud console ↗' })).not.toBeInTheDocument();
		await userEvent.click(row.getByRole('button', { name: 'Copy commands' }));
		expect(writeText).toHaveBeenCalledWith(COMMANDS.join('\n'));
		expect(await row.findByRole('button', { name: 'Copied' })).toBeInTheDocument();

		await userEvent.click(row.getByRole('button', { name: 'Check again' }));
		expect(await row.findByText('Still missing roles/pubsub.editor.')).toBeInTheDocument();
		expect(row.getByRole('link', { name: 'Open Cloud console ↗' })).toHaveAttribute('href', 'https://console.cloud.google.com/iam-admin/iam?project=demo-project');
		expect(bodies.map((entry) => entry.body)).toEqual([{ bot: 'gmail-work' }, { bot: 'gmail-work', fresh: true }]);
	});

	it('shows the detail and commands when access cannot be read', async () => {
		projectAccessRoutes([GOOGLE_ROW], {
			check: [accessReport('unknown', { detail: 'gcloud is not signed in on the computer.', commands: COMMANDS })]
		});
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));
		const row = within((await screen.findAllByTestId('service-account'))[0]);
		expect(await row.findByText('gcloud is not signed in on the computer.')).toBeInTheDocument();
		expect(row.getByLabelText('Commands to grant access')).toHaveTextContent('roles/pubsub.editor');
		expect(row.queryByRole('button', { name: 'Grant access' })).not.toBeInTheDocument();
		expect(row.queryByRole('link', { name: 'Open Cloud console ↗' })).not.toBeInTheDocument();
	});

	it('shows a failed grant as an error and keeps Grant access', async () => {
		projectAccessRoutes([GOOGLE_ROW], {
			check: [accessReport('missing', { detail: 'Missing roles.', grant_account: 'owner@example.com', commands: COMMANDS, can_grant: true })],
			grant: [jsonResponse({ error: 'project_access_grant_failed', message: 'The Cloud project roles could not be granted from this computer.' }, { status: 409 })]
		});
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));
		const row = within((await screen.findAllByTestId('service-account'))[0]);
		await userEvent.click(await row.findByRole('button', { name: 'Grant access' }));
		expect(await row.findByRole('alert')).toHaveTextContent('The Cloud project roles could not be granted from this computer.');
		expect(row.getByRole('button', { name: 'Grant access' })).toBeEnabled();
	});

	it('shows no step when the account turns out not to be a Google account', async () => {
		const { fetchMock } = projectAccessRoutes([GOOGLE_ROW], {
			check: [jsonResponse({ error: 'bot_method_unsupported', message: 'This account cannot connect that way.' }, { status: 400 })]
		});
		render(ServiceAccountsPanel);
		await userEvent.click(screen.getByText('Service accounts'));
		await screen.findAllByTestId('service-account');
		await vi.waitFor(() => expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/project-access'))).toBe(true));
		await vi.waitFor(() => expect(screen.queryByTestId('project-access')).not.toBeInTheDocument());
		expect(screen.queryByRole('alert')).not.toBeInTheDocument();
	});
});
