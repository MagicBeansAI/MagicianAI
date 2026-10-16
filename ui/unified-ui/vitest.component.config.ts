import { svelteTesting } from '@testing-library/svelte/vite';
import { defineProject, mergeConfig } from 'vitest/config';

import viteConfig from './vite.config';

export default mergeConfig(
	viteConfig,
	defineProject({
		plugins: [svelteTesting()],
		test: {
			name: 'component',
			environment: 'jsdom',
			include: ['src/**/*.component.test.ts'],
			setupFiles: ['./src/test/setup.ts'],
			// Mounting a real component, its stores, and its fetch mocks costs
			// hundreds of ms per case in isolation and several times that when the
			// whole suite runs in parallel. At the 5s default the heaviest files
			// (CommandPalette, TasksWorkspace) time out on a loaded machine and a
			// different subset fails each run, which reads as flakiness and trains
			// people to re-run rather than read the failure.
			testTimeout: 30_000,
			hookTimeout: 30_000
		}
	})
);
