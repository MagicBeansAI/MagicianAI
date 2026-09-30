import { PRESTO_ROUTE_SCHEMA_VERSION } from './schemas';
import type { MuijRenderableComponentType } from '../../components/generative/componentCatalog';

export type PrestoRoutePath = keyof typeof PRESTO_ROUTE_SCHEMA_VERSION;

export type PrestoLifecycleEvent =
	| 'route_mount'
	| 'route_unmount'
	| 'reconnect'
	| 'agent_change'
	| 'cycle_change';

export interface PrestoRouteContract {
	route: PrestoRoutePath;
	schemaVersion: string;
	requiredComponents: readonly MuijRenderableComponentType[];
	requiredLifecycleEvents: readonly PrestoLifecycleEvent[];
	operationalReadinessRequired: boolean;
}

type PrestoRouteContractMap = {
	[K in PrestoRoutePath]: PrestoRouteContract & { route: K };
};

const DEFAULT_LIFECYCLE_EVENTS = ['route_mount', 'route_unmount', 'reconnect'] as const;
const AGENT_LIFECYCLE_EVENTS = [
	'route_mount',
	'route_unmount',
	'reconnect',
	'agent_change',
	'cycle_change'
] as const;

export const PRESTO_ROUTE_CONTRACTS: PrestoRouteContractMap = {
	'/today': {
		route: '/today',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/today'],
		requiredComponents: ['Card', 'Button'],
		requiredLifecycleEvents: DEFAULT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/tasks': {
		route: '/tasks',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/tasks'],
		requiredComponents: ['Card', 'Button'],
		requiredLifecycleEvents: DEFAULT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/briefing': {
		route: '/briefing',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/briefing'],
		requiredComponents: ['Card', 'Form'],
		requiredLifecycleEvents: DEFAULT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/crew/new': {
		route: '/crew/new',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/crew/new'],
		requiredComponents: ['Card', 'Form'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/crew/[id]': {
		route: '/crew/[id]',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/crew/[id]'],
		requiredComponents: ['Card'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/crew/[id]/memory/[tier]': {
		route: '/crew/[id]/memory/[tier]',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/crew/[id]/memory/[tier]'],
		requiredComponents: ['Card', 'DataList'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/crew/[id]/rules': {
		route: '/crew/[id]/rules',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/crew/[id]/rules'],
		requiredComponents: ['Card', 'Table'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/history': {
		route: '/history',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/history'],
		requiredComponents: ['Card', 'Form'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/debug': {
		route: '/debug',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/debug'],
		requiredComponents: ['Card'],
		requiredLifecycleEvents: AGENT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: true
	},
	'/about': {
		route: '/about',
		schemaVersion: PRESTO_ROUTE_SCHEMA_VERSION['/about'],
		requiredComponents: ['Card'],
		requiredLifecycleEvents: DEFAULT_LIFECYCLE_EVENTS,
		operationalReadinessRequired: false
	}
};

function trimTrailingSlash(pathname: string): string {
	if (pathname.length > 1 && pathname.endsWith('/')) {
		return pathname.slice(0, -1);
	}
	return pathname;
}

export function canonicalizePrestoRoute(pathname: string): PrestoRoutePath | null {
	const normalized = trimTrailingSlash(pathname.trim());
	if (normalized in PRESTO_ROUTE_CONTRACTS) {
		return normalized as PrestoRoutePath;
	}

	if (/^\/crew\/[^/]+\/memory\/[^/]+$/.test(normalized)) {
		return '/crew/[id]/memory/[tier]';
	}
	if (/^\/crew\/[^/]+\/rules$/.test(normalized)) {
		return '/crew/[id]/rules';
	}
	if (/^\/crew\/[^/]+$/.test(normalized)) {
		return '/crew/[id]';
	}

	return null;
}

export function getRouteContract(pathname: string): PrestoRouteContract | null {
	const canonical = canonicalizePrestoRoute(pathname);
	if (!canonical) return null;
	return PRESTO_ROUTE_CONTRACTS[canonical];
}

export function isOperationalPrestoRoute(pathname: string): boolean {
	const contract = getRouteContract(pathname);
	return contract ? contract.operationalReadinessRequired : false;
}
