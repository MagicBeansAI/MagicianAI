/**
 * VibeDev cockpit UI state.
 *
 * The "Living Studio" cockpit's view state, lifted out of the `+page.svelte`
 * monolith: which stage tab is showing, Build vs Discuss mode, rail/terminal
 * layout, the preview device frame, and the auto-apply policy (now default
 * OFF — U5). Data lives in `codingSpineStore` / `vibeHitlStore` /
 * `vibeDevProjectStore`; this is purely the cockpit's own UI knobs.
 *
 * `policyPromptLine()` is exported so the submit pipeline's task description
 * tells the agent the SAME policy the toggle enforces (they used to drift).
 */
import { get, writable, type Readable } from 'svelte/store';
import { browser } from '$app/environment';

export type StageTab = 'preview' | 'code' | 'diff' | 'visual' | 'tests';
export type StudioMode = 'build' | 'discuss' | 'autopilot';
export type DeviceFrame = 'desktop' | 'tablet' | 'phone';

export interface VibeStudioState {
	stageTab: StageTab;
	mode: StudioMode;
	railExpanded: boolean;
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

/** The cheap default + the premium escalation profile (magician-config coding.profiles). */
const FLOOR_PROFILE = 'coding-balanced';
const PREMIUM_PROFILE = 'coding-premium';

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
	// Default AUTO (on). It only rides along for vision-capable profiles (gated in
	// visualSelfCorrectDirectiveBlock) and Pi skips non-visual changes at runtime —
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
		railExpanded: true,
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
			set(initialState());
		},

		/**
		 * The execution-policy line the submit pipeline appends so the agent's
		 * instructions match the toggle. Reads the live store value.
		 */
		policyPromptLine(): string {
			const state = get(store);
			if (state.mode === 'discuss') {
				return '- Discuss mode: this is a read-only request. Explain, plan, or review — do NOT modify files or stage code proposals.';
			}
			if (state.mode === 'autopilot') {
				return '- VibeDev Autopilot is enabled: this is an unattended run — apply your own proposals (apply_code_proposal), self-verify with run_project_checks, iterate until green on a dedicated branch (never main), and report the final status with notify_owner. Do not block on routine review.';
			}
			return state.autoApplyCodeProposals
				? '- VibeDev auto-apply is enabled for proposal-backed code diffs; keep changes focused and reviewable.'
				: '- VibeDev manual review is enabled; wait for approval before applying proposals.';
		},

		/**
		 * The full Autopilot operating directive appended to the task description
		 * (empty unless in autopilot mode). Tells the engineering-manager to run
		 * the unattended apply → check → fix loop on a branch and report back.
		 * The tools it references (apply_code_proposal / run_project_checks /
		 * notify_owner) are granted to the engineers / EM (M1 backend).
		 */
		autopilotDirectiveBlock(): string {
			if (get(store).mode !== 'autopilot') return '';
			return [
				'',
				'Autopilot policy (UNATTENDED — the owner has closed the session and will review on return):',
				'- This is an UNATTENDED autopilot run. Do NOT pause for routine approval. The owner has pre-authorized you to apply your own changes and iterate to completion. Only stop to escalate if you are genuinely blocked.',
				'- Before delegating any coding, create a dedicated work branch off the current HEAD with dugite (name it autopilot/<short-task-slug>). Never commit to or modify main — main must stay untouched.',
				'- Delegate implementation to the right engineer with this loop instruction, verbatim: "After each run_coding_task, immediately call apply_code_proposal with the returned proposal_id to apply your own change to the working tree, then call run_project_checks to run build/test/lint. If any check fails, read the diagnostics, make a focused fix, and repeat. Continue until all checks pass (green) or you reach 12 coding iterations. Do not wait for human approval at any point."',
				'- When the engineer returns green (or exhausts iterations), commit the accumulated work to the branch with dugite and capture the branch name and a `git diff --stat` summary for the morning review.',
				'- Report exactly one final status with notify_owner: success → kind=briefing severity=info ("Autopilot finished: <task>. Checks green. Branch <name>, <N> files changed. Review & merge when ready."); not green → kind=escalation severity=warn ("Autopilot needs you: <task>. Stopped on <reason>. Branch <name> has the partial work.").',
				'- If owner cross-channel delivery is configured, ALSO delegate_to_agent(personal-assistant) with a one-line recap so the owner gets the same status over their preferred channel (WhatsApp/email).',
				'- Respect the task iteration budget. If you cannot make progress, stop and escalate rather than loop.',
				'- Respect the cost budget stated in the Execution policy above: after each run_coding_task read session_stats.cost (cumulative USD), prefer the cheap profile as you approach the budget, and STOP + report rather than exceed it.'
			].join('\n');
		},

		/**
		 * Planning directive (Discuss mode → plan run). Empty unless mode==='discuss'.
		 * Tells the agent the run's outcome is a written PLAN, not a code diff: inspect
		 * the repo via run_coding_task with plan_only:true (read-only, stages no diff,
		 * captures the plan as the output), then YIELD with the plan as the result.
		 * Pairs with the `plan` task tag, which FORCES plan_only at the handler even if
		 * the agent ignores the arg (so the no-diff invariant cannot silently degrade).
		 */
		planDirectiveBlock(): string {
			if (get(store).mode !== 'discuss') return '';
			return [
				'',
				'Planning approach (Discuss — read-only, NO code changes, the PLAN is the deliverable):',
				'- Produce a concrete written plan. Do NOT modify files or stage code proposals.',
				'- Make ONE plan_only run_coding_task call (read-only — it stages NO diff and captures your plan as the run output; the `plan` tag also forces plan_only at the handler). Do NOT re-run it or start a second planning pass.',
				'- Structure the plan: objective, approach, key decisions, risks/unknowns, and the concrete files/areas to change when it is built.',
				'- As soon as that one call returns the plan, YIELD with it as your completed result (a substantive completed item / artifact) — never an empty yield, and never additional planning turns.'
			].join('\n');
		},

		/**
		 * Informational pin only. The server owns the task description; this
		 * helper is leftover from the client assembler and must not be a second
		 * policy. Magician enforces the committed constraint at dispatch.
		 */
		escalationPolicyLine(floorProfileId: string): string {
			const floor = floorProfileId || FLOOR_PROFILE;
			if (floor === PREMIUM_PROFILE) {
				return `- This request is pinned to coding_profile: ${PREMIUM_PROFILE}. Magician enforces that pin on every run_coding_task; do not pass a different coding_profile.`;
			}
			if (floor === FLOOR_PROFILE) {
				return `- This request is pinned to coding_profile: ${floor}. Magician allows the configured one-hop to ${PREMIUM_PROFILE}. You may pass coding_profile: ${PREMIUM_PROFILE} for a genuinely hard step or one that has failed twice; Magician rejects anything else.`;
			}
			return `- This request is pinned to coding_profile: ${floor}. Magician enforces that named pin on every run_coding_task. The pin is not an engine switch; do not pass a different coding_profile as authority.`;
		},

		/**
		 * Optional per-run cost-budget stop-condition (M2). Empty when no budget is
		 * set. References session_stats.cost, which the agent receives in every
		 * run_coding_task result — so this is an enforceable self-limit, not prose.
		 */
		costBudgetLine(): string {
			const budget = get(store).costBudgetUsd;
			if (budget == null) return '';
			return `- Cost budget: $${budget.toFixed(2)} for this run. After each run_coding_task, check session_stats.cost (cumulative USD); prefer the cheap profile as you approach it, and STOP and report your progress rather than exceed it.`;
		},

		/**
		 * Visual self-correction directive (M5). Empty unless "Auto" is on (default),
		 * the project is plausibly visual (`projectIsVisual`), and not in Discuss. We do
		 * NOT gate this directive on the initially selected profile's image support:
		 * the run may escalate from a text-only coding profile to a vision-capable one. Pi
		 * gates NEEDEDNESS at runtime (the directive skips non-visual changes, starts the
		 * preview, and self-skips if a run reports no image support), so it only runs the
		 * screenshot→critique→patch loop on visual work. Hard-capped at 3 passes.
		 */
		visualSelfCorrectDirectiveBlock(projectIsVisual: boolean): string {
			const state = get(store);
			if (!state.visualSelfCorrect || state.mode === 'discuss' || !projectIsVisual) return '';
			return [
				'',
				'Visual self-correction (opt-in — SEE what you build, do not fly blind):',
				'- After a change that affects the rendered UI, call screenshot_preview { project_id } to capture the running preview. If it returns ok=false (no preview running), start the dev server first; if the change is NOT visual (config, backend, tests), SKIP visual self-correction entirely.',
				'- Feed the screenshot back as a critic: call run_coding_task with attachment_ids set to the returned attachment_id and attachment_session_id set to the returned attachment_session_id, instructing it to compare the rendered screenshot against the goal and fix what looks wrong — broken layout, overflow, misalignment, poor spacing/contrast, missing or clipped content.',
				'- Then apply_code_proposal + run_project_checks, and screenshot_preview again to confirm the fix landed visually.',
				'- HARD CAP: at most 3 visual passes per change. Stop when it looks right or the cap is reached — never loop on cosmetics. This needs a coding profile that supports image inputs; if a run reports it does not, skip the visual loop and say so.'
			].join('\n');
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
