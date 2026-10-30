import { describe, expect, it } from 'vitest';
import { installFetchMock, jsonResponse } from '../../test/browser';
import { parseGoalNote, buildGoalTree, linkTarget, goalProgramHref, loadGoalWorkspaces, orphanNote, GOAL_READ_LIMITS, isGoalTargetDate, updateGoalTarget, updateGoalStatus, localDate, createGoalNote, createGoalFolder, isClosedGoal, openGoalTree, closedGoals, showsInOpenList, showsInClosedList, type Goal, type GoalNode } from './goals';

const main = `---
magician_kind: goal
workspace: acme
kind: main
owner: user
status: active
target: 2026-12-01
visibility: private
---
# Raise a seed round
Why it matters.`;

const sub = `---
magician_kind: goal
kind: sub
parent: "[[Raise a seed round]]"
peers: ["[[Find design partners]]"]
owner: cro
status: blocked
visibility: internal
program: fundraising.md
goal_id: harness:cro:fundraising
metrics:
  - investor_meetings
---
# Apply to accelerators`;

const goalNamed = (name: string, parent: string | null): Goal => parseGoalNote(`---\nmagician_kind: goal\n${parent ? `parent: "[[${parent}]]"\n` : ''}---\n# ${name}`, `G/${name}.md`)!;

describe('goal creation', () => {
	it('links a playbook in the initial note without scheduling work', async () => {
		const fetch = installFetchMock([{ method: 'POST', match: '/notes/file', handle: () => jsonResponse({}) }]);
		await createGoalNote({ workspace: 'acme', title: 'Launch', program: 'launch plan.md' });
		expect(fetch.calls).toHaveLength(1);
		const body = JSON.parse(fetch.calls[0].init!.body as string);
		expect(parseGoalNote(body.markdown, `${body.folder}/Launch.md`)).toMatchObject({
			program: 'launch plan.md', goalId: null, owner: 'user', status: 'proposed', visibility: 'private'
		});
	});

	it.each(['../outside.md', 'folder/plan.md', 'plan.md\nowner: ceo', '.hidden.md'])(
		'refuses an unsafe playbook reference before creating a note: %s', async program => {
			const fetch = installFetchMock([]);
			await expect(createGoalNote({ workspace: 'acme', title: 'Launch', program })).rejects.toThrow('Choose a playbook');
			expect(fetch.calls).toHaveLength(0);
		}
	);

	it('publishes a complete private proposed note in one request, with safe quoting', async () => {
		const fetch = installFetchMock([{ method: 'POST', match: '/notes/file', handle: () => jsonResponse({}) }]);
		await createGoalNote({ workspace: 'Founder "plan"', title: 'Launch', target: '2027-01-01', description: 'A clear outcome.' });
		expect(fetch.calls).toHaveLength(1);
		expect(fetch.calls[0].url).toContain('/notes/file/content');
		const body = JSON.parse(fetch.calls[0].init!.body as string);
		expect(body.folder).toBe('Workspaces/Founder "plan"/Goals/Launch');
		expect(parseGoalNote(body.markdown, `${body.folder}/Launch.md`)).toMatchObject({
			workspace: 'Founder "plan"', owner: 'user', status: 'proposed', visibility: 'private', kind: 'main', target: '2027-01-01', program: null, goalId: null
		});
		expect(body.markdown).toContain('A clear outcome.');
	});

	it('keeps a sub-goal alongside its parent and escapes linked names', async () => {
		const fetch = installFetchMock([{ method: 'POST', match: '/notes/file', handle: () => jsonResponse({}) }]);
		const parent = parseGoalNote(main, 'Workspaces/acme/Goals/Founder/Founder "plan".md')!;
		await createGoalNote({ workspace: 'acme', title: 'First milestone', parent });
		const body = JSON.parse(fetch.calls[0].init!.body as string);
		expect(body.folder).toBe('Workspaces/acme/Goals/Founder');
		expect(parseGoalNote(body.markdown, `${body.folder}/First milestone.md`)).toMatchObject({ kind: 'sub', parent: 'Founder "plan"' });
	});

	it('rejects path injection and a parent from another folder before writing', async () => {
		const fetch = installFetchMock([]);
		await expect(createGoalFolder('../escape')).rejects.toThrow('without slashes');
		await expect(createGoalNote({ workspace: 'acme', title: 'bad\nname' })).rejects.toThrow();
		await expect(createGoalNote({ workspace: 'acme', title: 'Launch', parent: goalNamed('Other', null) })).rejects.toThrow('Choose a parent');
		expect(fetch.calls).toHaveLength(0);
	});

	it('surfaces a collision without replacing or saving the existing note', async () => {
		const fetch = installFetchMock([{ method: 'POST', match: '/notes/file', handle: () => jsonResponse({ message: 'Already exists.' }, { status: 409 }) }]);
		await expect(createGoalNote({ workspace: 'acme', title: 'Launch' })).rejects.toThrow('Already exists.');
		expect(fetch.calls).toHaveLength(1);
	});
});

