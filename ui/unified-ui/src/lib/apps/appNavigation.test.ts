import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { parseAppDirectoryPage, type AppDirectoryEntry } from './appDirectory';
import {
	admitAppNavigation,
	appNavigationHref,
	appNavigationFallbackRoute,
	appNavigationSection,
	appNavigationTabs,
	mountAppNavigation,
	parseAppNavigationEntries,
	parseAppNavigationRoute,
	RESERVED_FIRST_PARTY_ROUTE_ROOTS,
	type AppNavigationEntry
} from './appNavigation';

function declaration(overrides: Partial<AppNavigationEntry> = {}): AppNavigationEntry {
	return {
		id: 'meetings_console',
		title: 'Meetings',
		route: '/meetings-console',
		placement: { kind: 'section', section: 'observe' },
		surface: { kind: 'custom_surface', entry_point: '/console', fallback_view: 'sessions' },
		...overrides
	};
}

function entry(overrides: Partial<AppDirectoryEntry> = {}): AppDirectoryEntry {
	return {
		installation_id: 'meetings',
		name: 'Meetings',
		description: 'Meeting console',
		icon: { kind: 'monogram', value: 'M' },
		package_version: '1.0.0',
		package_revision_ref: 'package:meetings:1',
		installation_generation: 3,
		status: 'enabled',
		views: [],
		actions: [],
		navigation: [declaration()],
		storage: { record_count: 0, revision_count: 0, payload_bytes: 0, attachment_bytes: 0 },
		record_count: 0,
		payload_bytes: 0,
		...overrides
	};
}

/**
 * The `app.navigation` block of a manifest this repo actually ships, read from
 * the bytes rather than restated. There is no YAML parser on the web side, so
 * this reads the one field the shell's admission turns on — the declared route
 * — and fails loudly if the block moves, which is the point: a seed edit should
 * surface here rather than change what first-party chrome shows.
 */
function seedNavigationRoutes(packageName: string): string[] {
	const manifest = join(
		process.cwd(), '..', '..', 'magician_data_v3', 'system', packageName, 'app', 'SKILL.md');
	const lines = readFileSync(manifest, 'utf8').split('\n');
	const start = lines.indexOf('  navigation:');
	expect(start, `${packageName} declares no navigation block; update this test`)
		.toBeGreaterThan(-1);
	const block: string[] = [];
	for (const line of lines.slice(start + 1)) {
		// A sibling key at the `app:` child indent closes the block.
		if (/^ {0,2}\S/.test(line)) break;
		block.push(line);
	}
	return block
		.map((line) => /^ {6}route: (\S+)$/.exec(line)?.[1])
		.filter((route): route is string => route !== undefined);
}

