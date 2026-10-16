import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import type { AppDirectoryEntry } from './appDirectory';

const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const MAX_REVIEW_COLLECTION_ITEMS = 256;
const MAX_REVIEW_TEXT_CHARS = 2_048;

export interface AppInstallationReview {
	installation_id: string;
	attempt_id: string;
	attempt_kind: 'initial_install' | 'reinstall' | 'update';
	package_revision_ref: string;
	package_content_digest: string;
	name: string;
	version: string;
	description: string;
	requested_tools: string[];
	requested_agents: string[];
	requested_personalities: string[];
	requested_interactive_capabilities: AppReviewedInteractiveCapabilityGrant[];
	tool_dispatch: AppReviewedToolDispatch[];
	workflows: AppReviewedWorkflowGrant[];
	workflow_material_bindings: AppReviewedWorkflowMaterialBinding[];
	workflow_material_digest: string;
	inert_workflows: AppInertWorkflow[];
	requested_data_handling_policy: AppReviewedDataHandlingPolicy;
	requested_background_execution: AppReviewedBackgroundExecution;
	requested_network_policy: AppReviewedNetworkPolicy;
	requested_resource_ceiling: AppReviewedResourceCeiling;
	permission_diff?: AppReviewedPermissionDiff;
	/**
	 * Scheduled autonomous behaviours the package asks for. These MUST be shown:
	 * omitting `granted_behaviors` at approval grants every requested behaviour
	 * (`select_behavior_grants` returns `requested.to_vec()` for `None`), so an
	 * unrendered entry here is authority the owner granted without seeing it.
	 */
	requested_behaviors: AppReviewedBehaviorGrant[];
	/**
	 * Event/notification behaviours. Explicit-deny by default — omitted and
	 * empty both grant none — so these are disclosure, not consent.
	 */
	requested_event_behaviors: AppReviewedBehaviorGrant[];
	requested_custom_surface?: AppReviewedCustomSurfaceRequest;
	/**
	 * Owner memory the app asks to read (`app_memory_read_v1`). Approving
	 * without a choice grants `default_grant`: requested non-sensitive tiers
	 * and agents while you use the app, nothing in the background.
	 */
	requested_memory_read?: AppReviewedMemoryRead;
	/**
	 * Secrets the app's tools ask to use (`app_secret_use_v1`), each with the
	 * one host it can reach. Nothing is granted unless the owner ticks it.
	 */
	requested_secret_uses?: AppSecretUseRequest[];
	/**
	 * How each network-capable or in-place tool runs (`app_in_place_skill_v1`):
	 * the skill it runs in place from and the hosts it declares.
	 */
	tool_runtime?: AppReviewedToolRuntime[];
	/** The owner may grant "any public host"; it stays off unless ticked. */
	offers_any_public_host?: boolean;
}

export interface AppReviewedToolRuntime {
	tool: string;
	/** The skill this tool runs in place from; absent for a copied tool. */
	in_place_from?: string;
	/** Hosts the tool declares. */
	declared_hosts?: string[];
	/** The tool declares no host: it reaches the hosts this app is granted. */
	reaches_granted_hosts?: boolean;
	/** The hosts this tool can actually reach if approved as requested. */
	reachable_hosts?: string[];
}

export interface AppSecretUseRequest {
	tool: string;
	secret_ref: string;
	required: boolean;
	/** The one host the key can reach; empty when there is not exactly one. */
	destination: string;
	/** Every declared host, when the tool declares several. */
	destinations?: string[];
	/** The tool declares no host: the key reaches the hosts this app is granted. */
	app_granted_hosts?: boolean;
	/** `config_file` when the key reaches the tool as a private config file. */
	delivery?: string;
	/** Why this key cannot be granted in this app, when it cannot. */
	not_grantable?: string;
}

export interface AppSecretUseGrant {
	tool: string;
	secret_ref: string;
	/** For a tool that declares no host: the hosts the key is limited to. */
	hosts?: string[];
	/** The owner's explicit "allow this key to be sent to any site". */
	any_site?: boolean;
}

export interface AppMemoryReadSelection {
	user_tiers: string[];
	agents: string[];
}

export interface AppMemoryReadGrant {
	schema: string;
	request_digest: string;
	engagement: 'owner_only';
	interactive: AppMemoryReadSelection;
	background: AppMemoryReadSelection;
}

export interface AppReviewedMemoryRead {
	request: { user_tiers: string[]; agents: string[]; purpose: string };
	request_digest: string;
	/** Requested tiers that are only granted by an explicit tick. */
	sensitive_tiers: string[];
	default_grant: AppMemoryReadGrant;
}

export interface AppReviewedBehaviorGrant {
	behavior_id: string;
	purpose: string;
	action: string;
	operations: string[];
}

export interface AppReviewedCustomSurfaceRequest {
	entry_points: Array<{ route: string; document: string; document_digest: string }>;
	executable_members: Array<{ path: string; content_digest: string; byte_len: number }>;
	scan_findings: Array<{ path: string; pattern: string }>;
	request_digest: string;
	entry_point_count: number;
	executable_member_count: number;
	executable_bytes: number;
	scan_finding_count: number;
	sandbox: string;
	csp: string;
}

export interface AppReviewedDataHandlingPolicy {
	classification_floor: 'public' | 'ordinary' | 'personal' | 'sensitive' | 'secret';
	model_processing: 'none' | 'local_only' | 'remote_allowed';
	personal_agent_access: 'denied' | 'approved_projection';
	memory_promotion: 'denied' | 'candidate_allowed';
	external_egress: 'denied' | 'approved_destinations' | 'any_public_host';
	approved_destinations: string[];
}

export type AppReviewedBackgroundExecution =
	| { mode: 'denied' }
	| { mode: 'granted'; min_interval_seconds: number; max_concurrent_runs: number };

export type AppReviewedNetworkPolicy =
	| { mode: 'denied' }
	| { mode: 'approved_destinations'; destinations: string[] };

export interface AppReviewedResourceCeiling {
	max_input_tokens: number;
	max_output_tokens: number;
	max_cost_microusd: number;
	max_paid_tool_invocations: number;
	max_active_seconds: number;
	max_lifetime_seconds: number;
	max_browser_network_actions: number;
	max_concurrent_foreground_runs: number;
	max_concurrent_background_runs: number;
	max_records: number;
	max_payload_bytes: number;
	max_attachment_bytes: number;
	max_monthly_tokens: number;
	max_monthly_cost_microusd: number;
}

export interface AppReviewedPermissionDiff {
	tools: 'unchanged' | 'narrowed' | 'expanded';
	agents: 'unchanged' | 'narrowed' | 'expanded';
	personalities: 'unchanged' | 'narrowed' | 'expanded';
	personal_agent_data: 'unchanged' | 'narrowed' | 'expanded';
	data_handling: 'unchanged' | 'narrowed' | 'expanded';
	resources: 'unchanged' | 'narrowed' | 'expanded';
	network_policy: 'unchanged' | 'narrowed' | 'expanded';
	background_execution: 'unchanged' | 'narrowed' | 'expanded';
	context_reads: 'unchanged' | 'narrowed' | 'expanded';
	interactive_capabilities: 'unchanged' | 'narrowed' | 'expanded';
	custom_surface_entry_points: 'unchanged' | 'narrowed' | 'expanded';
	background_behaviors: 'unchanged' | 'narrowed' | 'expanded';
	event_behaviors: 'unchanged' | 'narrowed' | 'expanded';
	owner_notifications: 'unchanged' | 'narrowed' | 'expanded';
	memory_read: 'unchanged' | 'narrowed' | 'expanded';
	secret_uses: 'unchanged' | 'narrowed' | 'expanded';
	any_public_host: 'unchanged' | 'narrowed' | 'expanded';
	requires_review: boolean;
	diff_digest: string;
}

