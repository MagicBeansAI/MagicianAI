/**
 * Theme fonts, split into what every page needs and what only some themes do.
 *
 * `app.html` used to request all twenty-two Google families in one stylesheet.
 * That is one 16KB response and 311 `@font-face` rules parsed on every load of
 * every route — measured on the public landing, where exactly three families
 * render: Outfit, Newsreader, and self-hosted Geist Mono. The others exist
 * for themes the visitor has not chosen and, on the marketing host, cannot
 * choose. Since 2026-09-06 longhand itself is three faces — Outfit, Manrope
 * and self-hosted Geist Mono — so the base half is six Google families.
 *
 * The shell now ships only the families the default `longhand` themes and the
 * `:root` fallbacks resolve to. Everything a costume theme needs — Press Start
 * 2P, Bungee Shade, Special Elite and the rest — arrives the moment such a
 * theme is actually applied.
 *
 * Deferring is safe because the shell's stylesheet was already asynchronous
 * (`media="print"` swapped on load) and every stack declares a system
 * fallback, so this changes WHEN a costume face arrives, not whether the page
 * can paint without it.
 */

/** Families reachable from `:root` or from `longhand` / `longhand-dark`.
 *  Kept in `app.html`'s own link — see the comment there before editing
 *  either list, they are two halves of one set. */
const BASE_FAMILIES =
	'family=Fira+Code:wght@400;500;600;700' +
	'&family=Fredoka:wght@300;400;500;600;700' +
	'&family=JetBrains+Mono:wght@400;500;600;700' +
	'&family=Manrope:wght@200..800' +
	'&family=Outfit:wght@100..900' +
	'&family=Quicksand:wght@400;500;600;700';

/** Everything the other twenty themes reach for. Bricolage Grotesque and
 *  Newsreader moved here on 2026-09-06 when longhand stopped using them; the
 *  two dispatches themes still do. Geist Mono is self-hosted (`app.css`
 *  declares its face) and is not a Google family. */
const COSTUME_FAMILIES =
	'family=Bagel+Fat+One' +
	'&family=Bricolage+Grotesque:opsz,wdth,wght@12..96,75..100,200..800' +
	'&family=Bungee' +
	'&family=Bungee+Shade' +
	'&family=Geist:wght@400;500;600;700' +
	'&family=IBM+Plex+Mono:wght@400;500;600;700' +
	'&family=Inter:wght@400;500;600;700' +
	'&family=Lilita+One' +
	'&family=Newsreader:ital,opsz,wght@0,6..72,400..600;1,6..72,400..600' +
	'&family=Permanent+Marker' +
	'&family=Pixelify+Sans:wght@400..700' +
	'&family=Press+Start+2P' +
	'&family=Rajdhani:wght@400;500;600;700' +
	'&family=Space+Grotesk:wght@400;500;600;700' +
	'&family=Special+Elite';

export const COSTUME_FONT_HREF = `https://fonts.googleapis.com/css2?${COSTUME_FAMILIES}&display=swap`;

/** Exported so a test can assert the two halves still cover the whole set
 *  rather than having silently drifted apart. */
export const BASE_FONT_HREF = `https://fonts.googleapis.com/css2?${BASE_FAMILIES}&display=swap`;

/** The themes that resolve entirely within `BASE_FAMILIES`. Everything else
 *  needs the costume sheet. Conservative by construction: a theme absent from
 *  this list gets the extra families whether or not it strictly needs them,
 *  so adding a theme can never silently lose its face. */
const BASE_ONLY_THEMES = new Set(['longhand', 'longhand-dark']);

const LINK_ID = 'magican-costume-fonts';

/**
 * Load the costume families if `theme` needs them. Idempotent, and a no-op on
 * the server and for the default themes.
 */
export function ensureThemeFonts(theme: string): void {
	if (typeof document === 'undefined') return;
	if (BASE_ONLY_THEMES.has(theme)) return;
	if (document.getElementById(LINK_ID)) return;

	const link = document.createElement('link');
	link.id = LINK_ID;
	link.rel = 'stylesheet';
	link.href = COSTUME_FONT_HREF;
	document.head.appendChild(link);
}
