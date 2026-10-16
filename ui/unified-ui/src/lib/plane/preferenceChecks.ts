/**
 * Settings → Engines: preference checks (P6.4, P7.2).
 *
 * - `GET /api/magician/v2/preference-alignment/status` — the mode actually in
 *   force, and what the saved config file would do if it loaded: accepted, or
 *   rejected with a stable `code` and the `property` to fix. A rejected file
 *   never reaches the running service, so the two can differ; the panel shows
 *   both rather than one.
 * - `GET`/`PUT /api/magician/v2/preference-alignment/policy` — the owner's
 *   explicit intervention policy. A write names the revision it was based on;
 *   a write from a stale screen is refused with the current policy.
 */
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type InterventionPolicy = 'advisory' | 'choice_for_material' | 'confirm_before_exception';

export const POLICY_CHOICES: Array<{ value: InterventionPolicy | ''; label: string; detail: string }> = [
	{
		value: '',
		label: 'Recommended',
		detail:
			'Hold for what you explicitly asked to be checked; offer a choice for clear conflicts with what you said; only mention inferred preferences.'
	},
	{
		value: 'advisory',
		label: 'Just mention it',
		detail:
			'Point out the conflict and carry on. A standing "check with me first" you set still holds.'
	},
	{
		value: 'choice_for_material',
		label: 'Offer a choice',
		detail:
			'For a clear conflict, pause and offer choices — including for preferences Magician inferred.'
	},
	{
		value: 'confirm_before_exception',
		label: 'Always ask first',
		detail:
			'Wait for your confirmation before acting against anything you said. Inferred preferences still only get a choice.'
	}
];

export interface ModeState {
	mode: string;
	configuredMode: string;
	modeForYou: string;
}

export interface ConfigRejection {
	code: string;
	property: string | null;
	reason: string;
}

export interface PreferenceCheckStatus {
	effective: ModeState;
	saved:
		| { status: 'accepted'; mode: ModeState | null; judgeRouteBound: boolean }
		| { status: 'rejected'; error: ConfigRejection };
	inSync: boolean;
}

export interface OwnerPolicy {
	status: 'ok';
	revision: number;
	global: InterventionPolicy | null;
	intentions: Record<string, InterventionPolicy>;
	updatedAtMs: number | null;
	updatedVia: string | null;
}

export type PolicyRead = OwnerPolicy | { status: 'unreadable'; message: string };

