/**
 * The owner-private read for one preference alignment hold. It lives apart
 * from `alignmentAsk.ts` because it needs the scope bearer: the HITL adapters
 * import `alignmentAsk.ts`, and the service worker imports the adapters (web
 * push), where `$app/environment` (behind the scope store) may not be loaded.
 */
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

import { parseAlignmentDetail, type AlignmentDetail } from './alignmentAsk';

export type AlignmentDetailOutcome =
	| { status: 'ok'; detail: AlignmentDetail }
	| { status: 'gone' }
	| { status: 'unavailable' };

/** The owner-private read for one hold. */
export async function fetchAlignmentDetail(correlationId: string): Promise<AlignmentDetailOutcome> {
	try {
		const response = await fetch(
			`/api/magician/v2/hitl/${encodeURIComponent(correlationId)}/alignment?surface=web`,
			{
				headers: scopedRequestHeaders({ Accept: 'application/json' }),
				cache: 'no-store'
			}
		);
		if (response.status === 404 || response.status === 409) return { status: 'gone' };
		if (!response.ok) return { status: 'unavailable' };
		const detail = parseAlignmentDetail(await response.json());
		return detail ? { status: 'ok', detail } : { status: 'unavailable' };
	} catch {
		return { status: 'unavailable' };
	}
}