export interface AppInteractiveCapabilityRequest {
	schema: 'magician.app-interactive-capability-request.v1';
	owner: 'browser' | 'macos' | 'android';
	allowed_origins: string[];
	target_profile_class: 'installation_ephemeral_headless' | 'owner_reviewed_macos_pairing' | 'owner_reviewed_android_pairing';
	target_selectors: {
		bundle_ids: string[];
		package_ids: string[];
		application_refs: string[];
		current_reviewed_pairing: boolean;
	};
	action_classes: string[];
	background: 'direct_owner' | 'reviewed_bounded_background';
	capture: 'structured_evidence_only' | 'reviewed_pixels';
	transfer: 'denied' | 'reviewed_artifacts';
	resources: {
		max_sessions: number;
		max_steps: number;
		max_duration_seconds: number;
		max_evidence_bytes: number;
		max_evidence_nodes: number;
		max_pixels: number;
		max_artifact_bytes: number;
		max_output_bytes: number;
	};
	expiry_session: {
		grant_lifetime_seconds: number;
		max_session_seconds: number;
		session: 'invocation_bound' | 'run_bound';
	};
}

export interface AppReviewedInteractiveCapabilityGrant {
	schema: 'magician.app-reviewed-interactive-capability-grant.v1';
	dependency_ref: string;
	locked_binding_digest: string;
	primitive_binding_digest: string;
	requested_request_digest: string;
	requested: AppInteractiveCapabilityRequest;
	granted: AppInteractiveCapabilityRequest;
	action: {
		action_ref: string;
		class: string;
		action_digest: string;
		input_schema_digest: string;
		result_schema_digest: string;
		effects: string[];
		implementation_plan_digest: string;
		result_byte_ceiling: number;
	};
	grant_digest: string;
}

export interface AppReviewMatrixRow {
	category: string;
	request: string;
	posture: 'denied' | 'conditional' | 'allowed' | 'bounded';
}

export interface AppInstallationApproveReceipt {
	installation_id: string;
	generation: number;
	status: 'enabled';
	grant_revision: number;
	schema_revision: number;
	surface_revision: number;
	approval_id: string;
	attempt_id: string;
	outcome: 'enabled' | 'already_enabled';
	inert_workflows: AppInertWorkflow[];
	granted_custom_surface_entry_points?: string[];
}

export interface AppReviewedToolDispatch {
	tool: string;
	dispatchable: boolean;
	attested_operations: string[];
	reason: string;
}

export interface AppReviewedWorkflowGrant {
	workflow_id: string;
	uses: string[];
	agent: string;
	personality?: string | null;
}

export interface AppReviewedWorkflowMaterialBinding {
	/** Present on the wire, where `exactKeysWithOptional` requires it, and
	 *  dropped by the parser — which builds its result without it. Optional so
	 *  the one type can describe both the received payload and the parsed
	 *  value, rather than a fixture having to lie about either. */
	schema?: 'magician.app-reviewed-workflow-material.v1';
	workflow_id: string;
	agent_ref: string;
	agent_definition_revision: number;
	agent_definition_digest: string;
	agent_descriptor_digest: string;
	personality_ref?: string | null;
	personality_content_digest?: string | null;
	personality_descriptor_digest?: string | null;
	binding_digest: string;
}

export interface AppInertWorkflow {
	workflow_id: string;
	reasons: string[];
}

export interface AppInstallationApproveRequest {
	review_material_digest: string;
	granted_tools?: string[];
	granted_agents?: string[];
	granted_personalities?: string[];
	granted_interactive_capabilities?: Array<Record<string, unknown>>;
	granted_custom_surface_entry_points?: Array<{
		route: string;
		document: string;
		reviewed_request_digest: string;
	}>;
	migration_run_id?: string;
	update_plan_digest?: string;
	destructive_migration_confirmed?: boolean;
	/** Omit to take the reviewed default memory grant. */
	granted_memory_read?: {
		reviewed_request_digest: string;
		interactive: AppMemoryReadSelection;
		background: AppMemoryReadSelection;
	};
	/** Owner-ticked secret uses; omitted or empty grants none. */
	granted_secret_uses?: AppSecretUseGrant[];
	/** The explicit "any public host" grant; off unless ticked. */
	granted_any_public_host?: boolean;
}

function responseError(body: unknown, fallback: string): Error {
	if (body && typeof body === 'object' && 'message' in body && typeof body.message === 'string') {
		return new Error(body.message);
	}
	return new Error(fallback);
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown>
		: null;
}

function exactKeys(value: Record<string, unknown>, required: readonly string[]): boolean {
	const expected = [...required].sort();
	const actual = Object.keys(value).sort();
	return expected.length === actual.length && expected.every((key, index) => key === actual[index]);
}

function exactKeysWithOptional(
	value: Record<string, unknown>,
	required: readonly string[],
	optional: readonly string[]
): boolean {
	const keys = Object.keys(value);
	const allowed = new Set([...required, ...optional]);
	return required.every((key) => Object.hasOwn(value, key)) && keys.every((key) => allowed.has(key));
}

