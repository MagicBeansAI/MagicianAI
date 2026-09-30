/**
 * Widget-sized mini-frame host (gate S4, web half).
 *
 * A mini-frame is the *reviewed escalation* for bespoke rendering, never a
 * default. Three separate host acts must all succeed before one pixel of app
 * HTML runs inside a page:
 *
 *  1. the widget render batch carries a `mini_frame` declaration beside a
 *     complete native model (the exact same-view fallback the runtime already
 *     compiles), so a refused frame degrades to real content and never to a
 *     hole;
 *  2. the host mints a plan for that exact installation/widget/generation —
 *     sandbox, deny-egress CSP, session, nonce, digest-keyed entry URL — the
 *     way `custom-surface-v1` mints one for a full-page scripted surface; and
 *  3. this client's page and session budgets, and the document-wide visible
 *     limit, still have room.
 *
 * Any one of those failing leaves the native model rendered. That is the whole
 * point of the escalation shape: the frame is additive authority on top of a
 * surface that already works without it.
 *
 * None of the three happens on the shipped host, and this module says so rather
 * than reading as though they do: the widget compiler (`compile_manifest_widget`)
 * strips `mini_frame_v1` out of the compiled capability set and copies no entry
 * point, so no render item carries a `mini_frame` member, and no `mini-frame-v1`
 * route exists to mint a plan against. Gate S4's host half is unbuilt; this is
 * the client half alone — inert today, pinned inert in `appMiniFrame.test.ts`,
 * and landed ahead of the host on purpose, so that the escalation cannot later
 * arrive *without* the budgets, the TTL and the fail-closed parse it has to come
 * through here.
 *
 * Budgets live here rather than in the component because they are the property
 * of the *document*, not of any one widget: N live renderer sessions on the
 * busiest page is exactly the cost the declarative-native default was chosen
 * to avoid.
 */

import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { AppSurfaceClientError } from './appSurfaceRuntime';
import { surfaceFrameSourceIsAdmitted } from './appScriptedSurface';
import {
	APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
	type AppWidgetMiniFrameDeclaration,
	type AppWidgetRenderItem
} from './appWidgets';

/**
 * The kernel sandbox constant, identical to the full-page scripted host:
 * `allow-scripts` only. `allow-same-origin` would give the frame the host
 * page's origin — its credentials, its storage, and on desktop its Tauri IPC —
 * so it is not a tunable, and a plan that names anything else is refused.
 */
export const MINI_FRAME_SANDBOX = 'allow-scripts';

/** At most two mini-frames may be admitted for one page's widget regions. */
export const MINI_FRAME_MAX_PER_PAGE = 2;
/** …and at most twelve across the whole browsing session. */
export const MINI_FRAME_MAX_PER_SESSION = 12;
/** …of which at most two may be mounted at the same time, anywhere. */
export const MINI_FRAME_MAX_VISIBLE = 2;
/**
 * A frame that has been continuously out of view for this long is unmounted.
 * Scrolling past a widget must not leave a renderer session running behind the
 * fold for the life of the page.
 */
export const MINI_FRAME_UNMOUNT_TTL_MS = 30_000;

export interface AppMiniFrameHostPlan {
	schema_version: 1;
	sandbox: string;
	csp: string;
	session_ref: string;
	nonce: string;
	installation_id: string;
	installation_generation: number;
	package_revision_ref: string;
	widget_id: string;
	entry_point: string;
	entry_document: string;
	entry_document_digest: string;
	entry_url: string;
	max_height_px: number;
}

