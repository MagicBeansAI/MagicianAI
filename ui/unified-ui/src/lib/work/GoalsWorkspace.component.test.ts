import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import GoalsWorkspace from './GoalsWorkspace.svelte';
import { forgetGoalWorkspaces, localDate } from './goals';

const goalNote = (fields: string, title: string) => `---\nmagician_kind: goal\n${fields}\n---\n# ${title}\n`;
const notes: Record<string, string> = {
	'Workspaces/acme/Goals/Raise a seed round.md': goalNote('kind: main\nowner: user\nstatus: active\ntarget: 2026-12-01\nvisibility: private', 'Raise a seed round'),
	'Workspaces/acme/Goals/Apply to accelerators.md': goalNote('kind: sub\nparent: "[[Raise a seed round]]"\npeers: ["[[Find design partners]]", "[[Hire a designer]]"]\nowner: cro\nstatus: blocked\nvisibility: internal\nprogram: fundraising.md', 'Apply to accelerators'),
	'Workspaces/acme/Goals/Find design partners.md': goalNote('kind: peer\nowner: user\nstatus: proposed\nvisibility: public', 'Find design partners'),
	'Workspaces/acme/Goals/Lost.md': goalNote('kind: sub\nparent: "[[Missing parent]]"\nstatus: active', 'Lost goal'),
	'Workspaces/acme/Goals/Lost step.md': goalNote('kind: sub\nparent: "[[Lost]]"\nstatus: proposed', 'Lost step')
};
const pathOf = (url: string) => new URL(url, 'http://localhost').searchParams.get('path');

/** Applies a front-matter edit the way the server does: only the named keys change. */
function patchFrontMatter(markdown: string, body: { set?: Record<string, string>; unset?: string[] }): string {
	let [, head, rest] = markdown.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/)!;
	for (const key of body.unset ?? []) head = head.split('\n').filter(line => !line.startsWith(`${key}:`)).join('\n');
	for (const [key, value] of Object.entries(body.set ?? {})) {
		head = head.split('\n').some(line => line.startsWith(`${key}:`))
			? head.split('\n').map(line => line.startsWith(`${key}:`) ? `${key}: ${value}` : line).join('\n')
			: `${head}\n${key}: ${value}`;
	}
	return `---\n${head}\n---\n${rest}`;
}

/** `revisions` stands in for the server's content revision; `conflict` makes the next edit stale.
 * `extra` adds goal notes to the shared set. */
function installNotes(workspaces: 'present' | 'missing' = 'present', options: { conflict?: boolean; extra?: Record<string, string>; programs?: 'available' | 'missing' | 'error'; programHandler?: () => Response | Promise<Response> } = {}) {
	const currentNotes: Record<string, string> = { ...notes, ...options.extra };
	const folders = workspaces === 'present' ? ['acme'] : [];
	const revisions: Record<string, number> = {};
	const revision = (path: string) => `rev-${revisions[path] ?? 0}`;
	const mock = installFetchMock([
		{ method: 'GET', match: '/v2/programs', handle: () => options.programHandler ? options.programHandler()
			: options.programs === 'error' ? jsonResponse({ message: 'Playbooks unavailable.' }, { status: 503 })
			: jsonResponse({ programs: options.programs === 'missing' ? [] : [{ name: 'fundraising.md', title: 'Fundraising' }] }) },
		{ method: 'GET', match: '/notes/tree?path=', handle: call => {
			const path = pathOf(call.url);
			if (path === 'Workspaces') return jsonResponse({ entries: folders.map(name => ({ name, relative_path: `Workspaces/${name}`, kind: 'dir', has_children: true })) });
			if (folders.some(folder => path === `Workspaces/${folder}/Goals`)) return jsonResponse({ entries: [
				...Object.keys(currentNotes).filter(note => note.startsWith(`${path}/`)).map(relative_path => ({ name: relative_path.split('/').pop(), relative_path, kind: 'file' })),
				{ name: 'Archive', relative_path: `${path}/Archive`, kind: 'dir', has_children: true }
			] });
			return jsonResponse({ error: 'not_found' }, { status: 404 });
		} },
		{ method: 'GET', match: '/notes/file?path=', handle: call => {
			const path = pathOf(call.url)!;
			return path in currentNotes ? jsonResponse({ title: '', relative_path: path, markdown: currentNotes[path], revision: revision(path) }) : jsonResponse({ error: 'not_found' }, { status: 404 });
		} },
		{ method: 'PATCH', match: '/notes/front-matter', handle: call => {
			const body = JSON.parse(call.init?.body as string);
			if (options.conflict) {
				options.conflict = false;
				revisions[body.path] = (revisions[body.path] ?? 0) + 1;
			}
			if (body.expected_revision !== revision(body.path)) return jsonResponse({ error: 'note_revision_conflict', current_revision: revision(body.path) }, { status: 409 });
			revisions[body.path] = (revisions[body.path] ?? 0) + 1;
			currentNotes[body.path] = patchFrontMatter(currentNotes[body.path], body);
			return jsonResponse({ relative_path: body.path, markdown: currentNotes[body.path], revision: revision(body.path) });
		} },
		{ method: 'POST', match: '/notes/file', handle: call => {
			const body = JSON.parse(call.init?.body as string);
			const folder = body.folder.split('/')[1];
			if (!folders.includes(folder)) folders.push(folder);
			currentNotes[`${body.folder}/${body.name}.md`] = body.markdown;
			return jsonResponse({ ok: true });
		} },
		{ method: 'POST', match: '/notes/tree', handle: call => {
			const body = JSON.parse(call.init?.body as string);
			folders.push(body.name);
			return jsonResponse({ relative_path: `${body.folder}/${body.name}`, kind: 'dir' });
		} }
	]);
	return mock;
}

