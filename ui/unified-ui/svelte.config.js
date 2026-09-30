import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	// Consult https://svelte.dev/docs/kit/integrations
	// for more information about preprocessors
	preprocess: vitePreprocess(),

	kit: {
		// Use static adapter for standalone deployment
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html',
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
