import { writable } from 'svelte/store';

import {
	fetchAttentionDeliveryPage,
	type AttentionDeliveredItem,
	type AttentionDeliveryPageResponse,
	type AttentionDeliveryRefreshReason,
	type AttentionDeliveryRootDecision
} from '$lib/attention/attentionDelivery';
import type {
	CanonicalAttentionOriginLane,
	CanonicalAttentionProjection
} from '$lib/attention/canonicalAttentionProjection';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

export interface AttentionDeliveryLaneState {
	projection: CanonicalAttentionProjection | null;
	root: AttentionDeliveryRootDecision | null;
	pages: AttentionDeliveryPageResponse[];
	items: AttentionDeliveredItem[];
	isLoading: boolean;
	hasMore: boolean;
	nextCursor: string | null;
	fallbackReason: string | null;
	refreshRequiredReason: AttentionDeliveryRefreshReason | null;
	scopeKey: string | null;
}

export interface AttentionDeliveryState {
	follow_up: AttentionDeliveryLaneState;
	worth_a_look: AttentionDeliveryLaneState;
}

function emptyLane(scopeKey: string | null = null): AttentionDeliveryLaneState {
	return {
		projection: null,
		root: null,
		pages: [],
		items: [],
		isLoading: false,
		hasMore: false,
		nextCursor: null,
		fallbackReason: null,
		refreshRequiredReason: null,
		scopeKey
	};
}

const initialState: AttentionDeliveryState = {
	follow_up: emptyLane(),
	worth_a_look: emptyLane()
};

function projectionBinding(projection: CanonicalAttentionProjection): string {
	return [
		projection.projection_id,
		projection.universe_digest,
		projection.policy.snapshot_id ?? '',
		projection.policy.model_version ?? '',
		projection.policy.seed_identity
	].join('\u0000');
}

function sameRoot(left: AttentionDeliveryRootDecision, right: AttentionDeliveryRootDecision): boolean {
	return left.decision_id === right.decision_id && left.lane === right.lane &&
		left.projection_id === right.projection_id && left.universe_digest === right.universe_digest &&
		left.policy_snapshot_id === right.policy_snapshot_id &&
		left.policy_model_version === right.policy_model_version &&
		left.posterior_version === right.posterior_version && left.seed_identity === right.seed_identity &&
		left.universe_size === right.universe_size && left.created_at === right.created_at &&
		left.expires_at === right.expires_at;
}

function exactReplay(left: AttentionDeliveryPageResponse, right: AttentionDeliveryPageResponse): boolean {
	return right.health.replay && JSON.stringify(left) === JSON.stringify(right);
}

