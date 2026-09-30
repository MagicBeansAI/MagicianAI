import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import HowTrack from './HowTrack.svelte';
import { HOW_CLAIMS } from './howTrack';
import { motionEnabled } from '$lib/motion';

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
});

describe('HowTrack', () => {
	it('states every how-claim', () => {
		motionEnabled.set(false);
		render(HowTrack);

		for (const claim of HOW_CLAIMS) {
			expect(screen.getByText(claim.title)).toBeInTheDocument();
			expect(screen.getByText(claim.line)).toBeInTheDocument();
		}
	});
});
