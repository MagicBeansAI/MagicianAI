import { describe, expect, it } from 'vitest';
import { contextPreparationRow } from './contextPreparation';
import { llmStepUsageParts } from './llmStepUsage';

describe('context and model step measurements', () => {
	it('keeps partial and cancelled phases honest and rejects unmeasured durations', () => {
		expect(contextPreparationRow('other', {phase_id:'a'})).toBeNull();
		expect(contextPreparationRow('context.retrieval.completed', {})).toBeNull();
		expect(contextPreparationRow('context.retrieval.completed', {phase_id:'a',status:'partial',duration_ms:123,summary:'Memory search: 100 ms'})).toMatchObject({key:'context:a',label:'Context prepared (partial)',durationMs:123,status:'done'});
		expect(contextPreparationRow('context.retrieval.completed', {phase_id:'b',status:'cancelled',duration_ms:-1})).toMatchObject({label:'Context preparation cancelled',durationMs:null});
	});
	it('names a compaction phase and shows its exact progress', () => {
		expect(contextPreparationRow('context.retrieval.started', {phase_id:'c',label:'Compacting conversation',status:'running',step:1,steps:3})).toMatchObject({key:'context:c',label:'Compacting conversation · 1/3',status:'running'});
		expect(contextPreparationRow('context.retrieval.started', {phase_id:'c',label:'Compacting conversation',step:4,steps:3})).toMatchObject({label:'Compacting conversation'});
		expect(contextPreparationRow('context.retrieval.completed', {phase_id:'c',label:'Compacting conversation',done_label:'Compaction did not complete',status:'failed'})).toMatchObject({label:'Compaction did not complete',status:'failed'});
		expect(contextPreparationRow('context.retrieval.started', {phase_id:'d'})).toMatchObject({label:'Preparing context'});
		expect(contextPreparationRow('context.retrieval.completed', {phase_id:'c',label:'Compacting conversation',done_label:'Conversation compacted',status:'completed',duration_ms:42000})).toMatchObject({label:'Conversation compacted',status:'done',durationMs:42000});
	});
	it('shows Decision Engine input tokens with zero output and measured zero cost', () => {
		expect(llmStepUsageParts({input_tokens:1200,output_tokens:0,cost:0,cache_read_tokens:0,correlation:{usage_availability:{tokens:true,cost:true,cache_read:false}}})).toEqual(['1.2k tok','$0.000']);
	});
	it('keeps unknown usage absent and shows reported cache participation', () => {
		expect(llmStepUsageParts({input_tokens:0,output_tokens:0,cost:0,correlation:{usage_availability:{tokens:false,cost:false}}})).toEqual([]);
		expect(llmStepUsageParts({input_tokens:1000,output_tokens:50,cache_read_tokens:800,cost_usd:0.002})).toEqual(['1.1k tok','80% cached','$0.002']);
	});
});