/** The exact target a plan may be minted for, taken from the rendered widget. */
export interface AppMiniFrameTarget {
	installation_id: string;
	installation_generation: number;
	widget_id: string;
	declaration: AppWidgetMiniFrameDeclaration;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function string(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function positiveInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function isAppReference(value: string): boolean {
	return value.length > 0 && value.length <= 192 && /^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(value);
}

/**
 * The single route family a mini-frame may come from. Both the plan mint and
 * the digest-keyed entry URL are checked against it, so whoever builds the host
 * half has to mount exactly this family: a mint served anywhere else leaves the
 * client inert with no error to notice, and an entry document served anywhere
 * else is refused below — the full-page `custom-surface-v1` family included,
 * whose sessions carry a bridge a widget-sized frame is never granted.
 */
export function miniFrameRoutePrefix(installationId: string): string {
	return `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/mini-frame-v1`;
}

/**
 * The frame target a rendered widget declares, or `null`.
 *
 * A declaration is admitted only beside a complete native model: `state:
 * 'ready'` is the render contract's way of saying the closed read compiled and
 * produced rows, and the mini-frame escalation may only ride on top of that.
 * An `unsupported` or `unavailable` widget that claimed a frame would be
 * asking the client to run app code in place of content the host could not
 * produce — the exact inversion this gate exists to refuse.
 *
 * On the shipped host this returns `null` for every item, because no compiled
 * item carries the member at all. The escalation begins the day the compiler
 * emits one, and not before.
 */
export function miniFrameTargetFromRenderItem(item: AppWidgetRenderItem): AppMiniFrameTarget | null {
	if (item.state !== 'ready' || item.mini_frame === undefined) return null;
	if (item.installation_generation === null || item.installation_generation <= 0) return null;
	return {
		installation_id: item.installation_id,
		installation_generation: item.installation_generation,
		widget_id: item.widget_id,
		declaration: item.mini_frame
	};
}

/**
 * Fail-closed parse of a host-minted mini-frame plan.
 *
 * Every field is checked against the target the client asked for, not merely
 * against a shape: a plan for another installation, another widget, or another
 * installation generation is a stale or substituted binding, and mounting it
 * would give one app's declaration another app's frame.
 */
export function parseMiniFrameHostPlan(
	value: unknown,
	target: AppMiniFrameTarget
): AppMiniFrameHostPlan {
	const refuse = (): never => {
		throw new AppSurfaceClientError('The mini-frame host was refused.', 500, 'invalid_mini_frame');
	};
	if (!isRecord(value)) refuse();
	const plan = value as Record<string, unknown>;
	const sandbox = string(plan.sandbox);
	const csp = string(plan.csp);
	const sessionRef = string(plan.session_ref);
	const nonce = string(plan.nonce);
	const installationId = string(plan.installation_id);
	const packageRevisionRef = string(plan.package_revision_ref);
	const widgetId = string(plan.widget_id);
	const entryPoint = string(plan.entry_point);
	const entryDocument = string(plan.entry_document);
	const entryDigest = string(plan.entry_document_digest);
	const entryUrl = string(plan.entry_url);
	const installationGeneration = positiveInteger(plan.installation_generation);
	const maxHeight = positiveInteger(plan.max_height_px);
	const assetPrefix = `${miniFrameRoutePrefix(target.installation_id)}/assets/`;
	const expectedEntryUrl = `${assetPrefix}${sessionRef}/${entryDigest}/${entryDocument}`;
	if (
		plan.schema_version !== 1
		// The sandbox token list is the kernel's, verbatim. `allow-same-origin`
		// is named explicitly so a future widening of the constant cannot let it
		// through by accident.
		|| sandbox !== MINI_FRAME_SANDBOX
		|| sandbox.includes('allow-same-origin')
		|| !csp.includes("default-src 'none'")
		|| !csp.includes("connect-src 'none'")
		|| !csp.includes("script-src 'self'")
		|| !csp.includes('frame-ancestors')
		|| !isAppReference(sessionRef)
		|| nonce.length === 0
		|| installationId !== target.installation_id
		|| installationGeneration !== target.installation_generation
		|| widgetId !== target.widget_id
		|| !isAppReference(packageRevisionRef)
		|| entryPoint !== target.declaration.entry_point
		|| !entryDocument.startsWith('surfaces/')
		|| !entryDocument.endsWith('.html')
		|| !entryDigest.startsWith('blake3:')
		|| entryUrl !== expectedEntryUrl
		|| !surfaceFrameSourceIsAdmitted(entryUrl)
		|| maxHeight > APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX
		|| maxHeight !== target.declaration.max_height_px
	) refuse();
	return {
		schema_version: 1,
		sandbox,
		csp,
		session_ref: sessionRef,
		nonce,
		installation_id: installationId,
		installation_generation: installationGeneration,
		package_revision_ref: packageRevisionRef,
		widget_id: widgetId,
		entry_point: entryPoint,
		entry_document: entryDocument,
		entry_document_digest: entryDigest,
		entry_url: entryUrl,
		max_height_px: maxHeight
	};
}

/**
 * Ask the host to mint a plan for one admitted target.
 *
 * Called only after `miniFrameTargetFromRenderItem` found a declaration, so a
 * deployment whose widgets declare no frames issues no requests at all. Any
 * refusal — including the 404 of a host that mints no mini-frames — throws,
 * and the caller keeps the native model.
 */
export async function fetchMiniFrameHostPlan(
	target: AppMiniFrameTarget,
	signal?: AbortSignal
): Promise<AppMiniFrameHostPlan> {
	const query = new URLSearchParams({ widget: target.widget_id });
	const response = await fetch(
		`${miniFrameRoutePrefix(target.installation_id)}/host?${query}`,
		{ headers: scopedRequestHeaders({ Accept: 'application/json' }), signal }
	);
	if (!response.ok) {
		throw new AppSurfaceClientError(
			'The mini-frame host is unavailable.',
			response.status,
			'app_mini_frame_unavailable'
		);
	}
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		throw new AppSurfaceClientError('The mini-frame host returned an invalid response.', 500, 'invalid_mini_frame');
	}
	return parseMiniFrameHostPlan(body, target);
}

/**
 * Why a frame was not admitted. `retired` is the TTL verdict: the page spent
 * its budget on this frame and let it scroll out of view past the deadline, so
 * the escalation is over for this page even though the slot is free again.
 */
export type MiniFrameRefusal = 'page_budget' | 'session_budget' | 'visible_budget' | 'retired';

export type MiniFrameVerdict =
	| { admitted: true }
	| { admitted: false; reason: MiniFrameRefusal };

/**
 * One page's claim on the session ledger. `AppSlotRegion` opens a lease when it
 * mounts and closes it when it unmounts, so leaving a page returns its visible
 * slots even if a frame's own teardown was skipped.
 *
 * The budget is spent per *frame*, not per mount. The reviewed escalation is
 * the installation/widget/generation triple the key names, so a host that tears
 * one frame down and mints it again — the rendered rows changed, the credential
 * rotated, the first mint failed — re-enters the frame it already paid for and
 * is charged nothing. Charging the re-mint instead would let an ordinary data
 * refresh spend the page and session budgets down to zero and disable the
 * escalation for good, which is a refusal nobody reviewed.
 *
 * Nothing refunds a spend, and a TTL unmount *retires* the frame on this page:
 * the escalation was taken, and a scroll reversal must not reopen it.
 */
export class MiniFramePageLease {
	private readonly held = new Set<string>();

