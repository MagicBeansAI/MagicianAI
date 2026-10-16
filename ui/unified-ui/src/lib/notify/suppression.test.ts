import { describe, expect, it } from 'vitest';

import {
	createActionableNotificationSuppression,
	type NotificationSuppressionStorage
} from './suppression';

function memoryStorage(): NotificationSuppressionStorage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => values.set(key, value)
	};
}

describe('actionable notification suppression', () => {
	it('persists a dismissed correlation id across instances', () => {
		const storage = memoryStorage();
		createActionableNotificationSuppression(storage).dismiss('pause-1');
		expect(createActionableNotificationSuppression(storage).has('pause-1')).toBe(true);
	});

	it('does not suppress a resurfaced notification with a new id', () => {
		const suppression = createActionableNotificationSuppression(memoryStorage());
		suppression.dismiss('pause-1');
		expect(suppression.has('pause-1')).toBe(true);
		expect(suppression.has('pause-2')).toBe(false);
	});

	it('fails open when persisted state is corrupt', () => {
		const suppression = createActionableNotificationSuppression({
			getItem: () => '{broken',
			setItem: () => {}
		});
		expect(suppression.has('pause-1')).toBe(false);
	});
});
