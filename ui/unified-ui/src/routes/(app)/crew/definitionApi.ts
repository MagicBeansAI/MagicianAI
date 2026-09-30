import { timedFetch } from '$lib/shared/fetch';
export interface AgentDefinitionRecord {
	definition: Record<string, unknown>;
	version: number;
	etag: string;
	created_at?: string;
	updated_at?: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
	const value = payload[field];
	return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
}

function readNumber(payload: Record<string, unknown>, field: string): number | undefined {
	const value = payload[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function parseAgentRecord(
	raw: unknown,
	etagFromHeader?: string
): AgentDefinitionRecord | null {
	const root = asRecord(raw);
	if (!root) return null;
	const definition = asRecord(root.definition);
	if (!definition) return null;

	return {
		definition,
		version: readNumber(root, 'version') ?? 0,
		etag: readString(root, 'etag') || etagFromHeader || '',
		created_at: readString(root, 'created_at'),
		updated_at: readString(root, 'updated_at')
	};
}

async function readApiError(response: Response): Promise<string> {
	let message = `Request failed (${response.status})`;
	try {
		const text = await response.text();
		if (!text) return message;
		try {
			const parsed = JSON.parse(text) as unknown;
			const root = asRecord(parsed);
			const rootMessage = root ? readString(root, 'message') : undefined;
			const errorRecord = root ? asRecord(root.error) : null;
			const nestedMessage = errorRecord ? readString(errorRecord, 'message') : undefined;
			message = `Request failed (${response.status}): ${nestedMessage || rootMessage || text}`;
		} catch {
			message = `Request failed (${response.status}): ${text}`;
		}
	} catch {
		// Best effort only.
	}
	return message;
}

async function expectOk(response: Response): Promise<void> {
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}
}

export async function fetchAgentDefinitionRecord(
	agentId: string
): Promise<AgentDefinitionRecord | null> {
	const normalized = agentId.trim();
	if (!normalized) {
		throw new Error('agentId is required');
	}

	const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalized)}`);
	if (response.status === 404) {
		return null;
	}
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
	if (!record) {
		throw new Error('Malformed agent record payload');
	}
	return record;
}

export async function createAgentFromYaml(yamlText: string): Promise<AgentDefinitionRecord> {
	const response = await timedFetch('/api/magician/v2/agents', {
		method: 'POST',
		headers: {
			'Content-Type': 'application/yaml'
		},
		body: yamlText
	});
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
	if (!record) {
		throw new Error('Malformed create agent response');
	}
	return record;
}

export async function updateAgentFromYaml(
	agentId: string,
	yamlText: string,
	ifMatch: string
): Promise<AgentDefinitionRecord> {
	const normalized = agentId.trim();
	if (!normalized) {
		throw new Error('agentId is required');
	}
	if (!ifMatch) {
		throw new Error('If-Match/etag is required for YAML update');
	}

	const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalized)}`, {
		method: 'PUT',
		headers: {
			'Content-Type': 'application/yaml',
			'If-Match': ifMatch
		},
		body: yamlText
	});
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
	if (!record) {
		throw new Error('Malformed update agent response');
	}
	return record;
}

/**
 * JSON Merge Patch (RFC 7386) against the agent definition. `null` removes a
 * key. `ifMatch` is the record's etag; a stale one returns 412.
 */
export async function patchAgentDefinition(
	agentId: string,
	patch: Record<string, unknown>,
	ifMatch: string
): Promise<AgentDefinitionRecord> {
	const normalized = agentId.trim();
	if (!normalized) {
		throw new Error('agentId is required');
	}
	if (!ifMatch) {
		throw new Error('If-Match/etag is required for a definition patch');
	}
	const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalized)}`, {
		method: 'PATCH',
		headers: {
			'Content-Type': 'application/json',
			'If-Match': ifMatch
		},
		body: JSON.stringify(patch)
	});
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
	if (!record) {
		throw new Error('Malformed patch agent response');
	}
	return record;
}

function yamlScalar(value: unknown): string {
	const serialized = JSON.stringify(value);
	return serialized === undefined ? 'null' : serialized;
}

function isScalar(value: unknown): boolean {
	return (
		value === null
		|| typeof value === 'string'
		|| typeof value === 'number'
		|| typeof value === 'boolean'
	);
}

function stringifyYaml(value: unknown, indent: number): string[] {
	const pad = ' '.repeat(indent);
	if (Array.isArray(value)) {
		if (value.length === 0) {
			return [`${pad}[]`];
		}
		const lines: string[] = [];
		for (const item of value) {
			if (isScalar(item)) {
				lines.push(`${pad}- ${yamlScalar(item)}`);
			} else {
				lines.push(`${pad}-`);
				lines.push(...stringifyYaml(item, indent + 2));
			}
		}
		return lines;
	}

	const record = asRecord(value);
	if (!record) {
		return [`${pad}${yamlScalar(value)}`];
	}

	const entries = Object.entries(record);
	if (entries.length === 0) {
		return [`${pad}{}`];
	}

	const lines: string[] = [];
	for (const [key, entryValue] of entries) {
		const serializedKey = JSON.stringify(key);
		if (isScalar(entryValue)) {
			lines.push(`${pad}${serializedKey}: ${yamlScalar(entryValue)}`);
			continue;
		}
		if (Array.isArray(entryValue) && entryValue.length === 0) {
			lines.push(`${pad}${serializedKey}: []`);
			continue;
		}
		if (asRecord(entryValue) && Object.keys(entryValue as Record<string, unknown>).length === 0) {
			lines.push(`${pad}${serializedKey}: {}`);
			continue;
		}
		lines.push(`${pad}${serializedKey}:`);
		lines.push(...stringifyYaml(entryValue, indent + 2));
	}
	return lines;
}

export function serializeDefinitionYaml(definition: Record<string, unknown>): string {
	return `${stringifyYaml(definition, 0).join('\n')}\n`;
}

export const DEFAULT_NEW_AGENT_YAML = `name: Demo Agent
description: Example agent definition
persona: You are a practical autonomous assistant.
trust_level: local
kind: Personal
tools:
  - browser
  - files
excluded_tools: []
delegation_targets: []
`;
