import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import DecisionRoutingPanel from './DecisionRoutingPanel.svelte';
import { installFetchMock, jsonResponse } from '../../test/browser';
import { setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';

afterEach(() => { cleanup(); setCurrentScopeBearerToken(null); });
const fixture = () => ({contract_version: 4, revision:'r1', pending:false, enabled:true,
 models: [{name:'jev',model:'jev-1',remote:true},{name:'kev-4b',model:'kev-4b',remote:false}],
 operations:[{name:'memory_applicability', routing:{local:{primary:'kev-4b',backup:null},cloud:{primary:'jev',backup:'kev-4b'}},
 configured_local:['kev-4b'],configured_cloud:['jev','kev-4b'],active_local:['kev-4b'],active_cloud:['jev','kev-4b'],allow_remote_when_local:false,
 threshold_names:['apply'],thresholds_by_model:{jev:{apply:.7},'kev-4b':{apply:.75}}}]
});
function setup(conflict = false) {
 setCurrentScopeBearerToken('mst_test_routing');
 const view = fixture();
 return installFetchMock([
  {method:'GET',match:'/plane/decision-routing',handle:()=>jsonResponse(view)},
  {method:'PUT',match:'/plane/decision-routing',handle:({init})=> {
   if (conflict) return jsonResponse({message:'Settings changed elsewhere. Reload before saving.'},{status:409});
   const update = JSON.parse(String(init?.body));
   return jsonResponse({...view, revision:'r2', pending:true, operations:[{...view.operations[0], ...update}]});
  }}
 ]);
}
describe('Decision model operation mappings',()=>{
 it('saves Kev primary with Jev backup and independent local mapping',async()=>{
  const {calls}=setup(); render(DecisionRoutingPanel);
  await screen.findByLabelText('Cloud primary');
  await fireEvent.change(screen.getByLabelText('Cloud backup'),{target:{value:''}});
  await fireEvent.change(screen.getByLabelText('Cloud primary'),{target:{value:'kev-4b'}});
  await fireEvent.change(screen.getByLabelText('Cloud backup'),{target:{value:'jev'}});
  await fireEvent.click(screen.getByRole('button',{name:'Save operation mapping'}));
  await screen.findByText(/waiting to become active/);
  const put=calls.find(c=>c.method==='PUT')!;
  const body=JSON.parse(String(put.init?.body));
  expect(body.routing.cloud).toEqual({primary:'kev-4b',backup:'jev'});
  expect(body.routing.local).toEqual({primary:'kev-4b',backup:null});
  expect(body.thresholds_by_model['kev-4b'].apply).toBe(.75);
  expect(screen.getByRole('button',{name:'Save operation mapping'})).toBeDisabled();
 });
 it('requires explicit remote permission for the local route',async()=>{
  setup(); render(DecisionRoutingPanel);
  await screen.findByLabelText('Local primary');
  await fireEvent.change(screen.getByLabelText('Local primary'),{target:{value:'jev'}});
  expect(screen.getByRole('button',{name:'Save operation mapping'})).toBeDisabled();
  await fireEvent.click(screen.getByRole('checkbox'));
  expect(screen.getByRole('button',{name:'Save operation mapping'})).toBeEnabled();
 });
 it('keeps edits and reports concurrent changes without claiming success',async()=>{
  setup(true); render(DecisionRoutingPanel);
  await screen.findByLabelText('Local primary');
  await fireEvent.click(screen.getByRole('button',{name:'Save operation mapping'}));
  await screen.findByRole('alert');
  expect(screen.getByRole('alert')).toHaveTextContent('changed elsewhere');
  expect(screen.queryByText('Decision routing configuration applied.')).not.toBeInTheDocument();
 });
 it('does not hide service failures behind a default route',async()=>{
  setCurrentScopeBearerToken('mst_test_routing');
  installFetchMock([{match:'/plane/decision-routing',handle:()=>jsonResponse({message:'Service down'},{status:503})}]);
  render(DecisionRoutingPanel);
  await waitFor(()=>expect(screen.getByRole('alert')).toHaveTextContent('Service down'));
  expect(screen.queryByRole('button',{name:'Save operation mapping'})).not.toBeInTheDocument();
 });
});

it('saves Jev alone without inventing a fallback',async()=>{
 const {calls}=setup(); render(DecisionRoutingPanel);
 await screen.findByLabelText('Cloud backup');
 await fireEvent.change(screen.getByLabelText('Cloud backup'),{target:{value:''}});
 await fireEvent.click(screen.getByRole('button',{name:'Save operation mapping'}));
 await screen.findByText(/waiting to become active/);
 const body=JSON.parse(String(calls.find(c=>c.method==='PUT')?.init?.body));
 expect(body.routing.cloud).toEqual({primary:'jev',backup:null});
});

it('requires a complete threshold set when switching to another model',async()=>{
 setCurrentScopeBearerToken('mst_test_routing');
 const view=fixture();
 view.models.push({name:'laya',model:'laya',remote:false});
 installFetchMock([{match:'/plane/decision-routing',handle:()=>jsonResponse(view)}]);
 render(DecisionRoutingPanel);
 await screen.findByLabelText('Local primary');
 await fireEvent.change(screen.getByLabelText('Local primary'),{target:{value:'laya'}});
 expect(screen.getByRole('button',{name:'Save operation mapping'})).toBeDisabled();
 await fireEvent.input(screen.getByLabelText('laya: apply'),{target:{value:'.75'}});
 expect(screen.getByRole('button',{name:'Save operation mapping'})).toBeEnabled();
});