function safeCount(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function boundedString(value: unknown, maximum: number): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

function exactReviewedOrigin(value: string): boolean {
	if (value === 'about:blank') return true;
	try {
		const origin = new URL(value);
		return origin.protocol === 'https:' && origin.username === '' && origin.password === '' &&
			origin.pathname === '/' && origin.search === '' && origin.hash === '' && origin.origin === value;
	} catch { return false; }
}

function stringList(value: unknown, maximum = MAX_REVIEW_COLLECTION_ITEMS): string[] | null {
	if (!Array.isArray(value) || value.length > maximum ||
		value.some((entry) => typeof entry !== 'string' || entry.length === 0 || entry.length > 192)) return null;
	const strings = value as string[];
	return new Set(strings).size === strings.length ? strings : null;
}

function parseDataHandling(value: unknown): AppReviewedDataHandlingPolicy | null {
	const item = record(value);
	const destinations = item
		? item.approved_destinations === undefined ? [] : stringList(item.approved_destinations)
		: null;
	if (!item || !exactKeysWithOptional(item, [
		'classification_floor', 'model_processing', 'personal_agent_access',
		'memory_promotion', 'external_egress'
	], ['approved_destinations']) || !['public', 'ordinary', 'personal', 'sensitive', 'secret'].includes(String(item.classification_floor)) ||
		!['none', 'local_only', 'remote_allowed'].includes(String(item.model_processing)) ||
		!['denied', 'approved_projection'].includes(String(item.personal_agent_access)) ||
		!['denied', 'candidate_allowed'].includes(String(item.memory_promotion)) ||
		!['denied', 'approved_destinations', 'any_public_host'].includes(String(item.external_egress)) || !destinations ||
		(item.external_egress !== 'any_public_host' &&
			(item.external_egress === 'denied') !== (destinations.length === 0))) return null;
	return { ...item, approved_destinations: destinations } as AppReviewedDataHandlingPolicy;
}

function parseBackground(value: unknown): AppReviewedBackgroundExecution | null {
	const item = record(value);
	if (!item) return null;
	if (item.mode === 'denied') return exactKeys(item, ['mode']) ? { mode: 'denied' } : null;
	if (item.mode !== 'granted' || !exactKeys(item, ['mode', 'min_interval_seconds', 'max_concurrent_runs']) ||
		!safeCount(item.min_interval_seconds) || item.min_interval_seconds === 0 ||
		!safeCount(item.max_concurrent_runs) || item.max_concurrent_runs === 0) return null;
	return item as unknown as AppReviewedBackgroundExecution;
}

function parseNetwork(value: unknown): AppReviewedNetworkPolicy | null {
	const item = record(value);
	if (item && item.mode === 'denied' && exactKeys(item, ['mode'])) return { mode: 'denied' };
	const destinations = item ? stringList(item.destinations) : null;
	if (!item || item.mode !== 'approved_destinations' || !exactKeys(item, ['mode', 'destinations']) ||
		!destinations || destinations.length === 0) return null;
	return { mode: 'approved_destinations', destinations };
}

const RESOURCE_FIELDS = [
	'max_input_tokens', 'max_output_tokens', 'max_cost_microusd', 'max_paid_tool_invocations',
	'max_active_seconds', 'max_lifetime_seconds', 'max_browser_network_actions',
	'max_concurrent_foreground_runs', 'max_concurrent_background_runs', 'max_records',
	'max_payload_bytes', 'max_attachment_bytes', 'max_monthly_tokens', 'max_monthly_cost_microusd'
] as const;

function parseResources(value: unknown): AppReviewedResourceCeiling | null {
	const item = record(value);
	if (!item || !exactKeys(item, RESOURCE_FIELDS) || RESOURCE_FIELDS.some((field) => !safeCount(item[field]))) return null;
	return item as unknown as AppReviewedResourceCeiling;
}

function parsePermissionDiff(value: unknown): AppReviewedPermissionDiff | undefined | null {
	if (value === undefined) return undefined;
	const item = record(value);
	const fields = ['tools', 'agents', 'personalities', 'personal_agent_data', 'data_handling',
		'resources', 'network_policy', 'background_execution', 'context_reads',
		'interactive_capabilities'] as const;
	// Match the backend's serde defaults. Event and notification axes are
	// omitted when unchanged; older reviews also predate the other two axes.
	const optionalFields = ['custom_surface_entry_points', 'background_behaviors',
		'event_behaviors', 'owner_notifications', 'memory_read', 'secret_uses',
		'any_public_host'] as const;
	if (!item || !exactKeysWithOptional(item, [...fields, 'requires_review', 'diff_digest'], optionalFields) ||
		fields.some((field) => !['unchanged', 'narrowed', 'expanded'].includes(String(item[field]))) ||
		optionalFields.some((field) => item[field] !== undefined &&
			!['unchanged', 'narrowed', 'expanded'].includes(String(item[field]))) ||
		typeof item.requires_review !== 'boolean' || typeof item.diff_digest !== 'string') return null;
	return {
		...item,
		...Object.fromEntries(optionalFields.map((field) => [field, item[field] ?? 'unchanged']))
	} as unknown as AppReviewedPermissionDiff;
}

function parseInteractiveRequest(value: unknown): AppInteractiveCapabilityRequest | null {
	const item = record(value);
	const selectors = item ? record(item.target_selectors) : null;
	const resources = item ? record(item.resources) : null;
	const expiry = item ? record(item.expiry_session) : null;
	const origins = item ? stringList(item.allowed_origins) : null;
	const actionClasses = item ? stringList(item.action_classes) : null;
	const bundleIds = selectors ? stringList(selectors.bundle_ids ?? []) : null;
	const packageIds = selectors ? stringList(selectors.package_ids ?? []) : null;
	const applicationRefs = selectors ? stringList(selectors.application_refs ?? []) : null;
	const resourceFields = [
		'max_sessions', 'max_steps', 'max_duration_seconds', 'max_evidence_bytes',
		'max_evidence_nodes', 'max_pixels', 'max_artifact_bytes', 'max_output_bytes'
	] as const;
	const owner = String(item?.owner ?? '');
	const actionClass = actionClasses?.length === 1 ? actionClasses[0] : '';
	const supportedClass = owner === 'browser' || owner === 'macos'
		? ['observe', 'navigate_or_launch', 'interact', 'outward_commit'].includes(actionClass)
		: owner === 'android'
			? ['observe', 'navigate_or_launch', 'interact', 'capture_pixels', 'outward_commit'].includes(actionClass)
			: false;
	const targetProfile = String(item?.target_profile_class ?? '');
	const targetValid = owner === 'browser'
		? targetProfile === 'installation_ephemeral_headless' && (origins?.length ?? 0) > 0 &&
			(origins?.every(exactReviewedOrigin) ?? false) && bundleIds?.length === 0 &&
			packageIds?.length === 0 && applicationRefs?.length === 0 &&
			selectors?.current_reviewed_pairing === false
		: owner === 'macos'
			? targetProfile === 'owner_reviewed_macos_pairing' && origins?.length === 0 &&
				packageIds?.length === 0 && ((bundleIds?.length ?? 0) > 0 ||
				(applicationRefs?.length ?? 0) > 0 || selectors?.current_reviewed_pairing === true)
			: owner === 'android' && targetProfile === 'owner_reviewed_android_pairing' &&
				origins?.length === 0 && bundleIds?.length === 0 && ((packageIds?.length ?? 0) > 0 ||
				(applicationRefs?.length ?? 0) > 0 || selectors?.current_reviewed_pairing === true);
	if (!item || !exactKeysWithOptional(item, [
		'schema', 'owner', 'target_profile_class', 'target_selectors', 'action_classes',
		'background', 'capture', 'transfer', 'resources', 'expiry_session'
	], ['allowed_origins']) ||
		item.schema !== 'magician.app-interactive-capability-request.v1' ||
		!['browser', 'macos', 'android'].includes(String(item.owner)) ||
		!['installation_ephemeral_headless', 'owner_reviewed_macos_pairing',
			'owner_reviewed_android_pairing'].includes(String(item.target_profile_class)) ||
		!origins || !actionClasses || actionClasses.length !== 1 || !supportedClass || !targetValid ||
		!selectors || !exactKeysWithOptional(selectors, ['current_reviewed_pairing'], [
			'bundle_ids', 'package_ids', 'application_refs'
		]) || !bundleIds || !packageIds || !applicationRefs ||
		typeof selectors.current_reviewed_pairing !== 'boolean' ||
		item.background !== 'direct_owner' ||
		!['structured_evidence_only', 'reviewed_pixels'].includes(String(item.capture)) ||
		item.capture !== (actionClass === 'capture_pixels' ? 'reviewed_pixels' : 'structured_evidence_only') ||
		item.transfer !== 'denied' ||
		!resources || !exactKeys(resources, resourceFields) ||
		resourceFields.some((field) => !safeCount(resources[field])) ||
		!expiry || !exactKeys(expiry, ['grant_lifetime_seconds', 'max_session_seconds', 'session']) ||
		!safeCount(expiry.grant_lifetime_seconds) || !safeCount(expiry.max_session_seconds) ||
		!['invocation_bound', 'run_bound'].includes(String(expiry.session))) return null;
	return {
		...(item as unknown as AppInteractiveCapabilityRequest),
		allowed_origins: origins,
		target_selectors: {
			bundle_ids: bundleIds,
			package_ids: packageIds,
			application_refs: applicationRefs,
			current_reviewed_pairing: selectors.current_reviewed_pairing
		}
	};
}

function parseInteractiveGrants(value: unknown): AppReviewedInteractiveCapabilityGrant[] | null {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return null;
	const parsed: AppReviewedInteractiveCapabilityGrant[] = [];
	for (const entry of value) {
		const item = record(entry);
		const requested = item ? parseInteractiveRequest(item.requested) : null;
		const granted = item ? parseInteractiveRequest(item.granted) : null;
		const action = item ? record(item.action) : null;
		const effects = action ? stringList(action.effects) : null;
		if (!item || !exactKeys(item, [
			'schema', 'dependency_ref', 'locked_binding_digest', 'primitive_binding_digest',
			'requested_request_digest', 'requested', 'granted', 'action', 'grant_digest'
		]) || item.schema !== 'magician.app-reviewed-interactive-capability-grant.v1' ||
			!boundedString(item.dependency_ref, 192) || !boundedString(item.locked_binding_digest, 96) ||
			!boundedString(item.primitive_binding_digest, 96) ||
			!boundedString(item.requested_request_digest, 96) || !boundedString(item.grant_digest, 96) ||
			!requested || !granted || !action || !exactKeys(action, [
				'action_ref', 'class', 'action_digest', 'input_schema_digest',
				'result_schema_digest', 'effects', 'implementation_plan_digest', 'result_byte_ceiling'
			]) || !boundedString(action.action_ref, 192) || action.class !== granted.action_classes[0] ||
			!boundedString(action.action_digest, 96) || !boundedString(action.input_schema_digest, 96) ||
			!boundedString(action.result_schema_digest, 96) || !effects ||
			!boundedString(action.implementation_plan_digest, 96) ||
			!safeCount(action.result_byte_ceiling)) return null;
		parsed.push(item as unknown as AppReviewedInteractiveCapabilityGrant);
	}
	return parsed;
}

export function appInstallationReviewMatrix(review: AppInstallationReview): AppReviewMatrixRow[] {
	const data = review.requested_data_handling_policy;
	const background = review.requested_background_execution;
	const network = review.requested_network_policy;
	const resources = review.requested_resource_ceiling;
	const rows: AppReviewMatrixRow[] = [
		{ category: 'Data classification', request: data.classification_floor, posture: 'bounded' },
		{ category: 'Model processing', request: data.model_processing.replaceAll('_', ' '), posture: data.model_processing === 'none' ? 'denied' : data.model_processing === 'local_only' ? 'bounded' : 'conditional' },
		{ category: 'Personal-agent data', request: data.personal_agent_access.replaceAll('_', ' '), posture: data.personal_agent_access === 'denied' ? 'denied' : 'conditional' },
		{ category: 'Memory proposal', request: data.memory_promotion.replaceAll('_', ' '), posture: data.memory_promotion === 'denied' ? 'denied' : 'conditional' },
		{ category: 'External egress', request: data.external_egress === 'denied' ? 'denied' : data.approved_destinations.join(', '), posture: data.external_egress === 'denied' ? 'denied' : 'conditional' },
		{ category: 'Network', request: network.mode === 'denied' ? 'denied' : network.destinations.join(', '), posture: network.mode === 'denied' ? 'denied' : 'conditional' },
		{ category: 'Background execution', request: background.mode === 'denied' ? 'denied' : `minimum ${background.min_interval_seconds}s; ${background.max_concurrent_runs} concurrent`, posture: background.mode === 'denied' ? 'denied' : 'bounded' },
		{ category: 'Foreground concurrency', request: String(resources.max_concurrent_foreground_runs), posture: 'bounded' },
		{ category: 'Lifetime', request: `${resources.max_lifetime_seconds}s`, posture: 'bounded' },
		{ category: 'Browser network actions', request: String(resources.max_browser_network_actions), posture: resources.max_browser_network_actions === 0 ? 'denied' : 'bounded' },
		{ category: 'Storage', request: `${resources.max_records} records; ${resources.max_payload_bytes.toLocaleString()} payload bytes; ${resources.max_attachment_bytes.toLocaleString()} attachment bytes`, posture: 'bounded' },
		{ category: 'Monthly model budget', request: `${resources.max_monthly_tokens.toLocaleString()} tokens; ${resources.max_monthly_cost_microusd.toLocaleString()} µUSD`, posture: 'bounded' }
	];
	for (const dispatch of review.tool_dispatch) {
		rows.push({
			category: `Capability ${grantDisplayName(dispatch.tool)}`,
			request: dispatch.dispatchable
				? dispatch.attested_operations.length > 0 ? dispatch.attested_operations.join(', ') : 'reviewed operation'
				: dispatch.reason,
			posture: dispatch.dispatchable ? 'conditional' : 'denied'
		});
	}
	return rows;
}

async function readJson(response: Response): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const size = Number(declared);
		if (!Number.isSafeInteger(size) || size < 0 || size > MAX_RESPONSE_BYTES) {
			throw new Error('The app review response exceeded its size limit.');
		}
	}
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_RESPONSE_BYTES) {
			await reader.cancel().catch(() => undefined);
			throw new Error('The app review response exceeded its size limit.');
		}
		chunks.push(value);
	}
	const encoded = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		encoded.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const text = new TextDecoder('utf-8', { fatal: true }).decode(encoded);
	if (!text) return null;
	try {
		return JSON.parse(text);
	} catch {
		throw new Error('The app review response is not valid JSON.');
	}
}

