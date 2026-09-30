import { browser } from '$app/environment';
import { writable, get } from 'svelte/store';
import { appendCurrentScopeQuery } from '$lib/stores/scopeIdentityStore';

/** The effective mode sent with a message. Unchanged wire vocabulary. */
export type PlanComposerMode = 'ask' | 'accept_in_scope' | 'plan';

/**
 * The permission axis of Do mode: prompt before each in-scope file edit, or
 * perform them without a prompt.
 */
export type ComposerPermission = 'ask' | 'accept_in_scope';

export interface PlanModeState {
	/** What a send will carry. `plan` wins over the permission axis. */
	mode: PlanComposerMode;
	/** The remembered permission, restored when Plan is switched back off. */
	permission: ComposerPermission;
	/** False until the backend has answered, so the UI can avoid claiming
	 *  a posture it has not confirmed. */
	permissionLoaded: boolean;
}

const PLANNING_STORAGE_KEY = 'chat_planning';
const LEGACY_MODE_STORAGE_KEY = 'chat_plan_mode';
const PREFERENCES_URL = '/api/magician/v2/ui/preferences';

/**
 * Planning is a harmless view preference, so it stays local.
 *
 * The permission axis deliberately does NOT, and is never cached here. It is
 * stored per principal+workspace on the backend, and a locally cached
 * `accept_in_scope` would survive a workspace switch and show a relaxed gate
 * for a scope that never enabled it. Painting `ask` until the backend answers
 * is the direction that cannot be wrong.
 */
function loadInitialPlanning(): boolean {
	if (!browser) return false;
	try {
		const stored = localStorage.getItem(PLANNING_STORAGE_KEY);
		if (stored === 'true') return true;
		if (stored === 'false') return false;
		// One-time carry-over from when a single key held the tri-state.
		return localStorage.getItem(LEGACY_MODE_STORAGE_KEY) === 'plan';
	} catch {
		return false;
	}
}

function persistPlanning(planning: boolean): void {
	if (!browser) return;
	try {
		localStorage.setItem(PLANNING_STORAGE_KEY, planning ? 'true' : 'false');
		// The legacy key could still carry `accept_in_scope`; drop it so a
		// permission never lingers in local storage.
		localStorage.removeItem(LEGACY_MODE_STORAGE_KEY);
	} catch {
		// ignore local storage failures
	}
}

function effectiveMode(planning: boolean, permission: ComposerPermission): PlanComposerMode {
	return planning ? 'plan' : permission;
}

function normalizePermission(value: unknown): ComposerPermission {
	return value === 'accept_in_scope' ? 'accept_in_scope' : 'ask';
}

function createPlanModeStore() {
	const planning = loadInitialPlanning();
	const initial: PlanModeState = {
		mode: effectiveMode(planning, 'ask'),
		permission: 'ask',
		permissionLoaded: false
	};
	const store = writable<PlanModeState>(initial);
	const { subscribe, set, update } = store;

	let isPlanning = planning;
	let hydrating: Promise<void> | null = null;

	function apply(nextPlanning: boolean, nextPermission: ComposerPermission, loaded: boolean) {
		isPlanning = nextPlanning;
		set({
			mode: effectiveMode(nextPlanning, nextPermission),
			permission: nextPermission,
			permissionLoaded: loaded
		});
	}

	async function savePermission(permission: ComposerPermission): Promise<void> {
		if (!browser) return;
		if ((window as any).__MAGICIAN_MISSING__) return;
		try {
			const response = await fetch(PREFERENCES_URL, {
				method: 'PUT',
				headers: { 'content-type': 'application/json' },
				// Only this field: a partial body leaves the scope's theme alone,
				// exactly as a theme write leaves this permission alone.
				body: JSON.stringify({ composer_permission_mode: permission })
			});
			if (!response.ok) throw new Error(`ui preferences save ${response.status}`);
			const preferences = await response.json();
			apply(isPlanning, normalizePermission(preferences?.composer_permission_mode), true);
		} catch {
			// Keep the optimistic local value; the next hydrate reconciles it.
		}
	}

	return {
		subscribe,

		/**
		 * Ask the backend which permission posture this scope carries.
		 * Safe to call repeatedly; concurrent calls share one request.
		 */
		hydrate(): Promise<void> {
			if (!browser) return Promise.resolve();
			if ((window as any).__MAGICIAN_MISSING__) return Promise.resolve();
			if (hydrating) return hydrating;
			hydrating = (async () => {
				try {
					const params = appendCurrentScopeQuery();
					const response = await fetch(`${PREFERENCES_URL}?${params.toString()}`);
					if (!response.ok) throw new Error(`ui preferences fetch ${response.status}`);
					const preferences = await response.json();
					apply(isPlanning, normalizePermission(preferences?.composer_permission_mode), true);
				} catch {
					// Leave the safe default in place; the composer still works,
					// it just keeps asking before it writes.
				} finally {
					hydrating = null;
				}
			})();
			return hydrating;
		},

		/**
		 * Set the effective mode. Selecting `plan` parks the permission rather
		 * than discarding it, so leaving Plan returns to whichever posture was
		 * in force.
		 */
		select(mode: PlanComposerMode) {
			const current = get(store);
			if (mode === 'plan') {
				persistPlanning(true);
				apply(true, current.permission, current.permissionLoaded);
				return;
			}
			persistPlanning(false);
			const permission = normalizePermission(mode);
			apply(false, permission, current.permissionLoaded);
			if (permission !== current.permission || !current.permissionLoaded) {
				void savePermission(permission);
			}
		},

		/** Choose the permission without disturbing the Plan toggle. */
		selectPermission(permission: ComposerPermission) {
			const current = get(store);
			apply(isPlanning, permission, current.permissionLoaded);
			void savePermission(permission);
		},

		/** Toggle Plan on or off, keeping the remembered permission. */
		setPlanning(planning: boolean) {
			persistPlanning(planning);
			update((state) => {
				isPlanning = planning;
				return { ...state, mode: effectiveMode(planning, state.permission) };
			});
		}
	};
}

export const planModeStore = createPlanModeStore();
