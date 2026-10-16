import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { findReferences, readReference } from './references';
import { setCurrentScopeBearerToken, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

const hit={provider:'local',relative_path:'facts.md',source_ref:'notes:local:facts.md',title:'Facts',modified_at_ms:1,matched_in_title:true,match_count:1,matches:[]};
const json=(data:unknown)=>new Response(JSON.stringify(data),{headers:{'content-type':'application/json'}});
beforeEach(()=>{scopeIdentityStore.observe('owner','workspace');setCurrentScopeBearerToken('synthetic-session');});
afterEach(()=>{vi.unstubAllGlobals();setCurrentScopeBearerToken('');scopeIdentityStore.reset();});

it('searches the provider read by the file endpoint and sends scoped headers',async()=>{
  const fetch=vi.fn(async(url:unknown,_init?:RequestInit)=>String(url).endsWith('/tree')?json({provider:'local'}):json({hits:[hit]}));
  vi.stubGlobal('fetch',fetch);
  const signal=new AbortController().signal;
  await findReferences('fact sheet',signal);
  expect(JSON.parse(fetch.mock.calls[1][1]?.body as string)).toEqual({query:'fact sheet',provider:'local',limit:12});
  const headers=fetch.mock.calls[1][1]?.headers as Headers;
  expect(headers.get('Authorization')).toBe('Bearer synthetic-session');
  expect(headers.has('X-Principal')).toBe(false);
  expect(headers.has('X-Workspace')).toBe(false);
});

it('rejects a reference if its provider changed before reading',async()=>{
  vi.stubGlobal('fetch',vi.fn(async()=>json({provider:'another',relative_path:'facts.md',title:'Facts',markdown:'Wrong provider bytes'})));
  await expect(readReference(hit,new AbortController().signal)).rejects.toThrow('provider changed');
});

it('discards a reference after a credential rotation',async()=>{
  let release:(response:Response)=>void=()=>{};
  vi.stubGlobal('fetch',vi.fn(()=>new Promise<Response>(resolve=>{release=resolve;})));
  const pending=readReference(hit,new AbortController().signal);
  setCurrentScopeBearerToken('synthetic-rotated-session');
  release(json({provider:'local',relative_path:'facts.md',title:'Facts',markdown:'Private old-session bytes'}));
  await expect(pending).rejects.toThrow('Workspace changed');
});
