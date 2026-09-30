import type { HitlSource } from '$lib/hitl/types';

export function chatHitlSource(
	escalationType: string | undefined,
	requestId: string | undefined
): HitlSource {
	if (requestId?.trim()) return 'user_request';
	if (escalationType === 'clarification') return 'clarification';
	return 'escalation';
}
