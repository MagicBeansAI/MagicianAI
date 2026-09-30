/**
 * VibeDev submit pipeline — **the client half only**.
 *
 * This file used to assemble the whole coding-task description in the browser
 * (`buildCodingTaskDescription`), create the task, patch the project pointer,
 * dispatch the execution and roll all three back by hand. That was the second
 * of two prose assemblers: the server grew its own for the `@vibedev` chat rail,
 * and the two could drift with nothing to catch it.
 *
 * They are now one. `POST /api/magician/v2/vibedev/runs` runs through
 * `VibeDevRunService::start_build` — the same entry the rail uses — which
 * composes the description, admits the run idempotently, creates it, pins the
 * project pointer and dispatches, all in one durable step. Handoff plan §10:
 * *"the cockpit and the facade both run through `VibeDevRunService::start_build`;
 * no second creation path exists."*
 *
 * **What stayed here, and why.** The studio's toggles live in `localStorage`
 * and the server has never been told about them, so the client still owns the
 * *preference* — it sends the boolean or the number behind each line
 * (`auto_apply`, `visual_self_correct`, `cost_budget_usd`, the mode, the coding
 * profile) and the server owns the words. Navigation, attachment clearing,
 * toasts and the store updates after a run starts are the caller's, as before.
 */
import { get } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import type { UploadedAttachment } from '$lib/stores/chatStore';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { serializeScheduleForApi, type Task } from '$lib/stores/taskStore';
import { vibeDevProjectStore, type VibeDevProject } from '$lib/stores/vibeDevProjectStore';
import { vibeStudioStore, type StudioMode } from '$lib/stores/vibeStudioStore';
import { codingChoiceFromSelection } from '$lib/stores/codingProfileStore';
import type { TaskSchedule } from '$lib/types/agents';

// 2026-06-19 (plan §10/§10.1): coding lead collapsed from EM "Bridge" into CTO "Forge".
// The authoritative lead for vibedev Build runs is server-side (`coding.lead_agent_id`),
// which overrides this client value. This const is the fallback owner for plan/Discuss runs
// (which the server override skips) — keep it pointed at the live lead (`cto`).
export const ENGINEERING_MANAGER_AGENT_ID = 'cto';
// A composer follow-up that should be THREADED into the in-view run (folded into the same
// run in the rail), as opposed to a Run-button follow-up which opens its own run row.
// The server writes the tag; the rail READS it, which is why it still lives here.
export const VIBEDEV_THREADED_TAG = 'vibedev-threaded';
/** Marks a Discuss/plan run. Mirrors the backend `PLAN_TASK_TAG` (run_coding_task.rs)
 *  that forces `plan_only`; also the outcome-type discriminator for follow-up framing. */
export const VIBEDEV_PLAN_TAG = 'plan';

export interface CodingProfileLike {
	id: string;
	label: string;
}

export interface SubmitContext {
	project: VibeDevProject;
	parentTask: Task | null;
	/** The composer's selected coding profile or Auto. Sent as
	 *  `coding_choice` (`{"kind":"auto"}` / `{"kind":"profile","profile_id"}`).
	 *  Named rows also keep `coding_profile_id` for older servers. */
	profile: CodingProfileLike | null;
	/** Whether the PROJECT is plausibly web/visual (backend `previewable`, or a
	 *  pinned preview_url). Gates the visual self-correction directive so it
	 *  doesn't ride along on non-web projects (Node libs, CLI, Rust/Python).
	 *  Defaults to visual when the caller omits it (conservative). */
	projectIsVisual?: boolean;
	/** Thread this follow-up into the in-view run (composer Send) rather than opening a
	 *  separate run (Run button). Tags the new task `vibedev-threaded` so the rail folds it
	 *  into the chain root instead of listing it as its own run. */
	threaded?: boolean;
	stagedAttachments: UploadedAttachment[];
	sessionId: string | null;
	/** Optional recurring schedule (cron) — e.g. a nightly Autopilot build. */
	schedule?: TaskSchedule;
	/** Optional seed context (M4) — a meeting transcript / chat thread the build was started from. */
	seedContent?: string;
	seedLabel?: string;
	/** Intent gate: when true, the run is saved as a user-visible (`Persistent`)
	 *  task in `/tasks`. Default/false → the run is created `Internal` (cockpit-only,
	 *  off the global task feed), still shown in the cockpit's own run history. */
	saveAsTask?: boolean;
	/** Extra completed-task ids the user referenced via `@task` chips in the
	 *  composer. Merged with the parent continuation ref into `reference_task_ids`
	 *  so the run attaches those tasks' outputs as backend continuation artifacts.
	 *  Callers must pre-filter to completed (the backend validator rejects others). */
	referenceTaskIds?: string[];
}

