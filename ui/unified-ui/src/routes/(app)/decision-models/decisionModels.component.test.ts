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
  {method:'GET',match:'/plane/decision-metrics',handle:()=>jsonResponse({rows:[],truncated:false,collected_since_ms:null,generated_at_ms:Date.now(),last_comparison_ms:null,capture_dropped:0,write_errors:0,read_ms:1})}
 ]);
 render(Page);
 await screen.findByText(/No decision evidence collected yet/);
 expect(calls.filter(c=>c.url.includes('/plane/decision-metrics')).length).toBe(1);
 expect(calls.some(c=>c.url.includes('/cases'))).toBe(false);
 expect(screen.getByText(/No comparison or human review evidence/)).toBeInTheDocument();
 await fireEvent.click(screen.getByRole('button',{name:'Load recent cases'}));
 await waitFor(()=>expect(calls.some(c=>c.url.includes('/cases'))).toBe(true));
});
