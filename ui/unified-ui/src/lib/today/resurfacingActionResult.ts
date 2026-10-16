/**
 * Present the outcome of a resurfacing contextual action.
 *
 * The same action can be raised from the legacy Worth-a-look band or from the
 * canonical attention lane, and a user should not be able to tell which surface
 * they used from the confirmation they get back. Everything surface-independent
 * — the toasts, their actions, and the Ask Presto navigation — lives here so the
 * two lanes cannot drift apart on wording or behaviour.
 *
 * Two outcomes stay with the caller because they are genuinely surface-specific:
 * a deeper summary needs somewhere to be shown, and "refresh afterwards" means
 * something different to a paginated band than to a delivery-bound lane. Both
 * are reported back rather than performed here.
 */
import { goto } from '$app/navigation';

import { showInfo, showSuccess } from '$lib/shared/stores/notifications';
import type { ResurfacingContextualActionResult } from './resurfacingQueries';

const TOAST_MS = 7000;

export interface ResurfacingActionPresentation {
	/** The caller should reload its list; the action changed server-side state. */
	shouldRefresh: boolean;
	/** Present when a deeper summary came back and the caller must display it. */
	deeperSummary: Extract<
		ResurfacingContextualActionResult,
		{ kind: 'deeper_summary' }
	> | null;
}

export async function presentResurfacingActionResult(
	result: ResurfacingContextualActionResult
): Promise<ResurfacingActionPresentation> {
	switch (result.kind) {
		case 'ask_presto':
			// Navigation replaces the surface, so there is nothing left to refresh.
			await goto(result.route);
			return { shouldRefresh: false, deeperSummary: null };

		case 'deeper_summary':
			showInfo('Deeper summary ready');
			return { shouldRefresh: false, deeperSummary: result };

		case 'task':
			showSuccess('Task created', undefined, TOAST_MS, {
				label: 'Open task',
				href: result.route
			});
			break;

		case 'reminder':
			showSuccess(
				'Apple Reminder created',
				result.provider === 'apple_reminders_macos'
					? 'Opened in Reminders on this Mac.'
					: undefined,
				TOAST_MS,
				result.app_url || result.route
					? { label: 'Open Reminders', href: result.app_url || result.route || '' }
					: undefined
			);
			break;

		case 'memory_candidate':
			showSuccess('Memory submitted for review', undefined, TOAST_MS, {
				label: 'Review',
				href: result.route
			});
			break;

		case 'share_draft':
			showSuccess('Share draft created', 'Approval is still required.', TOAST_MS, {
				label: 'Open draft',
				href: result.route
			});
			break;
	}
	return { shouldRefresh: true, deeperSummary: null };
}
