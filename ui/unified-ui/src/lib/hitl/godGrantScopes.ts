import type { AttentionPromptChoice } from '$lib/stores/attentionPromptStore';

export interface GodGrantScope {
	id: string;
	label: string;
	detail: string;
	sessionId: string;
	alwaysId: string;
}

const LABELS: Record<string, string> = {
	file: 'These files', folder: 'Containing folders', top: 'Top directories', repository: 'This repository',
	exact: 'Exact command', mcp_tool: 'This action', capability: 'Whole tool', prefix: 'Command prefix', program: 'Program',
	host: 'This host', any_host: 'Any host', engine_repo: 'Engine and repository',
	engine: 'This engine', any_harness: 'Any harness'
};

/** Group only complete, server-offered pairs. Unknown shapes keep the original controls. */
export function godGrantScopes(options: AttentionPromptChoice[]): GodGrantScope[] {
	const lasting = options.filter(({ id }) => id !== 'allow_once' && id !== 'deny');
	if (!lasting.length || lasting.some(({ id }) => !/^(session|always):[^:]+$/.test(id))) return [];
	const result: GodGrantScope[] = [];
	for (const option of lasting.filter(({ id }) => id.startsWith('session:'))) {
		const id = option.id.slice('session:'.length);
		const always = lasting.find((candidate) => candidate.id === `always:${id}`);
		if (!always) return [];
		result.push({ id, label: LABELS[id] ?? id,
			detail: option.label.replace(/^This session:\s*/, ''),
			sessionId: option.id, alwaysId: always.id });
	}
	return result.length * 2 === lasting.length ? result : [];
}