export async function fetchAppInstallationReview(
	installationId: string,
	signal?: AbortSignal
): Promise<AppInstallationReview> {
	const response = await fetch(`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/review`, {
		headers: scopedRequestHeaders(),
		signal
	});
	const body = await readJson(response);
	if (!response.ok) throw responseError(body, 'The app review could not be loaded.');
	const review = parseReview(body);
	if (!review || review.installation_id !== installationId) {
		const reason = review && review.installation_id !== installationId
			? `installation_id mismatch: ${review.installation_id} != ${installationId}`
			: lastReviewParseFailureReason();
		throw new Error(`The app review payload is invalid.${reason ? ` (${reason})` : ''}`);
	}
	return review;
}

export async function approveAppInstallation(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'package_revision_ref' | 'status'>,
	review: AppInstallationReview,
	request: AppInstallationApproveRequest,
	signal?: AbortSignal
): Promise<AppInstallationApproveReceipt> {
	assertApprovalCorrelation(entry, review, request);
	const installationId = entry.installation_id;
	const response = await fetch(`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/approve`, {
		method: 'POST',
		headers: {
			...scopedRequestHeaders(),
			'content-type': 'application/json'
		},
		body: JSON.stringify(request),
		signal
	});
	const body = await readJson(response);
	if (!response.ok) throw responseError(body, 'The app could not be approved.');
	const receipt = parseReceipt(body);
	if (!receipt || receipt.installation_id !== installationId ||
		receipt.attempt_id !== review.attempt_id ||
		receipt.generation !== entry.installation_generation + 1 ||
		receipt.status !== 'enabled') {
		throw new Error('The app approval receipt did not match the reviewed installation generation.');
	}
	return receipt;
}

function assertApprovalCorrelation(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'package_revision_ref' | 'status'>,
	review: AppInstallationReview,
	request: AppInstallationApproveRequest
): void {
	const expectedAttempt = entry.status === 'ready_for_review' ? 'initial_install'
		: entry.status === 'update_pending' ? 'update'
			: entry.status === 'uninstalled_retained' ? 'reinstall' : null;
	if (!expectedAttempt || review.installation_id !== entry.installation_id ||
		(expectedAttempt === 'initial_install' && review.package_revision_ref !== entry.package_revision_ref) ||
		(expectedAttempt !== 'initial_install' &&
			(!boundedString(request.migration_run_id, 192) || !boundedString(request.update_plan_digest, 96))) ||
		review.attempt_kind !== expectedAttempt ||
		request.review_material_digest !== review.workflow_material_digest) {
		throw new Error('The approval no longer matches the displayed installation review.');
	}
	const requested: Array<[readonly string[], readonly string[] | undefined]> = [
		[review.requested_tools, request.granted_tools],
		[review.requested_agents, request.granted_agents],
		[review.requested_personalities, request.granted_personalities]
	];
	for (const [allowed, granted = []] of requested) {
		if (granted.length > 256 || new Set(granted).size !== granted.length ||
			granted.some((item) => !allowed.includes(item))) {
			throw new Error('The approval grant is not a subset of the displayed review.');
		}
	}
	const surface = review.requested_custom_surface;
	const selected = request.granted_custom_surface_entry_points ?? [];
	if (selected.length > 8 || new Set(selected.map((entry) => entry.route)).size !== selected.length ||
		selected.some((entry) => !surface || entry.reviewed_request_digest !== surface.request_digest ||
			!surface.entry_points.some((allowed) => allowed.route === entry.route && allowed.document === entry.document))) {
		throw new Error('The interactive pages no longer match the displayed installation review.');
	}
}

export function grantDisplayName(ref: string): string {
	return ref.replace(/^(capability|agent|personality):/, '');
}