	/** Frames this page has paid for. Re-admitting one of these is free. */
	private readonly spent = new Set<string>();

	/** Frames the TTL swept out from under this page. They do not come back. */
	private readonly retired = new Set<string>();

	private closed = false;

	constructor(private readonly ledger: MiniFrameSessionLedger) {}

	admit(key: string, now: number): MiniFrameVerdict {
		if (this.closed) return { admitted: false, reason: 'page_budget' };
		// A hold this page never gave up can still have been reclaimed under it:
		// the TTL sweep is document-wide, so another region's timer frees this
		// frame's slot without this lease ever hearing about it. Re-entering on the
		// stale hold would mount a renderer the ledger has stopped counting, and a
		// document exceeds MINI_FRAME_MAX_VISIBLE by exactly one frame per page
		// that does it. The slot is gone either way, so the frame is retired.
		if (this.held.has(key)) {
			if (this.ledger.isMounted(key)) return { admitted: true };
			this.held.delete(key);
			this.retired.add(key);
		}
		if (this.retired.has(key)) return { admitted: false, reason: 'retired' };
		// A frame already paid for on this page costs nothing to re-enter; only a
		// frame this page has never admitted can exhaust the budget.
		if (!this.spent.has(key) && this.spent.size >= MINI_FRAME_MAX_PER_PAGE) {
			return { admitted: false, reason: 'page_budget' };
		}
		const verdict = this.ledger.admit(key, now);
		if (!verdict.admitted) return verdict;
		this.spent.add(key);
		this.held.add(key);
		return verdict;
	}

	release(key: string): void {
		if (!this.held.delete(key)) return;
		this.ledger.release(key);
	}

	/**
	 * Whether the admission this lease gave for `key` is still live.
	 *
	 * A mounted frame cannot see the parties that revoke it: its page lease can
	 * be closed under it — `AppSlotRegion` closes and reopens one on every scope
	 * or binding change — and the TTL sweep is document-wide, so another region's
	 * timer can reclaim its slot. In both cases the ledger stops counting a
	 * renderer that is still on screen, and the visible limit no longer bounds
	 * what is running. A holder therefore asks outright rather than inferring
	 * from the keys its own sweep happened to return, and both halves must agree:
	 * whichever side lost the admission, the honest answer is no.
	 */
	holds(key: string): boolean {
		return this.held.has(key) && this.ledger.isMounted(key);
	}

