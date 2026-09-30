/** Display label for a raw tool id. Shared by the Armory and the agent dock. */
export function capabilityLabel(value: string): string {
	return value
		.replace(/^mcp__[^_]+__/, '')
		.replace(/[_-]+/g, ' ')
		.replace(/\b\w/g, (letter) => letter.toUpperCase());
}

/** Coarse grouping for a tool id. Shared by the Armory and the agent dock. */
export function capabilityCategory(value: string): string {
	const lower = value.toLowerCase();
	if (/browser|web|search|fetch/.test(lower)) return 'Research and web';
	if (/file|shell|git|code|exec|test/.test(lower)) return 'Engineering';
	if (/mail|whatsapp|telegram|slack|notify/.test(lower)) return 'Communication';
	if (/memory|analytics|sql|duck|data/.test(lower)) return 'Knowledge and data';
	return 'General operations';
}
