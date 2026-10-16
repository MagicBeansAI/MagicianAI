/**
 * Whether a task "has a plan" — the one rule behind the plan glyph
 * (`PlanGlyph.svelte`) on the chat live task card, the Tasks list row and the
 * task drawer header. iOS and Android apply the same rule.
 *
 * Field names mirror `Task` in `$lib/stores/taskStore` (and the Plan act's
 * `hasPlanAct` in `taskPanelModel.ts`). The status fields are typed loosely so
 * a card's `{ status: string | null }` plan can be passed without a cast.
 */
export interface TaskPlanFields {
	hasPlan?: boolean | null;
	planStatus?: string | null;
	latestPlanId?: string | null;
	approvedPlanId?: string | null;
	planSteps?: readonly unknown[] | null;
}

/**
 * True when any of: `hasPlan === true`, a `planStatus` is set (any value,
 * including `rejected` / `failed` — the task was still planned), a latest or
 * approved plan id is set, or there is at least one plan step.
 */
export function taskHasPlan(task: TaskPlanFields | null | undefined): boolean {
	if (!task) return false;
	if (task.hasPlan === true) return true;
	if (task.planStatus !== undefined && task.planStatus !== null) return true;
	if (task.latestPlanId || task.approvedPlanId) return true;
	return (task.planSteps?.length ?? 0) > 0;
}

/** Message/store statuses that only a planned task passes through. */
const PLANNING_STATUSES: ReadonlySet<string> = new Set(['planning', 'draft', 'eliciting']);

export interface LiveTaskPlanInput {
	/** The chat card message's status (`message.content.status`). */
	messageStatus?: string | null;
	/** The task's status in the task store, when the host has it. */
	storeStatus?: string | null;
	/** The task store record (or its plan fields), when the host has it. */
	storePlan?: TaskPlanFields | null;
	/** The card's plan steps (the `plan.steps` prop). */
	planSteps?: readonly unknown[] | null;
}

/**
 * The chat live task card's variant: the card often has only a status message
 * and a partial store record, so a planning-phase status counts on its own.
 */
export function liveTaskHasPlan(input: LiveTaskPlanInput): boolean {
	const { messageStatus, storeStatus, storePlan, planSteps } = input;
	if (messageStatus && PLANNING_STATUSES.has(messageStatus)) return true;
	if (storeStatus && PLANNING_STATUSES.has(storeStatus)) return true;
	if (taskHasPlan(storePlan)) return true;
	return (planSteps?.length ?? 0) > 0;
}
