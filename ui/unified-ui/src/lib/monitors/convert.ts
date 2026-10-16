/**
 * Convert-to-monitor logic (Phase 7) — pure, component-free.
 *
 * The Tasks surface offers "Convert to monitor" on ELIGIBLE task rows and
 * routes into the existing `MonitorComposer` in `convert` mode. This module
 * owns the two pure decisions so they are unit-testable:
 *
 * 1. `canConvertTaskToMonitor` — client-side eligibility, derived from the
 *    task record alone (the backend re-checks and is authoritative):
 *    persistent lifecycle, a real mutable task (`source === 'task'`, not
 *    read-only), not already a monitor (`monitorRevision > 0` is the wire
 *    discriminator the list rows now carry — 0/absent means "never had a
 *    spec"). A missing schedule is fine: the converted monitor is simply
 *    run-on-demand.
 * 2. `convertFormFromTask` — the composer prefill: objective seeded from
 *    the task description (falling back to the title), title from the task
 *    title, cadence pinned to `none` because conversion NEVER touches the
 *    existing schedule (the POST body is `{spec, title?}` only).
 *
 * Conversion is user-explicit only — plan Phase 7 forbids inferring
 * monitors from title text, so there are deliberately NO heuristics here.
 */

import type { TaskSchedule } from '$lib/types/agents';
import { emptyMonitorForm, type MonitorFormValue } from './specForm';

/** The slice of the task-store `Task` record eligibility/prefill reads. */
export interface ConvertibleTaskLike {
	id: string;
	title: string;
	description: string;
	status: string;
	source: string;
	readOnly?: boolean;
	lifecycle?: 'persistent' | 'internal';
	monitorRevision?: number;
	schedule?: TaskSchedule;
}

/**
 * Client-side mirror of the backend eligibility gate
 * (`convert_task_to_monitor_v3_handler`): persistent + mutable + not
 * already a monitor. Archived tasks never reach the tasks list, and the
 * server still 409s (`monitor_already_exists` /
 * `task_not_eligible_for_monitor`) on anything the mirror wrongly admits.
 */
export function canConvertTaskToMonitor(task: ConvertibleTaskLike): boolean {
	if (task.source !== 'task' || task.readOnly === true) return false;
	if ((task.lifecycle ?? 'persistent') !== 'persistent') return false;
	if ((task.monitorRevision ?? 0) > 0) return false;
	return true;
}

/**
 * Prefill the composer for convert mode: objective from the description
 * (the task's "what to do" prose), falling back to the title; title kept.
 * Cadence is pinned to `none` — the convert POST carries no schedule, so
 * the task's existing schedule (or lack of one) stays exactly as it is.
 */
export function convertFormFromTask(task: ConvertibleTaskLike): MonitorFormValue {
	const form = emptyMonitorForm();
	form.title = task.title.trim();
	form.objective = task.description.trim() || task.title.trim();
	form.cadence = 'none';
	return form;
}

/**
 * The human line the convert review shows for the schedule the task KEEPS.
 * Mirrors `monitors_api::cadence_summary` for the store's parsed
 * `{cron, timezone}` shape (`unscheduled` = run-on-demand after convert).
 */
export function keptScheduleSummary(schedule: TaskSchedule | undefined | null): string {
	if (!schedule?.cron?.trim()) return 'unscheduled';
	const timezone = schedule.timezone?.trim();
	return timezone ? `Cron ${schedule.cron} (${timezone})` : `Cron ${schedule.cron}`;
}
