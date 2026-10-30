import { describe, expect, it } from 'vitest';

import { noteBody } from './noteFrontMatter';

describe('noteBody', () => {
	it('hides the front matter Magician writes', () => {
		const note = '---\nmagician_kind: note\ncreated_at: 2026-10-21T09:00:00Z\n---\n\n# Weekly report\n\nBody.\n';
		expect(noteBody(note)).toBe('# Weekly report\n\nBody.\n');
	});

	it('handles CRLF and a `...` terminator', () => {
		expect(noteBody('---\r\nmagician_kind: note\r\n...\r\n\r\nBody\r\n')).toBe('Body\r\n');
	});

	it('allows a BOM, leading blank lines and trailing spaces on fences', () => {
		expect(noteBody('﻿\n  \n---  \ntitle: x\n---\t \nBody')).toBe('Body');
	});

	it('accepts quoted keys, comments and nested values', () => {
		expect(noteBody('---\n# generated\n"my key": 1\ntags:\n  - a\n---\nBody')).toBe('Body');
	});

	it('hides an empty block and a note that is only front matter', () => {
		expect(noteBody('---\n---\nBody')).toBe('Body');
		expect(noteBody('---\na: 1\n---')).toBe('');
	});

	it('leaves a note without front matter unchanged', () => {
		const note = '# Title\n\n---\n\nmore';
		expect(noteBody(note)).toBe(note);
		expect(noteBody('')).toBe('');
		expect(noteBody('---')).toBe('---');
	});

	it('leaves an unterminated block unchanged', () => {
		const note = '---\nmagician_kind: note\n# Title\nBody';
		expect(noteBody(note)).toBe(note);
	});

	it('keeps a leading rule followed by prose', () => {
		const note = '---\nJust a paragraph after a rule.\n---\nMore';
		expect(noteBody(note)).toBe(note);
	});

	it('needs the opening fence on the first line and exact closing fences', () => {
		const late = 'Intro\n---\na: 1\n---\nBody';
		expect(noteBody(late)).toBe(late);
		const fuzzy = '---\na: 1\n----\nb: 2\n--- x\n';
		expect(noteBody(fuzzy)).toBe(fuzzy);
	});
});
