import {cleanup,fireEvent,render,waitFor} from '@testing-library/svelte';
import {afterEach,beforeEach,expect,it,vi} from 'vitest';
import AppDataCleanup from './AppDataCleanup.svelte';
import type {AppCleanupClient,AppCleanupJob} from './appDataCleanup';

beforeEach(()=>{
  HTMLDialogElement.prototype.showModal=function(){this.setAttribute('open','');};
  HTMLDialogElement.prototype.close=function(){this.removeAttribute('open');};
});
afterEach(()=>cleanup());

function fixture(latest=false){
  let job:AppCleanupJob={job_ref:'app-cleanup:test',installation_id:'install_test',status:latest?'running':'preview',
    selection:{entity:'post',timestamp_field:'created_at',before:'2020-01-01T00:00:00Z'},preview_digest:`blake3:${'a'.repeat(64)}`,
    matching_records:130,matching_payload_bytes:20000,deleted_records:latest?64:0,kept_changed_records:0,kept_referenced_records:0,remaining_records:latest?66:130,
    observed_at:new Date().toISOString(),expires_at:new Date(Date.now()+15*60000).toISOString(),updated_at:new Date().toISOString()};
  const client:AppCleanupClient={
    options:vi.fn(async()=>({installation_id:'install_test',entities:[{entity:'post',timestamp_fields:['created_at']}],latest_job:latest?structuredClone(job):null})),
    preview:vi.fn(async()=>structuredClone(job)),
    control:vi.fn(async(_,operation)=>{job={...job,status:operation==='pause'?'paused':operation==='cancel'?'cancelled':'running'};return structuredClone(job);}),
    advance:vi.fn(async()=>{
      if(job.status==='checkpointing')job={...job,status:'completed'};
      else {const removed=Math.min(64,job.remaining_records);job={...job,deleted_records:job.deleted_records+removed,remaining_records:job.remaining_records-removed};
        if(!job.remaining_records)job.status='checkpointing';}
      return structuredClone(job);
    }),
  };
  return client;
}

it('does no maintenance work until opened and requires preview plus explicit confirmation before deletion',async()=>{
  const client=fixture();const changed=vi.fn();
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',initialEntity:'post',createClient:()=>client,onChanged:changed});
  expect(client.options).not.toHaveBeenCalled();
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Preview cleanup'})).toBeTruthy());
  await fireEvent.click(page.getByRole('button',{name:'Preview cleanup'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Delete selected older data'})).toBeDisabled());
  expect(client.control).not.toHaveBeenCalled();expect(client.advance).not.toHaveBeenCalled();
  await fireEvent.click(page.getByRole('checkbox'));
  await fireEvent.click(page.getByRole('button',{name:'Delete selected older data'}));
  await waitFor(()=>expect(page.getByText(/Cleanup complete/)).toBeTruthy());
  expect(client.control).toHaveBeenCalledTimes(1);expect(vi.mocked(client.control).mock.calls[0][1]).toBe('confirm');
  expect(client.advance).toHaveBeenCalledTimes(4);expect(changed).toHaveBeenCalledTimes(1);
  expect(page.getByText('130 deleted')).toBeTruthy();
});

it('reopens saved progress without silently restarting deletion',async()=>{
  const client=fixture(true);
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',createClient:()=>client});
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Continue cleanup'})).toBeTruthy());
  expect(client.advance).not.toHaveBeenCalled();expect(page.getByText('64 deleted')).toBeTruthy();
  await fireEvent.click(page.getByRole('button',{name:'Continue cleanup'}));
  await waitFor(()=>expect(page.getByText(/Cleanup complete/)).toBeTruthy());
  expect(client.control).not.toHaveBeenCalled();expect(client.advance).toHaveBeenCalledTimes(3);
});

it('shows preview failures without authorizing any deletion',async()=>{
  const client=fixture();vi.mocked(client.preview).mockRejectedValue(new Error('The workspace changed.'));
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',createClient:()=>client});
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Preview cleanup'})).toBeTruthy());
  await fireEvent.click(page.getByRole('button',{name:'Preview cleanup'}));
  await waitFor(()=>expect(page.getByRole('alert').textContent).toContain('The workspace changed.'));
  expect(client.control).not.toHaveBeenCalled();expect(client.advance).not.toHaveBeenCalled();
});

it('requires a fresh preview after the cutoff changes',async()=>{
  const client=fixture();
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',createClient:()=>client});
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Preview cleanup'})).toBeTruthy());
  await fireEvent.click(page.getByRole('button',{name:'Preview cleanup'}));
  await waitFor(()=>expect(page.getByRole('checkbox')).toBeTruthy());
  await fireEvent.click(page.getByRole('checkbox'));
  await fireEvent.input(page.getByLabelText('Before date'),{target:{value:'2021-01-01'}});
  expect(page.queryByRole('button',{name:'Delete selected older data'})).toBeNull();
  expect(client.control).not.toHaveBeenCalled();expect(client.advance).not.toHaveBeenCalled();
});

it('locks the preview while confirmation is in flight',async()=>{
  const client=fixture();const original=vi.mocked(client.control).getMockImplementation()!;
  let release!:()=>void;const gate=new Promise<void>(resolve=>release=resolve);
  vi.mocked(client.control).mockImplementation(async(...args)=>{await gate;return original(...args);});
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',createClient:()=>client});
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Preview cleanup'})).toBeTruthy());
  await fireEvent.click(page.getByRole('button',{name:'Preview cleanup'}));
  await waitFor(()=>expect(page.getByRole('checkbox')).toBeTruthy());
  await fireEvent.click(page.getByRole('checkbox'));
  await fireEvent.click(page.getByRole('button',{name:'Delete selected older data'}));
  expect(page.getByRole('button',{name:'Preview cleanup'})).toBeDisabled();
  expect(page.getByRole('button',{name:'Discard preview'})).toBeDisabled();
  expect(page.getByLabelText('Before date')).toBeDisabled();
  release();await waitFor(()=>expect(page.getByText(/Cleanup complete/)).toBeTruthy());
  expect(client.control).toHaveBeenCalledTimes(1);
});

it('finishes the database checkpoint when stopping partial cleanup without deleting more records',async()=>{
  const client=fixture(true);const changed=vi.fn();
  vi.mocked(client.control).mockImplementation(async(job,operation)=>{expect(operation).toBe('cancel');return {...job,status:'checkpointing'};});
  vi.mocked(client.advance).mockImplementation(async(job)=>({...job,status:'cancelled'}));
  const page=render(AppDataCleanup,{installationId:'install_test',appName:'Town Square',createClient:()=>client,onChanged:changed});
  await fireEvent.click(page.getByRole('button',{name:'Delete older data…'}));
  await waitFor(()=>expect(page.getByRole('button',{name:'Stop remaining cleanup'})).toBeTruthy());
  await fireEvent.click(page.getByRole('button',{name:'Stop remaining cleanup'}));
  await waitFor(()=>expect(page.getByText(/Cleanup stopped. 64 records were already deleted/)).toBeTruthy());
  expect(client.advance).toHaveBeenCalledTimes(1);expect(changed).toHaveBeenCalledTimes(1);
});
