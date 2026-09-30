import {
	postAttentionImpression,
	ATTENTION_VISIBLE_MS_MAX,
	type AttentionDecisionItem,
	type AttentionImpressionPolicy,
	type AttentionImpressionReceipt,
	type AttentionImpressionRequest,
	type AttentionImpressionResult,
	type AttentionSurface
} from './attentionRouting';
import type { AttentionFeedbackAttribution } from './attentionBandit';

export interface VerifiedAttentionVisibilityOptions {
	decision_item: AttentionDecisionItem | null;
	impression_policy: AttentionImpressionPolicy | null;
	surface: AttentionSurface;
	/** Test seam; production uses the canonical impression endpoint. */
	record?: (request: AttentionImpressionRequest) => Promise<AttentionImpressionResult>;
}

interface DeliveryState {
	eventId: string;
	attempts: number;
	inFlight: boolean;
	completed: boolean;
	payload: AttentionImpressionRequest | null;
	receipt: AttentionImpressionReceipt | null;
	nodes: Set<HTMLElement>;
}

const VISIBILITY_RATIO = 0.5;
const MAX_DELIVERY_ATTEMPTS = 4;
const MAX_CACHED_IDENTITIES = 2_000;
const MAX_TIMER_MS = 2_147_483_647;
const deliveryByIdentity = new Map<string, DeliveryState>();

/**
 * Returns attribution for the exact selected card currently being acted on.
 * Decision-only attribution is still useful before a verified impression;
 * only a durable, server-issued and identity-matching impression id is added.
 */
export function attentionFeedbackAttribution(
	item: AttentionDecisionItem | null | undefined,
	policy: AttentionImpressionPolicy | null | undefined,
	surface: AttentionSurface
): AttentionFeedbackAttribution | null {
	if (!item || item.served_route !== surface) return null;
	const attribution: AttentionFeedbackAttribution = {
		decision_id: item.decision_id,
		candidate_id: item.candidate_id,
		source_revision: item.source_revision
	};
	if (!policy) return attribution;
	const receipt = deliveryByIdentity.get(identityKey(item, policy, surface))?.receipt;
	if (
		receipt?.verified === true &&
		receipt.decision_id === item.decision_id &&
		receipt.candidate_id === item.candidate_id &&
		receipt.source_revision === item.source_revision &&
		receipt.surface === surface
	) {
		attribution.impression_id = receipt.impression_id;
	}
	return attribution;
}

