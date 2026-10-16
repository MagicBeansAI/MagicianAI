import { expect, it } from 'vitest';
import { readInternalTaskRoute, internalTaskWorkHref } from './internalTaskRoute';

it('round-trips the list and exact task/run while retaining the Work return context', () => {
 const state = {limit: 25, offset: 50, sort: 'title' as const, order: 'asc' as const, agent: 'writer / owner', status: 'failed', query: 'launch & review', selected: 'internal-1', execution: 'execution-2'};
 const href = internalTaskWorkHref(state, new URLSearchParams({from:'/work?view=crew&agent=writer', view:'crew', marker:'one'}));
 const params = new URL(href, 'http://localhost').searchParams;
 expect(readInternalTaskRoute(params)).toEqual(state);
 expect(params.get('from')).toBe('/work?view=crew&agent=writer');
 expect(params.get('marker')).toBe('one');
 expect(params.get('type')).toBe('internal');
 expect(params.has('view')).toBe(false);
});

it('bounds invalid pagination/sorting and drops an execution when the task closes', () => {
 const state = readInternalTaskRoute(new URLSearchParams('limit=999999&offset=-1&sort=invalid&order=invalid&execution_id=old'));
 expect(state).toMatchObject({limit:50,offset:0,sort:'updated_at',order:'desc'});
 expect(internalTaskWorkHref(state)).toBe('/work?type=internal');
});
