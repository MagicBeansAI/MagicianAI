import { derived, get, writable } from 'svelte/store';
import {
	canonicalizePrestoRoute,
	type PrestoLifecycleEvent,
	type PrestoRoutePath
} from '../contracts/routeContracts';

export interface RouteLifecycleEntry {
	route: PrestoRoutePath;
	isMounted: boolean;
	mountedAt: number | null;
	unmountedAt: number | null;
	agentId: string | null;
	cycleId: string | null;
	mountCount: number;
	lastEvent: PrestoLifecycleEvent;
}

const routeLifecycleMap = writable<Map<PrestoRoutePath, RouteLifecycleEntry>>(new Map());
const activeRoute = writable<PrestoRoutePath | null>(null);

function normalizedMaybe(value: string | null | undefined): string | null {
	if (!value) return null;
	const trimmed = value.trim();
	return trimmed.length > 0 ? trimmed : null;
}

function upsertLifecycle(
	route: PrestoRoutePath,
	patch: Partial<RouteLifecycleEntry> & Pick<RouteLifecycleEntry, 'lastEvent'>
): void {
	routeLifecycleMap.update((state) => {
		const current = state.get(route);
		const nextEntry: RouteLifecycleEntry = {
			route,
			isMounted: current?.isMounted ?? false,
			mountedAt: current?.mountedAt ?? null,
			unmountedAt: current?.unmountedAt ?? null,
			agentId: current?.agentId ?? null,
			cycleId: current?.cycleId ?? null,
			mountCount: current?.mountCount ?? 0,
			lastEvent: patch.lastEvent
		};
		Object.assign(nextEntry, patch);
		const next = new Map(state);
		next.set(route, nextEntry);
		return next;
	});
}

export function mountPrestoRoute(pathname: string): void {
	const route = canonicalizePrestoRoute(pathname);
	if (!route) return;
	const now = Date.now();
	const current = get(routeLifecycleMap).get(route);
	upsertLifecycle(route, {
		isMounted: true,
		mountedAt: now,
		unmountedAt: null,
		mountCount: (current?.mountCount ?? 0) + 1,
		lastEvent: 'route_mount'
	});
	activeRoute.set(route);
}

export function unmountPrestoRoute(pathname: string): void {
	const route = canonicalizePrestoRoute(pathname);
	if (!route) return;
	upsertLifecycle(route, {
		isMounted: false,
		unmountedAt: Date.now(),
		lastEvent: 'route_unmount'
	});
	if (get(activeRoute) === route) {
		activeRoute.set(null);
	}
}

export function markPrestoReconnect(pathname: string): void {
	const route = canonicalizePrestoRoute(pathname);
	if (!route) return;
	upsertLifecycle(route, {
		lastEvent: 'reconnect'
	});
}

export function bindPrestoRouteAgentCycle(
	pathname: string,
	agentId?: string | null,
	cycleId?: string | null
): void {
	const route = canonicalizePrestoRoute(pathname);
	if (!route) return;
	const nextAgentId = normalizedMaybe(agentId);
	const nextCycleId = normalizedMaybe(cycleId);
	const current = get(routeLifecycleMap).get(route);
	const nextEvent: PrestoLifecycleEvent =
		(current?.agentId ?? null) !== nextAgentId ? 'agent_change' : 'cycle_change';

	upsertLifecycle(route, {
		agentId: nextAgentId,
		cycleId: nextCycleId,
		lastEvent: nextEvent
	});
}

export function resetPrestoRouteLifecycle(): void {
	routeLifecycleMap.set(new Map());
	activeRoute.set(null);
}

export const activePrestoRoute = derived(activeRoute, ($route) => $route);

export const prestoRouteLifecycleEntries = derived(routeLifecycleMap, ($map) =>
	Array.from($map.values()).sort((left, right) => left.route.localeCompare(right.route))
);
