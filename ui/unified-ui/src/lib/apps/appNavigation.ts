/**
 * First-party navigation declared by an installed app package (gate S3).
 *
 * The manifest carries `app.navigation` and the host validates it into package
 * authority, but until this module existed nothing on the web read it, so a
 * system package that declared a console added no navigation anywhere.
 *
 * Two deliberate narrowings, both because a manifest is review material and
 * never authority on its own:
 *
 *  1. **The declared route is identity, not a served path.** SvelteKit's route
 *     table is static, so serving `/meetings-console` literally would need a
 *     root rest route that swallows every unmatched URL — a shell-wide change
 *     to 404 handling bought for a declaration the server does not project
 *     yet. Instead the declared route is validated (so a manifest cannot smuggle
 *     `javascript:` or an off-site href into the shell) and used as the stable
 *     identity of the mounted entry, while the href is the canonical app-surface
 *     route this shell already serves: `/apps/<installation>/<surface>`.
 *  2. **Only an enabled installation contributes.** A package awaiting review,
 *     disabled or quarantined must not put a link in first-party chrome; that
 *     is the difference between "installed" and "granted".
 *
 * Nothing here trusts a title or a route to be safe by construction. Every
 * value is re-validated against the manifest grammar, and anything that fails
 * is dropped rather than repaired.
 */

/** Manifest ceilings mirrored from `apps::manifest` — never widened here. */
export const APP_NAVIGATION_MAX_ENTRIES = 16;
export const APP_NAVIGATION_MAX_TITLE_BYTES = 256;
const APP_NAVIGATION_MAX_ROUTE_BYTES = 512;
const APP_NAVIGATION_MAX_ROUTE_SEGMENTS = 16;

export type AppNavigationPlacement =
	| { kind: 'route' }
	| { kind: 'tab'; tab: string }
	| { kind: 'section'; section: string };

export type AppNavigationSurface =
	| { kind: 'view'; view: string }
	| { kind: 'custom_surface'; entry_point: string; fallback_view: string };

export interface AppNavigationEntry {
	id: string;
	title: string;
	route: string;
	placement: AppNavigationPlacement;
	surface: AppNavigationSurface;
}

/**
 * The minimum an installation must show to contribute navigation. Stated
 * structurally rather than as the directory entry type so this module stays the
 * *upstream* of the directory parser — the parser calls in here, and nothing
 * here needs to know what else a directory row carries.
 */
export interface AppNavigationSource {
	installation_id: string;
	installation_generation: number;
	package_revision_ref: string;
	status: string;
	navigation?: AppNavigationEntry[];
	views?: readonly { view_id: string; route: string }[];
}

/** One declaration admitted for this shell, bound to the installation it came from. */
export interface AppMountedNavigation {
	installation_id: string;
	installation_generation: number;
	package_revision_ref: string;
	entry: AppNavigationEntry;
	/** The canonical app-surface URL this shell serves for the declaration. */
	href: string;
}

/**
 * Route roots the shell owns. A declaration whose first segment collides with
 * one of these is dropped: even though the declared route is not served, an
 * entry titled after a first-party destination and rooted at its path reads to
 * a user as that destination. `api`, `_app` and `.well-known` mirror the
 * host-global names the manifest parser already refuses.
 */
export const RESERVED_FIRST_PARTY_ROUTE_ROOTS: ReadonlySet<string> = new Set([
	'.well-known', '_app', 'about', 'api', 'api-mining', 'approvals', 'apps', 'attention',
	'briefing', 'budget', 'channels', 'chat', 'claims-review', 'contextual-assist', 'crew', 'debug', 'desk',
	'dev', 'devsessions', 'draw-overlay', 'evals', 'events', 'evidence', 'feed', 'harness',
	'history', 'home', 'hud', 'internal-tasks', 'llm', 'login', 'manifesto', 'meetings',
	'memory', 'mirror', 'notes', 'notify-overlay', 'observe', 'privacy', 'resurfacing',
	'reviews', 'runtime', 'screen-region-picker', 'settings', 'skills', 'square', 'storage',
	't', 'tasks', 'terms', 'thinking-maps', 'today', 'town-square', 'triggers', 'vault',
	'vibe', 'warroom'
]);

