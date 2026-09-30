import type { MuijComponent } from '$lib/stores/muijStore';
import {
	getRouteContract,
	PRESTO_ROUTE_CONTRACTS,
	type PrestoRoutePath
} from '../contracts/routeContracts';
import { isDeterministicComponentId } from '../contracts/id';
import { SUPPORTED_MUIJ_COMPONENT_TYPE_SET } from '$lib/magician/components/generative/componentCatalog';

export const PRESTO_READINESS_SCENARIOS = [
	'load',
	'reconnect',
	'errors',
	'empty',
	'stale_cycle',
	'long_running_updates'
] as const;

export type PrestoReadinessScenario = (typeof PRESTO_READINESS_SCENARIOS)[number];

export interface RouteComponentValidationResult {
	checked: boolean;
	route: PrestoRoutePath | null;
	schemaVersion: string | null;
	ok: boolean;
	errors: string[];
	warnings: string[];
	digest: string;
}

function walkComponents(
	components: MuijComponent[],
	visit: (component: MuijComponent) => void
): void {
	for (const component of components) {
		visit(component);
		if (Array.isArray(component.children) && component.children.length > 0) {
			walkComponents(component.children, visit);
		}
	}
}

function uniqueSorted(values: Iterable<string>): string[] {
	return Array.from(new Set(values)).sort((left, right) => left.localeCompare(right));
}

export function validatePrestoRouteComponents(
	pathname: string,
	components: MuijComponent[]
): RouteComponentValidationResult {
	const contract = getRouteContract(pathname);
	if (!contract) {
		return {
			checked: false,
			route: null,
			schemaVersion: null,
			ok: true,
			errors: [],
			warnings: [],
			digest: 'not-presto-route'
		};
	}

	const componentTypes = new Set<string>();
	const seenIds = new Set<string>();
	const duplicateIds: string[] = [];
	const invalidIdFormat: string[] = [];

	walkComponents(components, (component) => {
		componentTypes.add(component.component_type);
		const normalizedId = component.id.trim();
		if (seenIds.has(normalizedId)) {
			duplicateIds.push(normalizedId);
		} else {
			seenIds.add(normalizedId);
		}
		if (!isDeterministicComponentId(normalizedId)) {
			invalidIdFormat.push(normalizedId);
		}
	});

	const missingRequiredComponents = contract.requiredComponents.filter(
		(componentType) => !componentTypes.has(componentType)
	);
	const unsupportedRequiredComponents = contract.requiredComponents.filter(
		(componentType) => !SUPPORTED_MUIJ_COMPONENT_TYPE_SET.has(componentType)
	);

	const errors: string[] = [];
	if (unsupportedRequiredComponents.length > 0) {
		errors.push(
			`route contract requires unsupported component type(s): ${unsupportedRequiredComponents.join(', ')}`
		);
	}
	if (missingRequiredComponents.length > 0) {
		errors.push(
			`missing required component type(s): ${missingRequiredComponents.join(', ')}`
		);
	}
	if (duplicateIds.length > 0) {
		errors.push(`duplicate component id(s): ${uniqueSorted(duplicateIds).join(', ')}`);
	}

	const warnings: string[] = [];
	if (invalidIdFormat.length > 0) {
		warnings.push(
			`non-deterministic/invalid component id format: ${uniqueSorted(invalidIdFormat).join(', ')}`
		);
	}

	const digest = [
		contract.route,
		contract.schemaVersion,
		`count:${components.length}`,
		`types:${uniqueSorted(componentTypes).join('|')}`,
		`errors:${errors.join('|')}`,
		`warnings:${warnings.join('|')}`
	].join('::');

	return {
		checked: true,
		route: contract.route,
		schemaVersion: contract.schemaVersion,
		ok: errors.length === 0,
		errors,
		warnings,
		digest
	};
}

/**
 * DEV-only convenience: validate a route's components against its contract and
 * log any violation / warning once per distinct result (deduped by digest).
 * Returns the digest to hold for the next call.
 *
 * Use this at the level that owns the COMPLETE route surface. `MuijRenderer`
 * calls it for the common single-renderer route, but pages that split one
 * route's surface across several renderers + natively-rendered chrome (e.g.
 * `/tasks`, whose compose Card+Button render as native Svelte components) MUST
 * validate the full pre-split surface here rather than per fragment — otherwise
 * each fragment is checked against the whole-route contract and falsely reports
 * required types that live in a sibling fragment or the native chrome.
 */
