import { defineProject, mergeConfig } from 'vitest/config';

import viteConfig from './vite.config';

export default mergeConfig(
	viteConfig,
	defineProject({
		test: {
			name: 'unit',
			environment: 'node',
			include: ['src/**/*.test.ts'],
			exclude: ['src/**/*.component.test.ts'],
			setupFiles: ['./src/test/setup.ts']
		}
	})
);
