import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	fetchMiniFrameHostPlan,
	miniFrameRoutePrefix,
	miniFrameTargetFromRenderItem,
	MiniFrameSessionLedger,
	MINI_FRAME_MAX_PER_PAGE,
	MINI_FRAME_MAX_PER_SESSION,
	MINI_FRAME_MAX_VISIBLE,
	MINI_FRAME_UNMOUNT_TTL_MS,
	parseMiniFrameHostPlan,
	type AppMiniFrameTarget
} from './appMiniFrame';
import { parseAppWidgetRenderBatch, type AppWidgetRenderItem } from './appWidgets';

const DIGEST = `blake3:${'c'.repeat(64)}`;
const SESSION = 'bridge-mini:meetings:1';
const ENTRY_URL =
	`/api/magician/v2/apps/installations/meetings/mini-frame-v1/assets/${SESSION}/${DIGEST}/surfaces/console.html`;

const TARGET: AppMiniFrameTarget = {
	installation_id: 'meetings',
	installation_generation: 3,
	widget_id: 'active_capture',
	declaration: { entry_point: '/console', max_height_px: 320 }
};

function hostPlan(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		schema_version: 1,
		sandbox: 'allow-scripts',
		csp: "default-src 'none'; connect-src 'none'; script-src 'self'; style-src 'unsafe-inline'; frame-ancestors 'self'",
		session_ref: SESSION,
		nonce: 'nonce-1',
		installation_id: 'meetings',
		installation_generation: 3,
		package_revision_ref: 'package:meetings:1',
		widget_id: 'active_capture',
		entry_point: '/console',
		entry_document: 'surfaces/console.html',
		entry_document_digest: DIGEST,
		entry_url: ENTRY_URL,
		max_height_px: 320,
		...overrides
	};
}

function renderPage(widget: Record<string, unknown>) {
	return {
		schema_version: 1,
		revision: DIGEST,
		etag: DIGEST,
		rendered_at: '2026-09-04T10:00:00Z',
		refresh_after: '2026-09-04T10:01:00Z',
		widgets: [{
			installation_id: 'meetings',
			widget_id: 'active_capture',
			title: 'Live capture',
			installation_generation: 3,
			revision: DIGEST,
			rendered_at: '2026-09-04T10:00:00Z',
			refresh_after: '2026-09-04T10:01:00Z',
			state: 'ready',
			model: {
				model: 'list',
				rows: [{ entity: 'capture_session', record_id: 's-1', record_revision: 1, fields: { status: 'listening' } }],
				hints: { display_field: 'status' },
				actions: []
			},
			...widget
		}]
	};
}

afterEach(() => vi.unstubAllGlobals());

describe('mini-frame declaration transport', () => {
	it('renders native and asks for nothing when no frame is declared', () => {
		// The shipped state of this gate: `compile_manifest_widget` compiles the
		// native same-view fallback and strips `mini_frame_v1`, so every real
		// render item arrives without the member — no target, no mint request.
		const batch = parseAppWidgetRenderBatch(renderPage({}));
		expect(miniFrameTargetFromRenderItem(batch.widgets[0])).toBeNull();
	});

	it('carries a bounded declaration and refuses an off-shape one', () => {
		const batch = parseAppWidgetRenderBatch(renderPage({
			mini_frame: { entry_point: '/console', max_height_px: 320 }
		}));
		expect(miniFrameTargetFromRenderItem(batch.widgets[0])).toEqual(TARGET);
		for (const bad of [
			{ entry_point: 'console', max_height_px: 320 },
			{ entry_point: '/console/../secret', max_height_px: 320 },
			{ entry_point: '/console?x=1', max_height_px: 320 },
			{ entry_point: '/console', max_height_px: 4000 },
			{ entry_point: '/console', max_height_px: 0 },
			{ entry_point: '/console' },
			{ entry_point: '/console', max_height_px: 320, extra: true }
		]) {
			expect(() => parseAppWidgetRenderBatch(renderPage({ mini_frame: bad }))).toThrow();
		}
	});

	it('refuses a frame claimed by a widget that produced no content', () => {
		// A frame may only escalate a working native model. `unsupported` and
		// `unavailable` say the host could not produce one, and running app code
		// in that hole is the inversion this gate exists to refuse.
		for (const state of ['unsupported', 'unavailable']) {
			const item = {
				installation_id: 'meetings',
				widget_id: 'active_capture',
				installation_generation: 3,
				revision: DIGEST,
				rendered_at: '2026-09-04T10:00:00Z',
				refresh_after: '2026-09-04T10:01:00Z',
				state,
				fallback: { kind: 'hide' },
				mini_frame: { entry_point: '/console', max_height_px: 320 }
			} as unknown as AppWidgetRenderItem;
			expect(miniFrameTargetFromRenderItem(item)).toBeNull();
		}
	});
});

