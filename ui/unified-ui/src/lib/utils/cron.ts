/**
 * Shared cron expression utilities.
 *
 * Uses `cronstrue` for human-readable descriptions and provides basic
 * validation for 5-field cron expressions used across the Presto UI
 * (Spells schedule chips, Doubles autonomous-config preview, etc.).
 */

import cronstrue from 'cronstrue';

/** Convert a 5-field cron expression to a plain-English description. */
export function cronToHumanReadable(cron: string): string {
	const trimmed = cron.trim();
	if (!trimmed) return '';
	try {
		return cronstrue.toString(trimmed, { use24HourTimeFormat: true });
	} catch {
		return trimmed;
	}
}

/** Basic validation for a 5-field cron expression. */
export function isValidCronExpression(cron: string): boolean {
	const trimmed = cron.trim();
	if (!trimmed) return false;
	try {
		cronstrue.toString(trimmed);
		return true;
	} catch {
		return false;
	}
}
