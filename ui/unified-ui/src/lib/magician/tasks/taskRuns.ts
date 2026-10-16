/**
 * **Which of a task's runs the panel's execution-scoped acts describe** — the
 * options a reader chooses between, and the one that is chosen.
 *
 * A task can be executed more than once: retried after a failure, re-run on a
 * schedule, dispatched again by hand. Every one of those is a separate execution
 * with its own event log, its own delegations and its own outcome, and the panel
 * showed exactly one of them — whichever the task record currently points at —
 * with no way to read an earlier attempt and nothing on screen saying there was
 * one to read.
 *
 * Pure, and shared by both adapters, which is what makes this feature pass
 * design §2's acceptance test: `/tasks` reads its runs out of
 * `/execution-panel`'s `output.recent_runs` and `/tasks?type=internal` reads
 * them out of `/details`'s `executions`, and neither surface needs a branch
 * because both hand the same three neutral facts to the same function. There is
 * no task-kind parameter here and there must not be one.
 *
 * **The three facts are the whole option**, and they are chosen against design
 * §1's opaque-identifier defect rather than for convenience. A control listing
 * bare execution ids is that defect in a new place: they are 32 hex characters,
 * they sort meaninglessly, and no reader can tell which of two is the one they
 * want. An ordinal, when it ran, and how it ended are what a reader actually
 * chooses between — and all three are already on both wires, so the label costs
 * no request.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §1 and §2, and
 * `docs/components/unified-ui/unified-task-panel.md`, *Choosing which run the
 * acts describe*.
 */

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: unknown): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/**
 * A finite epoch-millis instant, or `null`. Zero is a sentinel, not a time —
 * the same rule every other timestamp reader in this directory applies, and the
 * reason is the same: an epoch instant rendered as a date reads `1 Jan 1970`,
 * which is a confident answer to a question nothing recorded.
 */
function instant(raw: unknown): number | null {
	return typeof raw === 'number' && Number.isFinite(raw) && raw > 0 ? raw : null;
}

const MONTHS = [
	'Jan',
	'Feb',
	'Mar',
	'Apr',
	'May',
	'Jun',
	'Jul',
	'Aug',
	'Sep',
	'Oct',
	'Nov',
	'Dec'
] as const;

function pad(value: number): string {
	return value < 10 ? `0${value}` : `${value}`;
}

/**
 * When a run started, as `2 Jul 14:32` — or `null` when nothing timed it.
 *
 * **Day and month as well as the clock**, unlike `timelineClock`, which renders
 * `14:32:07` for rows the reader is scanning *within* one run. These options are
 * compared *across* runs, and a task retried over three days has three options
 * whose clocks say nothing about which is which. The seconds go, for the
 * opposite reason: nobody picks a run by its second.
 *
 * Local rather than UTC, and hand-built rather than `toLocaleDateString`, for
 * the reason `timelineClock` gives one file over: the reader's own wall clock is
 * what they compared the run against, and a locale-dependent format is a string
 * no test can pin without pinning the runner's locale too.
 *
 * The year is deliberately absent. A task's runs are days or weeks apart, not
 * years, and `2 Jul 2026 14:32` is four more characters in a control that has to
 * fit three of these in a 560px drawer. A run old enough for the year to matter
 * is one the ordinal already distinguishes.
 */
export function runStamp(at: number | null | undefined): string | null {
	const when = instant(at);
	if (when === null) return null;
	const date = new Date(when);
	return `${date.getDate()} ${MONTHS[date.getMonth()]} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/**
 * One run, as either surface's payload states it — the neutral input both
 * adapters map onto.
 *
 * Every field is nullable and untrusted, because both callers are reading JSON:
 * `/execution-panel`'s `recent_runs` declares a `TaskStatus` and an epoch
 * number, and `/details`'s execution states declare optional strings. Accepting
 * the loose shape here rather than at two call sites is what lets both adapters
 * hand over what they have and get the same answer.
 *
 * `status` is expected in **this panel's** status vocabulary — the five words
 * `verdictStatusOf` produces — because that is the vocabulary the verdict line
 * above the control is written in, and a control that said `completed` under a
 * verdict that said `Finished` would be two names for one outcome in one panel.
 * It is typed `string` rather than a union because neither wire guarantees it;
 * an unrecognised word is rendered as it arrived rather than dropped, which is
 * the honest treatment of a status this client does not model but the server
 * does.
 */
export interface RunRecord {
	executionId: string | null | undefined;
	startedAt: number | null | undefined;
	status: string | null | undefined;
}

/** One selectable run, as the control lists it. */
export interface TaskPanelRunOption {
	/** The run's id. Carried, never shown — see this module's header. */
	executionId: string;
	/**
	 * Where this run sits among the task's runs, **oldest is `#1`**.
	 *
	 * Counted from the start rather than the end so a run's number never changes:
	 * a reader who noticed `#2 failed` still finds `#2` after a fourth run lands,
	 * where counting back from the newest would have renumbered it to `#3`. The
	 * options are still *listed* newest-first, which is a different question.
	 */
	ordinal: number;
	startedAt: number | null;
	status: string | null;
	/** What the reader reads. See `runOptionLabel`. */
	label: string;
}

