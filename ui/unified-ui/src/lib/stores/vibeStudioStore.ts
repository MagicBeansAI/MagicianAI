/**
 * VibeDev cockpit UI state.
 *
 * The "Living Studio" cockpit's view state, lifted out of the `+page.svelte`
 * monolith: which stage tab is showing, Build vs Discuss mode, rail/terminal
 * layout, the preview device frame, and the auto-apply policy (now default
 * OFF — U5). Data lives in `codingSpineStore` / `vibeHitlStore` /
 * `vibeDevProjectStore`; this is purely the cockpit's own UI knobs.
 *
 * The task description an agent sees (policy, mode directives, budget, visual
 * self-correction) is assembled by the server from the structured run request.
 */
import { get, writable, type Readable } from 'svelte/store';
import { browser } from '$app/environment';

/** `branch` exists only for trusted projects (their changes are commits). */
export type StageTab = 'preview' | 'code' | 'diff' | 'branch' | 'visual' | 'tests';
export type StudioMode = 'build' | 'discuss' | 'autopilot';
export type DeviceFrame = 'desktop' | 'tablet' | 'phone';

export interface VibeStudioState {
	stageTab: StageTab;
	mode: StudioMode;
	railExpanded: boolean;
	/** The build stage (right column on wide screens, a right-side drawer below 1280px). */
	stageExpanded: boolean;
	/** The stage drawer is sliding out; it collapses when the animation ends. */
	stageClosing: boolean;
	terminalOpen: boolean;
	deviceFrame: DeviceFrame;
	autoApplyCodeProposals: boolean;
	/** Optional per-run cost budget in USD (M2). null = unlimited. */
	costBudgetUsd: number | null;
	/** Opt-in visual self-correction (M5): screenshot the preview → Pi critiques → patch → re-shoot. */
	visualSelfCorrect: boolean;
}

const AUTO_APPLY_STORAGE_KEY = 'magician.vibedev.autoApplyCodeProposals';
const COST_BUDGET_STORAGE_KEY = 'magician.vibedev.costBudgetUsd';
// `.auto` suffix: the semantics changed from off-by-default opt-in to Auto-by-
// default, so we ignore any pre-Auto stored value (which would otherwise pin it
// off). Unset = Auto; an explicit Off under the new key persists as before.
const VISUAL_SELF_CORRECT_STORAGE_KEY = 'magician.vibedev.visualSelfCorrect.auto';
const STUDIO_FLAG_STORAGE_KEY = 'magician.vibedev.studio';
/** Matches the stage drawer's slide-out animation in VibeStudio.svelte. */
const STAGE_DRAWER_CLOSE_MS = 200;

/** Below 1280px (and above the 768px phone layout) the open stage is a drawer. */
function stageIsDrawer(): boolean {
	if (!browser) return false;
	const width = window.innerWidth;
	if (width <= 768 || width > 1280) return false;
	return !window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
}
const STAGE_EXPANDED_STORAGE_KEY = 'magician.vibedev.stageExpanded';

/** Below 1280px the rail is a drawer over the conversation, so it starts closed there. */
function railStartsExpanded(): boolean {
	return !browser || window.innerWidth > 1280;
}

function loadStageExpanded(): boolean {
	if (!browser) return true;
	// Below 1280px the open stage is a drawer over the conversation, so it
	// starts closed there; the saved choice applies to wider screens.
	if (window.innerWidth <= 1280) return false;
	try {
		return localStorage.getItem(STAGE_EXPANDED_STORAGE_KEY) !== 'false';
	} catch {
		return true;
	}
}

function persistStageExpanded(value: boolean): void {
	if (!browser) return;
	try {
		localStorage.setItem(STAGE_EXPANDED_STORAGE_KEY, value ? 'true' : 'false');
	} catch {
		// best-effort
	}
}

function loadAutoApplyPreference(): boolean {
	// Default OFF (U5) — manual review unless the user opts into auto-apply.
	if (!browser) return false;
	try {
		const value = localStorage.getItem(AUTO_APPLY_STORAGE_KEY);
		if (value === 'true') return true;
		if (value === 'false') return false;
	} catch {
		// localStorage unavailable — keep the default.
	}
	return false;
}

function persistAutoApplyPreference(value: boolean): void {
	if (!browser) return;
	try {
		localStorage.setItem(AUTO_APPLY_STORAGE_KEY, value ? 'true' : 'false');
	} catch {
		// best-effort; the in-memory toggle still works
	}
}

function loadCostBudget(): number | null {
	if (!browser) return null;
	try {
		const raw = localStorage.getItem(COST_BUDGET_STORAGE_KEY);
		if (!raw) return null;
		const value = Number(raw);
		return Number.isFinite(value) && value > 0 ? value : null;
	} catch {
		return null;
	}
}

function persistCostBudget(value: number | null): void {
	if (!browser) return;
	try {
		if (value && value > 0) localStorage.setItem(COST_BUDGET_STORAGE_KEY, String(value));
		else localStorage.removeItem(COST_BUDGET_STORAGE_KEY);
	} catch {
		// best-effort
	}
}

