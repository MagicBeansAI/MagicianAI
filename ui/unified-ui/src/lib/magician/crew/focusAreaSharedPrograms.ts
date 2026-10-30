/**
 * Per-focus-area shared program documents (`autonomous_config.focus_areas[].shared_programs`).
 *
 * The key has three distinct meanings that must survive a form round trip:
 * - absent (or `null`): the area uses the global `harness.shared_program_docs` list;
 * - `[]`: the area gets no shared documents;
 * - a non-empty list: these program files replace the global list for the area.
 */
export type SharedDocsMode = 'global' | 'none' | 'choose';

export interface FocusAreaSharedDocs {
	mode: SharedDocsMode;
	/** Chosen names; kept while another mode is selected so switching back restores them. */
	selected: string[];
}

export type SharedDocumentStatus = 'available' | 'missing' | 'unknown';

export interface SharedDocumentOption {
	name: string;
	title: string;
	status: SharedDocumentStatus;
}

export const SHARED_PROGRAMS_KEY = 'shared_programs';

function uniqueNames(values: unknown[]): string[] {
	const names: string[] = [];
	for (const value of values) {
		if (typeof value !== 'string') continue;
		const name = value.trim();
		if (name && !names.includes(name)) names.push(name);
	}
	return names;
}

export function globalSharedDocs(): FocusAreaSharedDocs {
	return { mode: 'global', selected: [] };
}

export function cloneSharedDocs(docs: FocusAreaSharedDocs): FocusAreaSharedDocs {
	return { mode: docs.mode, selected: [...docs.selected] };
}

/** Reads the shared-documents state of a stored focus area record. */
export function sharedDocsFromFocusArea(record: Record<string, unknown> | null | undefined): FocusAreaSharedDocs {
	const raw = record ? record[SHARED_PROGRAMS_KEY] : undefined;
	if (!Array.isArray(raw)) return globalSharedDocs();
	const selected = uniqueNames(raw);
	return selected.length > 0 ? { mode: 'choose', selected } : { mode: 'none', selected: [] };
}

/**
 * The value to store: `undefined` means omit the key (global list), `[]` means no
 * shared documents. "Choose" with nothing checked stores `[]`.
 */
export function sharedProgramsValue(docs: FocusAreaSharedDocs): string[] | undefined {
	if (docs.mode === 'global') return undefined;
	if (docs.mode === 'none') return [];
	return uniqueNames(docs.selected);
}

/** Writes the shared-documents state onto a focus area record, removing the key for "global". */
export function applySharedPrograms(
	focusArea: Record<string, unknown>,
	docs: FocusAreaSharedDocs
): Record<string, unknown> {
	const next = { ...focusArea };
	delete next[SHARED_PROGRAMS_KEY];
	const value = sharedProgramsValue(docs);
	if (value !== undefined) next[SHARED_PROGRAMS_KEY] = value;
	return next;
}

export function setSharedDocsMode(docs: FocusAreaSharedDocs, mode: SharedDocsMode): FocusAreaSharedDocs {
	return { mode, selected: [...docs.selected] };
}

export function toggleSharedDocument(docs: FocusAreaSharedDocs, name: string, checked: boolean): FocusAreaSharedDocs {
	const selected = docs.selected.filter((entry) => entry !== name);
	if (checked) selected.push(name);
	return { mode: docs.mode, selected };
}

/**
 * Checkbox rows for "Choose documents": the program list, then names that are
 * selected or were stored but are not in it. Those are `missing` once the list
 * has loaded, `unknown` while it has not (`programs === null`).
 */
export function sharedDocumentOptions(
	programs: ReadonlyArray<{ name: string; title: string }> | null,
	retainedNames: readonly string[]
): SharedDocumentOption[] {
	const options: SharedDocumentOption[] = (programs || []).map((program) => ({
		name: program.name,
		title: program.title || program.name,
		status: 'available'
	}));
	for (const name of uniqueNames([...retainedNames])) {
		if (options.some((option) => option.name === name)) continue;
		options.push({ name, title: name, status: programs ? 'missing' : 'unknown' });
	}
	return options;
}
