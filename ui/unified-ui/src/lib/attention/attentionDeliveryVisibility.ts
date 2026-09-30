import {
	postAttentionDeliveryImpression,
	type AttentionDeliveredItem,
	type AttentionDeliveryImpressionRequest,
	type AttentionDeliveryImpressionReceipt,
	type AttentionDeliveryImpressionResult,
	type AttentionDeliveryPageResponse
} from './attentionDelivery';
import {
	ATTENTION_VISIBLE_MS_MAX,
	type AttentionSurface
} from './attentionRouting';
import type { CanonicalAttentionProjectionScope } from './canonicalAttentionProjection';
import type { AttentionFeedbackAttribution } from './attentionBandit';

export interface VerifiedAttentionDeliveryVisibilityOptions {
	response: AttentionDeliveryPageResponse;
	delivery: AttentionDeliveredItem;
	surface: AttentionSurface;
	scope: CanonicalAttentionProjectionScope;
	record?: (request: AttentionDeliveryImpressionRequest) => Promise<AttentionDeliveryImpressionResult>;
}

interface ExposureState {
	eventId: string;
	payload: AttentionDeliveryImpressionRequest | null;
	record: ((request: AttentionDeliveryImpressionRequest) => Promise<AttentionDeliveryImpressionResult>) | null;
	attempts: number;
	inFlight: boolean;
	completed: boolean;
	receipt: AttentionDeliveryImpressionReceipt | null;
}

const VISIBILITY_RATIO = 0.5;
const MAX_ATTEMPTS = 4;
const exposures = new Map<string, ExposureState>();

function withFrozenScope(
	options: VerifiedAttentionDeliveryVisibilityOptions
): VerifiedAttentionDeliveryVisibilityOptions {
	return {
		...options,
		scope: Object.freeze({
			principal: options.scope.principal,
			workspace: options.scope.workspace
		})
	};
}