export function inertWorkflowsForGrant(
	review: AppInstallationReview,
	grant: AppInstallationApproveRequest
): AppInertWorkflow[] {
	const tools = new Set(grant.granted_tools ?? []);
	const agents = new Set(grant.granted_agents ?? []);
	const personalities = new Set(grant.granted_personalities ?? []);
	return review.workflows.flatMap((workflow) => {
		const reasons: string[] = [];
		for (const tool of workflow.uses) {
			if (!tools.has(tool)) reasons.push(`missing tool ${grantDisplayName(tool)}`);
		}
		if (!agents.has(workflow.agent)) reasons.push(`missing agent ${grantDisplayName(workflow.agent)}`);
		if (workflow.personality && !personalities.has(workflow.personality)) {
			reasons.push(`missing personality ${grantDisplayName(workflow.personality)}`);
		}
		return reasons.length ? [{ workflow_id: workflow.workflow_id, reasons }] : [];
	});
}

export function defaultGrantedNames(review: AppInstallationReview): AppInstallationApproveRequest {
	const memory = review.requested_memory_read;
	return {
		review_material_digest: review.workflow_material_digest,
		granted_tools: [...review.requested_tools],
		granted_agents: [...review.requested_agents],
		granted_personalities: [...review.requested_personalities],
		...(review.requested_custom_surface ? { granted_custom_surface_entry_points: [] } : {}),
		// Secrets start unticked: a key is only ever granted explicitly.
		...(review.requested_secret_uses ? { granted_secret_uses: [] } : {}),
		// "Any public host" starts off: it is only ever granted explicitly.
		...(review.offers_any_public_host ? { granted_any_public_host: false } : {}),
		...(memory
			? {
					granted_memory_read: {
						reviewed_request_digest: memory.request_digest,
						interactive: {
							user_tiers: [...memory.default_grant.interactive.user_tiers],
							agents: [...memory.default_grant.interactive.agents]
						},
						background: {
							user_tiers: [...memory.default_grant.background.user_tiers],
							agents: [...memory.default_grant.background.agents]
						}
					}
				}
			: {})
	};
}

/** Grant only an explicitly selected page from the exact displayed review. */
export function toggleCustomSurfaceGrant(
	review: Pick<AppInstallationReview, 'requested_custom_surface'>,
	grant: AppInstallationApproveRequest,
	route: string
): AppInstallationApproveRequest {
	const surface = review.requested_custom_surface;
	const entry = surface?.entry_points.find((point) => point.route === route);
	if (!surface || !entry) throw new Error('This interactive page was not requested by the app.');
	const selected = grant.granted_custom_surface_entry_points ?? [];
	return {
		...grant,
		granted_custom_surface_entry_points: selected.some((point) => point.route === route)
			? selected.filter((point) => point.route !== route)
			: [...selected, { route: entry.route, document: entry.document, reviewed_request_digest: surface.request_digest }]
	};
}

/** Tick or untick the "any public host" grant the review offers. */
export function toggleAnyPublicHostGrant(
	review: Pick<AppInstallationReview, 'offers_any_public_host'>,
	grant: AppInstallationApproveRequest
): AppInstallationApproveRequest {
	if (!review.offers_any_public_host) {
		throw new Error('None of this app\'s tools can use the network.');
	}
	return { ...grant, granted_any_public_host: !grant.granted_any_public_host };
}

/**
 * Where a ticked key can travel, for the Secrets table: its declared host(s),
 * the hosts the app is granted, or any website under "any public host".
 */
export function secretReach(use: AppSecretUseRequest, granted?: AppSecretUseGrant): string {
	if (use.not_grantable) return 'cannot be sent anywhere in this app';
	if (use.destination === '*') return granted?.any_site ? 'any site' : 'any site, only if you allow it';
	if (use.destination) return use.destination;
	if (use.destinations?.length) return use.destinations.join(', ');
	if (granted?.any_site) return 'any site';
	return granted?.hosts?.length ? granted.hosts.join(', ') : 'pick the hosts';
}

/** The app's named hosts: the only hosts a host-less tool's key may be limited to. */
export function appNamedHosts(review: Pick<AppInstallationReview, 'requested_network_policy'>): string[] {
	const policy = review.requested_network_policy;
	return policy.mode === 'approved_destinations'
		? policy.destinations.filter((d) => d.startsWith('destination:')).map((d) => d.slice('destination:'.length))
		: [];
}

/**
 * Set where one key of a tool that declares no host (or declares any site,
 * `*`) may go: one or more of the app's named hosts, or — an explicit opt-in —
 * any site. An empty choice withdraws the key. The choice is the vault scope
 * and the only hosts the tool reaches while the key is injected.
 */
export function setSecretUseScope(
	review: Pick<AppInstallationReview, 'requested_secret_uses' | 'requested_network_policy' | 'requested_data_handling_policy'>,
	grant: AppInstallationApproveRequest,
	tool: string,
	secretRef: string,
	scope: { hosts: string[]; anySite: boolean }
): AppInstallationApproveRequest {
	const use = review.requested_secret_uses?.find((item) => item.tool === tool && item.secret_ref === secretRef);
	const star = use?.destination === '*';
	if (!use || use.not_grantable || (!use.app_granted_hosts && !star)) {
		throw new Error('This key cannot be scoped here.');
	}
	const named = appNamedHosts(review);
	const hosts = [...new Set(scope.hosts)].sort();
	if (hosts.some((host) => !named.includes(host)) || (star && hosts.length)) {
		throw new Error('A key can be limited only to the app\'s named hosts.');
	}
	if (scope.anySite && (hosts.length || review.requested_data_handling_policy.external_egress !== 'any_public_host')) {
		throw new Error('"Any site" is only for an app that asks for any public host.');
	}
	const others = (grant.granted_secret_uses ?? []).filter(
		(item) => !(item.tool === tool && item.secret_ref === secretRef)
	);
	const chosen: AppSecretUseGrant | null = scope.anySite
		? { tool, secret_ref: secretRef, any_site: true }
		: hosts.length
			? { tool, secret_ref: secretRef, hosts }
			: null;
	return { ...grant, granted_secret_uses: chosen ? [...others, chosen] : others };
}

/** Tick or untick one (tool, secret) pair the review requested. */
export function toggleSecretUseGrant(
	review: Pick<AppInstallationReview, 'requested_secret_uses'>,
	grant: AppInstallationApproveRequest,
	tool: string,
	secretRef: string
): AppInstallationApproveRequest {
	const use = review.requested_secret_uses?.find((item) => item.tool === tool && item.secret_ref === secretRef);
	if (!use) {
		throw new Error('This secret was not requested by the app.');
	}
	if (use.not_grantable || use.app_granted_hosts || use.destination === '*') {
		throw new Error('This key needs its hosts chosen, or cannot be granted.');
	}
	const selected = grant.granted_secret_uses ?? [];
	const ticked = selected.some((use) => use.tool === tool && use.secret_ref === secretRef);
	return {
		...grant,
		granted_secret_uses: ticked
			? selected.filter((use) => !(use.tool === tool && use.secret_ref === secretRef))
			: [...selected, { tool, secret_ref: secretRef }]
	};
}

/** Toggle one tier or agent in one run mode of a pending memory grant. */
export function toggleMemoryGrant(
	grant: AppInstallationApproveRequest,
	mode: 'interactive' | 'background',
	kind: 'user_tiers' | 'agents',
	name: string
): AppInstallationApproveRequest {
	const memory = grant.granted_memory_read;
	if (!memory) return grant;
	const values = new Set(memory[mode][kind]);
	if (values.has(name)) values.delete(name);
	else values.add(name);
	return {
		...grant,
		granted_memory_read: {
			...memory,
			[mode]: { ...memory[mode], [kind]: [...values].sort() }
		}
	};
}

/**
 * Why the last review payload was rejected. The owner-facing copy is
 * deliberately generic, which made a server that grew a field indistinguishable
 * from a genuinely malformed one. Recording the failing check turns 'invalid'
 * into something diagnosable without devtools.
 */
let lastReviewParseFailure: string | null = null;

export function lastReviewParseFailureReason(): string | null {
	return lastReviewParseFailure;
}

function rejectReview(reason: string): null {
	lastReviewParseFailure = reason;
	return null;
}

