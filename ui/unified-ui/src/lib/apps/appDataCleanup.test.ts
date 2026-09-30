import { describe,it,expect } from 'vitest';
import { cleanupCutoff,parseAppCleanupJob,type AppCleanupJob } from './appDataCleanup';

const job = ():AppCleanupJob => ({job_ref:'app-cleanup:test',installation_id:'install_test',status:'preview',
  selection:{entity:'post',timestamp_field:'created_at',before:'2020-01-01T00:00:00Z'},preview_digest:`blake3:${'a'.repeat(64)}`,
  matching_records:10001,matching_payload_bytes:200000,deleted_records:0,kept_changed_records:0,kept_referenced_records:0,remaining_records:10001,
  observed_at:'2026-09-11T00:00:00Z',expires_at:'2026-09-11T00:15:00Z',updated_at:'2026-09-11T00:00:00Z'});

describe('Owner app data cleanup',()=>{
  it('validates dates without rolling invalid days into another month',()=>{
    expect(new Date(cleanupCutoff('2024-02-29')).getDate()).toBe(29);
    for(const date of ['2025-02-29','2026-02-30','2026-13-01','','2026-1-01']) expect(()=>cleanupCutoff(date)).toThrow();
  });
  it('accepts a large selection but rejects mismatched scope and invalid accounting',()=>{
    expect(parseAppCleanupJob(job(),'install_test').matching_records).toBe(10001);
    expect(()=>parseAppCleanupJob(job(),'another_installation')).toThrow();
    for(const changed of [{remaining_records:10000},{deleted_records:-1},{status:'completed'},{preview_digest:'fake'}]) {
      expect(()=>parseAppCleanupJob({...job(),...changed},'install_test')).toThrow();
    }
  });
});
