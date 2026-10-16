import { expect, it } from 'vitest';
import { workSpecialistHref, isWorkSpecialistPath, carryWorkSpecialistContext } from './specialistRoutes';

it('sends supported filters and exact task/run return context to analytics', () => {
 const origin = new URL('http://localhost/work?thread=launch&selected=task-1&execution_id=run-2');
 const target = new URL(workSpecialistHref('/llm', origin, {agent:'writer / owner',task:'task-1'}),origin);
 expect(target.hash).toBe('#usage');
 expect(target.searchParams.get('agent_id')).toBe('writer / owner');
 expect(target.searchParams.get('task_id')).toBe('task-1');
 expect(target.searchParams.get('from')).toBe(origin.pathname+origin.search);
});
it('keeps the return item through a token or App subpage without replacing explicit context', () => {
 const origin = new URL('http://localhost/budget?from=%2Fwork%3Ftype%3Dinternal%26selected%3Dapp-run%26execution_id%3Dold');
 const target = new URL('http://localhost/budget/tokens/token%2Fone?tab=audit#details');
 const result = new URL(carryWorkSpecialistContext(origin,target)!, origin);
 expect(result.searchParams.get('from')).toBe('/work?type=internal&selected=app-run&execution_id=old');
 expect(result.searchParams.get('tab')).toBe('audit'); expect(result.hash).toBe('#details');
 expect(carryWorkSpecialistContext(origin,new URL('http://localhost/apps/install-1'))).toContain('from=');
 expect(carryWorkSpecialistContext(origin,new URL('https://external.example/apps/install-1'))).toBeNull();
 expect(carryWorkSpecialistContext(origin,new URL('http://localhost/budget?from=%2Fwork%3Fselected%3Dother'))).toBeNull();
 expect(carryWorkSpecialistContext(origin,new URL('http://localhost/chat'))).toBeNull();
});
it('keeps workspace tools honest about their scope and carries return across specialist navigation', () => {
 const target = new URL(workSpecialistHref('/budget',new URL('http://localhost/history?from=%2Fwork%3Fview%3Dcrew%26agent%3Dwriter'),{agent:'writer',task:'task-1'}),'http://localhost');
 expect(target.searchParams.has('agent_id')).toBe(false);
 expect(target.searchParams.has('task_id')).toBe(false);
 expect(target.searchParams.get('from')).toBe('/work?view=crew&agent=writer');
 expect(isWorkSpecialistPath('/budget/tokens/token-1')).toBe(true);
 expect(isWorkSpecialistPath('/budget/unknown')).toBe(false);
});

it('carries the actual Events task, member and run filters', () => {
 const origin = new URL('http://localhost/work?type=internal&selected=app-task&execution_id=run-1');
 const target = new URL(workSpecialistHref('/events', origin, {agent:'forge', task:'app-task', execution:'run-1'}), origin);
 expect(target.searchParams.get('agent_id')).toBe('forge');
 expect(target.searchParams.get('task_id')).toBe('app-task');
 expect(target.searchParams.get('execution_id')).toBe('run-1');
 expect(target.searchParams.get('from')).toBe(origin.pathname + origin.search);
});
