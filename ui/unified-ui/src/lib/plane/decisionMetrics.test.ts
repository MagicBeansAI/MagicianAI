import {describe,it,expect} from 'vitest';
import {emptyStats,mergeStats,groupStats,percentile,rate,type DailyRow} from './decisionMetrics';
describe('decision metrics summaries',()=>{
 it('unknown evidence and missing histogram never become 0% or zero latency',()=>{expect(rate(0,0)).toBe('No evidence');expect(percentile([],0.95)).toBeNull();});
 it('shows sample denominator and interval even at perfect agreement',()=>{const text=rate(3,3);expect(text).toContain('3/3');expect(text).toContain('95% CI');expect(text).not.toContain('100.0–100.0');});
 it('merges known token/cache coverage separately from numeric totals',()=>{const s=emptyStats();s.cost_usd=.01;s.counts={calls:2,priced_calls:1,cache_read_known:0};const merged=mergeStats([{day:1,kind:'usage',cohort:'c',operation:'o',model:'m',identity:{},stats:s}]);expect(merged.counts.calls).toBe(2);expect(merged.counts.priced_calls).toBe(1);expect(merged.cache_read_tokens).toBe(0);expect(merged.counts.cache_read_known).toBe(0);});
 it('groups a maximum-sized summary without mixing cost categories or mutating rows',()=>{const rows:DailyRow[]=Array.from({length:2048},(_,i)=>({day:i%30,kind:'usage',cohort:`c${i%512}`,operation:'classify',model:'jev',identity:{category:i%2?'monitoring':'decision'},stats:{...emptyStats(),counts:{calls:1},cost_usd:.01}}));const groups=groupStats(rows);expect(groups).toHaveLength(512);expect(groups.every(g=>g.stats.counts.calls===4)).toBe(true);expect(mergeStats(groups.map(g=>({...g.row,stats:g.stats}))).counts.calls).toBe(2048);expect(rows[0].stats.counts.calls).toBe(1);});
});
