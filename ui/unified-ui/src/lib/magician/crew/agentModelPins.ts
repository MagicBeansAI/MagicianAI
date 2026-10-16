/**
 * Per-agent model pins (`llm_routing` on an agent definition).
 *
 * An unpinned agent follows the global operation routing (`llm-router.yaml`),
 * and on a harness engine its eligible side-calls ride that harness. A pin
 * wins over both for the operations its lane covers — so pins are opt-in,
 * set here, and the default is none.
 */

export type ModelPinLane = 'planning' | 'evaluation' | 'correction_extraction' | 'memory_consolidation';

export interface ModelPinLaneSpec {
	lane: ModelPinLane;
	label: string;
	hint: string;
}

/** Order and wording shown in the Crew panel. `planning` is the catch-all. */
export const MODEL_PIN_LANES: ModelPinLaneSpec[] = [
	{
		lane: 'planning',
		label: 'Decisions & planning',
		hint: 'Every operation not covered below, including each agentic decision. On a harness engine the harness makes the decisions; this pin then applies to the remaining side-calls.'
	},
	{
		lane: 'evaluation',
		label: 'Evaluation & safety checks',
		hint: 'Tool evaluation, parameter safety and discovery safety checks.'
	},
	{
		lane: 'correction_extraction',
		label: 'Input interpretation',
		hint: 'Reading corrections and steering out of what the owner said.'
	},
	{
		lane: 'memory_consolidation',
		label: 'Memory consolidation',
		hint: 'Entity, insight and episode extraction after a run.'
	}
];

export interface ModelPinProfileOption {
	name: string;
	provider: string;
	model: string;
	class: string;
	installed: boolean;
	selectable: boolean;
}

export interface CodingProfileOption {
	id: string;
	label: string;
}

/**
 * Lane value for an endpoint pinned by `provider`/`model` rather than by a
 * router profile. The panel cannot edit such a pin, only keep or clear it.
 */
export const DIRECT_PIN_PREFIX = 'direct:';

export function isDirectPin(value: string): boolean {
	return value.startsWith(DIRECT_PIN_PREFIX);
}

export interface ModelPinState {
	/** Profile name, a `direct:provider/model` value, or `''` for unpinned. */
	lanes: Record<ModelPinLane, string>;
	coding_profile: string;
	/** Per-operation pins, shown read-only (edited via the YAML tab). */
	operations: Record<string, string>;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function trimmed(value: unknown): string {
	return typeof value === 'string' ? value.trim() : '';
}

/** What an endpoint pins: its profile, else its direct provider/model. */
function endpointPin(value: unknown): string {
	const endpoint = asRecord(value);
	const profile = trimmed(endpoint?.profile);
	if (profile) return profile;
	const provider = trimmed(endpoint?.provider);
	const model = trimmed(endpoint?.model);
	if (!provider && !model) return '';
	return `${DIRECT_PIN_PREFIX}${provider}/${model}`;
}

/** Read the current pins off a definition. Missing pins are `''`. */
export function modelPinStateFromDefinition(definition: Record<string, unknown> | null | undefined): ModelPinState {
	const routing = asRecord(definition?.llm_routing);
	const lanes = Object.fromEntries(
		MODEL_PIN_LANES.map(({ lane }) => [lane, endpointPin(routing?.[lane])])
	) as Record<ModelPinLane, string>;
	const operations: Record<string, string> = {};
	const ops = asRecord(routing?.operations);
	if (ops) {
		for (const [operation, endpoint] of Object.entries(ops)) {
			const profile = endpointPin(endpoint);
			if (profile) operations[operation] = profile;
		}
	}
	const coding = routing?.coding_profile;
	return {
		lanes,
		coding_profile: typeof coding === 'string' ? coding.trim() : '',
		operations
	};
}

export function modelPinStatesEqual(a: ModelPinState, b: ModelPinState): boolean {
	return (
		MODEL_PIN_LANES.every(({ lane }) => a.lanes[lane] === b.lanes[lane]) &&
		a.coding_profile === b.coding_profile &&
		JSON.stringify(a.operations) === JSON.stringify(b.operations)
	);
}

export function countModelPins(state: ModelPinState): number {
	return (
		MODEL_PIN_LANES.filter(({ lane }) => state.lanes[lane]).length +
		(state.coding_profile ? 1 : 0) +
		Object.keys(state.operations).length
	);
}

/**
 * The JSON Merge Patch (RFC 7386) body that turns `saved` into `next`. Only
 * what changed is sent, so a pin the panel cannot edit (a direct
 * provider/model endpoint, a per-operation pin) survives an unrelated save.
 * A cleared lane is sent as `null` (removed); when nothing at all stays
 * pinned the whole `llm_routing` block is removed, so an unpinned agent
 * carries no routing section.
 */
export function buildModelPinPatch(saved: ModelPinState, next: ModelPinState): Record<string, unknown> {
	if (countModelPins(next) === 0) {
		return { llm_routing: null };
	}
	const routing: Record<string, unknown> = {};
	for (const { lane } of MODEL_PIN_LANES) {
		const value = next.lanes[lane].trim();
		if (value === saved.lanes[lane]) continue;
		if (isDirectPin(value)) continue;
		routing[lane] = value ? { profile: value, provider: '', model: '' } : null;
	}
	const coding = next.coding_profile.trim();
	if (coding !== saved.coding_profile) {
		routing.coding_profile = coding || null;
	}
	return { llm_routing: routing };
}

/** Options a lane can pin to: concrete router profiles, API first. */
export function pinnableProfiles(profiles: ModelPinProfileOption[]): ModelPinProfileOption[] {
	const rank = (profile: ModelPinProfileOption) =>
		profile.class === 'api' ? 0 : profile.class === 'local' ? 1 : 2;
	return profiles
		.filter((profile) => profile.selectable)
		.slice()
		.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
}

/** Label for a `direct:provider/model` lane value. */
export function directPinLabel(value: string): string {
	return `${value.slice(DIRECT_PIN_PREFIX.length)} (direct model pin)`;
}

export function profileOptionLabel(profile: ModelPinProfileOption): string {
	const suffix = profile.class === 'harness' ? ' · harness' : profile.class === 'local' ? ' · local' : '';
	const missing = profile.installed ? '' : ' (not installed)';
	return `${profile.name} — ${profile.model}${suffix}${missing}`;
}