beforeEach(() => { forgetGoalWorkspaces(); scopeIdentityStore.observe('owner', 'work'); });
afterEach(() => { cleanup(); scopeIdentityStore.reset(); });

it('creates a goal with a selected playbook and displays its title', async () => {
	const fetch = installNotes();
	render(GoalsWorkspace);
	await screen.findByRole('region', { name: 'acme goals' });
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await screen.findByRole('option', { name: 'Fundraising (fundraising.md)' });
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Secure runway' } });
	await fireEvent.change(screen.getByLabelText('Playbook (optional)'), { target: { value: 'fundraising.md' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	const goal = await screen.findByRole('listitem', { name: 'Secure runway' });
	expect(await within(goal).findByRole('link', { name: 'Playbook: Fundraising →' })).toHaveAttribute('href', '/work?view=programs&program=fundraising.md');
	const writes = fetch.calls.filter(call => call.method !== 'GET');
	expect(writes).toHaveLength(1);
	expect(JSON.parse(writes[0].init!.body as string).markdown).toContain('program: "fundraising.md"');
});

it('distinguishes a missing playbook from an unsuccessful catalog lookup', async () => {
	const options = { programs: 'error' as 'available' | 'missing' | 'error' };
	installNotes('present', options);
	render(GoalsWorkspace);
	await screen.findByText('Playbook could not be checked');
	expect(screen.queryByText('Playbook missing')).not.toBeInTheDocument();
	options.programs = 'missing';
	await fireEvent.click(screen.getByRole('button', { name: 'Retry playbooks' }));
	expect(await screen.findByText('Playbook missing')).toBeInTheDocument();
	expect(screen.queryByText('Playbook could not be checked')).not.toBeInTheDocument();
});

it('allows a goal without a playbook when the catalog is unavailable', async () => {
	const fetch = installNotes('missing', { programs: 'error' });
	render(GoalsWorkspace);
	await screen.findByText(/No goal workspaces yet/);
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await screen.findByText(/You can still create this goal without one/);
	await fireEvent.input(screen.getByLabelText('Folder name'), { target: { value: 'Personal' } });
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Read a book' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	await screen.findByRole('listitem', { name: 'Read a book' });
	const writes = fetch.calls.filter(call => call.method !== 'GET');
	expect(writes).toHaveLength(1);
	expect(JSON.parse(writes[0].init!.body as string).markdown).not.toContain('program:');
});

it('discards the old scope playbook catalog when its transport ignores cancellation', async () => {
	let release!: (response: Response) => void, requests = 0;
	installNotes('present', { programHandler: () => ++requests === 1
		? new Promise<Response>(resolve => { release = resolve; }) : jsonResponse({ programs: [] }) });
	render(GoalsWorkspace);
	await waitFor(() => expect(requests).toBe(1));
	scopeIdentityStore.observe('other-owner', 'other-work');
	await screen.findByText('Playbook missing');
	release(jsonResponse({ programs: [{ name: 'fundraising.md', title: 'Private old playbook' }] }));
	await new Promise(resolve => setTimeout(resolve, 20));
	expect(screen.queryByText(/Private old playbook/)).not.toBeInTheDocument();
	expect(screen.getByText('Playbook missing')).toBeInTheDocument();
});

it('creates a private proposed goal from the Goals screen and reloads it', async () => {
	const fetch = installNotes();
	render(GoalsWorkspace);
	await screen.findByRole('region', { name: 'acme goals' });
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Ship v2' } });
	await fireEvent.change(screen.getByLabelText('Parent goal'), { target: { value: 'Workspaces/acme/Goals/Raise a seed round.md' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	const goal = await screen.findByRole('listitem', { name: 'Ship v2' });
	expect(within(goal).getByText('proposed')).toBeInTheDocument();
	expect(within(goal).getByText('private')).toBeInTheDocument();
	expect(within(screen.getByRole('listitem', { name: 'Raise a seed round' })).getByRole('listitem', { name: 'Ship v2' })).toBeInTheDocument();
	const writes = fetch.calls.filter(call => call.init?.method === 'POST');
	expect(writes).toHaveLength(1);
	expect(JSON.parse(writes[0].init!.body as string).folder).toBe('Workspaces/acme/Goals');
});

it('creates the first goal folder without chat', async () => {
	const fetch = installNotes('missing');
	render(GoalsWorkspace);
	await screen.findByText(/No goal workspaces yet/);
	await fireEvent.click(screen.getByRole('button', { name: 'New folder' }));
	await fireEvent.input(screen.getByLabelText('Folder name'), { target: { value: 'Personal' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create folder' }));
	await screen.findByRole('region', { name: 'Personal goals' });
	expect(fetch.calls.filter(call => call.init?.method === 'POST')).toHaveLength(1);
});

it('creates a first goal and its new folder together', async () => {
	const fetch = installNotes('missing');
	render(GoalsWorkspace);
	await screen.findByText(/No goal workspaces yet/);
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await fireEvent.input(screen.getByLabelText('Folder name'), { target: { value: 'Personal' } });
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Learn Spanish' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	await screen.findByRole('listitem', { name: 'Learn Spanish' });
	expect(fetch.calls.filter(call => call.init?.method === 'POST')).toHaveLength(1);
});

it('blocks a duplicate goal name and retains the form for correction', async () => {
	const fetch = installNotes();
	render(GoalsWorkspace);
	await screen.findByRole('region', { name: 'acme goals' });
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Raise a seed round' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	expect(await screen.findByRole('alert')).toHaveTextContent('already exists');
	expect(fetch.calls.filter(call => call.init?.method === 'POST')).toHaveLength(0);
	expect(screen.getByLabelText('Goal name')).toHaveValue('Raise a seed round');
});

it('discards a pending creation form on an authenticated scope change', async () => {
	installNotes();
	render(GoalsWorkspace);
	await screen.findByRole('region', { name: 'acme goals' });
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Private draft' } });
	scopeIdentityStore.observe('other-owner', 'other-work');
	await waitFor(() => expect(screen.queryByRole('form', { name: 'Create goal' })).not.toBeInTheDocument());
	expect(screen.queryByDisplayValue('Private draft')).not.toBeInTheDocument();
});

it('ignores a creation response arriving after a scope switch', async () => {
	let release!: (response: Response) => void;
	const fetch = installFetchMock([
		{ method: 'GET', match: '/notes/tree', handle: () => jsonResponse({ entries: [] }) },
		{ method: 'POST', match: '/notes/file', handle: () => new Promise<Response>(resolve => { release = resolve; }) }
	]);
	render(GoalsWorkspace);
	await screen.findByText(/No goal workspaces yet/);
	await fireEvent.click(screen.getByRole('button', { name: 'New goal' }));
	await fireEvent.input(screen.getByLabelText('Folder name'), { target: { value: 'Private folder' } });
	await fireEvent.input(screen.getByLabelText('Goal name'), { target: { value: 'Private draft' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Create goal' }));
	await waitFor(() => expect(release).toBeTypeOf('function'));
	scopeIdentityStore.observe('other-owner', 'other-work');
	await waitFor(() => expect(screen.queryByRole('form', { name: 'Create goal' })).not.toBeInTheDocument());
	release(jsonResponse({}));
	await new Promise(resolve => setTimeout(resolve, 10));
	expect(fetch.calls.filter(call => call.method === 'POST')).toHaveLength(1);
	expect(screen.queryByText('Private draft')).not.toBeInTheDocument();
});

it('nests sub-goals under their main goal with status, owner, program and note links', async () => {
	installNotes();
	render(GoalsWorkspace);
	const mainRow = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	const subRow = within(mainRow).getByRole('listitem', { name: 'Apply to accelerators' });
	expect(within(subRow).getByText('blocked')).toBeInTheDocument();
	expect(within(subRow).getByText('internal')).toBeInTheDocument();
	expect(within(subRow).getByRole('link', { name: 'cro' })).toHaveAttribute('href', '/work?view=crew&agent=cro');
	expect(within(subRow).getByRole('link', { name: /^Playbook/ })).toHaveAttribute('href', '/work?view=programs&program=fundraising.md');
	expect(within(subRow).getByRole('link', { name: 'Note →' })).toHaveAttribute('href', '/notes?path=Workspaces%2Facme%2FGoals%2FApply%20to%20accelerators.md&workspace=work');
	expect(within(subRow).getByRole('link', { name: 'Find design partners' })).toHaveAttribute('href', '/notes?path=Workspaces%2Facme%2FGoals%2FFind%20design%20partners.md&workspace=work');
	expect(within(subRow).queryByRole('link', { name: 'Hire a designer' })).not.toBeInTheDocument();
	expect(within(subRow).getByText('Hire a designer')).toBeInTheDocument();
	expect(within(mainRow).getByText('Owner: you')).toBeInTheDocument();
	expect(within(mainRow).getByText('Target 2026-12-01')).toBeInTheDocument();
	expect(screen.getByRole('listitem', { name: 'Find design partners' })).toBeInTheDocument();
});

it('heads Not linked with the goal whose parent is missing and keeps its sub-goal under it', async () => {
	installNotes();
	render(GoalsWorkspace);
	const orphans = await screen.findByRole('region', { name: 'acme goals not linked' });
	expect(within(orphans).getByRole('heading', { name: 'Not linked' })).toBeInTheDocument();
	const lost = within(orphans).getByRole('listitem', { name: 'Lost goal' });
	expect(within(lost).getByText('Parent goal not found: Missing parent')).toBeInTheDocument();
	const step = within(lost).getByRole('listitem', { name: 'Lost step' });
	expect(within(step).queryByText(/Parent goal not found/)).not.toBeInTheDocument();
	expect(within(orphans).getAllByRole('listitem')).toHaveLength(2);
	expect(within(screen.getByRole('listitem', { name: 'Raise a seed round' })).queryByText('Lost goal')).not.toBeInTheDocument();
});

it('points to chat when there are no goal workspaces', async () => {
	const fetch = installNotes('missing');
	render(GoalsWorkspace);
	expect(await screen.findByText(/No goal workspaces yet\./)).toBeInTheDocument();
	expect(screen.getByRole('link', { name: 'Ask Magican in chat' })).toHaveAttribute('href', '/chat');
	expect(screen.queryByRole('alert')).not.toBeInTheDocument();
	await waitFor(() => expect(fetch.calls).toHaveLength(1));
});

it('shows a retryable error when the notes cannot be listed', async () => {
	installFetchMock([{ match: '/notes/tree', handle: () => jsonResponse({ message: 'Notes are unavailable.' }, { status: 503 }) }]);
	render(GoalsWorkspace);
	const alert = await screen.findByRole('alert');
	expect(alert).toHaveTextContent('Notes are unavailable.');
	expect(within(alert).getByRole('button', { name: 'Retry' })).toBeEnabled();
});

it('discards an old workspace response even when its transport ignores cancellation', async () => {
	let release!: (response: Response) => void;
	let listings = 0;
	installFetchMock([
		{ match: call => pathOf(call.url) === 'Workspaces', handle: () => ++listings === 1
			? new Promise<Response>(resolve => { release = resolve; })
			: jsonResponse({ entries: [{ name: 'Acme', relative_path: 'Workspaces/Acme', kind: 'dir' }] }) },
		{ match: '/notes/tree', handle: () => jsonResponse({ entries: [] }) }
	]);
	render(GoalsWorkspace);
	await waitFor(() => expect(listings).toBe(1));
	scopeIdentityStore.observe('other-owner', 'other-work');
	await screen.findByRole('region', { name: 'Acme goals' });
	release(jsonResponse({ entries: [{ name: 'Private venture', relative_path: 'Workspaces/Private venture', kind: 'dir' }] }));
	await new Promise(resolve => setTimeout(resolve, 20));
	expect(screen.queryByText('Private venture')).not.toBeInTheDocument();
	expect(screen.getByRole('region', { name: 'Acme goals' })).toBeInTheDocument();
});

it('draws the tree from one folder per main goal and says how much it did not read', async () => {
	const g = 'Workspaces/acme/Goals';
	const folders: Record<string, Array<{ name: string; relative_path: string; kind: 'dir' | 'file'; has_children?: boolean }>> = {
		Workspaces: [{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true }],
		[g]: [
			{ name: 'Raise a seed round', relative_path: `${g}/Raise a seed round`, kind: 'dir', has_children: true },
			{ name: 'Ship the beta', relative_path: `${g}/Ship the beta`, kind: 'dir', has_children: true },
			{ name: 'Old flat goal.md', relative_path: `${g}/Old flat goal.md`, kind: 'file' }
		],
		[`${g}/Raise a seed round`]: [
			{ name: 'Deeper', relative_path: `${g}/Raise a seed round/Deeper`, kind: 'dir', has_children: true },
			{ name: 'Apply to accelerators.md', relative_path: `${g}/Raise a seed round/Apply to accelerators.md`, kind: 'file' },
			{ name: 'Raise a seed round.md', relative_path: `${g}/Raise a seed round/Raise a seed round.md`, kind: 'file' }
		],
		[`${g}/Ship the beta`]: [{ name: 'Ship the beta.md', relative_path: `${g}/Ship the beta/Ship the beta.md`, kind: 'file' }]
	};
	const markdown: Record<string, string> = {
		'Raise a seed round.md': goalNote('kind: main\nstatus: active', 'Raise a seed round'),
		'Apply to accelerators.md': goalNote('kind: sub\nparent: "[[Raise a seed round]]"\nstatus: active', 'Apply to accelerators'),
		'Ship the beta.md': goalNote('kind: main\nstatus: proposed', 'Ship the beta'),
		'Old flat goal.md': goalNote('kind: main\nstatus: proposed', 'Old flat goal')
	};
	installFetchMock([
		{ match: '/notes/tree', handle: call => {
			const path = pathOf(call.url)!;
			return path in folders ? jsonResponse({ entries: folders[path] }) : jsonResponse({ error: 'not_found' }, { status: 404 });
		} },
		{ match: '/notes/file', handle: call => jsonResponse({ markdown: markdown[pathOf(call.url)!.split('/').pop()!] }) }
	]);
	render(GoalsWorkspace, { props: { limits: { folderDepth: 1, folders: 10, notes: 3 } } });
	const mainRow = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	const subRow = within(mainRow).getByRole('listitem', { name: 'Apply to accelerators' });
	expect(within(subRow).getByRole('link', { name: 'Note →' })).toHaveAttribute('href', `/notes?path=${encodeURIComponent(`${g}/Raise a seed round/Apply to accelerators.md`)}&workspace=work`);
	expect(screen.getByRole('listitem', { name: 'Ship the beta' })).toBeInTheDocument();
	expect(screen.queryByRole('listitem', { name: 'Old flat goal' })).not.toBeInTheDocument();
	expect(screen.getByText(/^1 more note in Goals\/ not shown: this screen reads the first 3 notes/)).toBeInTheDocument();
	expect(screen.getByText(/^1 folder in Goals\/ not read: this screen reads folders up to 1 level deep, and at most 10 of them\.$/)).toBeInTheDocument();
});

it('collapses and expands the sub-goals tree, and supports Expand all / Collapse all', async () => {
	installNotes();
	render(GoalsWorkspace);
	const mainRow = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	expect(within(mainRow).getByRole('listitem', { name: 'Apply to accelerators' })).toBeInTheDocument();

	const toggleBtn = within(mainRow).getByRole('button', { name: 'Collapse sub-goals of Raise a seed round' });
	await fireEvent.click(toggleBtn);
	expect(within(mainRow).queryByRole('listitem', { name: 'Apply to accelerators' })).not.toBeInTheDocument();
	expect(within(mainRow).getByRole('button', { name: 'Expand 1 sub-goal' })).toBeInTheDocument();

	// Click expand all
	await fireEvent.click(screen.getByRole('button', { name: 'Expand all' }));
	expect(within(mainRow).getByRole('listitem', { name: 'Apply to accelerators' })).toBeInTheDocument();

	// Click collapse all
	await fireEvent.click(screen.getByRole('button', { name: 'Collapse all' }));
	expect(within(mainRow).queryByRole('listitem', { name: 'Apply to accelerators' })).not.toBeInTheDocument();
});

it.each([
	['Find design partners', '+ Set target date', 'Set target date for Find design partners'],
	['Raise a seed round', 'Target 2026-12-01', 'Target date for Raise a seed round']
])('opens the calendar from the full date chip and keyboard for %s', async (title, text, label) => {
	const { calls } = installNotes();
	render(GoalsWorkspace);
	const row = await screen.findByRole('listitem', { name: title });
	const input = within(row).getByLabelText(label);
	const showPicker = vi.fn();
	Object.defineProperty(input, 'showPicker', { value: showPicker });
	await fireEvent.click(within(row).getByText(text));
	expect(showPicker).toHaveBeenCalledOnce();
	await fireEvent.keyDown(input, { key: 'Enter' });
	await fireEvent.keyDown(input, { key: ' ' });
	expect(showPicker).toHaveBeenCalledTimes(3);
	expect(calls.some(call => call.init?.method === 'PATCH')).toBe(false);
});

it('shows marker when target date is not set and allows setting and clearing it', async () => {
	const { calls } = installNotes();
	render(GoalsWorkspace);
	const row = await screen.findByRole('listitem', { name: 'Find design partners' });
	expect(within(row).getByText('+ Set target date')).toBeInTheDocument();

	const input = within(row).getByLabelText('Set target date for Find design partners');
	await fireEvent.change(input, { target: { value: '2026-11-20' } });
	expect(within(row).getByText('Target 2026-11-20')).toBeInTheDocument();
	const patches = () => calls.filter(call => call.init?.method === 'PATCH').map(call => JSON.parse(call.init?.body as string));
	await waitFor(() => expect(patches()).toHaveLength(1));
	expect(patches()[0]).toEqual({ path: 'Workspaces/acme/Goals/Find design partners.md', expected_revision: 'rev-0', set: { target: '2026-11-20' } });
	await waitFor(() => expect(within(row).getByRole('button', { name: 'Remove target date for Find design partners' })).toBeEnabled());

	await fireEvent.click(within(row).getByRole('button', { name: 'Remove target date for Find design partners' }));
	expect(within(row).getByText('+ Set target date')).toBeInTheDocument();
	// The second edit sends the revision the first one returned.
	await waitFor(() => expect(patches()).toHaveLength(2));
	expect(patches()[1]).toEqual({ path: 'Workspaces/acme/Goals/Find design partners.md', expected_revision: 'rev-1', unset: ['target'] });
	expect(calls.some(call => call.init?.method === 'PUT')).toBe(false);
});

it('reloads and says why when the goal changed since it was read', async () => {
	installNotes('present', { conflict: true });
	render(GoalsWorkspace);
	const row = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	await fireEvent.change(within(row).getByLabelText('Target date for Raise a seed round'), { target: { value: '2027-01-15' } });
	expect(await screen.findByText(/“Raise a seed round” changed since this screen loaded, so its date was not saved\./)).toBeInTheDocument();
	const reloaded = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	expect(within(reloaded).getByText('Target 2026-12-01')).toBeInTheDocument();
});

it('compact: says a workspace has no goals rather than that there are no workspaces', async () => {
	installFetchMock([
		{ match: call => pathOf(call.url) === 'Workspaces', handle: () => jsonResponse({ entries: [{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true }] }) },
		{ match: '/notes/tree', handle: () => jsonResponse({ entries: [] }) }
	]);
	render(GoalsWorkspace, { props: { compact: true } });
	expect(await screen.findByText('No goals in acme yet.')).toBeInTheDocument();
	expect(screen.queryByText(/No goal workspaces yet/)).not.toBeInTheDocument();
});

it('compact: reads only what the card shows, counts the rest and reuses the read', async () => {
	const g = 'Workspaces/acme/Goals';
	const main = (name: string) => ({ name, relative_path: `${g}/${name}`, kind: 'dir' as const, has_children: true });
	const file = (folder: string, name: string) => ({ name: `${name}.md`, relative_path: `${g}/${folder}/${name}.md`, kind: 'file' as const });
	const folders: Record<string, unknown[]> = {
		Workspaces: [{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true }],
		[g]: ['A', 'B', 'C', 'D', 'E', 'F', 'G'].map(main),
		[`${g}/A`]: [file('A', 'A'), file('A', 'A1'), file('A', 'A2'), file('A', 'A3'), file('A', 'A4')],
		[`${g}/B`]: [file('B', 'B')], [`${g}/C`]: [file('C', 'C')]
	};
	const { calls } = installFetchMock([
		{ match: '/notes/tree', handle: call => jsonResponse({ entries: folders[pathOf(call.url)!] ?? [] }) },
		{ match: '/notes/file', handle: call => {
			const name = pathOf(call.url)!.split('/').pop()!.replace(/\.md$/, '');
			return jsonResponse({ markdown: name.length === 1 ? goalNote('kind: main\nstatus: active', name) : goalNote(`kind: sub\nparent: "[[A]]"\nstatus: active`, name), revision: 'r' });
		} }
	]);
	const first = render(GoalsWorkspace, { props: { compact: true } });
	const card = await screen.findByRole('article', { name: 'A' });
	expect(within(card).getByText('4 sub-goals')).toBeInTheDocument();
	expect(within(card).getByRole('link', { name: '+2 more sub-goals →' })).toHaveAttribute('href', '/work?view=goals');
	expect(screen.getByRole('link', { name: /All goals/ })).toHaveTextContent('5+');
	const reads = calls.filter(call => call.url.includes('/notes/file')).length;
	expect(reads).toBe(5);
	expect(calls.some(call => pathOf(call.url) === `${g}/F`)).toBe(true);
	expect(calls.some(call => pathOf(call.url) === `${g}/G`)).toBe(false);
	first.unmount();
	render(GoalsWorkspace, { props: { compact: true } });
	await screen.findByRole('article', { name: 'A' });
	expect(calls.filter(call => call.url.includes('/notes/file')).length).toBe(reads);
	await fireEvent.click(screen.getByRole('button', { name: 'Refresh goals' }));
	await waitFor(() => expect(calls.filter(call => call.url.includes('/notes/file')).length).toBe(reads * 2));
});

const closedNotes = {
	'Workspaces/acme/Goals/Close the round.md': goalNote('kind: sub\nparent: "[[Raise a seed round]]"\nstatus: done\nclosed: 2026-10-20', 'Close the round'),
	'Workspaces/acme/Goals/Sign the term sheet.md': goalNote('kind: sub\nparent: "[[Close the round]]"\nstatus: active', 'Sign the term sheet'),
	'Workspaces/acme/Goals/Old idea.md': goalNote('kind: main\nstatus: dropped', 'Old idea')
};

it('hides done and dropped goals, counts only open ones and keeps an open goal whose parent closed', async () => {
	installNotes('present', { extra: closedNotes });
	render(GoalsWorkspace);
	const mainRow = await screen.findByRole('listitem', { name: 'Raise a seed round' });
	expect(screen.queryByRole('listitem', { name: 'Close the round' })).not.toBeInTheDocument();
	expect(screen.queryByRole('listitem', { name: 'Old idea' })).not.toBeInTheDocument();
	const lifted = within(mainRow).getByRole('listitem', { name: 'Sign the term sheet' });
	expect(within(lifted).getByText('Its parent goal “Close the round” is closed.')).toBeInTheDocument();
	expect(screen.getByText('6 open goals')).toBeInTheDocument();
	expect(screen.getByRole('link', { name: 'Closed 2' })).toHaveAttribute('href', '/work?view=goals&goals=closed');
	expect(screen.getByRole('link', { name: 'Open 6' })).toHaveAttribute('aria-current', 'page');
});

it('lists closed goals newest first with their parent and closed date', async () => {
	installNotes('present', { extra: closedNotes });
	render(GoalsWorkspace, { props: { closed: true } });
	const list = await screen.findByRole('region', { name: 'acme closed goals' });
	expect(within(list).getAllByRole('listitem').map(item => item.getAttribute('aria-label'))).toEqual(['Close the round', 'Old idea']);
	const row = within(list).getByRole('listitem', { name: 'Close the round' });
	expect(within(row).getByText('Under Raise a seed round')).toBeInTheDocument();
	expect(within(row).getByText('Closed 2026-10-20')).toBeInTheDocument();
	expect(within(row).getByRole('link', { name: 'Close the round' })).toHaveAttribute('href', '/notes?path=Workspaces%2Facme%2FGoals%2FClose%20the%20round.md&workspace=work');
	expect(screen.queryByRole('listitem', { name: 'Raise a seed round' })).not.toBeInTheDocument();
	expect(screen.getByRole('link', { name: 'Closed 2' })).toHaveAttribute('aria-current', 'page');
});

it('marks a goal done, moves it under Closed and reopens it', async () => {
	const { calls } = installNotes();
	render(GoalsWorkspace);
	const row = await screen.findByRole('listitem', { name: 'Find design partners' });
	await fireEvent.click(within(row).getByRole('button', { name: 'Mark Find design partners done' }));
	expect(await screen.findByText('“Find design partners” is marked done. It is listed under Closed.')).toBeInTheDocument();
	await waitFor(() => expect(screen.queryByRole('listitem', { name: 'Find design partners' })).not.toBeInTheDocument());
	const patches = () => calls.filter(call => call.init?.method === 'PATCH').map(call => JSON.parse(call.init?.body as string));
	expect(patches()).toEqual([{ path: 'Workspaces/acme/Goals/Find design partners.md', expected_revision: 'rev-0', set: { status: 'done', closed: localDate() } }]);
	cleanup();
	render(GoalsWorkspace, { props: { closed: true } });
	const closedRow = await screen.findByRole('listitem', { name: 'Find design partners' });
	await fireEvent.click(within(closedRow).getByRole('button', { name: 'Reopen Find design partners' }));
	expect(await screen.findByText('“Find design partners” is open again.')).toBeInTheDocument();
	await waitFor(() => expect(screen.queryByRole('listitem', { name: 'Find design partners' })).not.toBeInTheDocument());
	expect(patches()[1]).toEqual({ path: 'Workspaces/acme/Goals/Find design partners.md', expected_revision: 'rev-1', set: { status: 'active' }, unset: ['closed'] });
});

it('compact: leaves closed goals off the card and its count, keeping an open goal whose parent closed', async () => {
	const g = 'Workspaces/acme/Goals';
	const main = (name: string) => ({ name, relative_path: `${g}/${name}`, kind: 'dir' as const, has_children: true });
	const file = (folder: string, name: string) => ({ name: `${name}.md`, relative_path: `${g}/${folder}/${name}.md`, kind: 'file' as const });
	const folders: Record<string, unknown[]> = {
		Workspaces: [{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true }],
		[g]: ['A', 'B', 'C'].map(main),
		[`${g}/A`]: [file('A', 'A'), file('A', 'A1')], [`${g}/B`]: [file('B', 'B'), file('B', 'B1')], [`${g}/C`]: [file('C', 'C')]
	};
	const fields: Record<string, string> = {
		A: 'kind: main\nstatus: active', A1: 'kind: sub\nparent: "[[A]]"\nstatus: done',
		B: 'kind: main\nstatus: done', B1: 'kind: sub\nparent: "[[B]]"\nstatus: active', C: 'kind: main\nstatus: dropped'
	};
	installFetchMock([
		{ match: '/notes/tree', handle: call => jsonResponse({ entries: folders[pathOf(call.url)!] ?? [] }) },
		{ match: '/notes/file', handle: call => {
			const name = pathOf(call.url)!.split('/').pop()!.replace(/\.md$/, '');
			return jsonResponse({ markdown: goalNote(fields[name], name), revision: 'r' });
		} }
	]);
	render(GoalsWorkspace, { props: { compact: true } });
	const card = await screen.findByRole('article', { name: 'A' });
	expect(within(card).queryByText(/sub-goal/)).not.toBeInTheDocument();
	expect(screen.getByRole('article', { name: 'B1' })).toBeInTheDocument();
	expect(screen.queryByRole('article', { name: 'B' })).not.toBeInTheDocument();
	expect(screen.queryByRole('article', { name: 'C' })).not.toBeInTheDocument();
	expect(screen.getByRole('link', { name: /All goals/ })).toHaveTextContent('2');
});
