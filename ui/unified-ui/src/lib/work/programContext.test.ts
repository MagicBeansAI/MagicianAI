import { expect, it } from 'vitest';
import { programMember, referencesProgram, programMetrics } from './programContext';
it('associates exact scoped paths and inherited sections without equating similarly named documents', () => {
 const member = programMember({definition:{agent_id:'ceo',kind:'Personal',harness:{program_section:'Company'},autonomous_config:{focus_areas:[{name:'Delivery',program:'team/direction.md'},{name:'Review'}]}},version:2})!;
 expect(member.references).toEqual([{path:'program.md',section:'Company'},{path:'team/direction.md',focus:'Delivery',section:undefined},{path:'program.md',section:'Company',focus:'Review'}]);
 expect(referencesProgram(member,'direction.md')).toBe(false);
 member.overview = {context:{program:{relative_path:'external.md'}},evaluation:{program_metrics:{documents:[{relative_path:'external.md',metrics:[{metric_id:'one'}]},{relative_path:'other/external.md',metrics:[{metric_id:'wrong'}]}]}}};
 expect(referencesProgram(member,'external.md')).toBe(true);
 expect(programMetrics(member,'external.md')).toEqual([{metric_id:'one'}]);
 expect(programMember({definition:{agent_id:'worker',kind:'Worker',harness:{}}})).toBeNull();
 expect(programMember({definition:{agent_id:'personal',kind:'Personal'}})).toBeNull();
});