function loadVisualSelfCorrect(): boolean {
	// Default AUTO (on). It only rides along for vision-capable profiles (the
	// server gates the directive) and Pi skips non-visual changes at runtime —
	// so the cost lands only on genuinely-visual work, not as a blanket opt-in.
	// 'Off' (explicit) = never. Stored choice wins; unset = Auto.
	if (!browser) return true;
	try {
		const stored = localStorage.getItem(VISUAL_SELF_CORRECT_STORAGE_KEY);
		return stored === null ? true : stored === 'true';
	} catch {
		return true;
	}
}

function persistVisualSelfCorrect(value: boolean): void {
	if (!browser) return;
	try {
		localStorage.setItem(VISUAL_SELF_CORRECT_STORAGE_KEY, value ? 'true' : 'false');
	} catch {
		// best-effort
	}
}

function initialState(): VibeStudioState {
	return {
		stageTab: 'preview',
		mode: 'build',
		railExpanded: railStartsExpanded(),
		stageExpanded: loadStageExpanded(),
		stageClosing: false,
		terminalOpen: false,
		deviceFrame: 'desktop',
		autoApplyCodeProposals: loadAutoApplyPreference(),
		costBudgetUsd: loadCostBudget(),
		visualSelfCorrect: loadVisualSelfCorrect()
	};
}

function createVibeStudioStore() {
	const store = writable<VibeStudioState>(initialState());
	const { subscribe, update, set } = store;
	let stageCloseTimer: ReturnType<typeof setTimeout> | null = null;
	function cancelStageClose(): void {
		if (stageCloseTimer !== null) clearTimeout(stageCloseTimer);
		stageCloseTimer = null;
	}
	function toggleStage(expanded?: boolean): void {
		cancelStageClose();
		update((state) => {
			const stageExpanded = expanded ?? !state.stageExpanded;
			persistStageExpanded(stageExpanded);
			return { ...state, stageExpanded, stageClosing: false };
		});
	}

	return {
		subscribe: subscribe as Readable<VibeStudioState>['subscribe'],

		setStageTab(stageTab: StageTab): void {
			update((state) => (state.stageTab === stageTab ? state : { ...state, stageTab }));
		},
		setMode(mode: StudioMode): void {
			update((state) => {
				if (state.mode === mode) return state;
				// Discuss is read-only Pi; Autopilot means the AGENT applies its own
				// proposals unattended — in both, the cockpit's client-side auto-apply
				// must be off (no Pi to apply for Discuss; agent owns apply for Autopilot,
				// so the cockpit must not race it on the same proposal).
				const autoApply =
					mode === 'discuss' || mode === 'autopilot' ? false : state.autoApplyCodeProposals;
				return { ...state, mode, autoApplyCodeProposals: autoApply };
			});
		},
		setDeviceFrame(deviceFrame: DeviceFrame): void {
			update((state) => ({ ...state, deviceFrame }));
		},
		toggleRail(expanded?: boolean): void {
			update((state) => ({
				...state,
				railExpanded: expanded ?? !state.railExpanded
			}));
		},
		toggleStage,
		/** Hide the stage; as a drawer it slides out first, then collapses. */
		closeStage(): void {
			const state = get(store);
			if (!state.stageExpanded || state.stageClosing) return;
			if (!stageIsDrawer()) {
				toggleStage(false);
				return;
			}
			update((s) => ({ ...s, stageClosing: true }));
			stageCloseTimer = setTimeout(() => toggleStage(false), STAGE_DRAWER_CLOSE_MS);
		},
		toggleTerminal(open?: boolean): void {
			update((state) => ({ ...state, terminalOpen: open ?? !state.terminalOpen }));
		},
		setAutoApply(value: boolean): void {
			persistAutoApplyPreference(value);
			update((state) => ({ ...state, autoApplyCodeProposals: value }));
		},
		setCostBudget(value: number | null): void {
			const normalized = value && value > 0 ? value : null;
			persistCostBudget(normalized);
			update((state) => ({ ...state, costBudgetUsd: normalized }));
		},
		setVisualSelfCorrect(value: boolean): void {
			persistVisualSelfCorrect(value);
			update((state) => ({ ...state, visualSelfCorrect: value }));
		},

		/** Reset transient view knobs (e.g. on project switch). */
		resetView(): void {
			cancelStageClose();
			set(initialState());
		}
	};
}

export const vibeStudioStore = createVibeStudioStore();

// ── Studio feature flag ──────────────────────────────────────────────────────
// The cockpit ships behind a flag with the monolith as fallback until parity is
// verified (blueprint P0 migration). `?studio=1`/`?studio=0` flips and persists;
// once verified the default flips to ON.
const STUDIO_DEFAULT_ON = true;

export function readStudioFlag(searchParams: URLSearchParams | null): boolean {
	if (!browser) return STUDIO_DEFAULT_ON;
	const param = searchParams?.get('studio');
	if (param === '1' || param === 'true') {
		try {
			localStorage.setItem(STUDIO_FLAG_STORAGE_KEY, 'true');
		} catch {
			/* ignore */
		}
		return true;
	}
	if (param === '0' || param === 'false') {
		try {
			localStorage.setItem(STUDIO_FLAG_STORAGE_KEY, 'false');
		} catch {
			/* ignore */
		}
		return false;
	}
	try {
		const stored = localStorage.getItem(STUDIO_FLAG_STORAGE_KEY);
		if (stored === 'true') return true;
		if (stored === 'false') return false;
	} catch {
		/* ignore */
	}
	return STUDIO_DEFAULT_ON;
}
