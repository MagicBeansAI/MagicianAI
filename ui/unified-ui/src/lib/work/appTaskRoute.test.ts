import { expect, it } from 'vitest';
import { appTaskHref } from './appTaskRoute';
it('opens the actual installation with exact internal execution return and rejects guessed provenance', () => {
 const id = 'task_app_'+ 'a'.repeat(64), origin = new URL('https://work.invalid/work?type=internal&selected='+id+'&execution_id=older');
 const href = new URL(appTaskHref({id,ui_thread_id:'app:town-square',created_by:'app_action'},origin),origin);
 expect(href.pathname).toBe('/apps/town-square'); expect(href.searchParams.get('from')).toBe(origin.pathname+origin.search);
 expect(new URL(appTaskHref({id,ui_thread_id:'app:town-square',created_by:'user'},origin),origin).pathname).toBe('/apps');
 expect(new URL(appTaskHref({id},origin,{task:{manifest:{ui_thread_id:'app:town-square',created_by:'app_action'}}}),origin).pathname).toBe('/apps/town-square');
 expect(new URL(appTaskHref({id,ui_thread_id:'app:../bad',created_by:'app_action'},origin),origin).pathname).toBe('/apps');
});
