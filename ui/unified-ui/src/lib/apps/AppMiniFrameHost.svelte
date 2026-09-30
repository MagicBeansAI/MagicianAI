<script lang="ts">
	/**
	 * Widget-sized sandboxed frame (gate S4).
	 *
	 * The component renders nothing until three independent things hold: the
	 * page lease admits it under the page/session/visible budgets, the host
	 * mints a plan for this exact installation/widget/generation, and the plan
	 * survives the fail-closed parse. Until then — and after any refusal — the
	 * parent keeps rendering the widget's native model, so a missing frame is
	 * never a missing widget.
	 *
	 * Visibility drives the unmount TTL. A frame scrolled out of view is torn
	 * down once the TTL passes and is not re-admitted: the page budget was
	 * already spent, and re-mounting on every scroll reversal would turn one
	 * reviewed escalation into an unbounded series of them.
	 *
	 * The admission is tracked together with the lease that granted it, and is
	 * re-taken whenever that lease is replaced. A parent may close a lease under
	 * a mounted frame — `AppSlotRegion` does exactly that on a scope or binding
	 * change — and releasing or re-entering against whichever lease happens to
	 * be the current prop would leave the iframe running with no slot behind it.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import {
		fetchMiniFrameHostPlan,
		miniFrameKey,
		MINI_FRAME_SANDBOX,
		MINI_FRAME_UNMOUNT_TTL_MS,
		type AppMiniFrameHostPlan,
		type MiniFramePageLease,
		type AppMiniFrameTarget
	} from './appMiniFrame';
	import { surfaceFrameSourceIsAdmitted } from './appScriptedSurface';

	export let target: AppMiniFrameTarget;
	export let lease: MiniFramePageLease;
	/** Changes whenever the rendered authority changes; forces a fresh mint. */
	export let authorityKey: string;

	const dispatch = createEventDispatcher<{ mounted: void; refused: void }>();

	let plan: AppMiniFrameHostPlan | null = null;
	let request: AbortController | null = null;
	let sweepTimer: ReturnType<typeof setInterval> | undefined;
	let observer: IntersectionObserver | null = null;
	let mountedAuthority = '';
	let mountedLease: MiniFramePageLease | null = null;
	let admittedKey = '';
	/**
	 * The lease that granted `admittedKey`, which is not always the `lease`
	 * prop: a parent that swaps leases hands this component a budget the frame
	 * was never admitted against. Every release, visibility report and sweep
	 * goes to the grantor, so a swap cannot spend one lease and credit another.
	 */
	let admittedLease: MiniFramePageLease | null = null;
	let retired = false;

	$: key = miniFrameKey(target);
	// A new lease is a new budget. The frame is re-admitted against it rather
	// than left mounted on a lease its page has already closed — that frame
	// would be invisible to the document-wide visible limit.
	$: if (authorityKey !== mountedAuthority || lease !== mountedLease) {
		mountedAuthority = authorityKey;
		mountedLease = lease;
		void reset();
	}

	async function reset(): Promise<void> {
		teardown();
		retired = false;
		const now = Date.now();
		const grantor = lease;
		const verdict = grantor.admit(key, now);
		if (!verdict.admitted) {
			dispatch('refused');
			return;
		}
		admittedKey = key;
		admittedLease = grantor;
		const expectedAuthority = authorityKey;
		const controller = new AbortController();
		request = controller;
		try {
			const minted = await fetchMiniFrameHostPlan(target, controller.signal);
			if (controller.signal.aborted || expectedAuthority !== authorityKey) return;
			// The parse already bound the plan to this target; the source check is
			// the same one the full-page scripted host applies, and it refuses a
			// Tauri or dev-server origin that would put the frame on the host page's
			// own origin.
			if (!surfaceFrameSourceIsAdmitted(minted.entry_url)) {
				releaseAdmission();
				dispatch('refused');
				return;
			}
			plan = minted;
			dispatch('mounted');
		} catch {
			// Every refusal — no such host, a stale binding, a malformed plan —
			// lands here, and the parent's native model stands. The admission is
			// returned so a page that mints no frames does not silently spend its
			// budget on failed attempts.
			if (!controller.signal.aborted) {
				releaseAdmission();
				dispatch('refused');
			}
		} finally {
			if (request === controller) request = null;
		}
	}

	function releaseAdmission(): void {
		plan = null;
		if (admittedKey && admittedLease !== null) admittedLease.release(admittedKey);
		admittedKey = '';
		admittedLease = null;
	}

	function teardown(): void {
		request?.abort();
		request = null;
		observer?.disconnect();
		observer = null;
		releaseAdmission();
	}

	function observeVisibility(node: HTMLIFrameElement): { destroy(): void } {
		if (typeof IntersectionObserver === 'undefined') {
			// Without an observer the TTL cannot be measured honestly, so the
			// frame counts as visible for its page's lifetime rather than being
			// swept on a clock that means nothing.
			admittedLease?.noteVisibility(admittedKey, true, Date.now());
			return { destroy: () => undefined };
		}
		observer?.disconnect();
		observer = new IntersectionObserver((entries) => {
			const visible = entries.some((entry) => entry.isIntersecting);
			if (admittedKey && admittedLease !== null) {
				admittedLease.noteVisibility(admittedKey, visible, Date.now());
			}
		});
		observer.observe(node);
		return {
			destroy: () => {
				observer?.disconnect();
				observer = null;
			}
		};
	}

	function sweep(): void {
		if (!admittedKey || admittedLease === null) return;
		// Running the sweep here also retires frames other regions admitted — the
		// TTL belongs to the document. This frame's own verdict is then read from
		// the grantor rather than from the keys this call happened to reclaim: an
		// admission is equally lost to another region's sweep or to the page lease
		// closing under a still-mounted iframe, and in both cases the ledger has
		// stopped counting a renderer that is still on screen.
		admittedLease.sweepExpired(Date.now());
		if (admittedLease.holds(admittedKey)) return;
		// The ledger already released the slot; drop the renderer and stay
		// retired for the life of this authority.
		admittedKey = '';
		admittedLease = null;
		plan = null;
		retired = true;
		dispatch('refused');
	}

	onMount(() => {
		// One quarter of the TTL keeps the worst-case overshoot bounded without
		// waking the page on a fast interval.
		sweepTimer = setInterval(sweep, Math.max(1_000, Math.floor(MINI_FRAME_UNMOUNT_TTL_MS / 4)));
	});

	onDestroy(() => {
		if (sweepTimer) clearInterval(sweepTimer);
		teardown();
	});
</script>

{#if plan && !retired}
	<iframe
		class="app-mini-frame"
		title={`App widget frame ${plan.widget_id}`}
		sandbox={MINI_FRAME_SANDBOX}
		referrerpolicy="no-referrer"
		loading="lazy"
		src={plan.entry_url}
		style={`height:${plan.max_height_px}px`}
		use:observeVisibility
	></iframe>
{/if}

<style>
	.app-mini-frame {
		display: block;
		width: 100%;
		max-width: 100%;
		border: 0;
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-soft, var(--bg-card, #fff));
	}
</style>