describe('mini-frame host plan admission', () => {
	it('accepts only a plan bound to the exact target that asked for it', () => {
		expect(parseMiniFrameHostPlan(hostPlan(), TARGET).entry_url).toBe(ENTRY_URL);
		for (const bad of [
			{ sandbox: 'allow-scripts allow-same-origin' },
			{ sandbox: '' },
			{ csp: "default-src 'self'" },
			{ csp: "default-src 'none'; script-src 'self'; frame-ancestors 'self'" },
			{ installation_id: 'other' },
			{ installation_generation: 4 },
			{ widget_id: 'recent_meetings' },
			{ entry_point: '/other' },
			{ entry_document: 'console.html' },
			{ entry_document: 'surfaces/console.js' },
			{ entry_document_digest: 'sha256:abc' },
			{ entry_url: 'https://evil.example/console.html' },
			{ entry_url: '//evil.example/console.html' },
			{ entry_url: 'tauri://localhost/console.html' },
			{ session_ref: '' },
			{ nonce: '' },
			{ max_height_px: 900 },
			{ max_height_px: 200 },
			{ schema_version: 2 }
		]) {
			expect(() => parseMiniFrameHostPlan(hostPlan(bad), TARGET)).toThrow();
		}
		expect(() => parseMiniFrameHostPlan(null, TARGET)).toThrow();
	});

	it('treats a host that mints no mini-frames as a refusal, not a frame', async () => {
		// The shipped server has no such route. The client must fail closed on
		// the 404 so the widget keeps rendering its native model.
		vi.stubGlobal('fetch', vi.fn(async () => new Response('', { status: 404 })));
		await expect(fetchMiniFrameHostPlan(TARGET)).rejects.toThrow();
	});

	it('mints from exactly one route family, and admits an entry URL from no other', async () => {
		// The host half of this gate is unbuilt, so this is the only executable
		// statement of the route it must mount. A mint answered somewhere else
		// leaves the client inert with nothing to notice, and an entry document
		// served under the full-page `custom-surface-v1` family — whose sessions
		// carry a bridge a widget-sized frame is never granted — is refused.
		const requested: string[] = [];
		vi.stubGlobal('fetch', vi.fn((input: unknown) => {
			requested.push(String(input));
			return Promise.resolve(new Response('', { status: 404 }));
		}));
		await expect(fetchMiniFrameHostPlan(TARGET)).rejects.toThrow();
		expect(miniFrameRoutePrefix('meetings'))
			.toBe('/api/magician/v2/apps/installations/meetings/mini-frame-v1');
		expect(requested).toEqual([`${miniFrameRoutePrefix('meetings')}/host?widget=active_capture`]);
		expect(() => parseMiniFrameHostPlan(
			hostPlan({ entry_url: ENTRY_URL.replace('mini-frame-v1', 'custom-surface-v1') }),
			TARGET
		)).toThrow();
	});
});

