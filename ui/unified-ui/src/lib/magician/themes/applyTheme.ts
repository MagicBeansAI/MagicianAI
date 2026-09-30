/**
 * Apply a `DashboardTheme` to the document by setting CSS variables on
 * `:root` and injecting stylesheet `<link>` tags for each font face the
 * theme declares.
 *
 * Naming convention: every variable starts with `--theme-` to namespace
 * cleanly alongside any app-level variables. Components that want to
 * inherit the active theme read these variables; components that want to
 * stay theme-agnostic continue using their own CSS without a `--theme-`
 * prefix.
 *
 * Idempotent: calling with the same theme twice in a row is cheap (CSS
 * variables overwrite in place; injected `<link>` tags are deduped by
 * `href`).
 */

import type { DashboardTheme } from '$lib/stores/themeStore';

const FONT_LINK_ATTR = 'data-theme-font';

function setVar(name: string, value: string): void {
	document.documentElement.style.setProperty(name, value);
}

function injectFontLink(url: string): void {
	const escaped = url.replace(/"/g, '\\"');
	if (document.querySelector(`link[${FONT_LINK_ATTR}="${escaped}"]`)) {
		return;
	}
	const link = document.createElement('link');
	link.rel = 'stylesheet';
	link.href = url;
	link.setAttribute(FONT_LINK_ATTR, url);
	document.head.appendChild(link);
}

export function applyThemeToCssVariables(theme: DashboardTheme): void {
	// Typography
	setVar('--theme-font-display', theme.fonts.display.family);
	setVar('--theme-font-body', theme.fonts.body.family);
	setVar('--theme-font-mono', theme.fonts.mono.family);

	// Palette
	setVar('--theme-color-background', theme.palette.background);
	setVar('--theme-color-surface', theme.palette.surface);
	setVar('--theme-color-foreground', theme.palette.foreground);
	setVar('--theme-color-foreground-muted', theme.palette.foreground_muted);
	setVar('--theme-color-accent', theme.palette.accent);
	if (theme.palette.accent_alt) {
		setVar('--theme-color-accent-alt', theme.palette.accent_alt);
	}
	setVar('--theme-color-border', theme.palette.border);
	setVar('--theme-color-shadow', theme.palette.shadow);

	// Chart colors — fixed-position slots so chart code can read
	// `--theme-chart-color-0` through `--theme-chart-color-7`. Wrap when
	// the palette is shorter than 8.
	const palette = theme.palette.chart_colors;
	for (let i = 0; i < 8; i++) {
		const color = palette[i % palette.length] ?? '#888888';
		setVar(`--theme-chart-color-${i}`, color);
	}

	// Spacing
	setVar(
		'--theme-container-max-width',
		`${theme.spacing.container_max_width_px}px`
	);
	setVar(
		'--theme-container-padding-x',
		`${theme.spacing.container_padding_x_px}px`
	);
	setVar(
		'--theme-container-padding-y',
		`${theme.spacing.container_padding_y_px}px`
	);
	setVar('--theme-block-gap', `${theme.spacing.block_gap_px}px`);
	setVar('--theme-prose-line-height', String(theme.spacing.prose_line_height));
	setVar('--theme-heading-scale', String(theme.spacing.heading_scale));

	// Atmosphere
	setVar('--theme-atmosphere-kind', theme.atmosphere.kind);
	setVar('--theme-atmosphere-intensity', String(theme.atmosphere.intensity));

	// Motion
	setVar(
		'--theme-motion-load-stagger',
		`${theme.motion.load_stagger_ms}ms`
	);
	setVar(
		'--theme-motion-load-duration',
		`${theme.motion.load_duration_ms}ms`
	);
	setVar('--theme-motion-load-easing', theme.motion.load_easing);
	setVar(
		'--theme-motion-hover-lift',
		`${theme.motion.hover_lift_px ?? 0}px`
	);

	// Font stylesheets
	for (const face of [theme.fonts.display, theme.fonts.body, theme.fonts.mono]) {
		if (face.url) {
			injectFontLink(face.url);
		}
	}
}
