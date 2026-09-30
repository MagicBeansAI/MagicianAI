import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { MuijComponent } from '$lib/stores/muijStore';
import { PRESTO_ROUTE_SCHEMA_VERSION } from '$lib/magician/presto/contracts/schemas';
import {
	PRESTO_ROUTE_CONTRACTS,
	type PrestoRoutePath
} from '$lib/magician/presto/contracts/routeContracts';
import {
	PRESTO_READINESS_SCENARIOS,
	createDefaultOperationalReadinessStatuses,
	createDefaultRouteReadinessStatus,
	validateOperationalRouteReadiness,
	validatePrestoRouteComponents,
	type PrestoReadinessScenario,
	type RouteReadinessStatus
} from './index';

// Route pages live inside the `(app)` route group so the layout chrome can
// wrap them. The group segment is invisible in URLs but is part of the on-
// disk path — keep these in sync if the grouping ever changes.
const PRESTO_ROUTE_SOURCE_FILES: Record<PrestoRoutePath, string> = {
	'/today': 'src/routes/(app)/today/+page.svelte',
	'/tasks': 'src/routes/(app)/tasks/+page.svelte',
	'/briefing': 'src/routes/(app)/briefing/+page.svelte',
	'/crew/new': 'src/routes/(app)/crew/new/+page.svelte',
	'/crew/[id]': 'src/routes/(app)/crew/[id]/+page.svelte',
	'/crew/[id]/memory/[tier]': 'src/routes/(app)/crew/[id]/memory/[tier]/+page.svelte',
	'/crew/[id]/rules': 'src/routes/(app)/crew/[id]/rules/+page.svelte',
	'/history': 'src/routes/(app)/history/+page.svelte',
	'/debug': 'src/routes/(app)/debug/+page.svelte',
	// `/about` is likewise redirect-only (308 → /#about).
	'/about': 'src/routes/(app)/about/+page.ts'
};

