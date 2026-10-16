import { expect, it } from 'vitest';
import { readAnalyticsRoute, analyticsHref } from './analyticsRoute';
it('round-trips all supported analytics dimensions and preserves the Work origin', () => {
 const origin = new URL('http://localhost/llm?from=%2Fwork%3Fselected%3Dtask-1#usage');
 const state = {range:0,agent:"writer's assistant",task:'task-1',operation:'llm.call',model:'provider/model',chat:'chat-1',group:'task_id' as const,tab:'health' as const};
 const url = new URL(analyticsHref(state,origin),origin);
 expect(readAnalyticsRoute(url)).toEqual(state);
 expect(url.searchParams.get('from')).toBe('/work?selected=task-1');
});
it('bounds unsupported range/group inputs and retains historical cost anchors', () => {
 expect(readAnalyticsRoute(new URL('http://localhost/llm?range=Infinity&group_by=unknown#today'))).toMatchObject({range:7,group:'operation',tab:'cost'});
});