type JsonRecord = Record<string, unknown>;

function record(value: unknown): JsonRecord | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as JsonRecord)
		: null;
}

function exactKeys(value: JsonRecord, keys: readonly string[]): boolean {
	const accepted = new Set(keys);
	return keys.every((key) => key in value) && Object.keys(value).every((key) => accepted.has(key));
}

function utf8Length(value: string): number {
	return new TextEncoder().encode(value).byteLength;
}

/** The manifest `AppName` grammar. */
function appName(value: unknown): value is string {
	return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
}

/**
 * Manifest title rule: bounded, non-control, not blank after trimming. The
 * host checks the same three things; a title that only looks empty would
 * render as an unlabelled, unexplainable link in the shell.
 */
function navigationTitle(value: unknown): value is string {
	return typeof value === 'string' &&
		value.trim().length > 0 &&
		utf8Length(value) <= APP_NAVIGATION_MAX_TITLE_BYTES &&
		// eslint-disable-next-line no-control-regex
		!/[\u0000-\u001f\u007f]/.test(value);
}

function routeSegment(value: string): boolean {
	return value.length > 0 && value.length <= 128 && /^[A-Za-z0-9_.-]+$/.test(value) &&
		value !== '.' && value !== '..';
}

/** ASCII-only collision key, matching the host's NFKC + ascii-lowercase rule. */
export function appRouteCollisionKey(value: string): string {
	return value.normalize('NFKC').toLowerCase();
}

/**
 * The manifest static-route grammar, narrowed: navigation routes may not carry
 * parameters (the host refuses a dynamic first-party route), and `/` itself is
 * not a mountable destination.
 */
export function parseAppNavigationRoute(value: unknown): string | null {
	if (typeof value !== 'string' || value.length === 0 || value.length > APP_NAVIGATION_MAX_ROUTE_BYTES) return null;
	const normalized = value.normalize('NFKC');
	if (
		normalized !== value ||
		/[^\u0020-\u007e]/.test(normalized) ||
		!normalized.startsWith('/') ||
		normalized === '/' ||
		/[\\?#%:]/.test(normalized)
	) return null;
	const segments = normalized.slice(1).split('/');
	if (segments.length === 0 || segments.length > APP_NAVIGATION_MAX_ROUTE_SEGMENTS) return null;
	if (!segments.every(routeSegment)) return null;
	return normalized;
}

function parsePlacement(value: unknown): AppNavigationPlacement | null {
	const placement = record(value);
	if (!placement) return null;
	if (placement.kind === 'route') return exactKeys(placement, ['kind']) ? { kind: 'route' } : null;
	if (placement.kind === 'tab') {
		return exactKeys(placement, ['kind', 'tab']) && appName(placement.tab)
			? { kind: 'tab', tab: placement.tab }
			: null;
	}
	if (placement.kind === 'section') {
		return exactKeys(placement, ['kind', 'section']) && appName(placement.section)
			? { kind: 'section', section: placement.section }
			: null;
	}
	return null;
}

function parseSurface(value: unknown): AppNavigationSurface | null {
	const surface = record(value);
	if (!surface) return null;
	if (surface.kind === 'view') {
		return exactKeys(surface, ['kind', 'view']) && appName(surface.view)
			? { kind: 'view', view: surface.view }
			: null;
	}
	if (surface.kind === 'custom_surface') {
		if (!exactKeys(surface, ['kind', 'entry_point', 'fallback_view'])) return null;
		const entryPoint = parseAppNavigationRoute(surface.entry_point);
		return entryPoint !== null && appName(surface.fallback_view)
			? { kind: 'custom_surface', entry_point: entryPoint, fallback_view: surface.fallback_view }
			: null;
	}
	return null;
}

/**
 * Strict wire parse of one installation's `app.navigation` projection.
 *
 * Returns `null` — never a partial list — when anything is off shape, because
 * a directory response that half-decodes is a contract mismatch, not a
 * degraded app. `undefined` (an older server that omits the additive field)
 * decodes to an empty list, which mounts nothing.
 */
export function parseAppNavigationEntries(value: unknown): AppNavigationEntry[] | null {
	if (value === undefined) return [];
	if (!Array.isArray(value) || value.length > APP_NAVIGATION_MAX_ENTRIES) return null;
	const entries: AppNavigationEntry[] = [];
	for (const item of value) {
		const declaration = record(item);
		if (!declaration || !exactKeys(declaration, ['id', 'title', 'route', 'placement', 'surface'])) return null;
		const route = parseAppNavigationRoute(declaration.route);
		const placement = parsePlacement(declaration.placement);
		const surface = parseSurface(declaration.surface);
		if (!appName(declaration.id) || !navigationTitle(declaration.title) ||
			route === null || placement === null || surface === null) return null;
		entries.push({ id: declaration.id, title: declaration.title, route, placement, surface });
	}
	const ids = new Set(entries.map((entry) => entry.id));
	const routes = new Set(entries.map((entry) => appRouteCollisionKey(entry.route)));
	if (ids.size !== entries.length || routes.size !== entries.length) return null;
	return entries;
}

function appNavigationViewRoute(installationId: string, viewId: string, views: AppNavigationSource['views']): string | null {
	if (!appName(viewId)) return null;
	const matches = views?.filter((view) => view.view_id === viewId) ?? [];
	if (matches.length !== 1) return null;
	// Directory routes are already shell URLs, not manifest-local paths.
	const prefix = `/apps/${installationId}`;
	const href = matches[0].route;
	if (href === prefix || href === `${prefix}/`) return '/';
	return href.startsWith(`${prefix}/`) ? parseAppNavigationRoute(href.slice(prefix.length)) : null;
}

/** Resolve a view's canonical directory URL or a scripted entry-point route. */
export function appNavigationHref(
	installationId: string,
	surface: AppNavigationSurface,
	views: AppNavigationSource['views'] = []
): string | null {
	if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(installationId)) return null;
	const route = surface.kind === 'view'
		? appNavigationViewRoute(installationId, surface.view, views)
		: parseAppNavigationRoute(surface.entry_point);
	if (route === null) return null;
	return `/apps/${installationId}${route === '/' ? '' : route}`;
}

