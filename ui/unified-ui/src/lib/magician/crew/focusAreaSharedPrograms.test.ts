import { describe, expect, it } from 'vitest';

import {
	applySharedPrograms,
	setSharedDocsMode,
	sharedDocsFromFocusArea,
	sharedDocumentOptions,
	sharedProgramsValue,
	toggleSharedDocument
} from './focusAreaSharedPrograms';

const area = { name: 'inbound sweep', description: 'Sweep inbound', program: 'outward_charter.md' };

describe('focus area shared programs', () => {
	it('reads an absent or null key as the global list and writes it back without the key', () => {
		for (const stored of [area, { ...area, shared_programs: null }]) {
			const docs = sharedDocsFromFocusArea(stored);
			expect(docs).toEqual({ mode: 'global', selected: [] });
			const saved = applySharedPrograms(stored, docs);
			expect('shared_programs' in saved).toBe(false);
			expect(saved).toEqual(area);
		}
	});

	it('reads [] as none and writes [] back', () => {
		const stored = { ...area, shared_programs: [] };
		const docs = sharedDocsFromFocusArea(stored);
		expect(docs).toEqual({ mode: 'none', selected: [] });
		expect(applySharedPrograms(stored, docs)).toEqual(stored);
	});

	it('reads a list as chosen documents and writes the same list back', () => {
		const stored = { ...area, shared_programs: ['facts.md', 'positioning.md'] };
		const docs = sharedDocsFromFocusArea(stored);
		expect(docs).toEqual({ mode: 'choose', selected: ['facts.md', 'positioning.md'] });
		expect(applySharedPrograms(stored, docs)).toEqual(stored);
	});

	it('switching modes changes only the shared_programs key', () => {
		const stored = { ...area, shared_programs: ['facts.md'] };
		const docs = sharedDocsFromFocusArea(stored);
		expect(applySharedPrograms(stored, setSharedDocsMode(docs, 'global'))).toEqual(area);
		expect(applySharedPrograms(stored, setSharedDocsMode(docs, 'none'))).toEqual({ ...area, shared_programs: [] });
		// The chosen names survive a detour through another mode.
		const back = setSharedDocsMode(setSharedDocsMode(docs, 'global'), 'choose');
		expect(sharedProgramsValue(back)).toEqual(['facts.md']);
	});

	it('saves [] when Choose documents has nothing checked', () => {
		const docs = toggleSharedDocument({ mode: 'choose', selected: ['facts.md'] }, 'facts.md', false);
		expect(docs.selected).toEqual([]);
		expect(sharedProgramsValue(docs)).toEqual([]);
		expect(applySharedPrograms({ ...area }, docs)).toEqual({ ...area, shared_programs: [] });
	});

	it('toggles names in order without duplicates', () => {
		let docs = setSharedDocsMode({ mode: 'global', selected: [] }, 'choose');
		docs = toggleSharedDocument(docs, 'facts.md', true);
		docs = toggleSharedDocument(docs, 'positioning.md', true);
		docs = toggleSharedDocument(docs, 'facts.md', true);
		expect(sharedProgramsValue(docs)).toEqual(['positioning.md', 'facts.md']);
	});

	it('lists program documents and keeps selected names that are not in the list', () => {
		const programs = [{ name: 'facts.md', title: 'Facts' }];
		expect(sharedDocumentOptions(programs, ['gone.md', 'facts.md'])).toEqual([
			{ name: 'facts.md', title: 'Facts', status: 'available' },
			{ name: 'gone.md', title: 'gone.md', status: 'missing' }
		]);
		expect(sharedDocumentOptions(null, ['gone.md'])).toEqual([
			{ name: 'gone.md', title: 'gone.md', status: 'unknown' }
		]);
	});
});