function record(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function text(value: unknown, fallback = ''): string {
	return typeof value === 'string' ? value : fallback;
}

function modeState(value: unknown): ModeState | null {
	const state = record(value);
	if (!state) return null;
	return {
		mode: text(state.mode, 'off'),
		configuredMode: text(state.configured_mode, text(state.mode, 'off')),
		modeForYou: text(state.mode_for_you, text(state.mode, 'off'))
	};
}

export function parseStatus(raw: unknown): PreferenceCheckStatus | null {
	const body = record(raw);
	const effective = modeState(body?.effective);
	const saved = record(body?.saved);
	if (!body || !effective || !saved) return null;
	if (saved.status === 'rejected') {
		const error = record(saved.error) ?? {};
		return {
			effective,
			saved: {
				status: 'rejected',
				error: {
					code: text(error.code, 'config_invalid'),
					property: typeof error.property === 'string' ? error.property : null,
					reason: text(error.reason)
				}
			},
			inSync: false
		};
	}
	return {
		effective,
		saved: {
			status: 'accepted',
			mode: modeState(saved.preference_alignment),
			judgeRouteBound: saved.judge_route_bound === true
		},
		inSync: body.in_sync === true
	};
}

const POLICIES: InterventionPolicy[] = ['advisory', 'choice_for_material', 'confirm_before_exception'];

function policyOf(value: unknown): InterventionPolicy | null {
	return typeof value === 'string' && (POLICIES as string[]).includes(value)
		? (value as InterventionPolicy)
		: null;
}

export function parsePolicy(raw: unknown): PolicyRead | null {
	const body = record(raw);
	if (!body) return null;
	if (body.status === 'unreadable') {
		return { status: 'unreadable', message: text(body.message) };
	}
	if (typeof body.revision !== 'number') return null;
	const intentions: Record<string, InterventionPolicy> = {};
	for (const [id, value] of Object.entries(record(body.intentions) ?? {})) {
		const policy = policyOf(value);
		if (policy) intentions[id] = policy;
	}
	return {
		status: 'ok',
		revision: body.revision,
		global: policyOf(body.global),
		intentions,
		updatedAtMs: typeof body.updated_at_ms === 'number' ? body.updated_at_ms : null,
		updatedVia: typeof body.updated_via === 'string' ? body.updated_via : null
	};
}

export async function fetchPreferenceCheckStatus(): Promise<PreferenceCheckStatus> {
	const response = await fetch('/api/magician/v2/preference-alignment/status', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(body?.message ?? 'Preference check status is unavailable.');
	const status = parseStatus(body);
	if (!status) throw new Error('The server did not describe preference checks.');
	return status;
}

export async function fetchOwnerPolicy(): Promise<PolicyRead> {
	const response = await fetch('/api/magician/v2/preference-alignment/policy', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(body?.message ?? 'Your check policy is unavailable.');
	const policy = parsePolicy(body);
	if (!policy) throw new Error('The server did not return your check policy.');
	return policy;
}

export type PolicySave =
	| { status: 'saved'; policy: OwnerPolicy }
	| { status: 'stale'; current: OwnerPolicy; message: string };

/**
 * Save the global choice. Refinements for single preferences are kept as
 * they are; `expectedRevision` is the revision this screen showed.
 */
export async function saveOwnerPolicy(
	expectedRevision: number,
	global: InterventionPolicy | null,
	intentions: Record<string, InterventionPolicy>,
	replaceUnreadable = false
): Promise<PolicySave> {
	const response = await fetch('/api/magician/v2/preference-alignment/policy', {
		method: 'PUT',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
		body: JSON.stringify({
			expected_revision: expectedRevision,
			global,
			intentions,
			replace_unreadable: replaceUnreadable,
			surface: 'web'
		})
	});
	const body = await response.json().catch(() => null);
	if (response.status === 409 && body?.error === 'stale_policy') {
		const current = parsePolicy(body.current);
		if (current?.status === 'ok') {
			return { status: 'stale', current, message: text(body.message, 'Your policy changed elsewhere.') };
		}
	}
	if (!response.ok) throw new Error(body?.message ?? 'Your check policy could not be saved.');
	const policy = parsePolicy(body);
	if (policy?.status !== 'ok') throw new Error('The server did not confirm your check policy.');
	return { status: 'saved', policy };
}

/**
 * A nudge that let an act through, with any offer to remember an inferred
 * preference. Read with `surface=web` (a private surface); a shared surface
 * would get only the open count.
 */
export interface PreferenceNotice {
	id: string;
	createdAtMs: number;
	actSummary: string;
	explanation: string;
	intentions: Array<{ id: string; statement: string }>;
	remember: { statement: string; wording: string; token: string } | null;
	state: 'open' | 'remembered' | 'dismissed' | 'stale';
}

export function parseNotices(raw: unknown): PreferenceNotice[] {
	const body = record(raw);
	if (!body || !Array.isArray(body.notices)) return [];
	return body.notices.flatMap((entry) => {
		const notice = record(entry);
		const id = text(notice?.id);
		if (!notice || !id) return [];
		const remember = record(notice.remember);
		const state = text(notice.state, 'open');
		return [
			{
				id,
				createdAtMs: typeof notice.created_at_ms === 'number' ? notice.created_at_ms : 0,
				actSummary: text(notice.act_summary),
				explanation: text(notice.explanation),
				intentions: (Array.isArray(notice.intentions) ? notice.intentions : []).flatMap((value) => {
					const intention = record(value);
					return intention && text(intention.id) && text(intention.statement)
						? [{ id: text(intention.id), statement: text(intention.statement) }]
						: [];
				}),
				remember:
					remember && text(remember.token) && text(remember.wording)
						? {
								statement: text(remember.statement),
								wording: text(remember.wording),
								token: text(remember.token)
							}
						: null,
				state: (['open', 'remembered', 'dismissed', 'stale'].includes(state)
					? state
					: 'open') as PreferenceNotice['state']
			}
		];
	});
}

export async function fetchPreferenceNotices(): Promise<PreferenceNotice[]> {
	const response = await fetch('/api/magician/v2/preference-alignment/notices?surface=web', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(body?.message ?? 'Notes from recent checks are unavailable.');
	return parseNotices(body);
}

/**
 * Answer one notice. Remembering carries the token the private read showed,
 * which binds the answer to the exact wording on screen.
 */
export async function answerPreferenceNotice(
	id: string,
	answer: 'remember' | 'dismiss',
	token?: string
): Promise<PreferenceNotice['state']> {
	const response = await fetch(
		`/api/magician/v2/preference-alignment/notices/${encodeURIComponent(id)}/answer`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
			body: JSON.stringify(answer === 'remember' ? { answer, token } : { answer })
		}
	);
	const body = await response.json().catch(() => null);
	if (response.status === 409 && body?.error === 'already_resolved') {
		return parseNotices({ notices: [body.notice] })[0]?.state ?? 'dismissed';
	}
	if (!response.ok) throw new Error(body?.message ?? 'The note could not be answered.');
	return parseNotices({ notices: [body] })[0]?.state ?? 'dismissed';
}
