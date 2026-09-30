// Phase 7 convert-to-monitor: the pure eligibility + prefill logic the
// Tasks surface uses before routing into MonitorComposer's `convert` mode.
// Eligibility mirrors the backend gate (`convert_task_to_monitor_v3_handler`):
// persistent + mutable + not already a monitor; the server stays
// authoritative for anything the mirror wrongly admits.
import { describe, expect, it } from 'vitest';
import {
	canConvertTaskToMonitor,
	convertFormFromTask,
	keptScheduleSummary,
	type ConvertibleTaskLike
} from './convert';
import { emptyMonitorForm } from './specForm';

function task(overrides: Partial<ConvertibleTaskLike> = {}): ConvertibleTaskLike {
	return {
		id: 'task_1',
		title: 'Acme pricing check',
		description: 'Check the Acme pricing page for plan changes',
		status: 'ready',
		source: 'task',
		...overrides
	};
}

describe('canConvertTaskToMonitor', () => {
	it('accepts a plain persistent task (scheduled or not)', () => {
		expect(canConvertTaskToMonitor(task())).toBe(true);
		expect(canConvertTaskToMonitor(task({ lifecycle: 'persistent' }))).toBe(true);
		// A missing schedule is fine — the converted monitor is run-on-demand.
		expect(canConvertTaskToMonitor(task({ schedule: undefined }))).toBe(true);
		expect(
			canConvertTaskToMonitor(task({ schedule: { cron: '0 9 * * *' }, monitorRevision: 0 }))
		).toBe(true);
	});

	it('rejects tasks that are already monitors (monitorRevision > 0)', () => {
		expect(canConvertTaskToMonitor(task({ monitorRevision: 1 }))).toBe(false);
		expect(canConvertTaskToMonitor(task({ monitorRevision: 7 }))).toBe(false);
	});

	it('rejects internal-lifecycle tasks', () => {
		expect(canConvertTaskToMonitor(task({ lifecycle: 'internal' }))).toBe(false);
	});

	it('rejects execution-sourced and read-only rows', () => {
		expect(canConvertTaskToMonitor(task({ source: 'execution' }))).toBe(false);
		expect(canConvertTaskToMonitor(task({ readOnly: true }))).toBe(false);
	});
});

describe('convertFormFromTask', () => {
	it('seeds the objective from the description and keeps the title', () => {
		const form = convertFormFromTask(task());
		expect(form.title).toBe('Acme pricing check');
		expect(form.objective).toBe('Check the Acme pricing page for plan changes');
	});

	it('falls back to the title when the description is blank', () => {
		const form = convertFormFromTask(task({ description: '   ' }));
		expect(form.objective).toBe('Acme pricing check');
	});

	it('pins cadence to none — conversion never touches the schedule', () => {
		const form = convertFormFromTask(task({ schedule: { cron: '0 9 * * *' } }));
		expect(form.cadence).toBe('none');
		expect(form.cronExpression).toBe('');
	});

	it('leaves every other field at the composer default', () => {
		const form = convertFormFromTask(task());
		const defaults = emptyMonitorForm();
		expect(form.urlsText).toBe(defaults.urlsText);
		expect(form.domainsText).toBe(defaults.domainsText);
		expect(form.matchMode).toBe(defaults.matchMode);
		expect(form.notificationPolicy).toBe(defaults.notificationPolicy);
		expect(form.notifyInitialBaseline).toBe(defaults.notifyInitialBaseline);
	});
});

describe('keptScheduleSummary', () => {
	it('mirrors the backend cadence string for kept cron schedules', () => {
		expect(keptScheduleSummary({ cron: '0 9 * * *' })).toBe('Cron 0 9 * * *');
		expect(keptScheduleSummary({ cron: '0 6 * * 1', timezone: 'America/Los_Angeles' })).toBe(
			'Cron 0 6 * * 1 (America/Los_Angeles)'
		);
	});

	it('labels schedule-less tasks unscheduled (run-on-demand after convert)', () => {
		expect(keptScheduleSummary(undefined)).toBe('unscheduled');
		expect(keptScheduleSummary(null)).toBe('unscheduled');
		expect(keptScheduleSummary({ cron: '   ' })).toBe('unscheduled');
	});
});
