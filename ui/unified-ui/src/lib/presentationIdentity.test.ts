import { describe, expect, it } from 'vitest';
import {
	ASSISTANT_FALLBACK_NAME,
	HOST_APP_NAME,
	PRODUCT_NAME,
	agentDisplayName
} from './presentationIdentity';

describe('presentation identity', () => {
	it('exposes the generated product defaults without backend service identity', () => {
		expect(PRODUCT_NAME.trim()).toBe(PRODUCT_NAME);
		expect(HOST_APP_NAME.trim()).toBe(HOST_APP_NAME);
		expect(ASSISTANT_FALLBACK_NAME.trim()).toBe(ASSISTANT_FALLBACK_NAME);
		expect(PRODUCT_NAME).not.toHaveLength(0);
		expect(HOST_APP_NAME).not.toHaveLength(0);
		expect(ASSISTANT_FALLBACK_NAME).not.toHaveLength(0);
		expect(`${PRODUCT_NAME} ${HOST_APP_NAME} ${ASSISTANT_FALLBACK_NAME}`.toLowerCase()).not.toContain(
			'magician'
		);
	});

	it('keeps agent identity separate from product identity', () => {
		expect(agentDisplayName({ name: ' Nova ', aliases: ['Fallback'] })).toBe('Nova');
		expect(agentDisplayName({ name: ' ', aliases: [' Helper '] })).toBe('Helper');
		expect(agentDisplayName(null)).toBe(ASSISTANT_FALLBACK_NAME);
	});
});
