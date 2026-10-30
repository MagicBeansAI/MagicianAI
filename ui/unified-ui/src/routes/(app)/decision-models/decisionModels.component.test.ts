import {afterEach,it,expect,vi} from 'vitest';
import {cleanup,render,screen,fireEvent,waitFor} from '@testing-library/svelte';
import {installFetchMock,jsonResponse} from '../../../test/browser';
import {setCurrentScopeBearerToken} from '$lib/stores/scopeIdentityStore';
vi.mock('$app/stores',async()=>{const {readable}=await import('svelte/store');return {page:readable({url:new URL('http://localhost/decision-models')})};});
import Page from './+page.svelte';
afterEach(()=>{cleanup();setCurrentScopeBearerToken(null);});
it('loads one bounded summary and loads cases only on request without faking quality',async()=>{
 setCurrentScopeBearerToken('mst_test_decisions');
 const {calls}=installFetchMock([
  {method:'GET',match:'/plane/decision-metrics/cases',handle:()=>jsonResponse({cases:[],next_before:null})},
  {method:'GET',match:'/plane/decision-metrics/health',handle:()=>jsonResponse({window_hours:24,since_ms:0,generated_at_ms:Date.now(),cases_scanned:0,unreadable_cases:0,truncated:false,operations:[],capture_dropped:0,write_errors:0,read_ms:1})},
  {method:'GET',match:'/plane/decision-metrics',handle:()=>jsonResponse({rows:[],truncated:false,collected_since_ms:null,generated_at_ms:Date.now(),last_comparison_ms:null,capture_dropped:0,write_errors:0,read_ms:1})}
 ]);
 render(Page);
 await screen.findByText(/No decision evidence collected yet/);
 expect(calls.filter(c=>c.url.includes('/plane/decision-metrics')&&!c.url.includes('/health')).length).toBe(1);
 await screen.findByText(/No decisions recorded in the last 24 h/);
 expect(calls.filter(c=>c.url.includes('/plane/decision-metrics/health?hours=24')).length).toBe(1);
 expect(calls.some(c=>c.url.includes('/cases'))).toBe(false);
 expect(screen.getByText(/No comparison or human review evidence/)).toBeInTheDocument();
 await fireEvent.click(screen.getByRole('button',{name:'Load recent cases'}));
 await waitFor(()=>expect(calls.some(c=>c.url.includes('/cases'))).toBe(true));
});
it('shows per-operation decision model health with a status and the last cause',async()=>{
 setCurrentScopeBearerToken('mst_test_decisions');
 const op={operation:'action_alignment',mode:'gate',cases:10,decided:2,unresolved:1,timeout:5,error:2,skipped:1,items:{offered:10,answered:3,eligible:2,failed:2,no_fitting_model:0,cancelled:0,not_started:5},p50_ms:3000,p95_ms:30500,budget_ms:30000,last_case_ms:Date.now(),last_error_ms:Date.now(),last_error:'timeout',causes:{timeout:5,item_failed:2},models:[{model:'jev-1.13.0',answered:3,p50_ms:400,p95_ms:900}],route_health:[]};
 installFetchMock([
  {method:'GET',match:'/plane/decision-metrics/health',handle:()=>jsonResponse({window_hours:24,since_ms:0,generated_at_ms:Date.now(),cases_scanned:11,unreadable_cases:0,truncated:false,operations:[op],capture_dropped:0,write_errors:0,read_ms:1})},
  {method:'GET',match:'/plane/decision-metrics',handle:()=>jsonResponse({rows:[],truncated:false,collected_since_ms:null,generated_at_ms:Date.now(),last_comparison_ms:null,capture_dropped:0,write_errors:0,read_ms:1})}
 ]);
 render(Page);
 const row=(await screen.findByRole('button',{name:'action alignment'})).closest('tr')!;
 expect(row.textContent).toMatch(/Failing/);
 expect(row.textContent).toMatch(/80% of decisions unresolved, timed out or errored/);
 expect(row.textContent).toMatch(/1 skipped/);
 expect(row.textContent).toMatch(/budget 30 s/);
 expect(row.querySelector('.cause')?.textContent).toBe('timeout');
});
