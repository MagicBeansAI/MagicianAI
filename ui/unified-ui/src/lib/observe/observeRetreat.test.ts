import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import {
	observeRetreatedSections,
	observeSectionMayRetreat,
	OBSERVE_CAPTURE_VISIBILITY_SECTIONS,
	OBSERVE_RETREAT_PAGE
} from './observeRetreat';

const observePage = readFileSync(
	join(process.cwd(), 'src/routes/(app)/observe/+page.svelte'),
	'utf8'
);
const slotRegion = readFileSync(
	join(process.cwd(), 'src/lib/apps/AppSlotRegion.svelte'),
	'utf8'
);

describe('the /observe retreat', () => {
	it('retires a duplicated section only once its pinned widget really rendered', () => {
		expect([...observeRetreatedSections(OBSERVE_RETREAT_PAGE, ['history'])])
			.toEqual(['recent_meetings']);
		expect([...observeRetreatedSections(OBSERVE_RETREAT_PAGE, [])]).toEqual([]);
		expect([...observeRetreatedSections(OBSERVE_RETREAT_PAGE, ['capture', 'reviews'])]).toEqual([]);
	});

	it('refuses a filled report from any other page', () => {
		// `history` is a slot name, not a global one: the same region on `/` is a
		// different slot, and its widget says nothing about this page.
		expect([...observeRetreatedSections('/', ['history'])]).toEqual([]);
		expect([...observeRetreatedSections('/apps/meetings/sessions', ['history'])]).toEqual([]);
	});

	it('never lets capture visibility retreat', () => {
		// The meetings increment pins capture visibility in two places at once —
		// the host-rendered TopBar dot and this page's own capture sections — and
		// an active capture must be visible in both. No widget can retire either.
		for (const section of OBSERVE_CAPTURE_VISIBILITY_SECTIONS) {
			expect(observeSectionMayRetreat(section)).toBe(false);
		}
		expect(observeSectionMayRetreat('anything_else')).toBe(false);
		expect(observeSectionMayRetreat('recent_meetings')).toBe(true);
	});

	it('mounts the slots the meetings package pins, in one batched region owner', () => {
		// Without these regions the package's system-default pins have nowhere to
		// land on web, which is the whole reason the retreat could not happen.
		expect(observePage).toContain("regions={['capture', 'reviews', 'history']}");
		expect(observePage.match(/<AppSlotRegion/g)).toHaveLength(1);
		expect(observePage).toContain('on:filled={(event) => (filledSlotRegions = event.detail.regions)}');
	});

	it('keeps the capture sections outside the retreat entirely', () => {
		// The capture renders are gated on activity alone; no retreat condition
		// may appear between the guard and the render.
		expect(observePage).toMatch(/\{#if anyActive\}\s*\{@render activeCaptures\(true\)\}/);
		expect(observePage).toMatch(/\{#if !anyActive\}\s*\{@render activeCaptures\(false\)\}/);
		expect(observePage).not.toMatch(/retreated[A-Za-z]*[^\n]*activeCaptures/);
	});

	it('never lets the retreat shrink what the page reports was captured', () => {
		// The retreat moves where meeting rows are rendered. The hero counter
		// must keep counting them, or the page under-reports capture.
		expect(observePage).toContain('$: recentCaptureCount = allRecentItems.length;');
		expect(observePage).toContain('$: recentItems = recentMeetingsRetreated');
	});

	it('counts only a widget that put records on the page as having filled a region', () => {
		// An `unavailable` or `unsupported` widget renders a placeholder — and so
		// does a `ready` one whose query projected no rows. Neither is a
		// replacement for the section it would retire, and retreating for the
		// empty one takes this page's own meeting rows away with it. The
		// predicate's own cases are pinned in AppSlotRegion.component.test.ts.
		expect(slotRegion).toContain('filter((item) => widgetFillsRegion(item.widget))');
		expect(slotRegion).toContain("if (widget.state !== 'ready') return false;");
	});
});
