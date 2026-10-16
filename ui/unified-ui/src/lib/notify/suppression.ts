const STORAGE_KEY = 'magician.notify.dismissed-actionable.v1';

export interface NotificationSuppressionStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

export interface ActionableNotificationSuppression {
	has(correlationId: string): boolean;
	dismiss(correlationId: string): void;
}

export function createActionableNotificationSuppression(
	storage: NotificationSuppressionStorage | null
): ActionableNotificationSuppression {
	const dismissed = new Set<string>();
	if (storage) {
		try {
			const stored = JSON.parse(storage.getItem(STORAGE_KEY) ?? '[]');
			if (Array.isArray(stored)) {
				for (const value of stored) {
					if (typeof value === 'string' && value.trim()) dismissed.add(value.trim());
				}
			}
		} catch {
			// A corrupt or unavailable preference must not break notifications.
		}
	}

	return {
		has(correlationId) {
			const normalized = correlationId.trim();
			return normalized.length > 0 && dismissed.has(normalized);
		},
		dismiss(correlationId) {
			const normalized = correlationId.trim();
			if (!normalized || dismissed.has(normalized)) return;
			dismissed.add(normalized);
			if (!storage) return;
			try {
				storage.setItem(STORAGE_KEY, JSON.stringify([...dismissed]));
			} catch {
				// Keep the in-memory suppression even when persistence is unavailable.
			}
		}
	};
}

let defaultSuppression: ActionableNotificationSuppression | null = null;

function notificationSuppression(): ActionableNotificationSuppression {
	if (defaultSuppression) return defaultSuppression;
	let storage: NotificationSuppressionStorage | null = null;
	try {
		if (typeof window !== 'undefined') storage = window.localStorage;
	} catch {
		storage = null;
	}
	defaultSuppression = createActionableNotificationSuppression(storage);
	return defaultSuppression;
}

export function isActionableNotificationSuppressed(correlationId: string): boolean {
	return notificationSuppression().has(correlationId);
}

export function suppressActionableNotification(correlationId: string): void {
	notificationSuppression().dismiss(correlationId);
}