describe('app navigation declarations', () => {
	it('treats an absent projection as no navigation at all', () => {
		// The closed door this gate is about: a server that does not project
		// `navigation` must add nothing, not fail and not guess.
		expect(parseAppNavigationEntries(undefined)).toEqual([]);
		expect(mountAppNavigation([entry({ navigation: undefined })])).toEqual([]);
	});

	it('refuses every off-shape declaration rather than mounting part of one', () => {
		expect(parseAppNavigationEntries('not-a-list')).toBeNull();
		expect(parseAppNavigationEntries([{ ...declaration(), extra: 1 }])).toBeNull();
		expect(parseAppNavigationEntries([{ ...declaration(), title: '   ' }])).toBeNull();
		expect(parseAppNavigationEntries([{ ...declaration(), id: 'not a name' }])).toBeNull();
		expect(parseAppNavigationEntries([{ ...declaration(), placement: { kind: 'modal' } }])).toBeNull();
		expect(parseAppNavigationEntries([{ ...declaration(), surface: { kind: 'view' } }])).toBeNull();
		expect(parseAppNavigationEntries([
			{ ...declaration(), surface: { kind: 'custom_surface', entry_point: '/console' } }
		])).toBeNull();
		expect(parseAppNavigationEntries(
			Array.from({ length: 17 }, (_, index) => declaration({ id: `nav_${index}`, route: `/nav-${index}` }))
		)).toBeNull();
		expect(parseAppNavigationEntries([declaration(), declaration({ route: '/other' })])).toBeNull();
		expect(parseAppNavigationEntries([declaration(), declaration({ id: 'other' })])).toBeNull();
	});

	it('accepts only bounded static first-party routes', () => {
		expect(parseAppNavigationRoute('/meetings-console')).toBe('/meetings-console');
		expect(parseAppNavigationRoute('/a/b/c')).toBe('/a/b/c');
		expect(parseAppNavigationRoute('/')).toBeNull();
		expect(parseAppNavigationRoute('meetings-console')).toBeNull();
		expect(parseAppNavigationRoute('/meetings/:id')).toBeNull();
		expect(parseAppNavigationRoute('/meetings?x=1')).toBeNull();
		expect(parseAppNavigationRoute('/meetings#x')).toBeNull();
		expect(parseAppNavigationRoute('/meetings%2f')).toBeNull();
		expect(parseAppNavigationRoute('/..')).toBeNull();
		expect(parseAppNavigationRoute('//evil.example')).toBeNull();
		expect(parseAppNavigationRoute('/méétings')).toBeNull();
		expect(parseAppNavigationRoute(`/${'a'.repeat(600)}`)).toBeNull();
	});

	it('derives the canonical app-surface href the shell already serves', () => {
		expect(appNavigationHref('meetings', { kind: 'view', view: 'sessions' }, [{ view_id: 'sessions', route: '/apps/meetings' }]))
			.toBe('/apps/meetings');
		expect(appNavigationHref('learning', { kind: 'view', view: 'queue' }, [{ view_id: 'queue', route: '/apps/learning/review/pending' }]))
			.toBe('/apps/learning/review/pending');
		expect(appNavigationHref('meetings', {
			kind: 'custom_surface', entry_point: '/console', fallback_view: 'sessions'
		})).toBe('/apps/meetings/console');
		expect(appNavigationHref('../evil', { kind: 'view', view: 'sessions' })).toBeNull();
	});

	it('does not invent view routes or mount unsafe and ambiguous view bindings', () => {
		const surface = { kind: 'view', view: 'queue' } as const;
		expect(appNavigationHref('learning', surface)).toBeNull();
		for (const route of ['//evil.example', '/queue', '/apps/other/queue', '/apps/learning/../other', '/apps/learning/items/:id', '/apps/learning/queue?x=1']) {
			expect(appNavigationHref('learning', surface, [{ view_id: 'queue', route }])).toBeNull();
		}
		expect(appNavigationHref('learning', surface, [
			{ view_id: 'queue', route: '/apps/learning' }, { view_id: 'queue', route: '/apps/learning/queue' }
		])).toBeNull();
	});

	it('mounts Learning at its declared root view route', () => {
		const mounted = mountAppNavigation([entry({
			installation_id: 'learning',
			views: [{ view_id: 'queue', label: 'Queue', route: '/apps/learning', pinned: false }],
			navigation: [declaration({ route: '/learning', surface: { kind: 'view', view: 'queue' } })]
		})]);
		expect(mounted.map((item) => item.href)).toEqual(['/apps/learning']);
	});

	it('resolves only the enabled installation’s exact declared fallback', () => {
		const source = entry({ views: [{ view_id: 'sessions', label: 'Sessions', route: '/apps/meetings', pinned: false }] });
		expect(appNavigationFallbackRoute(source, '/console')).toBe('/');
		expect(appNavigationFallbackRoute(source, '/another-page')).toBeNull();
		expect(appNavigationFallbackRoute({ ...source, status: 'disabled' }, '/console')).toBeNull();
		expect(appNavigationFallbackRoute({ ...source, views: [] }, '/console')).toBeNull();
		expect(appNavigationFallbackRoute({ ...source, views: [{ ...source.views[0], route: '/apps/meetings/console' }] }, '/console')).toBeNull();
		expect(appNavigationFallbackRoute({ ...source, navigation: [declaration(), declaration({
			id: 'other', route: '/other', surface: { kind: 'custom_surface', entry_point: '/console', fallback_view: 'missing' }
		})] }, '/console')).toBeNull();
	});

	it('mounts only an enabled installation', () => {
		expect(mountAppNavigation([entry()])).toHaveLength(1);
		for (const status of ['ready_for_review', 'disabled', 'quarantined', 'update_pending'] as const) {
			expect(mountAppNavigation([entry({ status })])).toEqual([]);
		}
	});

	it('reserves every route root the shell actually serves', () => {
		// The reserved set is hand-written, so it can only stay true if a new
		// first-party route makes this fail. Without that, adding a route would
		// silently open it to a manifest that claims the same name.
		const routeRoots = (['src/routes', 'src/routes/(app)'] as const).flatMap((directory) =>
			readdirSync(join(process.cwd(), directory), { withFileTypes: true })
				.filter((item) => item.isDirectory() &&
					!item.name.startsWith('(') && !item.name.startsWith('['))
				.map((item) => item.name.toLowerCase()));
		expect(routeRoots.length).toBeGreaterThan(20);
		for (const root of routeRoots) {
			expect(RESERVED_FIRST_PARTY_ROUTE_ROOTS.has(root)).toBe(true);
		}
	});

	it('drops a declaration rooted at a first-party destination, and says so', () => {
		for (const route of ['/observe', '/observe/console', '/apps/x', '/api/x', '/Observe/x']) {
			const admission = admitAppNavigation([entry({ navigation: [declaration({ route })] })]);
			expect(admission.mounted).toEqual([]);
			expect(admission.refused).toEqual([
				{ installation_id: 'meetings', route, reason: 'reserved_route_root' }
			]);
		}
	});

	it('pins a route two installations both claim to nobody, and refuses both', () => {
		const admission = admitAppNavigation([
			entry(),
			entry({ installation_id: 'imposter', navigation: [declaration({ route: '/Meetings-Console' })] })
		]);
		expect(admission.mounted).toEqual([]);
		// Naming only the loser would imply there was a winner.
		expect(admission.refused.map((item) => [item.installation_id, item.reason])).toEqual([
			['meetings', 'contested_route'],
			['imposter', 'contested_route']
		]);
	});

	it('reports a surface no app URL can be derived for', () => {
		const admission = admitAppNavigation([entry({ installation_id: '../evil' })]);
		expect(admission.mounted).toEqual([]);
		expect(admission.refused).toEqual([
			{ installation_id: '../evil', route: '/meetings-console', reason: 'unmountable_surface' }
		]);
	});

	it('does not report a non-enabled installation as a refusal', () => {
		// Showing nothing is what an installation awaiting review is *supposed* to
		// do. Reporting it would make the signal that matters unfindable.
		for (const status of ['ready_for_review', 'disabled', 'quarantined'] as const) {
			expect(admitAppNavigation([entry({ status })])).toEqual({ mounted: [], refused: [] });
		}
	});

	it('says what the shipped seed manifests actually mount', () => {
		// Two seed packages declare navigation. Nothing asserted which of them
		// reaches the shell, so `town_square` passing host validation and adding no
		// link anywhere was invisible from both ends.
		const townSquare = seedNavigationRoutes('town_square');
		expect(townSquare,
			'the town square seed navigation route changed; re-check what it mounts')
			.toEqual(['/town-square']);
		expect(seedNavigationRoutes('meetings'),
			'the meetings seed navigation route changed; re-check what it mounts')
			.toEqual(['/meetings-console']);

		// Only the route is taken from the shipped bytes — it is the value
		// admission turns on. The identity around it is a fixture.
		const admission = admitAppNavigation([
			entry({
				installation_id: 'town_square',
				navigation: [declaration({
					id: 'town_square',
					title: 'Town Square',
					route: townSquare[0],
					placement: { kind: 'route' },
					surface: { kind: 'custom_surface', entry_point: '/square', fallback_view: 'feed' }
				})]
			}),
			entry()
		]);
		expect(admission.mounted.map((item) => item.href)).toEqual(['/apps/meetings/console']);
		// `/town-square` is a first-party destination this shell already serves, so
		// the guard refuses it — and the refusal is now on the record instead of
		// being a `continue`. Whoever changes the seed to a mountable root (or
		// removes the declaration, since the shell hard-codes that tab already)
		// will fail this test, which is the signal that was missing.
		expect(admission.refused).toEqual([
			{ installation_id: 'town_square', route: townSquare[0], reason: 'reserved_route_root' }
		]);
	});

	it('separates tab placement from section placement', () => {
		const mounted = mountAppNavigation([
			entry(),
			entry({
				installation_id: 'claims',
				views: [{ view_id: 'queue', label: 'Queue', route: '/apps/claims/queue', pinned: false }],
				navigation: [declaration({
					id: 'claims_console',
					title: 'Claims',
					route: '/claims-console',
					placement: { kind: 'tab', tab: 'claims' },
					surface: { kind: 'view', view: 'queue' }
				})]
			})
		]);
		expect(mounted.map((item) => item.entry.route)).toEqual(['/claims-console', '/meetings-console']);
		expect(appNavigationTabs(mounted).map((item) => item.href)).toEqual(['/apps/claims/queue']);
		expect(appNavigationSection(mounted, 'observe').map((item) => item.href))
			.toEqual(['/apps/meetings/console']);
		expect(appNavigationSection(mounted, 'today')).toEqual([]);
	});

	it('carries the additive directory field and refuses an off-shape one', () => {
		const wireEntry = (): Record<string, unknown> => ({
			installation_id: 'meetings', name: 'Meetings', description: 'Meeting console',
			icon: { kind: 'monogram', value: 'M' }, package_version: '1.0.0',
			package_revision_ref: 'package:meetings:1', installation_generation: 3,
			status: 'enabled', views: [], actions: [],
			storage: { record_count: 0, revision_count: 0, payload_bytes: 0, attachment_bytes: 0 },
			record_count: 0, payload_bytes: 0
		});
		const wire = (navigation: unknown) => ({
			entries: [{ ...wireEntry(), navigation }],
			has_more: false
		});
		expect(parseAppDirectoryPage(wire([declaration()])).entries[0].navigation)
			.toEqual([declaration()]);
		// An older server omits the field entirely and still decodes.
		expect(parseAppDirectoryPage({ entries: [wireEntry()], has_more: false }).entries[0].navigation)
			.toEqual([]);
		expect(() => parseAppDirectoryPage(wire([{ id: 'x' }]))).toThrow();
	});
});
