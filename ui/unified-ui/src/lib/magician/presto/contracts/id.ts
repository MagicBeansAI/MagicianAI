import type { PrestoRoutePath } from './routeContracts';

const SAFE_SEGMENT_REGEX = /^[a-z0-9._:-]+$/;

export function sanitizeIdSegment(value: unknown): string {
	const raw = typeof value === 'string' ? value : String(value ?? '');
	const normalized = raw
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9._:-]/g, '-')
		.replace(/-+/g, '-')
		.replace(/^-+|-+$/g, '');
	return normalized;
}

export function routePrefixFromPath(route: PrestoRoutePath): string {
	const normalized = route
		.replace(/^\//, '')
		.replace(/\[(.+?)\]/g, '$1')
		.replace(/\//g, '.')
		.replace(/\.+/g, '.')
		.replace(/^\.|\.$/g, '');
	return normalized.length > 0 ? `presto.${normalized}` : 'presto.root';
}

export function buildPrestoComponentId(
	route: PrestoRoutePath,
	section: string,
	...parts: Array<string | number | null | undefined>
): string {
	const prefix = routePrefixFromPath(route);
	const tokens = [sanitizeIdSegment(section), ...parts.map((part) => sanitizeIdSegment(part))].filter(
		(token) => token.length > 0
	);
	return tokens.length > 0 ? `${prefix}.${tokens.join('.')}` : prefix;
}

export function isDeterministicComponentId(componentId: string): boolean {
	const normalized = componentId.trim();
	if (!normalized) return false;
	return SAFE_SEGMENT_REGEX.test(normalized);
}
