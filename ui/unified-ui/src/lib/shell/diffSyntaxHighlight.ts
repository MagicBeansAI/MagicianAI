/**
 * Lazy Shiki loader for the DiffStrip syntax-highlighting prop.
 *
 * Shiki ships TextMate grammars plus multiple runtime bundles. We pay
 * zero of that until a consumer flips `syntaxHighlight={true}` on at
 * least one DiffStrip instance, and we load individual language
 * grammars only when a matching diff file is expanded.
 *
 * Public API:
 *   - `loadHighlighter()` — returns a Promise<HighlighterCore>, idempotent
 *   - `ensureLanguagesLoaded(highlighter, languages)` — loads only the
 *     grammars needed by the currently visible diff files
 *   - `languageForPath(path)` — file-extension → Shiki language candidate
 *   - `BUNDLED_THEMES` — list of themes loaded with the core highlighter
 *
 * Theme switching: the highlighter is created with both light + dark
 * themes loaded; the renderer picks which one based on the document's
 * current `color-scheme` (or an explicit prop on DiffStrip).
 */
import type { HighlighterCore } from '@shikijs/core';
import type { BundledLanguage, BundledTheme, LanguageInput } from 'shiki';

/**
 * File extensions we can confidently map to canonical Shiki language
 * ids. The grammar itself is not loaded here; `ensureLanguageLoaded`
 * resolves the corresponding dynamic import only when a matching file
 * is actually rendered. Unknown extensions fall back to the extension
 * itself as a Shiki language candidate, which lets Shiki-supported
 * aliases work without us hand-maintaining all 200+ languages.
 */
const EXT_TO_LANG: Record<string, BundledLanguage> = {
	ts: 'typescript',
	mts: 'typescript',
	cts: 'typescript',
	tsx: 'tsx',
	js: 'javascript',
	mjs: 'javascript',
	cjs: 'javascript',
	jsx: 'jsx',
	rs: 'rust',
	py: 'python',
	go: 'go',
	svelte: 'svelte',
	html: 'html',
	htm: 'html',
	css: 'css',
	scss: 'scss',
	sass: 'scss',
	json: 'json',
	yaml: 'yaml',
	yml: 'yaml',
	md: 'markdown',
	mdx: 'markdown',
	sh: 'bash',
	bash: 'bash',
	zsh: 'bash',
	toml: 'toml',
	sql: 'sql',
	java: 'java',
	kt: 'kotlin',
	kts: 'kotlin',
	swift: 'swift',
	c: 'c',
	h: 'c',
	cpp: 'cpp',
	cc: 'cpp',
	cxx: 'cpp',
	hpp: 'cpp',
	hxx: 'cpp',
	m: 'objective-c',
	mm: 'objective-cpp',
	cs: 'csharp',
	php: 'php',
	dockerfile: 'dockerfile',
	mk: 'makefile',
	diff: 'diff',
	patch: 'diff',
	xml: 'xml',
	lua: 'lua',
	rb: 'ruby',
	rake: 'ruby',
	gemspec: 'ruby',
	scala: 'scala',
	sc: 'scala',
	hs: 'haskell',
	lhs: 'haskell',
	ex: 'elixir',
	exs: 'elixir',
	erl: 'erlang',
	hrl: 'erlang',
	clj: 'clojure',
	cljs: 'clojure',
	cljc: 'clojure',
	dart: 'dart',
	r: 'r',
	R: 'r',
	jl: 'julia',
	zig: 'zig',
	vue: 'vue',
	proto: 'protobuf',
	tf: 'terraform',
	tfvars: 'terraform',
	nix: 'nix',
	v: 'v',
	vim: 'vim',
	ml: 'ocaml',
	mli: 'ocaml',
	fs: 'fsharp',
	fsx: 'fsharp',
	pl: 'perl',
	pm: 'perl',
	ps1: 'powershell',
	d: 'd',
};

export const BUNDLED_THEMES: BundledTheme[] = ['github-light', 'github-dark'];

export function languageForPath(path: string | null | undefined): BundledLanguage | null {
	if (!path) return null;
	const lower = path.toLowerCase();
	// Filename-based detection (no extension).
	if (lower.endsWith('/dockerfile') || lower === 'dockerfile') return 'dockerfile';
	if (lower.endsWith('/makefile') || lower === 'makefile') return 'makefile';
	if (lower.endsWith('/rakefile') || lower === 'rakefile') return 'ruby';
	if (lower.endsWith('/gemfile') || lower === 'gemfile') return 'ruby';
	if (lower.endsWith('/cmakelists.txt') || lower === 'cmakelists.txt') return 'cmake';
	const idx = path.lastIndexOf('.');
	if (idx < 0 || idx === path.length - 1) return null;
	const rawExt = path.slice(idx + 1);
	const ext = rawExt === 'R' ? 'R' : rawExt.toLowerCase();
	return EXT_TO_LANG[ext] ?? (ext as BundledLanguage);
}