	/** Report one held frame's viewport visibility into the TTL clock. */
	noteVisibility(key: string, visible: boolean, now: number): void {
		if (!this.held.has(key)) return;
		this.ledger.noteVisibility(key, visible, now);
	}

	/**
	 * Sweep the session's out-of-view frames, and report the ones *this page*
	 * lost.
	 *
	 * The TTL is a property of the document, so the sweep clears every expired
	 * frame wherever it is held. What comes back is this page's own losses,
	 * measured against the ledger rather than read out of that global batch:
	 * every frame on a page shares one lease and each host sweeps on its own
	 * timer, so that batch answers for whichever caller reclaimed it and not for
	 * this one. A holder still asks `holds` for its own verdict — a lease closed
	 * under a mounted frame loses the admission with no sweep at all.
	 *
	 * A lost frame keeps its spend and is retired: the hold is forgotten so
	 * closing the page cannot release a slot the ledger already handed to
	 * someone else, and re-admitting it free of charge on the next mint would
	 * turn the escalation the TTL just ended into an unbounded series of them.
	 */
	sweepExpired(now: number): string[] {
		this.ledger.sweepExpired(now);
		const lost = [...this.held].filter((key) => !this.ledger.isMounted(key));
		for (const key of lost) {
			this.held.delete(key);
			this.retired.add(key);
		}
		return lost;
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		for (const key of [...this.held]) this.release(key);
	}
}

/**
 * Browser-session ledger for mini-frames: the session budget, the document-wide
 * visible limit, and the out-of-view TTL.
 *
 * Time is passed in rather than read, so the TTL is testable and so a component
 * cannot accidentally sweep against a different clock than it measured with.
 */
export class MiniFrameSessionLedger {
	/** Distinct frames admitted this session; re-admitting one is free. */
	private readonly sessionSpent = new Set<string>();

	private readonly mounted = new Map<string, { hiddenSince: number | null }>();

	openPage(): MiniFramePageLease {
		return new MiniFramePageLease(this);
	}

	/** Internal: page leases are the only supported entry point. */
	admit(key: string, now: number): MiniFrameVerdict {
		if (this.mounted.has(key)) return { admitted: true };
		if (!this.sessionSpent.has(key) && this.sessionSpent.size >= MINI_FRAME_MAX_PER_SESSION) {
			return { admitted: false, reason: 'session_budget' };
		}
		if (this.mounted.size >= MINI_FRAME_MAX_VISIBLE) {
			return { admitted: false, reason: 'visible_budget' };
		}
		this.sessionSpent.add(key);
		// A frame starts hidden and its TTL clock starts now: one admitted into
		// a region below the fold must expire on its own, not linger unseen
		// until the page is left.
		this.mounted.set(key, { hiddenSince: now });
		return { admitted: true };
	}

	release(key: string): void {
		this.mounted.delete(key);
	}

	isMounted(key: string): boolean {
		return this.mounted.has(key);
	}

	noteVisibility(key: string, visible: boolean, now: number): void {
		const state = this.mounted.get(key);
		if (!state) return;
		if (visible) {
			state.hiddenSince = null;
			return;
		}
		// Only the first hidden report starts the clock; a repeated report must
		// not keep pushing the deadline forward.
		if (state.hiddenSince === null) state.hiddenSince = now;
	}

	/**
	 * Unmount every frame that has been out of view past the TTL, returning the
	 * keys that were released so their hosts can tear the iframe down. A frame
	 * that was never reported visible has been hidden since admission.
	 */
	sweepExpired(now: number): string[] {
		const expired: string[] = [];
		for (const [key, state] of this.mounted) {
			if (state.hiddenSince !== null && now - state.hiddenSince >= MINI_FRAME_UNMOUNT_TTL_MS) {
				expired.push(key);
			}
		}
		for (const key of expired) this.mounted.delete(key);
		return expired;
	}
}

/** The one ledger for this browsing session. */
export const miniFrameSessionLedger = new MiniFrameSessionLedger();

/** Stable ledger key for one rendered widget's frame. */
export function miniFrameKey(target: AppMiniFrameTarget): string {
	return `${target.installation_id}\u0000${target.widget_id}\u0000${target.installation_generation}`;
}
