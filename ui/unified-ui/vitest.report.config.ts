import { defineConfig, mergeConfig } from 'vitest/config';

import baseConfig from './vitest.config';

const defaultRunDirectory = '../../coverage/frontend/results/manual';
const resultsPath = process.env.UI_TEST_RESULTS_PATH ?? `${defaultRunDirectory}/vitest-results.json`;
const coverageDirectory = process.env.UI_TEST_COVERAGE_DIR ?? `${defaultRunDirectory}/coverage`;
const terminalReporter = process.env.UI_TEST_VERBOSE === '1' ? 'verbose' : 'default';

export default mergeConfig(
	baseConfig,
	defineConfig({
		test: {
			reporters: [terminalReporter, 'json'],
			outputFile: {
				json: resultsPath
			},
			coverage: {
				enabled: process.env.COVERAGE === '1',
				provider: 'v8',
				reportsDirectory: coverageDirectory,
				reporter: ['text-summary', 'html', 'json-summary'],
				reportOnFailure: true
			}
		}
	})
);
