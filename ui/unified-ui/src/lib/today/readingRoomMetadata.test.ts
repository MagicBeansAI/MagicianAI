import { describe, expect, it } from 'vitest';
import { readingRoomDateline } from './readingRoomMetadata';
import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
import type { ResurfacingCard } from './resurfacingQueries';

describe('Reading Room dates', () => {
    it('uses the received time rather than when an item was annotated', () => {
        const date = readingRoomDateline({ received_at: 1700000000000, created_at: 1800000000000 } as ChannelFollowUp);
        expect(date?.iso).toBe('2023-11-14T22:13:20.000Z');
        expect(date?.text).toMatch(/^Received /);
    });
    it('distinguishes surfacing time from reference time on older servers', () => {
        const card = { surfaced_at: 1800000000000, temporal_anchor_at: 1700000000000 } as ResurfacingCard;
        expect(readingRoomDateline(undefined, card)?.text).toMatch(/^Surfaced /);
        expect(readingRoomDateline(undefined, { ...card, surfaced_at: null })?.text).toMatch(/^Reference /);
        expect(readingRoomDateline(undefined, { ...card, surfaced_at: 0 })?.text).toMatch(/^Reference /);
    });
    it('does not invent a date when metadata is missing', () => {
        expect(readingRoomDateline()).toBeNull();
        expect(readingRoomDateline({ received_at: 0, created_at: 0 } as ChannelFollowUp)).toBeNull();
    });
});
