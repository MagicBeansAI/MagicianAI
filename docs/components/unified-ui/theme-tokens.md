# Unified UI — Theme Tokens

All UI colors/surfaces/borders come from CSS custom properties ("theme tokens")
defined in `ui/unified-ui/src/app.css`. The base `:root {}` block holds the
default (light) theme; each additional theme is a `[data-theme="<name>"] {}`
block that redefines the same tokens. There are 22 themes (`VALID_THEMES`).

## The one rule

**Only use tokens that are defined for _every_ theme.** A `var(--token)` whose
`--token` is not defined in any theme silently falls back — to the CSS fallback
you wrote (`var(--token, #ccc)`) or to nothing — so it renders a hardcoded /
wrong value in dark themes. That is the most common theming bug here.

Quick audit for a component's `<style>`:

```bash
# from ui/unified-ui — lists tokens a file references but app.css never defines
awk '/<style>/,/<\/style>/' src/path/Component.svelte \
  | grep -oE "var\(--[a-z0-9-]+" | sed 's/var(--//' | sort -u \
  | while read t; do [ "$(grep -cE "\-\-$t\s*:" src/app.css)" -eq 0 ] && echo "UNDEFINED: --$t"; done
```

## Canonical token families (defined in every theme)

- **Backgrounds** — `--bg-base`, `--bg-surface`, `--bg-elevated`, `--bg-card`,
  `--bg-soft`, `--bg-cream`, `--bg-warm`
- **Text** — `--text-primary`, `--text-secondary`, `--text-muted`, `--text-faint`,
  `--text-ink`, `--text-on-accent`, `--text-body`
- **Borders** — `--border-default`, `--border-soft`
- **Accent** — `--accent-primary` (+ `-hover`, `-soft`), `--accent-secondary`
  (+ `-soft`), `--accent-tertiary`, named accents (`--accent-coral/plum/sage`)
- **Status / semantic color** — `--color-danger`, `--color-error`,
  `--color-warning`, `--color-success`, `--color-info` (each with a `-soft`
  variant); execution status: `--status-{attention,completed,failed,paused,running}`
  (+ `-soft`)
- **Shadows** — `--shadow-sm`, `--shadow-md`, `--shadow-lg`, `--shadow-glow`

Prefer these. When you need a semantic role, reach for the canonical name
(`--color-danger`, not `--danger`).

## `--text-on-accent` and the contrast contract

`:root` matches the same `<html>` element as `[data-theme="…"]` at the same
specificity, so a theme that never declares a token silently **inherits
`:root`'s value** and nothing looks broken. Therefore **every theme declares its
own `--text-on-accent`** — the general foreground for anything painted on
`--accent-primary`. `--accent-contrast` and `--accent-on-primary` alias it.
`--text-inverse` does not: it is painted on `--bg-inverse` (= `--text-primary`)
and resolves to `--bg-base`.

Two tokens, two contracts:

- **`--text-on-accent` (general accent surfaces, chat bubbles).** Rule: a
  saturated fill that is not itself light carries **white when white reaches at
  least 3:1**; dark ink stays only where white is unreadable (the mint, amber,
  gold and cyan accents of Arcane, Retro Dark, Mario Underground and Jarvis;
  Cartoon Night's pale pink; Mono Dark's white accent). Several white-labelled
  themes (Soft Machine, Bubbly, Risograph, 2D Cartoon Light, Jarvis Light,
  Mixtape Side B, Longhand Dark) therefore sit between 2.5:1 and 4.5:1 on their
  fill; reaching AA with white would need deeper fills — a palette decision not
  taken.
- **`--button-primary-color` (small-control labels).** The primary button's
  largest label is `0.875rem`/600, below WCAG's large-text exemption, so this
  token must clear **4.5:1 against every stop** of `--button-primary-bg`. It
  defaults to `--text-on-accent` at `:root`, and the themes above override it
  with their dark inks. About thirty call sites write
  `var(--button-primary-color, #fff)`, so it must stay defined at `:root`.
  **Judge a gradient at its worst stop** (eight themes use multi-stop button
  backgrounds).

`ui/unified-ui/src/lib/magician/tasks/taskPanelPresentation.test.ts` sweeps every
theme discovered in `app.css`, so a new theme that forgets `--text-on-accent`
fails the suite. `arcane-terminal-light` is exempted (marked in `app.css` as owing
an owner decision): its gradient's middle stop `#00937a` admits no compliant
foreground, so the background must move; the test fails if the exemption stops
being deserved.

## Theme fonts are split in two, and the halves are one set

`app.html` requests only the six Google families that `:root` and the default
`longhand` / `longhand-dark` resolve to. The costume families (Press Start 2P,
Bungee Shade, Special Elite, Bricolage Grotesque, Newsreader, Pixelify Sans, …)
live in `$lib/shared/themeFonts.ts` and are appended the first time such a theme
is applied. Geist Mono is self-hosted and declared once in `app.css`.

The hook is `themeStore`'s `syncTheme`, the single choke point for every theme
change (menu, `localStorage` restore, backend push). Why: one catalog with every
family is ~16KB of CSS and hundreds of `@font-face` rules parsed on every route,
while the landing renders three families. Safe because the shell link was already
async and every stack has a system fallback — this changes when a costume face
arrives, not whether the page paints.

