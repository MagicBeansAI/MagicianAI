<script lang="ts">
	/**
	 * InteractiveTerminalPane — embedded xterm.js terminal that
	 * renders bytes from a single `interactive_process` PTY session
	 * in real time.
	 *
	 * The backend reader thread fans every chunk through the realtime
	 * event bus as `RuntimeTransportEvent::InteractivePtyChunk` (base64
	 * encoded). This component subscribes, decodes, and feeds bytes
	 * into xterm so the user sees claude/codex/agy/opencode's TUI
	 * exactly as they'd see it in a real terminal.
	 *
	 * Developer Mode terminal core (see
	 * docs/components/magician/developer-mode-workbench.md). On mount
	 * it hydrates a non-draining replay buffer, then applies live
	 * offset-tagged chunks. It re-hydrates whenever it may have missed
	 * bytes: a hub resync (lag, reconnect, server restart), a gap in the
	 * offsets, or offsets starting over (a re-attached session after a
	 * restart). It keeps the PTY's window size equal to its own grid.
	 *
	 * Lazy-loaded: xterm + addons are dynamically imported on mount so
	 * the chat-mode bundle pays nothing for users who never flip the
	 * Developer Mode toggle.
	 */
	import { onMount, onDestroy, createEventDispatcher } from 'svelte';
	import { eventHub } from '$lib/realtime/eventHub';
	import {
		fetchInteractiveSessionBuffer,
		resizeInteractiveSession,
		writeInteractiveSessionStdin
	} from '$lib/shell/interactiveSessionApi';

	const dispatch = createEventDispatcher<{
		'user-typed': void;
	}>();

	export let sessionId: string;
	/** Retained for API back-compat (callers still pass it); no longer rendered. */
	export let program: string | null = null;
	export let active = true;
	export let autofocus = false;
	export let focusNonce = 0;

	let containerEl: HTMLDivElement | null = null;
	let term: import('@xterm/xterm').Terminal | null = null;
	let fitAddon: import('@xterm/addon-fit').FitAddon | null = null;
	let resizeObserver: ResizeObserver | null = null;
	let fontSizeMql: MediaQueryList | null = null;

	// Phone-width override. xterm.js sizing is driven by `options.fontSize`
	// (CSS can't help — the renderer measures the canvas), so we pick a
	// value at mount and re-apply when the viewport crosses the breakpoint.
	const DESKTOP_FONT_PX = 12.5;
	const MOBILE_FONT_PX = 9;

	function pickFontSize(): number {
		if (typeof window === 'undefined') return DESKTOP_FONT_PX;
		return window.matchMedia('(max-width: 767px)').matches
			? MOBILE_FONT_PX
			: DESKTOP_FONT_PX;
	}
	let themeObserver: MutationObserver | null = null;
	let unsubscribeEvents: (() => void) | null = null;
	type PtyChunk = { bytes: Uint8Array; offsetStart?: number; offsetEnd?: number };
	// Absolute PTY byte offset the screen has rendered up to.
	let replayOffset = 0;
	// A buffer fetch is in flight; live chunks wait for it.
	let hydrating = false;
	// The last fetch succeeded, so `replayOffset` is a real baseline.
	let hydrated = false;
	let hydrateGeneration = 0;
	let heldChunks: PtyChunk[] = [];
	let resizeTimer: ReturnType<typeof setTimeout> | null = null;
	let lastSentSize = '';
	let handledFocusNonce = -1;
	let wasActive = false;
	// `program` is part of the public API; ack to silence unused-prop checks.
	void program;

	type XtermTheme = NonNullable<import('@xterm/xterm').ITerminalOptions['theme']>;

	/** Parse a CSS color string (#rgb, #rrggbb, rgb(...), rgba(...)) into 0–255 RGB. */
	function parseColor(value: string): { r: number; g: number; b: number } | null {
		const v = value.trim();
		if (!v) return null;
		if (v.startsWith('#')) {
			const hex = v.slice(1);
			if (hex.length === 3) {
				return {
					r: parseInt(hex[0] + hex[0], 16),
					g: parseInt(hex[1] + hex[1], 16),
					b: parseInt(hex[2] + hex[2], 16)
				};
			}
			if (hex.length === 6 || hex.length === 8) {
				return {
					r: parseInt(hex.slice(0, 2), 16),
					g: parseInt(hex.slice(2, 4), 16),
					b: parseInt(hex.slice(4, 6), 16)
				};
			}
			return null;
		}
		const rgb = v.match(/rgba?\(\s*([\d.]+)\s*,?\s*([\d.]+)\s*,?\s*([\d.]+)/i);
		if (rgb) {
			return { r: Number(rgb[1]), g: Number(rgb[2]), b: Number(rgb[3]) };
		}
		return null;
	}

	/** Relative luminance per WCAG 2.0; used to pick light vs dark ANSI palette. */
	function isDarkBackground(color: string): boolean {
		const rgb = parseColor(color);
		if (!rgb) return true;
		// Simplified luminance: avoids gamma expansion (good enough for theme
		// classification). 0.5 is the conventional split point.
		const luma = (0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b) / 255;
		return luma < 0.5;
	}

	/** Turn an opaque color + alpha into rgba(...) for selection backgrounds. */
	function withAlpha(color: string, alpha: number): string {
		const rgb = parseColor(color);
		if (!rgb) return `rgba(255, 255, 255, ${alpha})`;
		return `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${alpha})`;
	}

	function buildXtermTheme(host: HTMLElement): XtermTheme {
		const cs = getComputedStyle(host);
		const read = (name: string, fallback: string): string =>
			cs.getPropertyValue(name).trim() || fallback;

		// Background: prefer an explicit terminal var; otherwise the surface
		// the terminal sits on (workbench body uses --bg-card-ish via the
		// elevated card. We choose --bg-card so the terminal reads as a panel
		// against the workbench backdrop, not as a hole punched in the page.
		const background = read('--terminal-bg', read('--bg-card', '#111418'));
		const foreground = read('--terminal-fg', read('--text-primary', '#e8e6e3'));
		const accent = read('--accent-primary', '#c2502a');
		const cursor = read('--terminal-cursor', accent);
		const cursorAccent = background;
		const selectionBg = read('--terminal-selection', withAlpha(accent, 0.25));

		const error = read('--color-error', '#ff6b6b');
		const success = read('--color-success', '#00bb7f');
		const warning = read('--color-warning', '#e5c07b');

		const isDark = isDarkBackground(background);

		// ANSI palette: error/success/warning come from the active theme so
		// the standard red/green/yellow ANSI slots match the rest of the UI
		// (a Claude/Codex "error" line glows the same red as a magician
		// danger button). Blue/magenta/cyan/white/black are picked from
		// well-tested One Dark / One Light defaults — themes don't define
		// these slots and inventing them per-theme would drift.
		const palette = isDark
			? {
					black: '#1a1d22',
					red: error,
					green: success,
					yellow: warning,
					blue: '#61afef',
					magenta: '#c678dd',
					cyan: '#56b6c2',
					white: '#abb2bf',
					brightBlack: '#5c6370',
					brightRed: error,
					brightGreen: success,
					brightYellow: warning,
					brightBlue: '#82b8f4',
					brightMagenta: '#d49ce0',
					brightCyan: '#7fd6da',
					brightWhite: '#dcdfe4'
				}
			: {
					black: '#2d2a26',
					red: error,
					green: success,
					yellow: warning,
					blue: '#3a5fcd',
					magenta: '#a13a9b',
					cyan: '#2c8e9b',
					white: '#5f6668',
					brightBlack: '#7d7d7d',
					brightRed: error,
					brightGreen: success,
					brightYellow: warning,
					brightBlue: '#1c3fb0',
					brightMagenta: '#7a2b75',
					brightCyan: '#1f6d78',
					brightWhite: '#1a1a1a'
				};

		return {
			background,
			foreground,
			cursor,
			cursorAccent,
			selectionBackground: selectionBg,
			selectionForeground: foreground,
			...palette
		};
	}

	function applyTheme(): void {
		if (!term || !containerEl) return;
		term.options.theme = buildXtermTheme(containerEl);
	}

	export function focusTerminal(): void {
		term?.focus();
	}

	function fitAndMaybeFocus(): void {
		setTimeout(() => {
			try {
				fitAddon?.fit();
			} catch {
				/* container may be transitioning between hidden and visible. */
			}
			if (autofocus) {
				focusTerminal();
			}
		}, 0);
	}

	$: if (term && autofocus && focusNonce !== handledFocusNonce) {
		handledFocusNonce = focusNonce;
		fitAndMaybeFocus();
	}

	$: {
		if (term && active && !wasActive) {
			wasActive = true;
			fitAndMaybeFocus();
			// Another viewer may have resized the shared PTY meanwhile.
			scheduleSizeSync(true);
		} else if (!active && wasActive) {
			wasActive = false;
		}
	}

	/**
	 * Tell the server this pane's grid so the CLI draws at the width we
	 * show. Debounced (a drag fires many resizes); skipped while the pane is
	 * hidden, where xterm still holds its last size. `force` re-sends an
	 * unchanged size — the PTY is shared, so a phone may have changed it.
	 */
	function scheduleSizeSync(force = false): void {
		if (resizeTimer) clearTimeout(resizeTimer);
		resizeTimer = setTimeout(() => {
			resizeTimer = null;
			if (!term || !active || !containerEl || containerEl.clientWidth === 0) return;
			const size = `${term.rows}x${term.cols}`;
			if (!force && size === lastSentSize) return;
			lastSentSize = size;
			resizeInteractiveSession(sessionId, term.rows, term.cols).catch(() => {
				lastSentSize = '';
			});
		}, 120);
	}

	function decodeBase64ToBytes(b64: string): Uint8Array {
		// atob handles standard base64; we trust the backend not to use URL-safe.
		const binary = atob(b64);
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) {
			bytes[i] = binary.charCodeAt(i);
		}
		return bytes;
	}

	function writePtyBytes(bytes: Uint8Array, offsetStart?: number, offsetEnd?: number): void {
		if (!term || bytes.byteLength === 0) return;
		if (typeof offsetStart === 'number' && typeof offsetEnd === 'number') {
			if (offsetEnd <= replayOffset) return;
			let nextBytes = bytes;
			if (offsetStart < replayOffset) {
				nextBytes = bytes.slice(Math.max(0, replayOffset - offsetStart));
			}
			term.write(nextBytes);
			replayOffset = Math.max(replayOffset, offsetEnd);
			return;
		}
		term.write(bytes);
	}

	/**
	 * Rebuild the screen from the server's replay buffer. Live chunks that
	 * arrive meanwhile are held and applied after, deduplicated by offset.
	 * A failed fetch (the session is not back yet after a restart) leaves no
	 * baseline, so the next live chunk triggers another attempt.
	 */
	async function rehydrate(): Promise<void> {
		const generation = ++hydrateGeneration;
		hydrating = true;
		heldChunks = [];
		try {
			const snapshot = await fetchInteractiveSessionBuffer(sessionId);
			if (generation !== hydrateGeneration || !term) return;
			const bytes = decodeBase64ToBytes(snapshot.bytes_b64 || '');
			term.reset();
			replayOffset = snapshot.start_offset;
			writePtyBytes(bytes, snapshot.start_offset, snapshot.end_offset);
			hydrated = true;
		} catch {
			if (generation === hydrateGeneration) hydrated = false;
		} finally {
			if (generation === hydrateGeneration) {
				hydrating = false;
				const held = heldChunks;
				heldChunks = [];
				for (const chunk of held) applyLiveChunk(chunk);
			}
		}
	}

	function applyLiveChunk(chunk: PtyChunk): void {
		if (hydrating) {
			heldChunks.push(chunk);
			return;
		}
		const { offsetStart, offsetEnd } = chunk;
		if (typeof offsetStart !== 'number' || typeof offsetEnd !== 'number') {
			term?.write(chunk.bytes);
			return;
		}
		// No baseline, a gap (bytes went missing in transit), or offsets that
		// started over (the session was re-attached by a restarted server):
		// the screen can only be trusted after a fresh replay.
		const restarted = offsetStart === 0 && replayOffset > 0 && offsetEnd <= replayOffset;
		if (!hydrated || offsetStart > replayOffset || restarted) {
			void rehydrate();
			heldChunks.push(chunk);
			return;
		}
		writePtyBytes(chunk.bytes, offsetStart, offsetEnd);
	}

	// One warning per failed request: a batch carries many keystrokes.
	const reportedStdinFailures = new WeakSet<Response>();

	async function postStdin(data: string): Promise<void> {
		if (!data) return;
		try {
			const resp = await writeInteractiveSessionStdin(sessionId, data);
			if (!resp.ok) {
				if (reportedStdinFailures.has(resp)) return;
				reportedStdinFailures.add(resp);
				// Surface as a transient warning in the terminal itself
				// — the user is typing into a pane that doesn't echo,
				// so a silent failure would be invisible.
				term?.writeln(`\r\n\x1b[33m[stdin failed: HTTP ${resp.status}]\x1b[0m`);
			} else {
				dispatch('user-typed');
			}
		} catch (error) {
			term?.writeln(`\r\n\x1b[33m[stdin error: ${error}]\x1b[0m`);
		}
	}

	onMount(async () => {
		if (!containerEl) return;

		// Lazy-import xterm so chat-mode users don't bundle it.
		const [{ Terminal }, { FitAddon }, { WebLinksAddon }, { SearchAddon }] = await Promise.all([
			import('@xterm/xterm'),
			import('@xterm/addon-fit'),
			import('@xterm/addon-web-links'),
			import('@xterm/addon-search')
		]);

		// xterm ships its own stylesheet; load it once per page.
		// Subsequent imports are deduped by Vite.
		await import('@xterm/xterm/css/xterm.css');

		const t = new Terminal({
			cursorBlink: true,
			scrollback: 5000,
			fontFamily:
				'ui-monospace, SF Mono, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
			fontSize: pickFontSize(),
			lineHeight: 1.25,
			// Initial theme; re-applied on `data-theme` change via the
			// MutationObserver wired below so switching magician themes
			// re-skins the CLI live.
			theme: buildXtermTheme(containerEl),
			convertEol: false,
			// stdin capture is always enabled; keystrokes flow directly to
			// the PTY's stdin so the user types into the agent's CLI
			// without any explicit take-over toggle.
			disableStdin: false,
			allowProposedApi: true
		});

		t.onData((data) => {
			void postStdin(data);
		});
		t.onResize(() => scheduleSizeSync());

		const fit = new FitAddon();
		t.loadAddon(fit);
		t.loadAddon(new WebLinksAddon());
		// Search addon: ctrl-F over scrollback. We expose the API via
		// term.searchAddon for future search-bar UI; for now the
		// addon's default chord shortcuts work.
		const search = new SearchAddon();
		t.loadAddon(search);
		t.open(containerEl);
		try {
			fit.fit();
		} catch {
			/* container may be 0-size briefly; the ResizeObserver below recovers. */
		}

		term = t;
		fitAddon = fit;
		if (autofocus) {
			fitAndMaybeFocus();
		}
		// xterm fires no resize when the fitted grid equals its default, so
		// the first size is sent explicitly; the PTY starts at the launcher's.
		scheduleSizeSync(true);
		// The device being typed on owns the shared PTY's size.
		t.textarea?.addEventListener('focus', () => scheduleSizeSync(true));

		resizeObserver = new ResizeObserver(() => {
			try {
				fitAddon?.fit();
			} catch {
				/* xterm internals occasionally throw during teardown; ignore. */
			}
		});
		resizeObserver.observe(containerEl);

		// Re-apply the xterm theme whenever the magician theme switches.
		// `data-theme` on <html> is the canonical signal; ThemeProvider /
		// the theme picker mutate it on user change. We also re-apply on
		// the `class` attribute (some themes augment with classes) to be
		// safe — the work is cheap and idempotent.
		themeObserver = new MutationObserver(() => {
			try {
				applyTheme();
			} catch {
				/* Best-effort live re-skin; ignore xterm internal hiccups. */
			}
		});
		themeObserver.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme', 'class']
		});

		// Live re-size on viewport breakpoint crossings (orientation flip,
		// devtools toggling mobile emulation, window resize across 768px).
		// Updating fontSize alone leaves the renderer at stale cell
		// dimensions, so re-fit on the next frame.
		fontSizeMql = window.matchMedia('(max-width: 767px)');
		const handleFontSizeBreakpoint = (): void => {
			if (!term) return;
			term.options.fontSize = pickFontSize();
			requestAnimationFrame(() => {
				try { fitAddon?.fit(); } catch { /* ignored — xterm internals */ }
			});
		};
		fontSizeMql.addEventListener('change', handleFontSizeBreakpoint);

		unsubscribeEvents = eventHub.subscribe(
			(event) => event.eventType === 'InteractivePtyChunk' && event.data?.session_id === sessionId,
			(event) => {
				const data = event.data;
				if (!data || typeof data.bytes_b64 !== 'string') return;
				try {
					applyLiveChunk({
						bytes: decodeBase64ToBytes(data.bytes_b64),
						offsetStart: typeof data.offset_start === 'number' ? data.offset_start : undefined,
						offsetEnd: typeof data.offset_end === 'number' ? data.offset_end : undefined
					});
				} catch {
					/* malformed chunk; skip silently rather than break the stream. */
				}
			},
			// The hub may have dropped events (lag, reconnect, a restarted
			// server): only a fresh replay makes the screen trustworthy again.
			() => void rehydrate()
		);
		void rehydrate();
	});

	onDestroy(() => {
		unsubscribeEvents?.();
		if (resizeTimer) clearTimeout(resizeTimer);
		hydrateGeneration += 1;
		resizeObserver?.disconnect();
		themeObserver?.disconnect();
		// MediaQueryList listener is anonymous-bound above; replacing it
		// with `null` drops the listener via GC once the MQL is gone.
		fontSizeMql = null;
		term?.dispose();
		term = null;
		fitAddon = null;
	});