describe('goal notes', () => {
	it('reads frontmatter fields and the title', () => {
		const goal = parseGoalNote(main, 'Workspaces/acme/Goals/Raise a seed round.md');
		expect(goal).toMatchObject({ name: 'Raise a seed round', title: 'Raise a seed round', kind: 'main',
			owner: 'user', status: 'active', target: '2026-12-01', visibility: 'private', parent: null, peers: [] });
	});

	it('reads links, inline lists and block lists', () => {
		const goal = parseGoalNote(sub, 'Workspaces/acme/Goals/Apply to accelerators.md');
		expect(goal).toMatchObject({ parent: 'Raise a seed round', peers: ['Find design partners'],
			program: 'fundraising.md', goalId: 'harness:cro:fundraising', metrics: ['investor_meetings'] });
	});

	it('ignores notes that are not goals', () => {
		expect(parseGoalNote('---\nmagician_kind: note\n---\n# x', 'a.md')).toBeNull();
		expect(parseGoalNote('# no frontmatter', 'b.md')).toBeNull();
	});

	it('strips link brackets and aliases', () => {
		expect(linkTarget('[[Raise a seed round|seed]]')).toBe('Raise a seed round');
		expect(linkTarget('plain')).toBe('plain');
	});

	it('drops comments, keeps quoted hashes and reads bare links and unindented lists', () => {
		const goal = parseGoalNote(`---
# written by the goal template
magician_kind: goal # a goal
parent: [[Raise a seed round]]
peers: [[[Find design partners]], "[[Hire a designer|design]]"]
status: "Blocked #2"
owner: Founder's assistant
metrics:
- investor_meetings # weekly
- "pipeline, qualified"
---
Body without a heading.`, 'Workspaces/acme/Goals/Close the round.md');
		expect(goal).toMatchObject({ title: 'Close the round', kind: 'sub', parent: 'Raise a seed round',
			peers: ['Find design partners', 'Hire a designer'], status: 'blocked #2', owner: "Founder's assistant",
			metrics: ['investor_meetings', 'pipeline, qualified'], visibility: 'private' });
	});

	it('keeps a hash inside a later quoted flow-list item', () => {
		const goal = parseGoalNote('---\nmagician_kind: goal\npeers: ["a", "b #2"] # peers\n---\n# x', 'x.md');
		expect(goal?.peers).toEqual(['a', 'b #2']);
	});

	it('takes the title only from an H1 on the first non-blank line and accepts a fence with trailing spaces', () => {
		expect(parseGoalNote('---  \nmagician_kind: goal\n---\n\n# Ship the beta\n', 'G/beta.md')?.title).toBe('Ship the beta');
		expect(parseGoalNote('---\nmagician_kind: goal\n---\nIntro first.\n# Later heading', 'G/Ship the beta.md')?.title).toBe('Ship the beta');
	});

	it('resolves a shared link key to the first goal listed', () => {
		const first = parseGoalNote('---\nmagician_kind: goal\n---\n# First', 'G/Plan.md')!;
		const second = parseGoalNote('---\nmagician_kind: goal\n---\n# Second', 'H/plan.md')!;
		expect(buildGoalTree([first, second]).byKey.get('plan')).toBe(first);
	});

	it('nests sub-goals under their parent and keeps orphans visible', () => {
		const goals = [parseGoalNote(main, 'G/Raise a seed round.md')!, parseGoalNote(sub, 'G/Apply to accelerators.md')!,
			parseGoalNote(sub.replace('Raise a seed round', 'Missing parent').replace('Apply to accelerators', 'Lost'), 'G/Lost.md')!];
		const tree = buildGoalTree(goals);
		expect(tree.roots.map(n => n.goal.name)).toEqual(['Raise a seed round']);
		expect(tree.roots[0].children.map(n => n.goal.name)).toEqual(['Apply to accelerators']);
		expect(tree.orphans.map(n => [n.goal.name, n.reason])).toEqual([['Lost', 'missing-parent']]);
	});

	it('heads a Not linked tree at a missing parent and keeps its sub-goals nested', () => {
		const tree = buildGoalTree([goalNamed('C', 'B'), goalNamed('B', 'Missing'), goalNamed('D', 'C')]);
		expect(tree.roots).toEqual([]);
		expect(tree.orphans.map(n => [n.goal.name, n.reason])).toEqual([['B', 'missing-parent']]);
		expect(tree.orphans[0].children.map(n => n.goal.name)).toEqual(['C']);
		expect(tree.orphans[0].children[0].children.map(n => n.goal.name)).toEqual(['D']);
		expect(orphanNote(tree.orphans[0])).toBe('Parent goal not found: Missing');
	});

	it('heads a self-parented goal and one member of each parent loop, placing every goal once', () => {
		const tree = buildGoalTree([goalNamed('Loop', 'Loop'), goalNamed('Pong', 'Ping'), goalNamed('Ping', 'Pong'), goalNamed('Tail', 'Pong'), goalNamed('Root', null), goalNamed('Under loop', 'Loop')]);
		expect(tree.roots.map(n => n.goal.name)).toEqual(['Root']);
		expect(tree.orphans.map(n => [n.goal.name, n.reason])).toEqual([['Loop', 'self-parent'], ['Ping', 'parent-loop']]);
		expect(tree.orphans[0].children.map(n => n.goal.name)).toEqual(['Under loop']);
		const ping = tree.orphans[1];
		expect(ping.children.map(n => n.goal.name)).toEqual(['Pong']);
		expect(ping.children[0].children.map(n => n.goal.name)).toEqual(['Tail']);
		const names = (nodes: GoalNode[]): string[] => nodes.flatMap(n => [n.goal.name, ...names(n.children)]);
		expect(names([...tree.roots, ...tree.orphans]).sort()).toEqual(['Loop', 'Ping', 'Pong', 'Root', 'Tail', 'Under loop']);
		expect(tree.orphans.map(orphanNote)).toEqual(['Names itself as its parent.', 'Part of a parent loop: its parent goals lead back to it.']);
	});

	it('matches a parent link by note name, ignoring folders, headings, extension and case', () => {
		const tree = buildGoalTree([goalNamed('Raise a seed round', null), goalNamed('Pitch deck', 'Workspaces/acme/Goals/raise a seed round.md#Why')]);
		expect(tree.roots[0].children.map(n => n.goal.name)).toEqual(['Pitch deck']);
		expect(tree.orphans).toEqual([]);
	});

	it('links a program by its document name', () => {
		expect(goalProgramHref('fundraising.md')).toBe('/work?view=programs&program=fundraising.md');
		expect(goalProgramHref('hiring plan')).toBe('/work?view=programs&program=hiring%20plan.md');
	});
});