`BASE_ONLY_THEMES` is conservative: a theme missing from it gets the costume
sheet, so adding a theme never silently loses its face. `themeFonts.test.ts`
asserts the halves union to the full set and that `app.html`'s href matches
`BASE_FONT_HREF` byte for byte (drift between `.html` and `.ts` would otherwise
render a fallback quietly).

## Compatibility aliases (backfill)

About 48 semantic tokens referenced in the app were never defined by any theme
(`--danger`, `--warning`, `--success`, `--accent`, `--text-tertiary`,
`--bg-hover`, `--border-subtle`, `--surface`, …). They are aliased to canonical
tokens on `:root, [data-theme]` at the end of `app.css`:

```css
:root, [data-theme] {
  --danger:        var(--color-danger);
  --accent:        var(--accent-primary);
  --text-tertiary: var(--text-muted);
  --surface:       var(--bg-surface);
  --bg-hover:      var(--bg-soft);
  --border-subtle: var(--border-soft);
  /* …see app.css for the full set… */
}
```

**New code should use the canonical tokens directly**; a theme may later give an
alias a distinct value, but the canonical token is the stable contract.

## Brand mark source

The "m" mark is one SVG path from `src/lib/shared/brand/mark.ts`
(`BRAND_MARK_PATH`, `BRAND_MARK_VIEWBOX`). The top bar and login card render it
inside their own coral tile; nothing renders a typographic "m" (its bearings and
x-height sit off-centre in a square tile). A surface adding the mark imports the
path and adds the `brand-mark` class for theme glows.

## Font voices

`--font-brand` is Outfit at `:root` and in every theme (themes that once
overrode it restate Outfit explicitly), so the wordmark is identical everywhere.
`--font-body` and `--font-data` are aliased at `:root` to the primary and mono
faces (they are consumed widely). Every theme pairs a **distinct display face
with a distinct primary face** — one family for both reads as one voice.

Longhand's three voices:

| Voice | Face | Tokens |
|---|---|---|
| What the product is: wordmark, headings, nav, section titles | Outfit | `--font-brand`, `--font-display` |
| How the product speaks: body copy, subtitles, labels, UI prose | Manrope | `--font-primary`, `--font-body` |
| What the machine is doing: code, logs, terminal, ids, timestamps, counters | Geist Mono | `--font-mono`, `--font-data` |

| Theme | Display | Primary | Mono |
|---|---|---|---|
| Longhand, Longhand Dark | Outfit | Manrope | Geist Mono |
| Soft Machine | Space Grotesk | Quicksand | JetBrains Mono |
| Soft Machine Dark | Fredoka | Quicksand | JetBrains Mono |
| Arcane Terminal, Light | Fira Code | IBM Plex Mono | Fira Code |
| Retro Light, Retro Dark | IBM Plex Mono | JetBrains Mono | JetBrains Mono / IBM Plex Mono |
| Mario 8-bit, Underground | Press Start 2P | Pixelify Sans | Press Start 2P |
| Risograph, Dark | Bricolage Grotesque | Manrope | JetBrains Mono |
| Mixtape, Side B | Permanent Marker | Special Elite | IBM Plex Mono |
| Mono, Mono Dark | Space Grotesk | Inter | JetBrains Mono |
| 2D Cartoon, Night | Lilita One | Fredoka | JetBrains Mono |
| Bubbly, Bubbly Dark | Fredoka | Quicksand | JetBrains Mono |
| Jarvis, Jarvis Light | Rajdhani | Manrope | JetBrains Mono |

## Theme specimen sheet

`/dev/theme-gallery` renders every shipped theme, light beside dark: the three
voices, a palette strip, the app's generative component library
(`src/lib/magician/components/generative/`: Button, Card, Input, Select,
Checkbox, RadioGroup, Toggle, Tabs, Badge, Progress, ProgressBar, Alert, Table),
a chart, a chat exchange with composer, a task card and a log block.

- **Each plate is its own document** (`?plate=<id>` in an iframe) that sets the
  theme on `<html>` before painting, so tokens, `body`-scoped theme overrides and
  daisyUI theme variables apply exactly as in the app (a `data-theme` on a
  section cannot reach body rules).
- Plates use the real components, not daisyUI classes restyled — several themes
  (Retro, Soft Machine, Arcane Light) carry their look inside those components.
  Plate chrome reads `--radius-*`. The chat exchange uses daisyUI chat classes
  mapped to tokens as `ChatPanel.svelte` does; bare daisyUI component classes
  would paint daisyUI's defaults under every theme.
- The root layout skips the theme store on this route (a plate's theme is never
  republished as the user's choice); iframes render only after mount (no
  recursive self-embed when prerendered); each plate reports its height via
  `postMessage` after switching to its single plate (the iframe `load` fires on
  the pre-hydration frame).
- `.plate` is a CSS container: one column under 760px of plate width, two voice
  tiles per row under 560px; every `.row` wraps. Bubbly follows Soft Machine for
  side-by-side comparison.
- Lives under `/dev` outside the `(app)` gate, makes no backend calls, loads the
  costume font sheet on mount, and is prerendered (`build/dev/theme-gallery.html`).
  `src/routes/dev/theme-gallery/page.test.ts` fails when a theme in
  `VALID_THEMES` has no plate and asserts the `muij-*` classes on every plate.
