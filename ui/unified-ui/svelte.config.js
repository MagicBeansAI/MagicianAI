import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	// Consult https://svelte.dev/docs/kit/integrations
	// for more information about preprocessors
	preprocess: vitePreprocess(),

	kit: {
		// `src/service-worker.ts` (HITL web push) is registered only when the
		// reader turns on browser notifications in Settings — see
		// `$lib/webPush/client` — never on every page load.
		serviceWorker: {
			register: false
		},
		// Use static adapter for standalone deployment
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: process.env.VITE_MARKETING_BUILD === 'true' ? '200.html' : 'index.html',
			precompress: false,
			strict: true
		})
	},

	// Suppress CSS warnings for Tailwind @apply directives and other known patterns
	onwarn: (warning, handler) => {
		// Suppress @apply warnings (Tailwind CSS)
		if (warning.code === 'css-unused-selector') return;
		if (warning.message?.includes('@apply')) return;
		if (warning.message?.includes('Unknown at rule')) return;
		// Suppress unknown CSS property warnings for Tailwind utilities
		if (warning.message?.includes('Unknown property')) return;
		// Suppress line-clamp compatibility warnings
		if (warning.message?.includes('line-clamp')) return;
		handler(warning);
	}
};

export default config;
