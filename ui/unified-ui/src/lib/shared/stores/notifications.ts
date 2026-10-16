// Notification system store
import { writable } from 'svelte/store';

export type NotificationType = 'error' | 'warning' | 'info' | 'success';

export interface Notification {
	id: string;
	type: NotificationType;
	title: string;
	message?: string;
	timestamp: number;
	duration?: number; // Auto-dismiss after N milliseconds (0 = no auto-dismiss)
	action?: { label: string; href: string };
}

interface NotificationState {
	notifications: Notification[];
}

const initialState: NotificationState = {
	notifications: []
};

function createNotificationStore() {
	const { subscribe, update } = writable<NotificationState>(initialState);

	let nextId = 1;

	function addNotification(
		type: NotificationType,
		title: string,
		message?: string,
		duration: number = 5000,
		action?: Notification['action']
	) {
		const id = `notification-${nextId++}`;
		const notification: Notification = {
			id,
			type,
			title,
			message,
			timestamp: Date.now(),
			duration,
			action
		};

		update((state) => ({
			notifications: [...state.notifications, notification]
		}));

		// Auto-dismiss if duration is set
		if (duration > 0) {
			setTimeout(() => {
				removeNotification(id);
			}, duration);
		}

		return id;
	}

	function removeNotification(id: string) {
		update((state) => ({
			notifications: state.notifications.filter((n) => n.id !== id)
		}));
	}

	return {
		subscribe,
		addNotification,
		removeNotification,

		/**
		 * Clear all notifications
		 */
		clear() {
			update((state) => ({
				notifications: []
			}));
		}
	};
}

export const notifications = createNotificationStore();

/**
 * Helper functions for common notification types
 */

export function showError(
	title: string,
	message?: string,
	duration: number = 8000,
	action?: Notification['action']
) {
	return notifications.addNotification('error', title, message, duration, action);
}

export function showWarning(
	title: string,
	message?: string,
	duration: number = 6000,
	action?: Notification['action']
) {
	return notifications.addNotification('warning', title, message, duration, action);
}

export function showInfo(
	title: string,
	message?: string,
	duration: number = 5000,
	action?: Notification['action']
) {
	return notifications.addNotification('info', title, message, duration, action);
}

export function showSuccess(
	title: string,
	message?: string,
	duration: number = 4000,
	action?: Notification['action']
) {
	return notifications.addNotification('success', title, message, duration, action);
}
