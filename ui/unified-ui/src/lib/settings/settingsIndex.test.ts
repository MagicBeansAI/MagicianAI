import { describe, expect, it } from 'vitest';
import {
	SETTINGS_CATEGORIES,
	cardMatches,
	categoryForHash,
	normalizeSettingsQuery
} from './settingsIndex';

const card = (id: string) => {
	for (const category of SETTINGS_CATEGORIES) {
		const found = category.cards.find((entry) => entry.id === id);
		if (found) return found;
	}
	throw new Error(`no card ${id}`);
};

describe('settings index', () => {
	it('gives every category and card a unique id', () => {
		const ids = SETTINGS_CATEGORIES.flatMap((category) => [
			`category:${category.id}`,
			...category.cards.map((entry) => `card:${entry.id}`)
		]);
		expect(new Set(ids).size).toBe(ids.length);
		const cardIds = SETTINGS_CATEGORIES.flatMap((category) => category.cards.map((entry) => entry.id));
		const categoryIds = SETTINGS_CATEGORIES.map((category) => category.id);
		expect(cardIds.filter((id) => categoryIds.includes(id))).toEqual([]);
	});

	it('normalizes queries', () => {
		expect(normalizeSettingsQuery('  Text   TO speech ')).toBe('text to speech');
	});

	it('matches every card on an empty query', () => {
		expect(cardMatches(card('budgets'), '   ')).toBe(true);
	});

	it('matches on keywords people search for', () => {
		expect(cardMatches(card('budgets'), 'spend')).toBe(true);
		expect(cardMatches(card('god-mode'), 'Python')).toBe(true);
		expect(cardMatches(card('outward-actions'), 'whatsapp')).toBe(true);
		expect(cardMatches(card('budgets'), 'whatsapp')).toBe(false);
	});

	it('finds the versions card under About, last', () => {
		expect(SETTINGS_CATEGORIES.at(-1)?.id).toBe('about');
		expect(cardMatches(card('versions'), 'version')).toBe(true);
		expect(cardMatches(card('versions'), 'magicutor')).toBe(true);
		expect(cardMatches(card('versions'), 'build')).toBe(true);
		expect(cardMatches(card('budgets'), 'version')).toBe(false);
	});

	it('requires every query word', () => {
		expect(cardMatches(card('meet-calls'), 'google call')).toBe(true);
		expect(cardMatches(card('meet-calls'), 'google budget')).toBe(false);
	});

	it('matches the rendered text of a card', () => {
		expect(cardMatches(card('notes-folder'), 'fallback folder', 'New notes go to the fallback folder')).toBe(true);
	});

	it('maps hashes to categories', () => {
		expect(categoryForHash('#voice')).toBe('voice');
		expect(categoryForHash('#workspace')).toBe('account');
		expect(categoryForHash('#budgets')).toBe('agents');
		expect(categoryForHash('#browser-notifications-panel')).toBe('notifications');
		expect(categoryForHash('#tts-voices')).toBe('voice');
		expect(categoryForHash('#preferences')).toBe('agents');
		expect(categoryForHash('#outward-actions')).toBe('agents');
		expect(categoryForHash('#preference-checks')).toBe('agents');
		expect(categoryForHash('#versions')).toBe('about');
		expect(categoryForHash('#about')).toBe('about');
		expect(categoryForHash('#nope')).toBeNull();
		expect(categoryForHash('')).toBeNull();
	});
});