function eventId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	return `attention-impression-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function identityKey(
	item: AttentionDecisionItem,
	policy: AttentionImpressionPolicy,
	surface: AttentionSurface
): string {
	return JSON.stringify([
		item.decision_id,
		item.candidate_id,
		item.source_revision,
		surface,
		policy.visibility_rule_version
	]);
}

function deliveryState(key: string): DeliveryState {
	const existing = deliveryByIdentity.get(key);
	if (existing) return existing;
	if (deliveryByIdentity.size >= MAX_CACHED_IDENTITIES) {
		for (const [cachedKey, cached] of deliveryByIdentity) {
			if (cached.completed && cached.nodes.size === 0) {
				deliveryByIdentity.delete(cachedKey);
				break;
			}
		}
	}
	const created: DeliveryState = {
		eventId: eventId(),
		attempts: 0,
		inFlight: false,
		completed: false,
		payload: null,
		receipt: null,
		nodes: new Set()
	};
	deliveryByIdentity.set(key, created);
	return created;
}

function viewportClass(): string {
	if (typeof window === 'undefined') return 'unknown';
	if (window.innerWidth < 640) return 'compact';
	if (window.innerWidth < 1024) return 'medium';
	return 'wide';
}

function clientVersion(): string {
	const value = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__ : '';
	return value.trim() && [...value].length <= 128 && ![...value].some((char) => /\p{Cc}/u.test(char))
		? value
		: 'development';
}

function setStatus(state: DeliveryState, status: string): void {
	for (const node of state.nodes) node.dataset.attentionImpression = status;
}

function publishReceipt(state: DeliveryState, receipt: AttentionImpressionReceipt): void {
	for (const node of state.nodes) {
		node.dataset.attentionImpression = receipt.verified ? 'verified' : 'recorded_unverified';
		node.dataset.attentionImpressionId = receipt.impression_id;
		node.dataset.attentionImpressionDeduplicated = String(receipt.deduplicated);
		node.dataset.attentionImpressionVisibleMs = String(receipt.accumulated_visible_ms);
		node.dispatchEvent(
			new CustomEvent<AttentionImpressionReceipt>('attentionimpressionreceipt', {
				detail: receipt
			})
		);
	}
}

function clearNodeDebug(node: HTMLElement): void {
	delete node.dataset.attentionImpression;
	delete node.dataset.attentionImpressionId;
	delete node.dataset.attentionImpressionDeduplicated;
	delete node.dataset.attentionImpressionVisibleMs;
	delete node.dataset.attentionDecisionId;
	delete node.dataset.attentionCandidateId;
	delete node.dataset.attentionVisibilityRule;
}

function retryDelay(attempts: number): number {
	return Math.min(8_000, 500 * 2 ** Math.max(0, attempts - 1));
}

async function deliver(
	state: DeliveryState,
	record: (request: AttentionImpressionRequest) => Promise<AttentionImpressionResult>
): Promise<void> {
	if (!state.payload || state.inFlight || state.completed) return;
	state.inFlight = true;
	state.attempts += 1;
	setStatus(state, state.attempts === 1 ? 'sending' : 'retrying');
	const result = await record(state.payload);
	state.inFlight = false;
	if (result.ok) {
		state.completed = true;
		state.receipt = result.receipt;
		publishReceipt(state, result.receipt);
		return;
	}
	if (result.retryable && state.attempts < MAX_DELIVERY_ATTEMPTS) {
		setStatus(state, 'retry_scheduled');
		setTimeout(() => void deliver(state, record), retryDelay(state.attempts));
		return;
	}
	setStatus(state, result.code ? `rejected_${result.code}` : 'delivery_degraded');
}

/**
 * Reusable Svelte action for verified impressions. Merely returning or mounting
 * a card records nothing. A report is created only after one uninterrupted
 * visible dwell and contains identifiers/viewport metadata, never card text.
 */
export function verifiedAttentionVisibility(
	node: HTMLElement,
	initial: VerifiedAttentionVisibilityOptions
): { update: (next: VerifiedAttentionVisibilityOptions) => void; destroy: () => void } {
	if (typeof document === 'undefined') {
		return { update: () => {}, destroy: () => {} };
	}
	let options = initial;
	let observer: IntersectionObserver | null = null;
	let dwellTimer: ReturnType<typeof setTimeout> | null = null;
	let visibleSince: number | null = null;
	let intersecting = false;
	let state: DeliveryState | null = null;
	let key: string | null = null;
	let destroyed = false;

	function clearDwell(): void {
		if (dwellTimer !== null) clearTimeout(dwellTimer);
		dwellTimer = null;
		visibleSince = null;
		if (state && !state.payload && !state.completed) setStatus(state, 'observing');
	}

	function stopObserver(): void {
		clearDwell();
		intersecting = false;
		observer?.disconnect();
		observer = null;
		if (state) state.nodes.delete(node);
		state = null;
		key = null;
	}

	function thresholdReached(): void {
		const item = options.decision_item;
		const policy = options.impression_policy;
		if (
			destroyed ||
			!item ||
			!policy ||
			!state ||
			visibleSince === null ||
			document.visibilityState === 'hidden'
		) {
			clearDwell();
			return;
		}
		const elapsed = Date.now() - visibleSince;
		if (elapsed < policy.min_visible_ms) {
			dwellTimer = setTimeout(
				thresholdReached,
				Math.min(MAX_TIMER_MS, policy.min_visible_ms - elapsed)
			);
			return;
		}
		const visibleMs = Math.min(
			ATTENTION_VISIBLE_MS_MAX,
			Math.max(policy.min_visible_ms, elapsed)
		);
		clearDwell();
		intersecting = false;
		observer?.disconnect();
		observer = null;
		if (!state.payload) {
			state.payload = {
				event_id: state.eventId,
				decision_id: item.decision_id,
				candidate_id: item.candidate_id,
				source_revision: item.source_revision,
				surface: options.surface,
				visible_ms: visibleMs,
				visibility_rule_version: policy.visibility_rule_version,
				client_type: 'web',
				client_version: clientVersion(),
				viewport_class: viewportClass()
			};
		}
		void deliver(state, options.record ?? postAttentionImpression);
	}

	function beginDwell(): void {
		if (
			dwellTimer !== null ||
			visibleSince !== null ||
			!options.impression_policy ||
			!state ||
			state.payload ||
			state.completed
		) return;
		visibleSince = Date.now();
		setStatus(state!, 'timing');
		dwellTimer = setTimeout(
			thresholdReached,
			Math.min(MAX_TIMER_MS, options.impression_policy.min_visible_ms)
		);
	}

	function onIntersection(entries: IntersectionObserverEntry[]): void {
		const entry = entries[entries.length - 1];
		intersecting =
			entry?.isIntersecting === true &&
			entry.intersectionRatio >= VISIBILITY_RATIO;
		if (intersecting && document.visibilityState !== 'hidden') beginDwell();
		else clearDwell();
	}

	function configure(next: VerifiedAttentionVisibilityOptions): void {
		stopObserver();
		clearNodeDebug(node);
		options = next;
		const item = options.decision_item;
		const policy = options.impression_policy;
		if (
			destroyed ||
			!item ||
			!policy ||
			!item.selected ||
			item.served_route !== options.surface ||
			typeof IntersectionObserver === 'undefined'
		) {
			return;
		}
		key = identityKey(item, policy, options.surface);
		state = deliveryState(key);
		state.nodes.add(node);
		node.dataset.attentionDecisionId = item.decision_id;
		node.dataset.attentionCandidateId = item.candidate_id;
		node.dataset.attentionVisibilityRule = policy.visibility_rule_version;
		if (state.receipt) {
			publishReceipt(state, state.receipt);
			return;
		}
		if (state.payload) {
			setStatus(state, state.inFlight ? 'sending' : 'retry_scheduled');
			return;
		}
		setStatus(state, 'observing');
		observer = new IntersectionObserver(onIntersection, { threshold: VISIBILITY_RATIO });
		observer.observe(node);
	}

	function onDocumentVisibility(): void {
		if (document.visibilityState === 'hidden') clearDwell();
		else if (intersecting) beginDwell();
	}

	document.addEventListener('visibilitychange', onDocumentVisibility);
	configure(initial);

	return {
		update(next) {
			const nextItem = next.decision_item;
			const nextPolicy = next.impression_policy;
			const nextKey = nextItem && nextPolicy
				? identityKey(nextItem, nextPolicy, next.surface)
				: null;
			if (nextKey !== key) configure(next);
			else options = next;
		},
		destroy() {
			destroyed = true;
			stopObserver();
			document.removeEventListener('visibilitychange', onDocumentVisibility);
		}
	};
}

/** Test-only reset for stable module-level idempotency state. */
export function resetVerifiedAttentionVisibilityForTests(): void {
	deliveryByIdentity.clear();
}