function eventId(): string {
	return typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
		? crypto.randomUUID()
		: `attention-delivery-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function clientVersion(): string {
	const version = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__ : '';
	return version.trim() ? version : 'development';
}

function viewportClass(): string {
	if (typeof window === 'undefined') return 'unknown';
	if (window.innerWidth < 640) return 'compact';
	if (window.innerWidth < 1024) return 'medium';
	return 'wide';
}

function validBinding(options: VerifiedAttentionDeliveryVisibilityOptions): boolean {
	const { delivery, response, scope, surface } = options;
	const { root_decision: root, page } = response;
	const bound = response.items.find((item) => item.position === delivery.position);
	if (!bound) return false;
	return scope.principal.trim().length > 0 && scope.workspace.trim().length > 0 &&
		root.lane === surface && delivery.item.served_lane === surface &&
		bound.candidate_id === delivery.candidate_id &&
		bound.source_revision === delivery.source_revision &&
		bound.exposure_token === delivery.exposure_token &&
		bound.root_policy_propensity === delivery.root_policy_propensity &&
		bound.conditional_delivery_propensity === delivery.conditional_delivery_propensity &&
		bound.item.canonical_id === delivery.item.canonical_id &&
		delivery.candidate_id === delivery.item.canonical_id &&
		delivery.source_revision === delivery.item.source_revision &&
		delivery.position >= page.page_start + 1 &&
		delivery.position <= page.page_start + response.items.length &&
		page.expires_at === root.expires_at && root.expires_at > Date.now() &&
		delivery.exposure_token.length > 0;
}

function identity(options: VerifiedAttentionDeliveryVisibilityOptions): string {
	return [
		options.scope.principal,
		options.scope.workspace,
		options.response.root_decision.decision_id,
		options.response.root_decision.projection_id,
		options.response.root_decision.universe_digest,
		options.response.page.delivery_id,
		options.response.page.page_index,
		options.delivery.position,
		options.delivery.candidate_id,
		options.delivery.source_revision ?? '',
		options.delivery.exposure_token,
		options.response.impression_policy.visibility_rule_version
	].join('\u0000');
}

async function deliver(
	state: ExposureState,
	node: HTMLElement
): Promise<void> {
	if (!state.payload || !state.record || state.inFlight || state.completed) return;
	state.inFlight = true;
	state.attempts += 1;
	node.dataset.attentionDeliveryImpression = state.attempts === 1 ? 'sending' : 'retrying';
	const result = await state.record(state.payload);
	state.inFlight = false;
	if (result.ok) {
		state.completed = true;
		state.receipt = result.receipt;
		node.dataset.attentionDeliveryImpression = result.receipt.verified ? 'verified' : 'recorded_unverified';
		node.dataset.attentionImpressionId = result.receipt.impression_id;
		return;
	}
	if (result.retryable && state.attempts < MAX_ATTEMPTS) {
		node.dataset.attentionDeliveryImpression = 'retry_scheduled';
		setTimeout(() => void deliver(state, node), Math.min(8_000, 500 * 2 ** (state.attempts - 1)));
		return;
	}
	node.dataset.attentionDeliveryImpression = result.code ? `rejected_${result.code}` : 'delivery_degraded';
}

/** Delivery-aware counterpart to verifiedAttentionVisibility. Mounting or
 * receiving a root item records nothing: only a server-delivered item with an
 * exact exposure token can start the verified dwell clock. */
export function verifiedAttentionDeliveryVisibility(
	node: HTMLElement,
	initial: VerifiedAttentionDeliveryVisibilityOptions
): { update: (next: VerifiedAttentionDeliveryVisibilityOptions) => void; destroy: () => void } {
	if (typeof document === 'undefined') return { update: () => {}, destroy: () => {} };
	let options = initial;
	let key = '';
	let state: ExposureState | null = null;
	let observer: IntersectionObserver | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let visibleSince: number | null = null;
	let intersecting = false;
	let destroyed = false;

	function clearDwell(): void {
		if (timer !== null) clearTimeout(timer);
		timer = null;
		visibleSince = null;
	}

	function stop(): void {
		clearDwell();
		observer?.disconnect();
		observer = null;
		intersecting = false;
	}

	function threshold(): void {
		if (destroyed || !state || visibleSince === null || document.visibilityState === 'hidden' ||
			!validBinding(options)) {
			clearDwell();
			return;
		}
		const elapsed = Date.now() - visibleSince;
		if (elapsed < options.response.impression_policy.min_visible_ms) {
			timer = setTimeout(threshold, options.response.impression_policy.min_visible_ms - elapsed);
			return;
		}
		const visibleMs = Math.min(
			ATTENTION_VISIBLE_MS_MAX,
			Math.max(elapsed, options.response.impression_policy.min_visible_ms)
		);
		clearDwell();
		observer?.disconnect();
		observer = null;
		if (!state.payload) {
			const frozenScope = Object.freeze({
				principal: options.scope.principal,
				workspace: options.scope.workspace
			});
			const expectedRootPolicyPropensity = options.delivery.root_policy_propensity;
			state.payload = Object.freeze({
				event_id: state.eventId,
				decision_id: options.response.root_decision.decision_id,
				candidate_id: options.delivery.candidate_id,
				source_revision: options.delivery.source_revision,
				surface: options.surface,
				visible_ms: visibleMs,
				visibility_rule_version: options.response.impression_policy.visibility_rule_version,
				client_type: 'web',
				client_version: clientVersion(),
				viewport_class: viewportClass(),
				delivery_id: options.response.page.delivery_id,
				page_index: options.response.page.page_index,
				position: options.delivery.position,
				exposure_token: options.delivery.exposure_token
			});
			state.record = options.record ?? ((request) => postAttentionDeliveryImpression(
				request,
				frozenScope,
				expectedRootPolicyPropensity
			));
		}
		void deliver(state, node);
	}

	function begin(): void {
		if (!state || timer !== null || visibleSince !== null || state.payload || state.completed) return;
		visibleSince = Date.now();
		node.dataset.attentionDeliveryImpression = 'timing';
		timer = setTimeout(threshold, options.response.impression_policy.min_visible_ms);
	}

	function configure(next: VerifiedAttentionDeliveryVisibilityOptions): void {
		stop();
		options = withFrozenScope(next);
		key = validBinding(options) ? identity(options) : '';
		if (!key || typeof IntersectionObserver === 'undefined') {
			state = null;
			return;
		}
		state = exposures.get(key) ?? {
			eventId: eventId(), payload: null, record: null, attempts: 0, inFlight: false,
			completed: false, receipt: null
		};
		exposures.set(key, state);
		node.dataset.attentionDeliveryId = options.response.page.delivery_id;
		node.dataset.attentionDeliveredPosition = String(options.delivery.position);
		node.dataset.attentionExposureToken = options.delivery.exposure_token;
		if (state.completed) {
			node.dataset.attentionDeliveryImpression = 'verified';
			return;
		}
		observer = new IntersectionObserver((entries) => {
			const entry = entries[entries.length - 1];
			intersecting = entry?.isIntersecting === true && entry.intersectionRatio >= VISIBILITY_RATIO;
			if (intersecting && document.visibilityState !== 'hidden') begin();
			else clearDwell();
		}, { threshold: VISIBILITY_RATIO });
		observer.observe(node);
		node.dataset.attentionDeliveryImpression = 'observing';
	}

	function documentVisibility(): void {
		if (document.visibilityState === 'hidden') clearDwell();
		else if (intersecting) begin();
	}

	document.addEventListener('visibilitychange', documentVisibility);
	configure(initial);
	return {
		update(next) {
			const frozenNext = withFrozenScope(next);
			const nextKey = validBinding(frozenNext) ? identity(frozenNext) : '';
			if (nextKey !== key) configure(frozenNext);
			else options = frozenNext;
		},
		destroy() {
			destroyed = true;
			stop();
			document.removeEventListener('visibilitychange', documentVisibility);
		}
	};
}

export function attentionDeliveryFeedbackAttribution(
	response: AttentionDeliveryPageResponse,
	delivery: AttentionDeliveredItem,
	surface: AttentionSurface,
	scope: CanonicalAttentionProjectionScope
): AttentionFeedbackAttribution | null {
	const options = withFrozenScope({ response, delivery, surface, scope });
	if (!validBinding(options)) return null;
	const attribution: AttentionFeedbackAttribution = {
		decision_id: response.root_decision.decision_id,
		candidate_id: delivery.candidate_id,
		source_revision: delivery.source_revision,
		delivery_id: response.page.delivery_id
	};
	const receipt = exposures.get(identity(options))?.receipt;
	if (receipt?.verified && receipt.decision_id === attribution.decision_id &&
		receipt.candidate_id === attribution.candidate_id &&
		receipt.source_revision === attribution.source_revision &&
		receipt.delivery_id === attribution.delivery_id) attribution.impression_id = receipt.impression_id;
	return attribution;
}

export function resetVerifiedAttentionDeliveryVisibilityForTests(): void {
	exposures.clear();
}
