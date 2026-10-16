// Recurring Monitors Phase 0 — web side of the cross-platform contract
// check. Reads the CANONICAL fixtures from magician/tests/fixtures/monitors/
// (single source of truth — the Rust and iOS tests read the same files) and
// asserts they satisfy the TypeScript contract in monitor.ts plus the
// plan's semantic invariants (§7.3 removal safety, §7.4 dedupe key).
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type {
	MonitorListPageV1,
	MonitorRunResultV1,
	MonitorSpecV1,
	MonitorUpdateDetailV1
} from './monitor';

const FIXTURES = fileURLToPath(
	new URL('../../../../../magician/tests/fixtures/monitors/', import.meta.url)
);

function load<T>(name: string): T {
	return JSON.parse(readFileSync(`${FIXTURES}${name}`, 'utf8')) as T;
}

describe('monitor contract fixtures (canonical, shared with Rust + iOS)', () => {
	it('MonitorSpecV1 parses with valid enums and schema version 1', () => {
		const spec = load<MonitorSpecV1>('monitor_spec_v1.json');
		expect(spec.schema_version).toBe(1);
		expect(['strict', 'balanced', 'broad']).toContain(spec.match_mode);
		expect(['material_changes', 'every_run', 'never']).toContain(spec.notification_policy);
		expect(spec.sources.urls.length).toBeGreaterThan(0);
		for (const url of spec.sources.urls) expect(() => new URL(url)).not.toThrow();
	});

	it('changed run carries findings and the change fingerprint', () => {
		const run = load<MonitorRunResultV1>('monitor_run_result_v1_changed.json');
		expect(run.status).toBe('changed');
		expect(run.complete_scan).toBe(true);
		expect(run.change_fingerprint).toBeTruthy();
		expect(run.findings.length).toBeGreaterThan(0);
		for (const finding of run.findings) {
			expect(['new', 'updated', 'unchanged', 'possibly_removed']).toContain(
				finding.classification
			);
			expect(finding.stable_key).toBeTruthy();
			expect(finding.content_fingerprint).toBeTruthy();
		}
	});

	it('unchanged run has no change fingerprint (nothing to notify)', () => {
		const run = load<MonitorRunResultV1>('monitor_run_result_v1_unchanged.json');
		expect(run.status).toBe('unchanged');
		expect(run.change_fingerprint).toBeUndefined();
		expect(run.findings).toEqual([]);
	});

	it('degraded run respects removal safety (§7.3)', () => {
		const run = load<MonitorRunResultV1>('monitor_run_result_v1_degraded.json');
		expect(run.status).toBe('degraded');
		expect(run.complete_scan).toBe(false);
		expect(run.counts.possibly_removed).toBe(0);
		expect(run.source_outcomes.some((o) => o.status === 'auth_failed' && !o.complete)).toBe(
			true
		);
		expect(run.access_problem?.kind).toBe('auth_failed');
	});

	it('list page uses the cursor envelope (§8)', () => {
		const page = load<MonitorListPageV1>('monitor_list_page_v1.json');
		expect(Array.isArray(page.items)).toBe(true);
		expect(page).toHaveProperty('next_cursor');
		expect(page.limit).toBeGreaterThan(0);
		for (const item of page.items) {
			expect(item.task_id).toBeTruthy();
			expect(['active', 'paused']).toContain(item.state);
			expect(['ok', 'needs_attention', 'failing']).toContain(item.health);
		}
	});

	it('update detail dedupe key contains every §7.4 component', () => {
		const update = load<MonitorUpdateDetailV1>('monitor_update_detail_v1.json');
		const key = update.notification.dedupe_key;
		for (const component of [
			update.monitor_task_id,
			String(update.monitor_revision),
			update.change_fingerprint,
			update.notification.channel
		]) {
			expect(key).toContain(component);
		}
		const producing = load<MonitorRunResultV1>('monitor_run_result_v1_changed.json');
		expect(update.change_fingerprint).toBe(producing.change_fingerprint);
	});
});