// ── the bits of prompt shape the CLIENT still reads ──────────────────────────
//
// Everything that composed the task DESCRIPTION is gone — it lives in
// `magician/src/magician_v2/vibedev/run_service.rs` now, in one place, pinned by
// `the_cockpit_build_description_is_byte_identical_to_the_client_assembler`.
// What is left here is what the cockpit's own UI needs: the run title it shows
// in a toast, the prefix its rail strips, and whether a run will take a
// follow-up.

export function taskAcceptsFollowUp(task: Task): boolean {
	if (task.synthesisPending) return false;
	return task.status === 'completed' || task.status === 'failed' || task.status === 'cancelled';
}

/** The run's human title WITHOUT the "VibeDev · " prefix — first non-empty
 *  prompt line, whitespace-collapsed and truncated. Use where the surrounding
 *  UI already says "VibeDev" (e.g. toasts). Mirrors the server's
 *  `vibedev_run_task_title`, which is what actually names the run. */
export function bareTitle(prompt: string): string {
	const firstLine = prompt.split('\n').find((line) => line.trim().length > 0)?.trim() ?? prompt;
	const compact = firstLine.replace(/\s+/g, ' ').trim();
	return compact.length > 58 ? `${compact.slice(0, 58)}…` : compact;
}

/** Strip the machine "VibeDev · " / "VibeDev follow-up · " prefix (the server's
 *  `vibedev_run_task_title` adds it) off a stored run TITLE, for surfaces that
 *  already live inside the studio (rail rows, delete confirms). The single home
 *  for the pattern — don't copy the regex into views. */
export function stripRunTitlePrefix(title: string): string {
	return title.replace(/^VibeDev( follow-up)? · /, '');
}

export interface SubmitResult {
	taskId: string;
	isFollowUp: boolean;
	/** True when the run was created on a schedule (fires on cron, not now). */
	scheduled: boolean;
}

interface StartVibeDevRunResponse {
	task_id?: string;
	execution_id?: string | null;
	is_follow_up?: boolean;
	scheduled?: boolean;
	replayed?: boolean;
}

function buildUrl(path: string): string {
	return path;
}

/** The studio-toggle half of the request — the preferences the caller owns
 *  rather than the composer state `SubmitContext` carries. `submitCodingRun`
 *  reads them from `vibeStudioStore`; the legacy cockpit has no studio store
 *  (no mode switch, no budget, no visual-self-correct control) and passes
 *  `mode: 'build'` plus its own auto-apply flag, omitting the rest so the
 *  server applies its defaults. */
export interface RunPreferences {
	mode: StudioMode;
	autoApply: boolean;
	visualSelfCorrect?: boolean;
	costBudgetUsd?: number;
}
export function designDirectiveBlock(): string {
	return `
Design Directive (Golden Path):
- Follow the design system and aesthetic guidelines in AGENTS.md.
- If a visual reference is provided, match its layout, spacing, and palette precisely.
- Ensure high-quality aesthetics, precise typography, and harmonious colors.
`;
}

/** The modern studio's entry: resolves the mode and toggles from
 *  `vibeStudioStore`, then starts the run. */
export async function submitCodingRun(prompt: string, ctx: SubmitContext): Promise<SubmitResult> {
	const studio = get(vibeStudioStore);
	
	const directive = designDirectiveBlock();
	const fullPrompt = `${directive}\n${prompt}`;

	return startVibeDevRun(fullPrompt, ctx, {
		mode: studio.mode,
		autoApply: studio.autoApplyCodeProposals,
		visualSelfCorrect: studio.visualSelfCorrect,
		costBudgetUsd: studio.costBudgetUsd ?? undefined
	});
}