function createAttentionDeliveryStore() {
	const { subscribe, set, update } = writable<AttentionDeliveryState>(initialState);
	const generations: Record<CanonicalAttentionOriginLane, number> = {
		follow_up: 0,
		worth_a_look: 0
	};
	const expiryTimers: Record<CanonicalAttentionOriginLane, ReturnType<typeof setTimeout> | null> = {
		follow_up: null,
		worth_a_look: null
	};
	let snapshot = initialState;
	subscribe((state) => (snapshot = state));

	function replaceLane(lane: CanonicalAttentionOriginLane, next: AttentionDeliveryLaneState): void {
		update((state) => ({ ...state, [lane]: next }));
	}

	function currentScopeKey(): string {
		const scope = getCurrentScopeIdentity();
		return [scope.principal, scope.workspace].join('\u0000');
	}

	function clearExpiry(lane: CanonicalAttentionOriginLane): void {
		if (expiryTimers[lane] !== null) clearTimeout(expiryTimers[lane]);
		expiryTimers[lane] = null;
	}

	function scheduleExpiry(
		lane: CanonicalAttentionOriginLane,
		scopeKey: string,
		projection: CanonicalAttentionProjection,
		pageSize: number,
		root: AttentionDeliveryRootDecision
	): void {
		clearExpiry(lane);
		const delay = Math.min(2_147_483_647, Math.max(0, root.expires_at - Date.now()));
		expiryTimers[lane] = setTimeout(() => {
			expiryTimers[lane] = null;
			const current = snapshot[lane];
			if (!current.root || !sameRoot(current.root, root) || current.scopeKey !== scopeKey ||
				!current.projection || projectionBinding(current.projection) !== projectionBinding(projection)) return;
			if (root.expires_at > Date.now()) {
				scheduleExpiry(lane, scopeKey, projection, pageSize, root);
				return;
			}
			void refresh(lane, scopeKey, projection, pageSize);
		}, delay);
	}

	async function refresh(
		lane: CanonicalAttentionOriginLane,
		scopeKey: string,
		projection: CanonicalAttentionProjection,
		pageSize: number
	): Promise<void> {
		clearExpiry(lane);
		const scope = getCurrentScopeIdentity();
		if ([scope.principal, scope.workspace].join('\u0000') !== scopeKey) {
			generations[lane] += 1;
			replaceLane(lane, { ...emptyLane(scopeKey), fallbackReason: 'scope_mismatch' });
			return;
		}
		const generation = ++generations[lane];
		const previous = snapshot[lane];
		const keepVisibleRows =
			previous.scopeKey === scopeKey &&
			previous.items.length > 0 &&
			previous.fallbackReason === null;
		replaceLane(lane, {
			...(keepVisibleRows ? previous : emptyLane(scopeKey)),
			projection,
			isLoading: true,
			scopeKey,
			fallbackReason: null,
			refreshRequiredReason: null
		});
		const result = await fetchAttentionDeliveryPage({ lane, scope, projection, pageSize });
		if (generation !== generations[lane]) return;
		if (currentScopeKey() !== scopeKey) {
			clearExpiry(lane);
			replaceLane(lane, { ...emptyLane(), fallbackReason: 'scope_mismatch' });
			return;
		}
		if (result.kind !== 'page' || result.response.page.page_index !== 0 ||
			result.response.page.page_start !== 0 || result.response.page.cursor !== null ||
			result.response.root_decision.expires_at <= Date.now()) {
			replaceLane(lane, {
				...emptyLane(scopeKey),
				fallbackReason: result.kind === 'fallback'
					? result.reason
					: result.kind === 'refresh_required'
						? null
						: 'invalid_first_delivery_page',
				refreshRequiredReason: result.kind === 'refresh_required' ? result.refresh.reason : null
			});
			return;
		}
		const response = result.response;
		replaceLane(lane, {
			projection,
			root: response.root_decision,
			pages: [response],
			items: response.items,
			isLoading: false,
			hasMore: response.page.has_more,
			nextCursor: response.page.next_cursor,
			fallbackReason: null,
			refreshRequiredReason: null,
			scopeKey
		});
		scheduleExpiry(lane, scopeKey, projection, pageSize, response.root_decision);
	}

	async function ensure(
		lane: CanonicalAttentionOriginLane,
		scopeKey: string,
		projection: CanonicalAttentionProjection,
		pageSize: number
	): Promise<void> {
		const state = snapshot[lane];
		if (state.isLoading && state.scopeKey === scopeKey && state.projection &&
			projectionBinding(state.projection) === projectionBinding(projection)) return;
		if (state.root && state.scopeKey === scopeKey && state.projection &&
			projectionBinding(state.projection) === projectionBinding(projection) &&
			state.root.expires_at > Date.now()) return;
		await refresh(lane, scopeKey, projection, pageSize);
	}

	async function loadMore(lane: CanonicalAttentionOriginLane): Promise<void> {
		const before = snapshot[lane];
		if (before.isLoading || !before.root || !before.projection || !before.hasMore ||
			!before.nextCursor || !before.scopeKey) return;
		if (before.root.expires_at <= Date.now() || currentScopeKey() !== before.scopeKey) {
			await refresh(lane, before.scopeKey, before.projection, before.pages[0]?.page.page_size ?? 1);
			return;
		}
		const generation = generations[lane];
		const cursor = before.nextCursor;
		replaceLane(lane, { ...before, isLoading: true });
		const result = await fetchAttentionDeliveryPage({
			lane,
			scope: getCurrentScopeIdentity(),
			projection: before.projection,
			cursor
		});
		if (generation !== generations[lane]) return;
		if (currentScopeKey() !== before.scopeKey || snapshot[lane].scopeKey !== before.scopeKey) {
			clearExpiry(lane);
			replaceLane(lane, { ...emptyLane(), fallbackReason: 'scope_mismatch' });
			return;
		}
		if (result.kind === 'refresh_required') {
			replaceLane(lane, {
				...emptyLane(before.scopeKey),
				refreshRequiredReason: result.refresh.reason
			});
			void refresh(lane, before.scopeKey, before.projection, before.pages[0]?.page.page_size ?? 1);
			return;
		}
		if (result.kind === 'fallback') {
			clearExpiry(lane);
			replaceLane(lane, { ...emptyLane(before.scopeKey), fallbackReason: result.reason });
			return;
		}
		const response = result.response;
		const current = snapshot[lane];
		if (response.root_decision.expires_at <= Date.now() || !current.root || !current.projection ||
			!sameRoot(current.root, response.root_decision)) {
			replaceLane(lane, { ...emptyLane(before.scopeKey), refreshRequiredReason: 'binding_mismatch' });
			void refresh(lane, before.scopeKey, before.projection, before.pages[0]?.page.page_size ?? 1);
			return;
		}
		const replay = current.pages.find((known) => known.page.delivery_id === response.page.delivery_id);
		if (replay) {
			if (response.page.cursor === cursor && exactReplay(replay, response)) {
				replaceLane(lane, { ...current, isLoading: false });
			} else {
				replaceLane(lane, { ...emptyLane(before.scopeKey), refreshRequiredReason: 'binding_mismatch' });
				void refresh(lane, before.scopeKey, before.projection, before.pages[0]?.page.page_size ?? 1);
			}
			return;
		}
		const last = current.pages[current.pages.length - 1];
		const contiguous = response.page.page_index === last.page.page_index + 1 &&
			response.page.page_size === last.page.page_size &&
			response.page.page_start === current.items.length && response.page.cursor === cursor &&
			response.items.every((item, index) => item.position === current.items.length + index + 1) &&
			!response.items.some((item) => current.items.some((known) =>
				known.candidate_id === item.candidate_id || known.exposure_token === item.exposure_token));
		if (!contiguous) {
			replaceLane(lane, { ...emptyLane(before.scopeKey), refreshRequiredReason: 'binding_mismatch' });
			void refresh(lane, before.scopeKey, before.projection, before.pages[0]?.page.page_size ?? 1);
			return;
		}
		const items = [...current.items, ...response.items];
		replaceLane(lane, {
			...current,
			pages: [...current.pages, response],
			items,
			isLoading: false,
			hasMore: response.page.has_more,
			nextCursor: response.page.next_cursor,
			fallbackReason: null,
			refreshRequiredReason: null
		});
	}

	return {
		subscribe,
		ensure,
		refresh,
		loadMore,
		clear(lane?: CanonicalAttentionOriginLane): void {
			if (lane) {
				clearExpiry(lane);
				generations[lane] += 1;
				replaceLane(lane, emptyLane());
				return;
			}
			clearExpiry('follow_up');
			clearExpiry('worth_a_look');
			generations.follow_up += 1;
			generations.worth_a_look += 1;
			set({ follow_up: emptyLane(), worth_a_look: emptyLane() });
		}
	};
}

export const attentionDeliveryStore = createAttentionDeliveryStore();