function parseReview(value: unknown): AppInstallationReview | null {
	const item = record(value);
	if (!item || !exactKeysWithOptional(item, [
		'installation_id', 'attempt_id', 'attempt_kind', 'package_revision_ref',
		'package_content_digest', 'name', 'version', 'description', 'requested_tools',
		'requested_agents', 'requested_personalities', 'tool_dispatch', 'workflows',
		'workflow_material_bindings', 'workflow_material_digest', 'inert_workflows',
		'requested_data_handling_policy', 'requested_background_execution',
		'requested_network_policy', 'requested_resource_ceiling'
	], [
		'permission_diff', 'requested_interactive_capabilities', 'requested_contribution_ports',
		// The server skips these when empty, so they appear only for packages
		// that actually use behaviours, custom surfaces or event projections.
		// Every shipped system package declares behaviours, so omitting them
		// from this allow-list rejected the whole payload as invalid and made
		// those packages unreviewable in the UI.
		'requested_behaviors', 'requested_event_behaviors', 'requested_custom_surface',
		'requested_behavior_input_selectors', 'requested_behavior_output_schemas',
		'requested_event_projection_schemas', 'requested_event_output_schemas',
		// Notification authority. Explicit-deny like event behaviours —
		// `select_notification_grants` returns an empty vec when the grant
		// field is omitted — so accepting it without rendering grants nothing.
		'requested_notifications',
		// Owner memory request (`app_memory_read_v1`); present only for apps
		// that ask to read the owner's memory.
		'requested_memory_read',
		// Secret uses (`app_secret_use_v1`); present only when a locked tool
		// uses a secret. Explicit-grant: omitted grants none.
		'requested_secret_uses',
		// In-place tools and hosts (`app_in_place_skill_v1`); present only when
		// a locked tool runs in place or declares hosts.
		'tool_runtime', 'offers_any_public_host'
	])) return rejectReview(`unexpected or missing top-level key(s): ${Object.keys(record(value) ?? {}).join(', ')}`);
	const requestedTools = stringList(item.requested_tools);
	const requestedAgents = stringList(item.requested_agents);
	const requestedPersonalities = stringList(item.requested_personalities);
	const requestedInteractive = parseInteractiveGrants(item.requested_interactive_capabilities ?? []);
	if (
		!boundedString(item.installation_id, 128) || !boundedString(item.attempt_id, 192) ||
		!boundedString(item.package_revision_ref, 192) || !boundedString(item.package_content_digest, 96) ||
		!boundedString(item.name, 256) || !boundedString(item.version, 128) ||
		typeof item.description !== 'string' || item.description.length > 2_048 ||
		!requestedTools || !requestedAgents || !requestedPersonalities || !requestedInteractive ||
		!Array.isArray(item.workflows) || item.workflows.length > MAX_REVIEW_COLLECTION_ITEMS ||
		!Array.isArray(item.workflow_material_bindings) ||
		item.workflow_material_bindings.length > MAX_REVIEW_COLLECTION_ITEMS ||
		!Array.isArray(item.tool_dispatch) || item.tool_dispatch.length > MAX_REVIEW_COLLECTION_ITEMS ||
		!Array.isArray(item.inert_workflows) || item.inert_workflows.length > MAX_REVIEW_COLLECTION_ITEMS ||
		!boundedString(item.workflow_material_digest, 96)
	) {
		return rejectReview('a required scalar or array field failed its bound/type check');
	}
	const attemptKind = item.attempt_kind;
	if (attemptKind !== 'initial_install' && attemptKind !== 'reinstall' && attemptKind !== 'update') {
		return rejectReview(`unknown attempt_kind: ${String(attemptKind)}`);
	}
	const workflows = parseWorkflows(item.workflows);
	const workflowMaterialBindings = parseWorkflowMaterialBindings(item.workflow_material_bindings);
	const dataHandling = parseDataHandling(item.requested_data_handling_policy);
	const background = parseBackground(item.requested_background_execution);
	const network = parseNetwork(item.requested_network_policy);
	const resources = parseResources(item.requested_resource_ceiling);
	const permissionDiff = parsePermissionDiff(item.permission_diff);
	const toolDispatch = parseToolDispatch(item.tool_dispatch);
	const inertWorkflows = parseInert(item.inert_workflows);
	const behaviors = parseBehaviorGrants(item.requested_behaviors ?? []);
	const eventBehaviors = parseBehaviorGrants(item.requested_event_behaviors ?? []);
	const customSurface = parseCustomSurface(item.requested_custom_surface);
	const memoryRead = parseMemoryRead(item.requested_memory_read);
	// A memory request that fails to parse is authority the owner would grant
	// unseen (the default grant applies when the choice is omitted), so it
	// rejects the whole payload rather than being dropped.
	if (memoryRead === null) return rejectReview('requested_memory_read failed to parse');
	const secretUses = parseSecretUses(item.requested_secret_uses);
	if (secretUses === null) return rejectReview('requested_secret_uses failed to parse');
	const toolRuntime = parseToolRuntime(item.tool_runtime);
	if (toolRuntime === null) return rejectReview('tool_runtime failed to parse');
	if (item.offers_any_public_host !== undefined && typeof item.offers_any_public_host !== 'boolean') {
		return rejectReview('offers_any_public_host is not a boolean');
	}
	// A behaviour that fails to parse must not be silently dropped: approval
	// grants every requested behaviour when the grant field is omitted, so a
	// dropped entry is undisclosed authority. Fail the whole payload instead.
	if (
		behaviors === null || eventBehaviors === null || customSurface === null ||
		workflows.length !== item.workflows.length ||
		!Array.isArray(item.tool_dispatch) || toolDispatch.length !== item.tool_dispatch.length ||
		!Array.isArray(item.inert_workflows) || inertWorkflows.length !== item.inert_workflows.length ||
		!workflowMaterialBindings ||
		workflowMaterialBindings.length !== workflows.length ||
		!dataHandling || !background || !network || !resources || permissionDiff === null ||
		workflowMaterialBindings.some(
			(binding) => !workflows.some((workflow) => workflow.workflow_id === binding.workflow_id)
		)
	) return rejectReview(`consistency: behaviors=${behaviors === null ? 'BAD' : behaviors.length} eventBehaviors=${eventBehaviors === null ? 'BAD' : eventBehaviors.length} customSurface=${customSurface === null ? 'BAD' : customSurface ? 'ok' : 'absent'} workflows=${workflows.length}/${(item.workflows as unknown[]).length} toolDispatch=${toolDispatch.length}/${(item.tool_dispatch as unknown[]).length} inert=${inertWorkflows.length}/${(item.inert_workflows as unknown[]).length} bindings=${workflowMaterialBindings ? workflowMaterialBindings.length : 'BAD'} dataHandling=${dataHandling ? 'ok' : 'BAD'} background=${background ? 'ok' : 'BAD'} network=${network ? 'ok' : 'BAD'} resources=${resources ? 'ok' : 'BAD'} permissionDiff=${permissionDiff === null ? 'BAD' : 'ok'}`);
	return {
		installation_id: item.installation_id,
		attempt_id: item.attempt_id,
		attempt_kind: attemptKind,
		package_revision_ref: item.package_revision_ref,
		package_content_digest: item.package_content_digest,
		name: item.name,
		version: item.version,
		description: item.description,
		requested_tools: requestedTools,
		requested_agents: requestedAgents,
		requested_personalities: requestedPersonalities,
		requested_interactive_capabilities: requestedInteractive,
		tool_dispatch: toolDispatch,
		workflows,
		workflow_material_bindings: workflowMaterialBindings,
		workflow_material_digest: item.workflow_material_digest,
		inert_workflows: inertWorkflows,
		requested_data_handling_policy: dataHandling,
		requested_background_execution: background,
		requested_network_policy: network,
		requested_resource_ceiling: resources,
		requested_behaviors: behaviors,
		requested_event_behaviors: eventBehaviors,
		...(permissionDiff ? { permission_diff: permissionDiff } : {}),
		...(customSurface ? { requested_custom_surface: customSurface } : {}),
		...(memoryRead ? { requested_memory_read: memoryRead } : {}),
		...(secretUses ? { requested_secret_uses: secretUses } : {}),
		...(toolRuntime ? { tool_runtime: toolRuntime } : {}),
		...(item.offers_any_public_host ? { offers_any_public_host: true } : {})
	};
}