/**
 * Start a coding run for the cockpit — **one request**.
 *
 * `POST /vibedev/runs` runs the server's `VibeDevRunService::start_build`, which
 * composes the description, admits the run durably, creates the task, pins the
 * project's `active_root_task_id` and dispatches the execution. On a dispatch
 * failure the SERVER rolls back, unwinding the pointer before deleting the
 * orphan — the ordering this function used to own, now somewhere a closed tab
 * cannot interrupt.
 *
 * The caller still handles navigation, attachment clearing and toasts.
 *
 * ## `client_run_id`
 *
 * Minted per submission and folded into the run's server-derived idempotency
 * key, so a re-sent submission returns the run it already started rather than
 * buying a second multi-hour build. The honest limit is the rail's: a *new*
 * submission is a new id and therefore a new run, which is what you want when
 * the user really does ask for the same thing twice.
 */
export async function startVibeDevRun(
	prompt: string,
	ctx: SubmitContext,
	prefs: RunPreferences
): Promise<SubmitResult> {
	const { project, parentTask, schedule } = ctx;
	if (!project?.project_id) throw new Error('Could not prepare a VibeDev project');
	const codingChoice = codingChoiceFromSelection(ctx.profile?.id);

	const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/runs'), {
		method: 'POST',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
		body: JSON.stringify({
			prompt,
			project_id: project.project_id,
			// The studio's mode switch. The server maps `discuss` onto its plan
			// mode and `autopilot` onto its own, so all three are one field rather
			// than a mode plus a flag.
			mode: prefs.mode,
			client_run_id: crypto.randomUUID(),
			parent_task_id: parentTask?.id,
			// Composer Send threads into the in-view run; the Run button does not.
			threaded: Boolean(ctx.threaded && parentTask),
			save_as_task: ctx.saveAsTask === true,
			// Serialized by the SAME helper `POST /v3/tasks` uses, so there is one
			// schedule wire shape rather than two.
			schedule: schedule?.cron ? serializeScheduleForApi(schedule) : undefined,
			// Just the `@task` chips: the server adds the parent continuation
			// reference itself, because only it knows whether the parent is a clean
			// completed run.
			reference_task_ids: ctx.referenceTaskIds ?? [],
			seed_content: ctx.seedContent,
			seed_label: ctx.seedLabel,
			attachments: ctx.stagedAttachments.map((attachment) => ({
				attachment_id: attachment.attachment_id,
				filename: attachment.filename,
				label: attachment.label,
				mime_type: attachment.mime_type,
				size: attachment.size
			})),
			attachment_session_id: ctx.sessionId,
			// The caller-owned preferences. The client keeps owning them; the
			// server owns the prose they produce. Omitted optionals serialize to
			// absent fields, which the server fills with its own defaults.
			auto_apply: prefs.autoApply,
			visual_self_correct: prefs.visualSelfCorrect,
			project_is_visual: ctx.projectIsVisual !== false,
			cost_budget_usd: prefs.costBudgetUsd,
			...(codingChoice ? { coding_choice: codingChoice } : {}),
			...(codingChoice?.kind === 'profile' ? { coding_profile_id: codingChoice.profile_id } : {}),
			// The owner a Discuss run keeps: the server's `coding.lead_agent_id`
			// override applies to BUILD runs only, so a plan run would otherwise
			// fall back to the generic assistant.
			agent_id: ENGINEERING_MANAGER_AGENT_ID
		})
	});

	if (!response.ok) {
		const payload = (await response.json().catch(() => null)) as
			| { message?: string; error?: string }
			| null;
		throw new Error(
			payload?.message || payload?.error || `Could not start the VibeDev run (${response.status})`
		);
	}
	const body = (await response.json()) as StartVibeDevRunResponse;
	if (!body?.task_id) throw new Error('The VibeDev run started but returned no task id');
	return {
		taskId: body.task_id,
		isFollowUp: body.is_follow_up ?? Boolean(parentTask),
		scheduled: body.scheduled ?? Boolean(schedule)
	};
}

/** Resolve the active VibeDev project from the store (3-tier fallback). */
export function resolveActiveProject(routeProjectId: string | null): VibeDevProject | null {
	const state = get(vibeDevProjectStore);
	return (
		(routeProjectId
			? state.projects.find((project) => project.project_id === routeProjectId)
			: null) ??
		state.projects.find((project) => project.project_id === state.activeProjectId) ??
		state.projects.find((project) => project.chat_session_status === 'active') ??
		state.projects.find((project) => !project.archived) ??
		null
	);
}
