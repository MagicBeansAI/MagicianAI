import { timedFetch } from '$lib/shared/fetch';

export interface ProgramReference { path: string; section?: string; focus?: string }
export interface ProgramMember {
 id: string; name: string; version: number | null; references: ProgramReference[];
 overview: Record<string, unknown> | null; error: string;
}
export function record(value: unknown): Record<string, unknown> {
 return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string,unknown> : {};
}
export function text(value: unknown): string { return typeof value === 'string' ? value : ''; }
function array(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
export function programMember(value: unknown): ProgramMember | null {
 const entry = record(value), definition = record(entry.definition), harness = record(definition.harness);
 const id = text(definition.agent_id);
 if (!id || definition.kind !== 'Personal' || !definition.harness || typeof definition.harness !== 'object' || Array.isArray(definition.harness)) return null;
 const section = text(harness.program_section).trim() || undefined;
 const references: ProgramReference[] = [{path:'program.md',section}];
 for (const raw of array(record(definition.autonomous_config).focus_areas)) {
  const focus = record(raw), path = text(focus.program).trim();
  references.push({path: path || 'program.md',focus: text(focus.name),section:path ? undefined : section});
 }
 return {id,name:text(definition.name) || id,version:typeof entry.version === 'number' ? entry.version : null,references,overview:null,error:''};
}
export function activeProgramState(member: ProgramMember): Record<string,unknown> {
 return record(record(member.overview?.evaluation).active_program_state);
}
export function referencesProgram(member: ProgramMember, name: string): boolean {
 const current = record(member.overview?.context).program;
 return member.references.some(ref => ref.path === name) || text(record(current).relative_path) === name;
}
export function programMetrics(member: ProgramMember, name: string): Array<Record<string,unknown>> {
 return array(record(record(member.overview?.evaluation).program_metrics).documents)
  .filter(raw => text(record(raw).relative_path) === name)
  .flatMap(raw => array(record(raw).metrics).map(record));
}
export async function listProgramMembers(signal: AbortSignal): Promise<ProgramMember[]> {
 const members: ProgramMember[] = []; let offset = 0;
 for (;;) {
  const response = await timedFetch(`/api/magician/v2/agents?${new URLSearchParams({limit:'100',offset:String(offset)})}`,{signal});
  if (!response.ok) throw new Error(`Could not load responsible crew (${response.status}).`);
  const result = record(await response.json()), rows = array(result.agents);
  members.push(...rows.map(programMember).filter((member): member is ProgramMember => member !== null));
  offset += rows.length;
  if (!rows.length || typeof result.total_count !== 'number' || offset >= result.total_count) return members;
 }
}
export async function readProgramMember(member: ProgramMember, signal: AbortSignal): Promise<ProgramMember> {
 try {
  const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(member.id)}/harness-overview`,{signal});
  if (!response.ok) throw new Error(`Playbook progress unavailable (${response.status}).`);
  return {...member,overview:record(await response.json())};
 } catch (error) {
  if (signal.aborted) throw error;
  return {...member,error:error instanceof Error ? error.message : String(error)};
 }
}