export function reportPrestoRouteValidation(
	pathname: string,
	components: MuijComponent[],
	previousDigest: string
): string {
	const validation = validatePrestoRouteComponents(pathname, components);
	if (!validation.checked || validation.digest === previousDigest) {
		return previousDigest;
	}
	if (!validation.ok) {
		console.error(`[GAUI Contract] ${validation.route} failed: ${validation.errors.join('; ')}`);
	}
	if (validation.warnings.length > 0) {
		console.warn(
			`[GAUI Contract] ${validation.route} warnings: ${validation.warnings.join('; ')}`
		);
	}
	return validation.digest;
}

export interface RouteReadinessStatus {
	route: PrestoRoutePath;
	scenarioResults: Record<PrestoReadinessScenario, boolean>;
	notes: string[];
}

export function createDefaultRouteReadinessStatus(route: PrestoRoutePath): RouteReadinessStatus {
	return {
		route,
		scenarioResults: {
			load: false,
			reconnect: false,
			errors: false,
			empty: false,
			stale_cycle: false,
			long_running_updates: false
		},
		notes: []
	};
}

export interface OperationalRouteReadinessValidationResult {
	ok: boolean;
	requiredRoutes: PrestoRoutePath[];
	errors: string[];
	warnings: string[];
	digest: string;
}

function operationalPrestoRoutes(): PrestoRoutePath[] {
	return Object.values(PRESTO_ROUTE_CONTRACTS)
		.filter((contract) => contract.operationalReadinessRequired)
		.map((contract) => contract.route)
		.sort((left, right) => left.localeCompare(right));
}

export function createDefaultOperationalReadinessStatuses(): RouteReadinessStatus[] {
	return operationalPrestoRoutes().map((route) => createDefaultRouteReadinessStatus(route));
}

export function validateOperationalRouteReadiness(
	statuses: RouteReadinessStatus[]
): OperationalRouteReadinessValidationResult {
	const requiredRoutes = operationalPrestoRoutes();
	const requiredRouteSet = new Set<PrestoRoutePath>(requiredRoutes);
	const statusByRoute = new Map<PrestoRoutePath, RouteReadinessStatus>();
	const duplicateRoutes: PrestoRoutePath[] = [];
	const extraRoutes: PrestoRoutePath[] = [];

	for (const status of statuses) {
		if (!requiredRouteSet.has(status.route)) {
			extraRoutes.push(status.route);
			continue;
		}
		if (statusByRoute.has(status.route)) {
			duplicateRoutes.push(status.route);
			continue;
		}
		statusByRoute.set(status.route, status);
	}

	const errors: string[] = [];
	if (duplicateRoutes.length > 0) {
		errors.push(`duplicate readiness status route(s): ${uniqueSorted(duplicateRoutes).join(', ')}`);
	}

	for (const route of requiredRoutes) {
		const status = statusByRoute.get(route);
		if (!status) {
			errors.push(`missing readiness status for route: ${route}`);
			continue;
		}
		const missingScenarios = PRESTO_READINESS_SCENARIOS.filter(
			(scenario) => status.scenarioResults[scenario] !== true
		);
		if (missingScenarios.length > 0) {
			errors.push(
				`incomplete readiness scenarios for ${route}: ${missingScenarios.join(', ')}`
			);
		}
	}

	const warnings: string[] = [];
	if (extraRoutes.length > 0) {
		warnings.push(
			`received readiness statuses for non-operational route(s): ${uniqueSorted(extraRoutes).join(', ')}`
		);
	}

	const digest = [
		`required:${requiredRoutes.length}`,
		`provided:${statuses.length}`,
		`errors:${errors.join('|')}`,
		`warnings:${warnings.join('|')}`
	].join('::');

	return {
		ok: errors.length === 0,
		requiredRoutes,
		errors,
		warnings,
		digest
	};
}
