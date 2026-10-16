import type { HitlSource } from '$lib/hitl/types';

export function chatHitlSource(
	escalationType: string | undefined,
	requestId: string | undefined
): HitlSource {
	if (requestId?.trim()) return 'user_request';
	if (escalationType === 'clarification') return 'clarification';
	return 'escalation';
}

/**
 * The option id an escalation card's Answer control reports when the card
 * carries no options of its own. It is not a choice: the handler opens the
 * card's prompt for it, and never treats it as one of the backend's options.
 */
export const ESCALATION_OPEN_PROMPT_ID = '__open_prompt';