function hostList(value: unknown): string[] | null {
	if (value === undefined) return [];
	if (!Array.isArray(value) || value.length > 64 || !value.every((host) => boundedString(host, 253))) return null;
	return value as string[];
}

/** `undefined` when absent; `null` when present but malformed. */
function parseToolRuntime(value: unknown): AppReviewedToolRuntime[] | undefined | null {
	if (value === undefined) return undefined;
	if (!Array.isArray(value) || value.length === 0 || value.length > MAX_REVIEW_COLLECTION_ITEMS) return null;
	const parsed: AppReviewedToolRuntime[] = [];
	for (const entry of value) {
		const item = record(entry);
		const hosts = item ? hostList(item.declared_hosts) : null;
		if (
			!item || !hosts ||
			!exactKeysWithOptional(item, ['tool'], ['in_place_from', 'declared_hosts', 'reaches_granted_hosts', 'reachable_hosts']) ||
			!hostList(item.reachable_hosts) ||
			!boundedString(item.tool, 192) ||
			(item.in_place_from !== undefined && !boundedString(item.in_place_from, 128)) ||
			(item.reaches_granted_hosts !== undefined && typeof item.reaches_granted_hosts !== 'boolean')
		) return null;
		parsed.push({
			tool: item.tool as string,
			...(item.in_place_from ? { in_place_from: item.in_place_from as string } : {}),
			...(hosts.length ? { declared_hosts: hosts } : {}),
			...(item.reaches_granted_hosts ? { reaches_granted_hosts: true } : {}),
			...((item.reachable_hosts as string[] | undefined)?.length
				? { reachable_hosts: item.reachable_hosts as string[] }
				: {})
		});
	}
	return parsed;
}

/** `undefined` when absent; `null` when present but malformed. */
function parseSecretUses(value: unknown): AppSecretUseRequest[] | undefined | null {
	if (value === undefined) return undefined;
	if (!Array.isArray(value) || value.length === 0 || value.length > 32) return null;
	const parsed: AppSecretUseRequest[] = [];
	for (const entry of value) {
		const item = record(entry);
		const destinations = item ? hostList(item.destinations) : null;
		if (
			!item || !destinations ||
			!exactKeysWithOptional(item, ['tool', 'secret_ref', 'required', 'destination'],
				['destinations', 'app_granted_hosts', 'delivery', 'not_grantable']) ||
			(item.not_grantable !== undefined && !boundedString(item.not_grantable, 512)) ||
			!boundedString(item.tool, 192) || !boundedString(item.secret_ref, 128) ||
			typeof item.destination !== 'string' || item.destination.length > 253 ||
			typeof item.required !== 'boolean' ||
			(item.app_granted_hosts !== undefined && typeof item.app_granted_hosts !== 'boolean') ||
			(item.delivery !== undefined && item.delivery !== 'config_file') ||
			// A key must say where it can go.
			(!item.destination && !destinations.length && item.app_granted_hosts !== true)
		) return null;
		parsed.push({
			tool: item.tool as string,
			secret_ref: item.secret_ref as string,
			required: item.required,
			destination: item.destination as string,
			...(destinations.length ? { destinations } : {}),
			...(item.app_granted_hosts ? { app_granted_hosts: true } : {}),
			...(item.delivery ? { delivery: item.delivery as string } : {}),
			...(item.not_grantable ? { not_grantable: item.not_grantable as string } : {})
		});
	}
	return parsed;
}

function parseMemorySelection(value: unknown): AppMemoryReadSelection | null {
	const item = record(value);
	if (!item || !exactKeysWithOptional(item, [], ['user_tiers', 'agents'])) return null;
	const userTiers = stringList(item.user_tiers ?? []);
	const agents = stringList(item.agents ?? []);
	if (!userTiers || !agents) return null;
	return { user_tiers: userTiers, agents };
}

/** `undefined` when absent; `null` when present but malformed. */
function parseMemoryRead(value: unknown): AppReviewedMemoryRead | undefined | null {
	if (value === undefined) return undefined;
	const item = record(value);
	if (!item || !exactKeysWithOptional(item, ['request', 'request_digest', 'sensitive_tiers', 'default_grant'], [])) {
		return null;
	}
	const request = record(item.request);
	const grant = record(item.default_grant);
	if (!request || !grant || !exactKeysWithOptional(request, ['purpose'], ['user_tiers', 'agents'])) return null;
	const userTiers = stringList(request.user_tiers ?? []);
	const agents = stringList(request.agents ?? []);
	const sensitive = stringList(item.sensitive_tiers);
	const interactive = parseMemorySelection(grant.interactive);
	const background = parseMemorySelection(grant.background);
	if (
		!userTiers || !agents || !sensitive || !interactive || !background ||
		typeof request.purpose !== 'string' || request.purpose.length > 512 ||
		!boundedString(item.request_digest, 96) || !boundedString(grant.request_digest, 96) ||
		grant.engagement !== 'owner_only' || typeof grant.schema !== 'string'
	) return null;
	return {
		request: { user_tiers: userTiers, agents, purpose: request.purpose },
		request_digest: item.request_digest as string,
		sensitive_tiers: sensitive,
		default_grant: {
			schema: grant.schema,
			request_digest: grant.request_digest as string,
			engagement: 'owner_only',
			interactive,
			background
		}
	};
}

/** `null` means a malformed entry was present — reject rather than drop it. */
function parseBehaviorGrants(value: unknown): AppReviewedBehaviorGrant[] | null {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return null;
	const grants: AppReviewedBehaviorGrant[] = [];
	for (const entry of value) {
		const item = record(entry);
		if (!item) return null;
		const operations = stringList(item.operations ?? []);
		if (
			!boundedString(item.behavior_id, 192) ||
			typeof item.purpose !== 'string' || item.purpose.length > MAX_REVIEW_TEXT_CHARS ||
			!boundedString(item.action, 192) || !operations
		) return null;
		grants.push({
			behavior_id: item.behavior_id,
			purpose: item.purpose,
			action: item.action,
			operations
		});
	}
	return grants;
}