/** A refused scripted page may use only its own declared, enabled-app fallback. */
export function appNavigationFallbackRoute(source: AppNavigationSource, entryRoute: string): string | null {
	if (source.status !== 'enabled') return null;
	const routes = new Set<string>();
	for (const { surface } of source.navigation ?? []) {
		if (surface.kind !== 'custom_surface' || surface.entry_point !== entryRoute) continue;
		const route = appNavigationViewRoute(source.installation_id, surface.fallback_view, source.views);
		if (route === null || route === entryRoute) return null;
		routes.add(route);
	}
	return routes.size === 1 ? [...routes][0] : null;
}

/**
 * Why a declaration the wire parser already accepted never reached the shell.
 *
 * A non-enabled installation is deliberately absent from this list: showing no
 * navigation is that installation's *correct* state, not something to report.
 * These three are the cases where an enabled package shipped a well-formed
 * declaration and this shell still refused it — and nobody else is positioned
 * to notice. The host's manifest parser reserves only the host-global route
 * roots (`api`, `_app`, `.well-known`); it does not, and should not, carry this
 * shell's route table, since a first-party page added tomorrow would otherwise
 * retroactively invalidate a package installed today. So a route this shell
 * refuses is admitted upstream and dies here, which is exactly the shape of
 * drop that has to be said out loud rather than swallowed by a `continue`.
 */
export type AppNavigationRefusalReason =
	| 'reserved_route_root'
	| 'unmountable_surface'
	| 'contested_route';

export interface AppNavigationRefusal {
	installation_id: string;
	route: string;
	reason: AppNavigationRefusalReason;
}

export interface AppNavigationAdmission {
	mounted: AppMountedNavigation[];
	refused: AppNavigationRefusal[];
}

