import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const ROUTES = ['square/+page.svelte'] as const;

function routeSource(route: (typeof ROUTES)[number]): string {
	return readFileSync(join(process.cwd(), 'src/routes/(app)', route), 'utf8');
}

function functionBody(source: string, name: string, nextName: string): string {
	const start = source.indexOf(`async function ${name}`);
	const end = source.indexOf(`async function ${nextName}`, start + 1);
	expect(start).toBeGreaterThanOrEqual(0);
	expect(end).toBeGreaterThan(start);
	return source.slice(start, end);
}

function reactiveScopeChangeBody(source: string): string {
	const marker = '$: if (browser && todayMounted && currentTodayScopeKey !== lastTodayScopeKey)';
	const start = source.indexOf(marker);
	const end = source.indexOf('\n\t$: ', start + marker.length);
	expect(start).toBeGreaterThanOrEqual(0);
	expect(end).toBeGreaterThan(start);
	return source.slice(start, end);
}

describe.each(ROUTES)('%s attention routing', (route) => {
	it('prefers payload-bearing Today targets over source and task navigation', () => {
		const body = functionBody(routeSource(route), 'openTodayItem', 'openTaskRoute');
		const targetIndex = body.indexOf('todayHitlOpenTarget(item)');
		const sourceNavigationIndex = body.indexOf('goto(sourceUrl');
		const taskNavigationIndex = body.indexOf('openTaskRoute(item.task_id)');
		expect(targetIndex).toBeGreaterThanOrEqual(0);
		expect(targetIndex).toBeLessThan(sourceNavigationIndex);
		expect(targetIndex).toBeLessThan(taskNavigationIndex);
	});

	it('prefers payload-bearing activity targets over type and task navigation', () => {
		const body = functionBody(routeSource(route), 'openActivityItem', 'removeActivityItem');
		const targetIndex = body.indexOf('hitlOpenTargetFromFeedItem(item)');
		const typeNavigationIndex = body.indexOf("item.item_type === 'agent_learning'");
		const taskNavigationIndex = body.indexOf('openTaskRoute(item.task_id)');
		expect(targetIndex).toBeGreaterThanOrEqual(0);
		expect(targetIndex).toBeLessThan(typeNavigationIndex);
		expect(targetIndex).toBeLessThan(taskNavigationIndex);
	});

	it('opens the Attention center when the legacy route cannot be opened', () => {
		const body = functionBody(routeSource(route), 'openTodayAttentionItem', 'openTodayItem');
		expect(body).toMatch(
			/if \(!openAttentionRoute\(item\.source_url, todayAttentionItemId\(item\)\)\)\s*\{\s*openAttentionCenter\(\)/
		);
	});

	it('reloads message follow-ups immediately after a scope reset', () => {
		const body = reactiveScopeChangeBody(routeSource(route));
		const resetIndex = body.indexOf('clearTodayScopeState()');
		const reloadIndex = body.indexOf('loadTodayChannelFollowUps(1)');
		expect(resetIndex).toBeGreaterThanOrEqual(0);
		expect(reloadIndex).toBeGreaterThan(resetIndex);
	});
});