function routeToken(route: PrestoRoutePath): string {
	return route
		.replace(/^\//, '')
		.replace(/\[|\]/g, '')
		.replace(/[^a-zA-Z0-9]+/g, '.')
		.replace(/\.+/g, '.')
		.replace(/^\.|\.$/g, '')
		.toLowerCase() || 'root';
}

function buildComponent(
	route: PrestoRoutePath,
	componentType: string,
	index: number,
	idOverride?: string
): MuijComponent {
	return {
		id: idOverride ?? `presto.acceptance.${routeToken(route)}.${index}.${componentType.toLowerCase()}`,
		component_type: componentType,
		label: `Acceptance ${componentType}`,
		props: {}
	};
}

function buildRequiredComponents(route: PrestoRoutePath): MuijComponent[] {
	return PRESTO_ROUTE_CONTRACTS[route].requiredComponents.map((componentType, index) =>
		buildComponent(route, componentType, index)
	);
}

function scenarioResults(value: boolean): Record<PrestoReadinessScenario, boolean> {
	return {
		load: value,
		reconnect: value,
		errors: value,
		empty: value,
		stale_cycle: value,
		long_running_updates: value
	};
}

function markReady(status: RouteReadinessStatus): RouteReadinessStatus {
	return {
		...status,
		scenarioResults: scenarioResults(true)
	};
}

describe('presto migration acceptance checks', () => {
	it('keeps route contract coverage aligned with route schema map', () => {
		const contractRoutes = Object.keys(PRESTO_ROUTE_CONTRACTS).sort();
		const schemaRoutes = Object.keys(PRESTO_ROUTE_SCHEMA_VERSION).sort();
		expect(contractRoutes).toEqual(schemaRoutes);
	});

	it('validates required component sets for every presto route contract', () => {
		const routes = Object.keys(PRESTO_ROUTE_CONTRACTS) as PrestoRoutePath[];
		for (const route of routes) {
			const result = validatePrestoRouteComponents(route, buildRequiredComponents(route));
			expect(result.checked).toBe(true);
			expect(result.route).toBe(route);
			expect(result.schemaVersion).toBe(PRESTO_ROUTE_SCHEMA_VERSION[route]);
			expect(result.ok).toBe(true);
			expect(result.errors).toEqual([]);
		}
	});

	it('fails readiness validation on missing required component types', () => {
		const route: PrestoRoutePath = '/tasks';
		const result = validatePrestoRouteComponents(route, [
			buildComponent(route, 'Card', 0)
		]);
		expect(result.ok).toBe(false);
		expect(result.errors.some((error) => error.includes('missing required component type(s)'))).toBe(
			true
		);
	});

	it('fails readiness validation on duplicate component IDs', () => {
		const route: PrestoRoutePath = '/tasks';
		const duplicateId = 'presto.acceptance.tasks.duplicate';
		const result = validatePrestoRouteComponents(route, [
			buildComponent(route, 'Card', 0, duplicateId),
			buildComponent(route, 'Button', 1, duplicateId)
		]);
		expect(result.ok).toBe(false);
		expect(result.errors.some((error) => error.includes('duplicate component id(s)'))).toBe(true);
	});

	it('returns no-op validation for non-presto routes', () => {
		const result = validatePrestoRouteComponents('/unknown-route', []);
		expect(result.checked).toBe(false);
		expect(result.ok).toBe(true);
		expect(result.digest).toBe('not-presto-route');
	});

	it('creates default operational readiness statuses for all required routes', () => {
		const statuses = createDefaultOperationalReadinessStatuses();
		const requiredRoutes = Object.values(PRESTO_ROUTE_CONTRACTS)
			.filter((contract) => contract.operationalReadinessRequired)
			.map((contract) => contract.route)
			.sort((left, right) => left.localeCompare(right));
		expect(statuses.map((status) => status.route)).toEqual(requiredRoutes);
		for (const status of statuses) {
			for (const scenario of PRESTO_READINESS_SCENARIOS) {
				expect(status.scenarioResults[scenario]).toBe(false);
			}
		}
	});

	it('passes operational readiness validation when all required routes are complete', () => {
		const readyStatuses = createDefaultOperationalReadinessStatuses().map(markReady);
		const result = validateOperationalRouteReadiness(readyStatuses);
		expect(result.ok).toBe(true);
		expect(result.errors).toEqual([]);
		expect(result.requiredRoutes.length).toBeGreaterThan(0);
	});

	it('fails operational readiness validation for duplicate, missing, or incomplete routes', () => {
		const readyStatuses = createDefaultOperationalReadinessStatuses().map(markReady);
		const duplicateStatus = { ...readyStatuses[0] };
		const incomplete = createDefaultRouteReadinessStatus(readyStatuses[1].route);
		const result = validateOperationalRouteReadiness([
			...readyStatuses.slice(3),
			duplicateStatus,
			duplicateStatus,
			incomplete
		]);
		expect(result.ok).toBe(false);
		expect(result.errors.some((error) => error.includes('duplicate readiness status route(s)'))).toBe(
			true
		);
		expect(result.errors.some((error) => error.includes('missing readiness status for route'))).toBe(
			true
		);
		expect(
			result.errors.some((error) => error.includes('incomplete readiness scenarios'))
		).toBe(true);
	});

	it('keeps route pages aligned with the native/MUIJ boundary', () => {
		const legacyTokens = ['TaskList', 'TaskItem', 'DailyBriefingPreview'];
		const muijBoundaryRoutes = new Set<PrestoRoutePath>(['/briefing', '/crew/[id]', '/debug']);
		for (const [route, relativePath] of Object.entries(PRESTO_ROUTE_SOURCE_FILES) as Array<
			[PrestoRoutePath, string]
		>) {
			const absolutePath = resolve(process.cwd(), relativePath);
			expect(existsSync(absolutePath), `${route} missing source file`).toBe(true);
			const source = readFileSync(absolutePath, 'utf8');
			// Redirect-only routes have no page to render GAUI; existence of
			// the redirect + absence of legacy tokens is the whole contract.
			if (relativePath.endsWith('+page.ts')) {
				expect(source.includes('redirect('), `${route} redirect file lost its redirect`).toBe(true);
				for (const token of legacyTokens) {
					expect(source.includes(token), `${route} still references legacy token ${token}`).toBe(
						false
					);
				}
				continue;
			}
			const hasMuijRenderBoundary =
				source.includes('MuijRenderer') ||
				source.includes('PublishedScrollCanvas') ||
				source.includes('LiveAgentSurface');
			expect(
				hasMuijRenderBoundary,
				`${route} MUIJ render boundary mismatch`
			).toBe(muijBoundaryRoutes.has(route));
			for (const token of legacyTokens) {
				expect(source.includes(token), `${route} still references legacy token ${token}`).toBe(
					false
				);
			}
		}
	});
});