</script>

<div class="terminal-pane">
	<div class="terminal-pane__body" bind:this={containerEl}></div>
</div>

<style>
	.terminal-pane {
		display: flex;
		flex-direction: column;
		flex: 1 1 auto;
		/* min-height: 0 lets the flex chain shrink the pane to fit
		   shorter viewports. Previously `min-height: 280px` snapped
		   the pane to a fixed floor that read as "workbench doesn't
		   adjust to screen height". The xterm body has its own min
		   below for the usability floor. */
		min-height: 0;
		/* Match the magician theme: prefer an explicit terminal var,
		   otherwise the surrounding card bg. The xterm canvas paints
		   its own background on top via `buildXtermTheme()`; this
		   value only shows during the brief mount window before
		   xterm renders. No border / border-radius here — the
		   workbench card already provides the frame and a nested
		   bezel reads as visual noise / extra spacing. */
		background: var(--terminal-bg, var(--bg-card, #111418));
		overflow: hidden;
	}

	.terminal-pane__body {
		flex: 1 1 auto;
		overflow: hidden;
		/* No padding: previously `padding: 6px` created a visible
		   gap between the xterm canvas and the workbench edges
		   (after full-bleed). The xterm renderer paints its own
		   background and includes char-cell-aware spacing already.
		   `min-height: 0` so the flex chain can shrink. */
		min-height: 0;
	}

	.terminal-pane__body :global(.xterm) {
		height: 100% !important;
	}
</style>
