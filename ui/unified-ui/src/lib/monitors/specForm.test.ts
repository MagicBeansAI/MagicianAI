// Composer-side mirror of the backend monitor-spec admission gate.
// The reasons and normalization MUST match monitor_spec.rs
// `validate_and_normalize` (same stable snake_case strings), and
// `cadenceSummary` MUST match monitors_api::cadence_summary — the review
// card promises exactly what the server will show.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { MonitorSpecV1 } from '../types/monitor';
import {
	cadenceSummary,
	emptyMonitorForm,
	formFromSpec,
	MAX_LIST_ENTRIES,
	MAX_LIST_ENTRY_CHARS,
	MAX_OBJECTIVE_CHARS,
	scheduleFromForm,
	specFromForm,
	splitLines,
	validateAndNormalizeSpec
} from './specForm';

const FIXTURES = fileURLToPath(
	new URL('../../../../../magician/tests/fixtures/monitors/', import.meta.url)
);

function fixtureSpec(): MonitorSpecV1 {
	return JSON.parse(readFileSync(`${FIXTURES}monitor_spec_v1.json`, 'utf8')) as MonitorSpecV1;
}

describe('validateAndNormalizeSpec (backend admission mirror)', () => {
	it('accepts the canonical fixture unchanged (already normalized)', () => {
		const result = validateAndNormalizeSpec(fixtureSpec());
		expect(result.ok).toBe(true);
		if (result.ok) expect(result.spec).toEqual(fixtureSpec());
	});

	it('rejects with the same stable reasons the server uses', () => {
		const cases: Array<[Partial<MonitorSpecV1>, string]> = [
			[{ schema_version: 2 as unknown as 1 }, 'monitor_schema_version_unsupported'],
			[{ objective: '   ' }, 'monitor_objective_required'],
			[{ objective: 'x'.repeat(MAX_OBJECTIVE_CHARS + 1) }, 'monitor_objective_too_long']
		];
		for (const [patch, reason] of cases) {
			const result = validateAndNormalizeSpec({ ...fixtureSpec(), ...patch });
			expect(result.ok).toBe(false);
			if (!result.ok) expect(result.reason).toBe(reason);
		}
	});

	it('rejects bad source URLs exactly like the server', () => {
		const bad = fixtureSpec();
		bad.sources = { ...bad.sources, urls: ['javascript:alert(1)'] };
		const schemeResult = validateAndNormalizeSpec(bad);
		expect(!schemeResult.ok && schemeResult.reason).toBe(
			'monitor_source_url_scheme_unsupported'
		);

		const invalid = fixtureSpec();
		invalid.sources = { ...invalid.sources, urls: ['not a url at all'] };
		const invalidResult = validateAndNormalizeSpec(invalid);
		expect(!invalidResult.ok && invalidResult.reason).toBe('monitor_source_url_invalid');
	});

	it('requires at least one of urls/domains/query_seeds', () => {
		const spec = fixtureSpec();
		spec.sources = { urls: [], domains: ['  '], authenticated_sources: ['acct'] };
		spec.query_seeds = [''];
		const result = validateAndNormalizeSpec(spec);
		expect(!result.ok && result.reason).toBe('monitor_sources_required');
	});

	it('trims, dedupes (first-seen), and drops empty entries like the server', () => {
		const spec = fixtureSpec();
		spec.objective = '  Watch the pricing page  ';
		spec.query_seeds = [' acme pricing ', '', 'acme pricing', 'acme tiers'];
		spec.include_rules = ['  ', 'plans', 'plans'];
		const result = validateAndNormalizeSpec(spec);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.spec.objective).toBe('Watch the pricing page');
			expect(result.spec.query_seeds).toEqual(['acme pricing', 'acme tiers']);
			expect(result.spec.include_rules).toEqual(['plans']);
		}
	});

	it('bounds list sizes and entry lengths with the field-specific reasons', () => {
		const tooMany = fixtureSpec();
		tooMany.query_seeds = Array.from({ length: MAX_LIST_ENTRIES + 1 }, (_, i) => `seed ${i}`);
		const tooManyResult = validateAndNormalizeSpec(tooMany);
		expect(!tooManyResult.ok && tooManyResult.reason).toBe(
			'monitor_query_seeds_too_many_entries'
		);

		const tooLong = fixtureSpec();
		tooLong.exclude_rules = ['y'.repeat(MAX_LIST_ENTRY_CHARS + 1)];
		const tooLongResult = validateAndNormalizeSpec(tooLong);
		expect(!tooLongResult.ok && tooLongResult.reason).toBe(
			'monitor_exclude_rules_entry_too_long'
		);
	});
});