let highlighterPromise: Promise<HighlighterCore> | null = null;

/**
 * Returns the process-wide Shiki highlighter core. Lazily creates it
 * on first call; subsequent calls return the same instance. Languages
 * are intentionally not loaded here — individual grammars are loaded
 * on demand by `ensureLanguageLoaded`.
 */
export function loadHighlighter(): Promise<HighlighterCore> {
	if (highlighterPromise) return highlighterPromise;
	highlighterPromise = Promise.all([
		import('@shikijs/core'),
		import('@shikijs/engine-javascript'),
		import('@shikijs/themes/github-light'),
		import('@shikijs/themes/github-dark'),
	]).then(([core, engine, githubLight, githubDark]) =>
		core.createHighlighterCore({
			engine: engine.createJavaScriptRegexEngine(),
			themes: [githubLight.default, githubDark.default],
		})
	);
	return highlighterPromise;
}

const languageLoadPromises = new WeakMap<HighlighterCore, Map<BundledLanguage, Promise<void>>>();

export function ensureLanguagesLoaded(
	highlighter: HighlighterCore,
	languages: Iterable<BundledLanguage | null | undefined>
): Promise<void> {
	const unique = new Set<BundledLanguage>();
	for (const language of languages) {
		if (language) unique.add(language);
	}
	return Promise.all([...unique].map((language) => ensureLanguageLoaded(highlighter, language))).then(
		() => undefined
	);
}

export function ensureLanguageLoaded(
	highlighter: HighlighterCore,
	language: BundledLanguage
): Promise<void> {
	if (highlighter.getLoadedLanguages().includes(language)) return Promise.resolve();

	let byLanguage = languageLoadPromises.get(highlighter);
	if (!byLanguage) {
		byLanguage = new Map();
		languageLoadPromises.set(highlighter, byLanguage);
	}

	const existing = byLanguage.get(language);
	if (existing) return existing;

	const loadPromise = loadBundledLanguage(language)
		.then(async (registration) => {
			if (!registration) return;
			await highlighter.loadLanguage(registration);
		})
		.catch(() => {
			byLanguage?.delete(language);
		});
	byLanguage.set(language, loadPromise);
	return loadPromise;
}

async function loadBundledLanguage(language: BundledLanguage): Promise<LanguageInput | null> {
	const { bundledLanguages } = await import('shiki');
	const loader = bundledLanguages[language];
	if (!loader) return null;
	const module = await loader();
	return module.default;
}

/**
 * Render a single line of code as syntax-highlighted HTML, with the
 * leading diff marker (`+`, `-`, ` `) stripped before highlighting
 * and re-prefixed on output. Falls back to the raw escaped text when:
 *   - the highlighter isn't ready yet (caller hasn't awaited it)
 *   - the language is null (unknown file extension)
 *   - shiki itself errors out (rare; fall back to plain text)
 *
 * The output is a string of HTML — caller must use `{@html ...}` in
 * Svelte. Marker characters render as plain text because they aren't
 * part of the source language.
 */
export function highlightLine(
	highlighter: HighlighterCore | null,
	codeWithMarker: string,
	language: BundledLanguage | null,
	theme: BundledTheme
): string {
	if (!highlighter || !language) return escapeHtml(codeWithMarker);

	// Split the diff marker (+/-/space) from the code. Hunk/meta lines
	// (++/--/@) shouldn't reach here — caller filters them — but be
	// defensive.
	const firstChar = codeWithMarker.charAt(0);
	const marker = ['+', '-', ' '].includes(firstChar) ? firstChar : '';
	const code = marker ? codeWithMarker.slice(1) : codeWithMarker;

	try {
		const html = highlighter.codeToHtml(code, {
			lang: language,
			theme,
			structure: 'inline',
		});
		// Shiki's `inline` structure returns just the styled spans
		// (no wrapping <pre><code>). Prefix the marker as plain text.
		return (marker ? escapeHtml(marker) : '') + html;
	} catch {
		// Grammar might not be loaded (lang fell back to plaintext);
		// shiki throws. Render plain escaped text.
		return escapeHtml(codeWithMarker);
	}
}

function escapeHtml(s: string): string {
	return s
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;');
}
