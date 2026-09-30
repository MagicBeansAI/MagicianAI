export const PRESTO_ROUTE_SCHEMA_VERSION = {
	'/today': 'presto.tasks-v1',
	'/tasks': 'presto.tasks-v1',
	'/briefing': 'scroll-surface-v1',
	'/crew/new': 'presto.double-definition-v1',
	'/crew/[id]': 'presto.double-detail-v1',
	'/crew/[id]/memory/[tier]': 'presto.lore-tier-v1',
	'/crew/[id]/rules': 'presto.double-covenant-v1',
	'/history': 'presto.chronicle-v1',
	'/debug': 'presto.veil-v1',
	'/about': 'presto.legend-v1'
} as const;

export type PrestoSchemaRoutePath = keyof typeof PRESTO_ROUTE_SCHEMA_VERSION;

export function schemaForPrestoRoute(route: PrestoSchemaRoutePath): string {
	return PRESTO_ROUTE_SCHEMA_VERSION[route];
}