describe('form → spec/schedule', () => {
	it('splits textarea line lists and builds a validatable spec', () => {
		const form = emptyMonitorForm();
		form.objective = 'Watch the Acme pricing page';
		form.urlsText = 'https://acme.example/pricing\n\n https://acme.example/plans ';
		form.excludeRulesText = 'blog posts';
		const spec = specFromForm(form);
		expect(spec.sources.urls).toEqual([
			'https://acme.example/pricing',
			'https://acme.example/plans'
		]);
		expect(spec.exclude_rules).toEqual(['blog posts']);
		const result = validateAndNormalizeSpec(spec);
		expect(result.ok).toBe(true);
	});

	it('splitLines drops blank and whitespace-only lines', () => {
		expect(splitLines(' a \n\n\r\n b\n')).toEqual(['a', 'b']);
	});

	it('builds the externally-tagged Cron schedule wire shape from presets', () => {
		const form = emptyMonitorForm();
		form.cadence = 'weekly-mon-9';
		form.timezone = 'UTC';
		const result = scheduleFromForm(form);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.schedule).toEqual({
				kind: { Cron: { expression: '0 9 * * 1', timezone: 'UTC' } }
			});
		}
	});

	it('supports custom cron, explicit no-schedule, and rejects malformed cron', () => {
		const custom = emptyMonitorForm();
		custom.cadence = 'custom';
		custom.cronExpression = '15 7 * * 2';
		const customResult = scheduleFromForm(custom);
		expect(customResult.ok && cadenceSummary(customResult.schedule)).toBe('Cron 15 7 * * 2');

		const none = emptyMonitorForm();
		none.cadence = 'none';
		const noneResult = scheduleFromForm(none);
		expect(noneResult.ok && noneResult.schedule).toBeNull();

		const missing = emptyMonitorForm();
		missing.cadence = 'custom';
		missing.cronExpression = '  ';
		const missingResult = scheduleFromForm(missing);
		expect(!missingResult.ok && missingResult.reason).toBe('monitor_schedule_required');

		const malformed = emptyMonitorForm();
		malformed.cadence = 'custom';
		malformed.cronExpression = 'every day at nine';
		const malformedResult = scheduleFromForm(malformed);
		expect(!malformedResult.ok && malformedResult.reason).toBe('monitor_schedule_invalid');
	});

	it('round-trips detail → form → spec for editing', () => {
		const spec = fixtureSpec();
		const schedule = { kind: { Cron: { expression: '0 9 * * 1', timezone: 'UTC' } } };
		const form = formFromSpec('Acme pricing watch', spec, schedule);
		expect(form.title).toBe('Acme pricing watch');
		expect(form.cadence).toBe('weekly-mon-9');
		expect(form.timezone).toBe('UTC');
		expect(specFromForm(form)).toEqual(spec);
	});

	it('round-trips notify_initial_baseline through form → spec → form (T4)', () => {
		// Opted-in baseline: spec → form → spec → normalized spec keeps true.
		const optedIn = { ...fixtureSpec(), notify_initial_baseline: true };
		const form = formFromSpec('Baseline watch', optedIn);
		expect(form.notifyInitialBaseline).toBe(true);
		const rebuilt = specFromForm(form);
		expect(rebuilt.notify_initial_baseline).toBe(true);
		const validated = validateAndNormalizeSpec(rebuilt);
		expect(validated.ok).toBe(true);
		if (validated.ok) expect(validated.spec.notify_initial_baseline).toBe(true);

		// Default quiet baseline survives the same loop as false.
		const quiet = fixtureSpec();
		expect(quiet.notify_initial_baseline).toBe(false);
		const quietForm = formFromSpec('Quiet watch', quiet);
		expect(quietForm.notifyInitialBaseline).toBe(false);
		const quietValidated = validateAndNormalizeSpec(specFromForm(quietForm));
		expect(quietValidated.ok && quietValidated.spec.notify_initial_baseline).toBe(false);
	});

	it('rejects an authenticated_sources-only form with monitor_sources_required (T4, backend parity)', () => {
		// Authenticated-source NAMES alone cannot scope a run in Phase 1 —
		// the backend admission gate 400s with monitor_sources_required and
		// this mirror must agree so the composer fails identically offline.
		const form = emptyMonitorForm();
		form.objective = 'Watch the signed-in dashboard';
		form.authenticatedText = 'acme-dashboard\nacme-billing';
		const result = validateAndNormalizeSpec(specFromForm(form));
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.reason).toBe('monitor_sources_required');
	});
});