/**
 * The runs a reader can choose between, and the one the acts are describing.
 *
 * **`null` at one run, and that is the type doing the work.** The owner asked
 * for the control "if more than 1 execution", and a slice that is absent rather
 * than a list that is short is how the panel's other acts already spell that:
 * `plan: null` is no Plan act, not a greyed one. A disabled dropdown asserts
 * there is a choice and then refuses it, which is the type check design §4
 * forbids sneaking back in through styling.
 */
export interface TaskPanelRuns {
	/** Newest first, as the control lists them. Always two or more. */
	options: readonly TaskPanelRunOption[];
	/** The option the acts below are about. Always one of `options`. */
	selectedId: string;
}

/**
 * What one option reads as: `#3 · 2 Jul 14:32 · failed`.
 *
 * **Segments that have nothing to say are dropped rather than filled**, the same
 * rule `provenanceRows` applies to a provenance row and `runSummary` to a
 * segment of the Run act's header. A run nothing timed reads `#3 · failed`; one
 * whose status did not arrive reads `#3 · 2 Jul 14:32`; one with neither reads
 * `#3`, which is still enough to choose by because the list is ordered. The
 * alternatives are both worse: `#3 · unknown · failed` invents a fact, and
 * dropping the option entirely would hide a run that exists.
 *
 * The ordinal is never dropped — it is the only segment guaranteed to
 * distinguish two options, and it is what a reader says out loud.
 *
 * `·` is the separator the verdict line and the act headers already use for
 * "another fact about the same thing", so the control reads in the panel's own
 * voice rather than introducing a second punctuation for one idea.
 */
export function runOptionLabel(
	ordinal: number,
	startedAt: number | null | undefined,
	status: string | null | undefined
): string {
	const segments = [`#${ordinal}`];
	const stamp = runStamp(startedAt);
	if (stamp !== null) segments.push(stamp);
	const word = text(status);
	if (word !== null) segments.push(word);
	return segments.join(' · ');
}

/**
 * The task's runs as the control lists them, or `null` when there is no choice
 * to offer.
 *
 * `null` in four situations, and every one of them is "this panel cannot honestly
 * offer a choice" rather than an error:
 *
 * 1. **Fewer than two runs.** The owner's condition, and the common case — one
 *    run means nothing changes for the overwhelming majority of tasks.
 * 2. **No selection was passed.** The caller could not say which run the acts
 *    are about, so a control would have to guess, and a dropdown showing `#3`
 *    over acts describing `#1` is worse than no dropdown: every value on screen
 *    would be individually correct.
 * 3. **The selection is not one of the runs.** Same failure, arrived at from the
 *    other side — a `<select>` whose value is absent from its options renders
 *    the first one, silently claiming the reader is looking at the newest run.
 *    This is the case a task row that moved on between fetches produces.
 * 4. **Every record was unusable.** A run with no id cannot be selected, so it
 *    is dropped; if that leaves fewer than two, case 1 applies.
 *
 * **Duplicate ids collapse to one option**, keeping the first. Both wires build
 * their list from a tree walk, and an id appearing twice would otherwise be two
 * options that are the same run — indistinguishable to the reader and, worse,
 * shifting every ordinal after it.
 *
 * **Ordinals are assigned chronologically and the list is then reversed**, which
 * is two orders for two different jobs and neither is the incoming one. The
 * ordinal has to count from the oldest run so it is stable as runs are added
 * (see `TaskPanelRunOption.ordinal`); the list has to be newest-first because
 * that is the run a reader opening a panel almost always wants. Deriving both
 * from the sort rather than trusting the payload's order is deliberate: one wire
 * sorts by `updated_at` and the other by nothing at all, so an incoming order
 * would be a fact about the backend rather than about the runs.
 *
 * A run with no recorded start sorts oldest — `?? 0` — rather than being dropped
 * or floated to the top. `Array.prototype.sort` is stable, so several of them
 * keep the order they arrived in, and the option still renders with its ordinal
 * and its status. Guessing an instant for it would be the approximation design
 * §5 forbids; hiding it would hide a run.
 */
export function runsSliceOf(
	records: readonly RunRecord[],
	selectedId: string | null | undefined
): TaskPanelRuns | null {
	const rows: Array<{ executionId: string; startedAt: number | null; status: string | null }> = [];
	const ids = new Set<string>();
	for (const record of records ?? []) {
		const executionId = text(record?.executionId);
		if (executionId === null || ids.has(executionId)) continue;
		ids.add(executionId);
		rows.push({
			executionId,
			startedAt: instant(record?.startedAt),
			status: text(record?.status)
		});
	}
	if (rows.length < 2) return null;

	const selected = text(selectedId);
	if (selected === null || !ids.has(selected)) return null;

	rows.sort((left, right) => (left.startedAt ?? 0) - (right.startedAt ?? 0));
	const options = rows
		.map(
			(row, index): TaskPanelRunOption => ({
				executionId: row.executionId,
				ordinal: index + 1,
				startedAt: row.startedAt,
				status: row.status,
				label: runOptionLabel(index + 1, row.startedAt, row.status)
			})
		)
		.reverse();

	return { options, selectedId: selected };
}