const pathOf = (url: string) => new URL(url, 'http://local').searchParams.get('path');

describe('goal workspaces', () => {
	it('reads every goal note in each workspace Goals folder', async () => {
		const fetch = installFetchMock([
			{ method: 'GET', match: '/notes/tree?path=', handle: call => {
				const path = pathOf(call.url);
				if (path === 'Workspaces') return jsonResponse({ path, entries: [
					{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true },
					{ name: 'README.md', relative_path: 'Workspaces/README.md', kind: 'file' }
				] });
				if (path === 'Workspaces/acme/Goals') return jsonResponse({ path, entries: [
					{ name: 'Raise a seed round.md', relative_path: 'Workspaces/acme/Goals/Raise a seed round.md', kind: 'file' },
					{ name: 'Apply to accelerators.md', relative_path: 'Workspaces/acme/Goals/Apply to accelerators.md', kind: 'file' },
					{ name: 'Archive', relative_path: 'Workspaces/acme/Goals/Archive', kind: 'dir', has_children: true },
					{ name: 'pitch.pdf', relative_path: 'Workspaces/acme/Goals/pitch.pdf', kind: 'file' }
				] });
				return jsonResponse({ error: 'not_found' }, { status: 404 });
			} },
			{ method: 'GET', match: '/notes/file?path=', handle: call => {
				const path = pathOf(call.url)!;
				return jsonResponse({ title: '', relative_path: path, markdown: path.endsWith('Raise a seed round.md') ? main : sub });
			} }
		]);
		const workspaces = await loadGoalWorkspaces();
		expect(workspaces).toHaveLength(1);
		expect(workspaces[0]).toMatchObject({ name: 'acme', path: 'Workspaces/acme', problems: [], notShown: 0, foldersNotRead: 0 });
		expect(workspaces[0].goals.map(g => g.name)).toEqual(['Raise a seed round', 'Apply to accelerators']);
		expect(workspaces[0].goals[1].path).toBe('Workspaces/acme/Goals/Apply to accelerators.md');
		const read = fetch.calls.filter(call => call.url.includes('/notes/file')).map(call => pathOf(call.url));
		expect(read).toEqual(['Workspaces/acme/Goals/Raise a seed round.md', 'Workspaces/acme/Goals/Apply to accelerators.md']);
		expect(fetch.calls[0].url).toBe('/api/magician/v2/notes/tree?path=Workspaces');
	});

	it('treats a missing Workspaces folder as no workspaces', async () => {
		installFetchMock([{ match: '/notes/tree', handle: () => jsonResponse({ error: 'not_found' }, { status: 404 }) }]);
		await expect(loadGoalWorkspaces()).resolves.toEqual([]);
	});

	it('fails when the Workspaces folder cannot be listed', async () => {
		installFetchMock([{ match: '/notes/tree', handle: () => jsonResponse({ error: 'notes_unavailable', message: 'Notes are unavailable.' }, { status: 503 }) }]);
		await expect(loadGoalWorkspaces()).rejects.toThrow('Notes are unavailable.');
	});

	it('keeps the readable goals and reports a note that could not be read', async () => {
		installFetchMock([
			{ match: call => pathOf(call.url) === 'Workspaces', handle: () => jsonResponse({ entries: [
				{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir' },
				{ name: 'empty', relative_path: 'Workspaces/empty', kind: 'dir' }
			] }) },
			{ match: call => pathOf(call.url) === 'Workspaces/acme/Goals', handle: () => jsonResponse({ entries: [
				{ name: 'Raise a seed round.md', relative_path: 'Workspaces/acme/Goals/Raise a seed round.md', kind: 'file' },
				{ name: 'Broken.md', relative_path: 'Workspaces/acme/Goals/Broken.md', kind: 'file' }
			] }) },
			{ match: call => pathOf(call.url) === 'Workspaces/empty/Goals', handle: () => jsonResponse({ error: 'not_found' }, { status: 404 }) },
			{ match: call => pathOf(call.url)?.endsWith('Broken.md') ?? false, handle: () => jsonResponse({ error: 'io_error' }, { status: 500 }) },
			{ match: '/notes/file', handle: () => jsonResponse({ markdown: main }) }
		]);
		const workspaces = await loadGoalWorkspaces();
		expect(workspaces.map(w => w.name)).toEqual(['acme', 'empty']);
		expect(workspaces[0].goals.map(g => g.name)).toEqual(['Raise a seed round']);
		expect(workspaces[0].problems).toEqual(['Could not read Workspaces/acme/Goals/Broken.md (500).']);
		expect(workspaces[1]).toMatchObject({ goals: [], problems: [] });
	});

	// A notes space as the tree API lists it: one level per request, folders first.
	type Entry = { name: string; relative_path: string; kind: 'dir' | 'file'; has_children?: boolean };
	function notesSpace(paths: string[], { broken = [] as string[] } = {}) {
		const folders = new Map<string, Entry[]>();
		const add = (folder: string, entry: Entry) => {
			const entries = folders.get(folder) ?? [];
			if (!entries.some(e => e.relative_path === entry.relative_path)) entries.push(entry);
			folders.set(folder, entries);
		};
		for (const path of paths) {
			const parts = path.split('/');
			for (let i = 1; i < parts.length; i++) {
				const folder = parts.slice(0, i).join('/'), relative_path = parts.slice(0, i + 1).join('/');
				const isFile = i === parts.length - 1 && path.endsWith('.md');
				add(folder, { name: parts[i], relative_path, kind: isFile ? 'file' : 'dir', ...(isFile ? {} : { has_children: !(i === parts.length - 1) }) });
			}
		}
		for (const entries of folders.values()) entries.sort((a, b) => a.kind.localeCompare(b.kind) || a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
		const fetch = installFetchMock([
			{ match: '/notes/tree', handle: call => {
				const path = pathOf(call.url)!;
				if (broken.includes(path)) return jsonResponse({ error: 'io_error', message: `Could not list ${path}.` }, { status: 500 });
				return folders.has(path) ? jsonResponse({ path, entries: folders.get(path) }) : jsonResponse({ error: 'not_found' }, { status: 404 });
			} },
			{ match: '/notes/file', handle: call => {
				const path = pathOf(call.url)!;
				const name = path.split('/').pop()!.replace(/\.md$/, '');
				return jsonResponse({ relative_path: path, markdown: `---\nmagician_kind: goal\n---\n# ${name}\n` });
			} }
		]);
		return {
			listed: () => fetch.calls.filter(call => call.url.includes('/notes/tree')).map(call => pathOf(call.url)),
			read: () => fetch.calls.filter(call => call.url.includes('/notes/file')).map(call => pathOf(call.url))
		};
	}

	it('reads one folder per main goal, its sub-goals beside it, and legacy notes directly in Goals', async () => {
		const g = 'Workspaces/acme/Goals';
		const space = notesSpace([
			`${g}/Ship Picker v1/Pilot with three warehouses.md`,
			`${g}/Ship Picker v1/Ship Picker v1.md`,
			`${g}/Ship Picker v1/Drafts/Draft goal.md`,
			`${g}/Hire two field engineers/Hire two field engineers.md`,
			`${g}/Empty folder`,
			`${g}/Old flat goal.md`
		]);
		const [acme] = await loadGoalWorkspaces();
		// The main goal's own note leads its folder; a folder's notes come before its subfolders.
		expect(acme.goals.map(goal => goal.path)).toEqual([
			`${g}/Hire two field engineers/Hire two field engineers.md`,
			`${g}/Ship Picker v1/Ship Picker v1.md`,
			`${g}/Ship Picker v1/Pilot with three warehouses.md`,
			`${g}/Ship Picker v1/Drafts/Draft goal.md`,
			`${g}/Old flat goal.md`
		]);
		expect(acme.goals.map(goal => goal.name)).toContain('Pilot with three warehouses');
		expect(acme).toMatchObject({ problems: [], notShown: 0, foldersNotRead: 0 });
		// A folder the tree API reports as empty is never listed.
		expect(space.listed()).not.toContain(`${g}/Empty folder`);
		expect(space.listed()).toContain(`${g}/Ship Picker v1/Drafts`);
	});

	it('places a sub-goal by its parent field, not by the folder it sits in', async () => {
		const g = 'Workspaces/acme/Goals';
		installFetchMock([
			{ match: '/notes/tree', handle: call => {
				const path = pathOf(call.url);
				if (path === 'Workspaces') return jsonResponse({ entries: [{ name: 'acme', relative_path: 'Workspaces/acme', kind: 'dir', has_children: true }] });
				if (path === g) return jsonResponse({ entries: [
					{ name: 'Raise a seed round', relative_path: `${g}/Raise a seed round`, kind: 'dir', has_children: true },
					{ name: 'Ship the beta', relative_path: `${g}/Ship the beta`, kind: 'dir', has_children: true }
				] });
				if (path === `${g}/Raise a seed round`) return jsonResponse({ entries: [{ name: 'Raise a seed round.md', relative_path: `${g}/Raise a seed round/Raise a seed round.md`, kind: 'file' }] });
				if (path === `${g}/Ship the beta`) return jsonResponse({ entries: [
					{ name: 'Apply to accelerators.md', relative_path: `${g}/Ship the beta/Apply to accelerators.md`, kind: 'file' },
					{ name: 'Ship the beta.md', relative_path: `${g}/Ship the beta/Ship the beta.md`, kind: 'file' }
				] });
				return jsonResponse({ error: 'not_found' }, { status: 404 });
			} },
			{ match: '/notes/file', handle: call => {
				const path = pathOf(call.url)!;
				if (path.endsWith('Raise a seed round.md')) return jsonResponse({ markdown: main });
				if (path.endsWith('Apply to accelerators.md')) return jsonResponse({ markdown: sub });
				return jsonResponse({ markdown: '---\nmagician_kind: goal\nkind: main\n---\n# Ship the beta' });
			} }
		]);
		const [acme] = await loadGoalWorkspaces();
		const tree = buildGoalTree(acme.goals);
		expect(tree.roots.map(node => [node.goal.name, node.children.map(child => child.goal.name)])).toEqual([
			['Raise a seed round', ['Apply to accelerators']], ['Ship the beta', []]
		]);
	});

	it('stops at the folder depth limit and counts the folders it did not read', async () => {
		const g = 'Workspaces/acme/Goals';
		const space = notesSpace([`${g}/Main/Main.md`, `${g}/Main/Two/Three/Deep.md`, `${g}/Main/Two/Three/Four/Too deep.md`]);
		const [acme] = await loadGoalWorkspaces(undefined, { ...GOAL_READ_LIMITS, folderDepth: 3 });
		expect(acme.goals.map(goal => goal.name)).toEqual(['Main', 'Deep']);
		expect(acme).toMatchObject({ foldersNotRead: 1, notShown: 0 });
		expect(space.listed()).not.toContain(`${g}/Main/Two/Three/Four`);
	});

	it('reads at most the note limit and the folder limit per workspace and counts the rest', async () => {
		const g = 'Workspaces/acme/Goals';
		const space = notesSpace([`${g}/A/A.md`, `${g}/A/A1.md`, `${g}/A/A2.md`, `${g}/B/B.md`, `${g}/C/C.md`, `${g}/Flat.md`]);
		const [acme] = await loadGoalWorkspaces(undefined, { folderDepth: 3, folders: 2, notes: 3 });
		expect(acme.goals.map(goal => goal.name)).toEqual(['A', 'A1', 'A2']);
		// B.md and Flat.md are listed but past the note limit; folder C is past the folder limit.
		expect(acme).toMatchObject({ notShown: 2, foldersNotRead: 1, problems: [] });
		expect(space.read()).toHaveLength(3);
		expect(space.listed()).not.toContain(`${g}/C`);
	});

	it('reports a main goal folder that could not be listed and still shows the others', async () => {
		const g = 'Workspaces/acme/Goals';
		notesSpace([`${g}/Good/Good.md`, `${g}/Bad/Bad.md`, `${g}/Flat.md`], { broken: [`${g}/Bad`] });
		const [acme] = await loadGoalWorkspaces();
		expect(acme.goals.map(goal => goal.name)).toEqual(['Good', 'Flat']);
		expect(acme.problems).toEqual([`Could not list ${g}/Bad.`]);
	});
});

describe('updateGoalTarget', () => {
	it('accepts only real calendar dates written YYYY-MM-DD', () => {
		expect(isGoalTargetDate('2026-11-20')).toBe(true);
		expect(isGoalTargetDate('2028-02-29')).toBe(true);
		for (const bad of ['2026-02-30', '2027-02-29', '2026-13-01', '2026-1-5', '20/11/2026', '2026-11-20T00:00', '']) expect(isGoalTargetDate(bad)).toBe(false);
	});

	it('refuses an invalid date without a request and reports a stale revision as a conflict', async () => {
		const { calls } = installFetchMock([
			{ method: 'PATCH', match: '/notes/front-matter', handle: () => jsonResponse({ error: 'note_revision_conflict', current_revision: 'blake3:new' }, { status: 409 }) }
		]);
		const goal = { path: 'Workspaces/acme/Goals/A/A.md', revision: 'blake3:old' };
		await expect(updateGoalTarget(goal, '2026-02-30')).resolves.toMatchObject({ ok: false, reason: 'invalid' });
		expect(calls).toHaveLength(0);
		await expect(updateGoalTarget(goal, '2026-11-20')).resolves.toMatchObject({ ok: false, reason: 'conflict' });
		expect(JSON.parse(calls[0].init?.body as string)).toEqual({ path: goal.path, expected_revision: 'blake3:old', set: { target: '2026-11-20' } });
		await expect(updateGoalTarget({ path: goal.path }, null)).resolves.toMatchObject({ ok: false, reason: 'conflict' });
		expect(calls).toHaveLength(1);
	});

	it('clears with unset and returns the saved revision', async () => {
		const { calls } = installFetchMock([
			{ method: 'PATCH', match: '/notes/front-matter', handle: () => jsonResponse({ relative_path: 'G.md', markdown: '', revision: 'blake3:next' }) }
		]);
		await expect(updateGoalTarget({ path: 'G.md', revision: 'blake3:old' }, null)).resolves.toEqual({ ok: true, revision: 'blake3:next' });
		expect(JSON.parse(calls[0].init?.body as string)).toEqual({ path: 'G.md', expected_revision: 'blake3:old', unset: ['target'] });
	});
});

describe('closed goals', () => {
	const goal = (name: string, parent: string | null, status: string, closed?: string) => parseGoalNote(`---\nmagician_kind: goal\n${parent ? `parent: "[[${parent}]]"\n` : ''}status: ${status}\n${closed ? `closed: ${closed}\n` : ''}---\n# ${name}`, `G/${name}.md`)!;

	it('reads the closed date and treats done and dropped as closed', () => {
		expect(goal('A', null, 'Done', '2026-10-28')).toMatchObject({ status: 'done', closed: '2026-10-28' });
		expect(goal('B', null, 'active').closed).toBeNull();
		expect(['done', 'dropped', 'active', 'blocked', 'proposed'].map(status => isClosedGoal({ status }))).toEqual([true, true, false, false, false]);
	});

	it('moves open goals under a closed one up to the nearest open goal and names the closed parent', () => {
		const tree = openGoalTree(buildGoalTree([
			goal('Main', null, 'active'), goal('Step', 'Main', 'done'), goal('Task', 'Step', 'active'),
			goal('Gone', null, 'dropped'), goal('Kept', 'Gone', 'proposed'),
			goal('Lost', 'Missing', 'active'), goal('Lost done', 'Missing', 'done'), goal('Lost step', 'Lost done', 'blocked')
		]));
		expect(tree.roots.map(node => node.goal.name)).toEqual(['Main', 'Kept']);
		expect(tree.roots[0].closedParent).toBeUndefined();
		expect(tree.roots[0].children.map(node => [node.goal.name, node.closedParent])).toEqual([['Task', 'Step']]);
		expect(tree.roots[1].closedParent).toBe('Gone');
		expect(tree.orphans.map(node => [node.goal.name, node.reason, node.closedParent])).toEqual([
			['Lost', 'missing-parent', undefined], ['Lost step', 'missing-parent', 'Lost done']
		]);
	});

	it('lists closed goals most recently closed first, undated ones last by title', () => {
		const goals = [goal('B', null, 'done'), goal('Old', null, 'done', '2026-01-02'), goal('Open', null, 'active'), goal('A', null, 'dropped'), goal('New', null, 'done', '2026-10-28')];
		expect(closedGoals(goals).map(g => g.name)).toEqual(['New', 'Old', 'A', 'B']);
	});

	it('lists a workspace whose goals are all closed only under Closed', () => {
		const space = (goals: Goal[], read: { problems?: string[]; notShown?: number; foldersNotRead?: number } = {}) =>
			({ name: 'w', path: 'Workspaces/w', goals, problems: [], notShown: 0, foldersNotRead: 0, ...read });
		const allClosed = space([goal('Main', null, 'done'), goal('Step', 'Main', 'dropped')]);
		const mixed = space([goal('Main', null, 'done'), goal('Step', 'Main', 'active')]);
		const open = space([goal('Main', null, 'active')]);
		const empty = space([]);
		expect([allClosed, mixed, open, empty].map(showsInOpenList)).toEqual([false, true, true, true]);
		expect([allClosed, mixed, open, empty].map(showsInClosedList)).toEqual([true, true, false, false]);
		// A partial read may have missed goals, so both lists keep the workspace and its notes.
		for (const read of [{ problems: ['G/x.md could not be read'] }, { notShown: 2 }, { foldersNotRead: 1 }]) {
			expect(showsInOpenList(space(allClosed.goals, read))).toBe(true);
			expect(showsInClosedList(space(open.goals, read))).toBe(true);
		}
	});
});

describe('updateGoalStatus', () => {
	it('marks a goal done with the date it closed and reopens it as active without one', async () => {
		const { calls } = installFetchMock([
			{ method: 'PATCH', match: '/notes/front-matter', handle: () => jsonResponse({ relative_path: 'G.md', markdown: '', revision: 'blake3:next' }) }
		]);
		await expect(updateGoalStatus({ path: 'G.md', revision: 'blake3:old' }, 'done', undefined, '2026-10-28')).resolves.toEqual({ ok: true, revision: 'blake3:next' });
		await expect(updateGoalStatus({ path: 'G.md', revision: 'blake3:next' }, 'active')).resolves.toMatchObject({ ok: true });
		expect(calls.map(call => JSON.parse(call.init?.body as string))).toEqual([
			{ path: 'G.md', expected_revision: 'blake3:old', set: { status: 'done', closed: '2026-10-28' } },
			{ path: 'G.md', expected_revision: 'blake3:next', set: { status: 'active' }, unset: ['closed'] }
		]);
	});

	it('refuses without a revision and dates by the local calendar', async () => {
		await expect(updateGoalStatus({ path: 'G.md' }, 'done')).resolves.toMatchObject({ ok: false, reason: 'conflict' });
		expect(localDate(new Date(2026, 0, 5, 23, 30))).toBe('2026-01-05');
	});
});