/** `undefined` when absent, `null` when present but malformed. */
function parseCustomSurface(value: unknown): AppReviewedCustomSurfaceRequest | null | undefined {
	if (value === undefined) return undefined;
	const item = record(value);
	if (!item || !exactKeys(item, ['entry_points', 'executable_members', 'executable_bytes',
		'scan_findings', 'sandbox', 'csp', 'request_digest'])) return null;
	const entryPoints = item.entry_points;
	const members = item.executable_members;
	const findings = item.scan_findings;
	if (
		!Array.isArray(entryPoints) || entryPoints.length === 0 || entryPoints.length > 8 ||
		!Array.isArray(members) || members.length > MAX_REVIEW_COLLECTION_ITEMS ||
		!Array.isArray(findings) || findings.length > MAX_REVIEW_COLLECTION_ITEMS ||
		typeof item.executable_bytes !== 'number' || !Number.isSafeInteger(item.executable_bytes) || item.executable_bytes < 0 ||
		item.sandbox !== 'allow-scripts' || !boundedString(item.csp, 1_024) || !boundedString(item.request_digest, 96)
	) return null;
	const entries: AppReviewedCustomSurfaceRequest['entry_points'] = [];
	const executables: AppReviewedCustomSurfaceRequest['executable_members'] = [];
	const scans: AppReviewedCustomSurfaceRequest['scan_findings'] = [];
	for (const raw of entryPoints) {
		const entry = record(raw);
		if (!entry || !exactKeys(entry, ['route', 'document', 'document_digest']) ||
			!boundedString(entry.route, 512) || !boundedString(entry.document, 1_024) ||
			!boundedString(entry.document_digest, 96)) return null;
		entries.push({ route: entry.route, document: entry.document, document_digest: entry.document_digest });
	}
	if (new Set(entries.map((entry) => entry.route)).size !== entries.length) return null;
	for (const raw of members) {
		const member = record(raw);
		if (!member || !exactKeys(member, ['path', 'content_digest', 'byte_len']) ||
			!boundedString(member.path, 1_024) || !boundedString(member.content_digest, 96) ||
			typeof member.byte_len !== 'number' || !Number.isSafeInteger(member.byte_len) || member.byte_len < 0) return null;
		executables.push({ path: member.path, content_digest: member.content_digest, byte_len: member.byte_len });
	}
	if (new Set(executables.map((member) => member.path)).size !== executables.length ||
		executables.reduce((bytes, member) => bytes + member.byte_len, 0) !== item.executable_bytes) return null;
	for (const raw of findings) {
		const finding = record(raw);
		if (!finding || !exactKeys(finding, ['path', 'pattern']) ||
			!boundedString(finding.path, 1_024) || !boundedString(finding.pattern, 128)) return null;
		scans.push({ path: finding.path, pattern: finding.pattern });
	}
	return {
		entry_points: entries,
		executable_members: executables,
		scan_findings: scans,
		request_digest: item.request_digest,
		entry_point_count: entryPoints.length,
		executable_member_count: members.length,
		executable_bytes: item.executable_bytes,
		scan_finding_count: findings.length,
		sandbox: item.sandbox,
		csp: item.csp
	};
}

function parseWorkflowMaterialBindings(value: unknown): AppReviewedWorkflowMaterialBinding[] | null {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return null;
	const bindings: AppReviewedWorkflowMaterialBinding[] = [];
	const workflowIds = new Set<string>();
	for (const entry of value) {
		if (!entry || typeof entry !== 'object') return null;
		const item = entry as Record<string, unknown>;
		const personalityFields = [
			item.personality_ref,
			item.personality_content_digest,
			item.personality_descriptor_digest
		];
		const hasPersonality = personalityFields.every((field) => typeof field === 'string');
		const hasNoPersonality = personalityFields.every((field) => field === null || field === undefined);
		if (
			!exactKeysWithOptional(item, [
				'schema', 'workflow_id', 'agent_ref', 'agent_definition_revision',
				'agent_definition_digest', 'agent_descriptor_digest', 'binding_digest'
			], ['personality_ref', 'personality_content_digest', 'personality_descriptor_digest']) ||
			item.schema !== 'magician.app-reviewed-workflow-material.v1' ||
			!boundedString(item.workflow_id, 192) ||
			workflowIds.has(item.workflow_id) ||
			!boundedString(item.agent_ref, 192) ||
			typeof item.agent_definition_revision !== 'number' ||
			!Number.isSafeInteger(item.agent_definition_revision) ||
			item.agent_definition_revision < 1 ||
			!boundedString(item.agent_definition_digest, 96) ||
			!boundedString(item.agent_descriptor_digest, 96) ||
			!boundedString(item.binding_digest, 96) ||
			(hasPersonality && personalityFields.some((field) => !boundedString(field, 192))) ||
			(!hasPersonality && !hasNoPersonality)
		) {
			return null;
		}
		workflowIds.add(item.workflow_id);
		bindings.push({
			workflow_id: item.workflow_id,
			agent_ref: item.agent_ref,
			agent_definition_revision: item.agent_definition_revision,
			agent_definition_digest: item.agent_definition_digest,
			agent_descriptor_digest: item.agent_descriptor_digest,
			personality_ref: hasPersonality ? item.personality_ref as string : null,
			personality_content_digest:
				hasPersonality ? item.personality_content_digest as string : null,
			personality_descriptor_digest:
				hasPersonality ? item.personality_descriptor_digest as string : null,
			binding_digest: item.binding_digest
		});
	}
	return bindings;
}

function parseReceipt(value: unknown): AppInstallationApproveReceipt | null {
	const item = record(value);
	if (!item || !exactKeysWithOptional(item, [
		'installation_id', 'generation', 'status', 'grant_revision', 'schema_revision',
		'surface_revision', 'approval_id', 'attempt_id', 'outcome', 'inert_workflows'
	], ['granted_custom_surface_entry_points'])) return null;
	const inertWorkflows = parseInert(item.inert_workflows);
	const customSurfaceEntries = stringList(item.granted_custom_surface_entry_points ?? []);
	if (
		typeof item.installation_id !== 'string' || item.installation_id.length === 0 ||
		!safeCount(item.generation) || item.generation === 0 || item.status !== 'enabled' ||
		!safeCount(item.grant_revision) || item.grant_revision === 0 ||
		!safeCount(item.schema_revision) || item.schema_revision === 0 ||
		!safeCount(item.surface_revision) || item.surface_revision === 0 ||
		!boundedString(item.approval_id, 192) ||
		!boundedString(item.attempt_id, 192) ||
		(item.outcome !== 'enabled' && item.outcome !== 'already_enabled') ||
		!Array.isArray(item.inert_workflows) || inertWorkflows.length !== item.inert_workflows.length ||
		!customSurfaceEntries
	) {
		return null;
	}
	return {
		installation_id: item.installation_id,
		generation: item.generation,
		status: 'enabled',
		grant_revision: item.grant_revision,
		schema_revision: item.schema_revision,
		surface_revision: item.surface_revision,
		approval_id: item.approval_id,
		attempt_id: item.attempt_id,
		outcome: item.outcome,
		inert_workflows: inertWorkflows,
		...(item.granted_custom_surface_entry_points !== undefined
			? { granted_custom_surface_entry_points: customSurfaceEntries } : {})
	};
}

function parseToolDispatch(value: unknown): AppReviewedToolDispatch[] {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return [];
	return value.flatMap((entry) => {
		if (!entry || typeof entry !== 'object') return [];
		const item = entry as Record<string, unknown>;
		const operations = stringList(item.attested_operations);
		if (!exactKeys(item, ['tool', 'dispatchable', 'attested_operations', 'reason']) ||
			!boundedString(item.tool, 192) || typeof item.dispatchable !== 'boolean' ||
			!boundedString(item.reason, MAX_REVIEW_TEXT_CHARS) || !operations) return [];
		return [
			{
				tool: item.tool,
				dispatchable: item.dispatchable === true,
				attested_operations: operations,
				reason: item.reason
			}
		];
	});
}

function parseWorkflows(value: unknown): AppReviewedWorkflowGrant[] {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return [];
	return value.flatMap((entry) => {
		if (!entry || typeof entry !== 'object') return [];
		const item = entry as Record<string, unknown>;
		const uses = stringList(item.uses);
		if (!exactKeysWithOptional(item, ['workflow_id', 'uses', 'agent'], ['personality']) ||
			!boundedString(item.workflow_id, 192) || !boundedString(item.agent, 192) || !uses ||
			(item.personality !== undefined && item.personality !== null && !boundedString(item.personality, 192))) {
			return [];
		}
		return [
			{
				workflow_id: item.workflow_id,
				uses,
				agent: item.agent,
				personality: typeof item.personality === 'string' ? item.personality : null
			}
		];
	});
}

function parseInert(value: unknown): AppInertWorkflow[] {
	if (!Array.isArray(value) || value.length > MAX_REVIEW_COLLECTION_ITEMS) return [];
	return value.flatMap((entry) => {
		if (!entry || typeof entry !== 'object') return [];
		const item = entry as Record<string, unknown>;
		const reasons = stringList(item.reasons);
		if (!exactKeys(item, ['workflow_id', 'reasons']) || !boundedString(item.workflow_id, 192) || !reasons) return [];
		return [
			{
				workflow_id: item.workflow_id,
				reasons
			}
		];
	});
}
