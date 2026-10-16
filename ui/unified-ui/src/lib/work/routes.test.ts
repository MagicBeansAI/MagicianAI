import { describe, expect, it } from 'vitest';
import { legacyTasksWorkHref, legacyRelentlessWorkHref, resolveWorkView, taskQualityHref, workQualityHref, workReturnHref, workTaskHref, workAgentHref, workSelectionKey, workCrewEditorHref, workCrewRulesHref, workMemoryTierHref } from './routes';
import { canonicalWorkHref, resolvedWorkTaskHref, isTaskSelectionHref } from './routes';

describe('Work navigation compatibility', () => {
	it.each(['task', 'run', 'proposal', 'campaign'])('reloads Quality when its %s deep-link target changes', key => {
		const first = new URLSearchParams({ view: 'quality', [key]: 'first' });
		const second = new URLSearchParams({ view: 'quality', [key]: 'second' });
		expect(workSelectionKey(first, 'tasks')).not.toBe(workSelectionKey(second, 'tasks'));
	});
	it('keeps a task inspector mounted while its selection, filters or plan focus change', () => {
		expect(workSelectionKey(new URLSearchParams('selected=one&filter=running'), 'tasks'))
			.toBe(workSelectionKey(new URLSearchParams('selected=two&filter=all&plan=open'), 'tasks'));
	});
	it('preserves every task, run, plan, monitor-update and repeated legacy parameter', () => {
		const params = new URLSearchParams('type=monitors&selected=task%2F1&update=update%2F2&execution=run%3A3&act=output&plan=open&tag=a&tag=b&compose=1');
		const result = new URL(legacyTasksWorkHref(params), 'https://work.invalid');
		expect(result.pathname).toBe('/work');
		expect([...result.searchParams]).toEqual([...params]);
	});
	it('preserves all legacy quality targets, encoded values, return context and repeated filters', () => {
		const params = new URLSearchParams({ task: 'task/1', run: 'run:2', proposal: 'proposal&3', campaign: 'campaign 4', from: '/work?selected=task%2F1&plan=open' });
		params.append('tag', 'a');
		params.append('tag', 'b');
		const target = new URL(legacyRelentlessWorkHref(params), 'https://work.invalid');
		expect(target.pathname).toBe('/work');
		expect(target.searchParams.get('view')).toBe('quality');
		target.searchParams.delete('view');
		expect([...target.searchParams]).toEqual([...params]);
	});
	it('keeps internal and monitor identities separate', () => {
		expect(workTaskHref('same/id', 'internal')).toBe('/work?type=internal&selected=same%2Fid');
		expect(workTaskHref('same/id', 'monitors')).toBe('/work?type=monitors&selected=same%2Fid');
	});
	it('falls back to All work for an unknown view without choosing a specialist destination', () => {
		expect(resolveWorkView(new URLSearchParams('view=unknown'))).toBe('all');
	});
	it('returns from quality to the original selected task and deep inspector', () => {
		const origin = new URL('https://work.invalid/work?selected=task%2F1&plan=open&filter=running');
		const target = new URL(taskQualityHref('task/1', origin), origin);
		expect(target.searchParams.get('task')).toBe('task/1');
		expect(workReturnHref(target.searchParams.get('from'))).toBe(origin.pathname + origin.search);
	});
	it('keeps the original work item while following nested quality targets', () => {
		const from = '/work?selected=task%2F1&execution=run%3A2&plan=open&filter=running';
		const origin = new URL(`https://work.invalid${workQualityHref({ campaign: 'campaign/1' }, new URL(`https://work.invalid${from}`))}`);
		const target = new URL(workQualityHref({ run: 'run&3', campaign: 'campaign/1' }, origin), origin);
		expect(target.searchParams.get('run')).toBe('run&3');
		expect(target.searchParams.get('campaign')).toBe('campaign/1');
		expect(target.searchParams.get('from')).toBe(from);
	});
	it.each(['', 'https://outside.example/work', '/work/other'])('does not add a recursive or unsafe return path when moving within Quality: %s', from => {
		const origin = new URL('https://work.invalid/work?view=quality&campaign=one');
		if (from) origin.searchParams.set('from', from);
		const target = new URL(workQualityHref({ task: 'task/1' }, origin), origin);
		expect(target.searchParams.has('from')).toBe(false);
	});
	it('opens the responsible crew member without losing the selected task and run', () => {
		const origin = new URL('https://work.invalid/work?selected=task%2F1&execution=run%3A2&filter=running');
		const target = new URL(workAgentHref('owner/id', origin), origin);
		expect(target.searchParams.get('agent')).toBe('owner/id');
		expect(workReturnHref(target.searchParams.get('from'))).toBe(origin.pathname + origin.search);
	});
	it.each(['https://outside.example/work', '//outside.example/work', '/workspace', '/work/other', 'javascript:alert(1)'])('rejects a return link outside Work: %s', value => {
		expect(workReturnHref(value)).toBeNull();
	});
	it.each(['/work/crew/new', '/work/crew/owner%2Fone/edit', '/work/crew/owner/rules', '/work/crew/owner/memory/goal?goal_id=goal%2Fone'])('allows a known Work deep page as a return destination: %s', href => {
		expect(workReturnHref(href)).toBe(href);
	});
	it.each(['/work/crew', '/work/crew/x/delete', '/work/crew/x/memory/tier/extra', '/work/crew/x/../../outside', '/work?from=https%3A%2F%2Foutside.example%2Fwork', '/work?from=%2Fwork&from=%2Fwork'])('rejects unknown or unsafe nested Work destinations: %s', href => {
		expect(workReturnHref(href)).toBeNull();
	});
	it('bounds nested return chains', () => {
		let href = '/work?selected=original';
		for (let i = 0; i < 5; i++) href = `/work?${new URLSearchParams({ from: href })}`;
		expect(workReturnHref(href)).toBeNull();
	});
	it('keeps member, tier, goal and originating task identities in deep routes', () => {
		const origin = new URL('https://work.invalid/work?selected=task%2Fone&execution=run%3Atwo&plan=open');
		for (const href of [workCrewEditorHref('owner/one', origin), workCrewRulesHref('owner/one', origin), workMemoryTierHref('owner/one', 'goal/tier', 'goal&one', origin)]) {
			const target = new URL(href, origin);
			expect(target.pathname).toContain('/work/crew/owner%2Fone/');
			expect(target.searchParams.get('from')).toBe(origin.pathname + origin.search);
		}
		expect(new URL(workMemoryTierHref('owner', 'goal', 'goal&one'), origin).searchParams.get('goal_id')).toBe('goal&one');
	});

});


it('opens thread Work with selected run, composer and repeated query values', () => {
 const target = new URL(canonicalWorkHref('/t/Launch/tasks?selected=task-1&execution_id=run-2&plan=open&compose=1&view=crew&type=internal&tag=a&tag=b'), 'https://work.invalid');
 expect(target.pathname).toBe('/work');
 expect(target.searchParams.get('thread')).toBe('launch');
 expect(target.searchParams.has('view')).toBe(false); expect(target.searchParams.has('type')).toBe(false);
 expect(target.searchParams.get('selected')).toBe('task-1'); expect(target.searchParams.get('execution_id')).toBe('run-2');
 expect(target.searchParams.getAll('tag')).toEqual(['a','b']);
 expect(workSelectionKey(new URLSearchParams('thread=launch'), 'tasks')).not.toBe(workSelectionKey(new URLSearchParams('thread=other'), 'tasks'));
});

it('resolves untyped Today task links while keeping their exact execution and plan state', () => {
 const href = resolvedWorkTaskHref('task / one', '/work?type=internal&selected=task+%2F+one', '/work?selected=task+%2F+one&execution_id=older&plan=open&tag=a&tag=b');
 const url = new URL(href, 'https://work.invalid');
 expect(url.searchParams.get('type')).toBe('internal');
 expect(url.searchParams.get('execution_id')).toBe('older'); expect(url.searchParams.get('plan')).toBe('open');
 expect(url.searchParams.getAll('tag')).toEqual(['a','b']);
 expect(isTaskSelectionHref('/work?selected=one')).toBe(true);
 expect(isTaskSelectionHref('/work?view=quality&task=one')).toBe(false);
 expect(isTaskSelectionHref('/work?type=monitors&selected=one')).toBe(false);
 expect(isTaskSelectionHref('/tasks?type=monitors&selected=one&update=exact')).toBe(false);
 expect(isTaskSelectionHref('//external.example/work?selected=one')).toBe(false);
 expect(workReturnHref('/work/?selected=one')).toBe('/work/?selected=one');
});

it.each([
 ['/tasks?selected=task%2Fone&execution_id=old&plan=open&tag=a&tag=b#output', '/work?selected=task%2Fone&execution_id=old&plan=open&tag=a&tag=b#output'],
 ['/relentless?proposal=owner%2Fone&run=run%2Ftwo', '/work?proposal=owner%2Fone&run=run%2Ftwo&view=quality'],
 ['/crew/owner%2Fone?tab=memory', '/work?tab=memory&view=crew&agent=owner%2Fone'],
 ['/crew/new?edit=owner%2Fone', '/work/crew/owner%2Fone/edit'],
 ['/crew/owner/rules', '/work/crew/owner/rules'],
 ['/crew/owner/memory/goal?goal_id=launch', '/work/crew/owner/memory/goal?goal_id=launch'],
 ['/internal-tasks?selected=cycle&execution_id=old&view=crew', '/work?selected=cycle&execution_id=old&type=internal'],
 ['/debug/tasks?selected=cycle', '/work?selected=cycle&type=internal'],
 ['/harness?tag=a&tag=b', '/work?tag=a&tag=b&view=operations']
])('opens saved in-app context through Work: %s', (source, destination) => {
 expect(canonicalWorkHref(source)).toBe(destination);
});

it.each(['/work?view=crew', '/crew/owner/delete', '/api/magician/v3/tasks/one', '/tasks-archive', '/t/launch/chat', '/crew/%ZZ', '//external.example/tasks', 'https://external.example/tasks', '/\\external.example/tasks'])('leaves unrelated or unsafe links unchanged: %s', source => {
 expect(canonicalWorkHref(source)).toBe(source);
});