/**
 * Mount every admitted declaration carried by a directory page, and account for
 * the ones that were not.
 *
 * Fail closed at three points: a non-enabled installation contributes nothing,
 * a route rooted at a first-party destination is dropped, and a route claimed
 * by more than one installation is pinned to nobody — the same rule the host
 * applies to a contested slot default, and for the same reason: silently
 * picking a winner means store order decides what the shell says.
 *
 * Pure, so a caller can decide what a refusal is worth; `mountAppNavigation` is
 * the impure edge that reports them.
 */
export function admitAppNavigation(entries: readonly AppNavigationSource[]): AppNavigationAdmission {
	const claimed = new Map<string, number>();
	const candidates: AppMountedNavigation[] = [];
	const refused: AppNavigationRefusal[] = [];
	for (const entry of entries) {
		if (entry.status !== 'enabled') continue;
		for (const declaration of entry.navigation ?? []) {
			const root = appRouteCollisionKey(declaration.route.slice(1).split('/')[0] ?? '');
			if (RESERVED_FIRST_PARTY_ROUTE_ROOTS.has(root)) {
				refused.push({
					installation_id: entry.installation_id,
					route: declaration.route,
					reason: 'reserved_route_root'
				});
				continue;
			}
			const href = appNavigationHref(entry.installation_id, declaration.surface, entry.views);
			if (href === null) {
				refused.push({
					installation_id: entry.installation_id,
					route: declaration.route,
					reason: 'unmountable_surface'
				});
				continue;
			}
			const key = appRouteCollisionKey(declaration.route);
			claimed.set(key, (claimed.get(key) ?? 0) + 1);
			candidates.push({
				installation_id: entry.installation_id,
				installation_generation: entry.installation_generation,
				package_revision_ref: entry.package_revision_ref,
				entry: declaration,
				href
			});
		}
	}
	const mounted: AppMountedNavigation[] = [];
	for (const candidate of candidates) {
		if (claimed.get(appRouteCollisionKey(candidate.entry.route)) === 1) {
			mounted.push(candidate);
			continue;
		}
		// Every claimant is refused, not just the losers: there are no winners.
		refused.push({
			installation_id: candidate.installation_id,
			route: candidate.entry.route,
			reason: 'contested_route'
		});
	}
	mounted.sort((left, right) =>
		left.entry.route < right.entry.route ? -1 : left.entry.route > right.entry.route ? 1 : 0);
	return { mounted, refused };
}

// Bounded by (directory page size × the manifest's 16-entry ceiling × 3 reasons)
// for one page load, so it needs no eviction. The poller re-derives the same
// admission every few minutes and a refusal is a standing fact, not an event:
// warning on each tick would bury it.
const warnedNavigationRefusals = new Set<string>();

/**
 * The mounted list, with each distinct refusal said once.
 *
 * The wrapper exists so the drop is not invisible. Until it did, a shipped
 * system package could declare navigation, pass host validation, ride the
 * directory response intact, and add nothing anywhere — with no signal to the
 * author, the operator, or a test. The guard still refuses; it just no longer
 * refuses quietly.
 */
export function mountAppNavigation(entries: readonly AppNavigationSource[]): AppMountedNavigation[] {
	const admission = admitAppNavigation(entries);
	for (const refusal of admission.refused) {
		const key = `${refusal.installation_id}\u0000${refusal.route}\u0000${refusal.reason}`;
		if (warnedNavigationRefusals.has(key)) continue;
		warnedNavigationRefusals.add(key);
		console.warn(
			`app navigation refused (${refusal.reason}): installation ` +
			`${refusal.installation_id} declared ${refusal.route}`);
	}
	return admission.mounted;
}

/** Declarations placed as a top-level shell tab. */
export function appNavigationTabs(mounted: readonly AppMountedNavigation[]): AppMountedNavigation[] {
	return mounted.filter((item) => item.entry.placement.kind === 'tab');
}

/** Declarations placed inside one named first-party section (e.g. `observe`). */
export function appNavigationSection(
	mounted: readonly AppMountedNavigation[],
	section: string
): AppMountedNavigation[] {
	const wanted = appRouteCollisionKey(section);
	return mounted.filter((item) =>
		item.entry.placement.kind === 'section' &&
		appRouteCollisionKey(item.entry.placement.section) === wanted);
}
