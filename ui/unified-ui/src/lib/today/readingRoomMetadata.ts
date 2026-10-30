import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
import type { ResurfacingCard } from './resurfacingQueries';
import type { CanonicalAttentionItem } from '$lib/attention/canonicalAttentionProjection';

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

/** Keep source receipt time distinct from when a saved item was resurfaced. */
export function readingRoomDateline(
	followUp?: ChannelFollowUp | null,
	worth?: ResurfacingCard | null,
	item?: CanonicalAttentionItem | null
): { iso: string; text: string } | null {
	const received = followUp?.received_at ?? (item?.origin_lane === 'follow_up' && 'received_at' in item.payload ? item.payload.received_at : null);
	const surfaced = worth?.surfaced_at && worth.surfaced_at > 0 ? worth.surfaced_at : null;
	const timestamp = surfaced ?? worth?.temporal_anchor_at ?? (received && received > 0 ? received : followUp?.created_at);
	if (typeof timestamp !== 'number' || timestamp <= 0) return null;
	const date = new Date(timestamp);
	if (!Number.isFinite(date.getTime())) return null;
	const label = worth ? surfaced ? 'Surfaced' : 'Reference' : received && received > 0 ? 'Received' : 'Added';
	return { iso: date.toISOString(), text: `${label} ${dateTime.format(date)}` };
}