describe('mini-frame page and session budgets', () => {
	it('spends at most the page budget, and never refunds it to another frame', () => {
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		expect(lease.admit('a', 0)).toEqual({ admitted: true });
		expect(lease.admit('b', 0)).toEqual({ admitted: true });
		expect(lease.admit('c', 0)).toEqual({ admitted: false, reason: 'page_budget' });
		lease.release('a');
		lease.release('b');
		expect(lease.admit('c', 0)).toEqual({ admitted: false, reason: 'page_budget' });
		expect(MINI_FRAME_MAX_PER_PAGE).toBe(2);
	});

	it('charges one frame once, however often its host re-mints it', () => {
		// The host tears its frame down and mints a fresh plan whenever the
		// rendered authority changes, and a widget's revision is a digest over the
		// rendered rows — so an ordinary data refresh does it. Charging that
		// re-entry would spend both budgets to zero and leave the frame refused
		// for the life of the page and then of the session, which is a refusal
		// nobody reviewed.
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		for (let refresh = 0; refresh < MINI_FRAME_MAX_PER_SESSION * 3; refresh += 1) {
			expect(lease.admit('a', refresh)).toEqual({ admitted: true });
			lease.release('a');
		}
		expect(lease.admit('a', 0)).toEqual({ admitted: true });
		expect(lease.admit('b', 0)).toEqual({ admitted: true });
		expect(lease.admit('c', 0)).toEqual({ admitted: false, reason: 'page_budget' });
		// …and the session ledger was charged for those two frames, not for the
		// re-mints: every remaining distinct frame still fits.
		lease.close();
		for (let index = 0; index < MINI_FRAME_MAX_PER_SESSION - 2; index += 1) {
			const page = ledger.openPage();
			expect(page.admit(`later-${index}`, 0)).toEqual({ admitted: true });
			page.release(`later-${index}`);
		}
		expect(ledger.openPage().admit('overflow', 0))
			.toEqual({ admitted: false, reason: 'session_budget' });
	});

	it('caps how many frames may be mounted at once across every page', () => {
		const ledger = new MiniFrameSessionLedger();
		const first = ledger.openPage();
		const second = ledger.openPage();
		expect(first.admit('a', 0).admitted).toBe(true);
		expect(first.admit('b', 0).admitted).toBe(true);
		expect(second.admit('c', 0)).toEqual({ admitted: false, reason: 'visible_budget' });
		first.release('a');
		expect(second.admit('c', 0)).toEqual({ admitted: true });
	});

	it('stops admitting once the session budget is spent', () => {
		const ledger = new MiniFrameSessionLedger();
		for (let page = 0; page < MINI_FRAME_MAX_PER_SESSION / MINI_FRAME_MAX_PER_PAGE; page += 1) {
			const lease = ledger.openPage();
			for (let index = 0; index < MINI_FRAME_MAX_PER_PAGE; index += 1) {
				const key = `p${page}-${index}`;
				expect(lease.admit(key, 0)).toEqual({ admitted: true });
				lease.release(key);
			}
		}
		expect(ledger.openPage().admit('overflow', 0))
			.toEqual({ admitted: false, reason: 'session_budget' });
	});

	it('unmounts a frame that stayed out of view past the TTL', () => {
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		lease.admit('a', 1_000);
		lease.noteVisibility('a', true, 1_500);
		expect(lease.sweepExpired(1_500 + MINI_FRAME_UNMOUNT_TTL_MS * 4)).toEqual([]);
		lease.noteVisibility('a', false, 2_000);
		// A repeated hidden report must not keep pushing the deadline out.
		lease.noteVisibility('a', false, 2_500);
		expect(lease.sweepExpired(2_000 + MINI_FRAME_UNMOUNT_TTL_MS - 1)).toEqual([]);
		expect(lease.sweepExpired(2_000 + MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['a']);
		expect(ledger.isMounted('a')).toBe(false);
	});

	it('expires a frame that was never seen, counting from admission', () => {
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		lease.admit('a', 0);
		expect(lease.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['a']);
	});

	it('retires a frame the TTL swept out from under its page', () => {
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		expect(lease.admit('a', 0)).toEqual({ admitted: true });
		expect(lease.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['a']);
		// The spend stands, so re-entry is free — which is exactly why the TTL
		// verdict has to be recorded: the next data refresh re-mints, and a frame
		// the reader scrolled away from must not come back for free.
		expect(lease.admit('a', MINI_FRAME_UNMOUNT_TTL_MS))
			.toEqual({ admitted: false, reason: 'retired' });
		// Retirement is the page's, not the session's: navigating opens a new
		// lease that may spend its own budget on the same frame.
		expect(ledger.openPage().admit('a', MINI_FRAME_UNMOUNT_TTL_MS))
			.toEqual({ admitted: true });
	});

	it('reports the frames this page lost even when another lease swept first', () => {
		// Every frame on a page shares one lease and each host sweeps on its own
		// timer. A host that read the ledger's global expiry batch would miss its
		// own frame whenever a sibling's timer got there first, and would leave a
		// renderer session running in an iframe the ledger has already reclaimed.
		const ledger = new MiniFrameSessionLedger();
		const first = ledger.openPage();
		const second = ledger.openPage();
		expect(first.admit('a', 0)).toEqual({ admitted: true });
		expect(second.admit('b', 0)).toEqual({ admitted: true });
		expect(second.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['b']);
		expect(first.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['a']);
		expect(ledger.isMounted('a')).toBe(false);
	});

	it('returns every visible slot when the page closes', () => {
		const ledger = new MiniFrameSessionLedger();
		const first = ledger.openPage();
		first.admit('a', 0);
		first.admit('b', 0);
		first.close();
		expect(ledger.isMounted('a')).toBe(false);
		const second = ledger.openPage();
		expect(second.admit('c', 0)).toEqual({ admitted: true });
	});
});

describe('mini-frame admissions revoked under a mounted frame', () => {
	it('tells a holder its admission is gone once the page lease closes', () => {
		// `AppSlotRegion` closes its lease on unmount and on every scope or
		// binding change. A frame that kept answering to the closed lease would
		// be a renderer the ledger has stopped counting.
		const ledger = new MiniFrameSessionLedger();
		const lease = ledger.openPage();
		lease.admit('a', 0);
		expect(lease.holds('a')).toBe(true);
		lease.close();
		expect(lease.holds('a')).toBe(false);
	});

	it('tells a holder its slot was reclaimed by another region\'s sweep', () => {
		// The TTL is document-wide, so the lease whose timer fires is not
		// necessarily the lease that admitted the frame. The page that lost the
		// slot never sees it in its own sweep's return value.
		const ledger = new MiniFrameSessionLedger();
		const first = ledger.openPage();
		const second = ledger.openPage();
		expect(first.admit('a', 0)).toEqual({ admitted: true });
		expect(second.admit('b', 0)).toEqual({ admitted: true });
		// The sweep clears every expired frame but reports only the losses of the
		// page that ran it, so `first` hears nothing from `second`'s timer.
		expect(second.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS)).toEqual(['b']);
		expect(ledger.isMounted('a')).toBe(false);
		expect(first.holds('a')).toBe(false);
	});

	it('refuses to re-enter a frame off a hold the ledger no longer backs', () => {
		// Without this the stale hold is a free admission: the frame remounts
		// with no ledger entry behind it, and every page holding one exceeds
		// MINI_FRAME_MAX_VISIBLE by exactly that frame.
		const ledger = new MiniFrameSessionLedger();
		const first = ledger.openPage();
		const second = ledger.openPage();
		first.admit('a', 0);
		second.sweepExpired(MINI_FRAME_UNMOUNT_TTL_MS);
		expect(first.admit('a', MINI_FRAME_UNMOUNT_TTL_MS))
			.toEqual({ admitted: false, reason: 'retired' });
		expect(ledger.isMounted('a')).toBe(false);
	});

	it('re-admits against the lease that replaced the one closed under it', () => {
		// The replacement is a fresh budget, and the frame has to be taken from
		// it: three live renderers on one document is what a frame left on the
		// closed lease would cost.
		const ledger = new MiniFrameSessionLedger();
		const closed = ledger.openPage();
		expect(closed.admit('a', 0)).toEqual({ admitted: true });
		closed.close();
		expect(closed.holds('a')).toBe(false);
		const reopened = ledger.openPage();
		expect(reopened.admit('a', 0)).toEqual({ admitted: true });
		expect(reopened.admit('b', 0)).toEqual({ admitted: true });
		expect(reopened.admit('c', 0)).toEqual({ admitted: false, reason: 'page_budget' });
		expect(ledger.isMounted('a')).toBe(true);
		expect(MINI_FRAME_MAX_VISIBLE).toBe(2);
	});
});