describe('cadenceSummary (monitors_api mirror)', () => {
	it('matches the canonical list fixture cadence strings (I6 parity)', () => {
		// The fixture pins the IMPLEMENTATION format (`Cron <expr> (<tz>)`),
		// and this mirror must render the identical string for the same
		// schedule — list rows, chat preview, and the composer review card
		// all promise one summary.
		const fixture = JSON.parse(
			readFileSync(`${FIXTURES}monitor_list_page_v1.json`, 'utf8')
		) as { items: Array<{ cadence_summary: string }> };
		expect(
			cadenceSummary({
				kind: { Cron: { expression: '0 6 * * 1', timezone: 'America/Los_Angeles' } }
			})
		).toBe(fixture.items[0].cadence_summary);
		expect(
			cadenceSummary({
				kind: { Cron: { expression: '0 7 * * *', timezone: 'America/Los_Angeles' } }
			})
		).toBe(fixture.items[1].cadence_summary);
		expect(fixture.items[0].cadence_summary).toBe('Cron 0 6 * * 1 (America/Los_Angeles)');
		expect(fixture.items[1].cadence_summary).toBe('Cron 0 7 * * * (America/Los_Angeles)');
	});

	it('matches the server strings for every schedule kind', () => {
		expect(cadenceSummary(null)).toBe('unscheduled');
		expect(cadenceSummary(undefined)).toBe('unscheduled');
		expect(
			cadenceSummary({ kind: { Cron: { expression: '0 9 * * 1', timezone: 'UTC' } } })
		).toBe('Cron 0 9 * * 1 (UTC)');
		expect(cadenceSummary({ kind: { Cron: { expression: '0 9 * * 1' } } })).toBe(
			'Cron 0 9 * * 1'
		);
		// The schedule-level timezone is the fallback, like the server.
		expect(
			cadenceSummary({
				kind: { Cron: { expression: '0 9 * * 1' } },
				timezone: 'America/New_York'
			})
		).toBe('Cron 0 9 * * 1 (America/New_York)');
		expect(cadenceSummary({ kind: { Interval: { seconds: 3600 } } })).toBe('Every 3600s');
		expect(cadenceSummary({ kind: { Once: { at: '2026-08-01T09:00:00+00:00' } } })).toBe(
			'Once at 2026-08-01T09:00:00+00:00'
		);
		expect(cadenceSummary({ kind: { OnEvent: { event_pattern: 'mail.received' } } })).toBe(
			'On event mail.received'
		);
	});
});
