# Unified UI — Theme Tokens

All UI colors, surfaces and borders come from CSS custom properties ("theme
tokens") in `ui/unified-ui/src/app.css`. The base `:root {}` block is the
default (light) theme; each other theme is a `[data-theme="<name>"] {}` block
that redefines the same tokens. There are 24 themes (`VALID_THEMES`).

**Default theme.** `sorbet` (light): `DEFAULT_THEME` in `themeStore.ts`, the
`data-theme` attribute and inline-script fallback in `app.html`, and
`default_theme()` in `magician/src/magician_v2/ui_preferences.rs` all name it;
`themeRegistry.test.ts` pins the frontend three. Dark is opt-in; `sorbet-dark`
is never chosen automatically. A saved theme is kept (the `app.html` script
persists the resolved theme on first load and the theme store writes it to the
backend when the scope has none), and no migration rewrites a stored choice.

**Retired themes.** `bubbly`, `bubbly-dark`, `soft-machine` and
`soft-machine-dark` open as Sorbet on the same side (`sorbet-dark` or
`sorbet`). `RETIRED_THEME_ALIASES` in `themeStore.ts` holds the map;
`app.html`'s pre-hydration script carries a copy (`retiredThemes`) so the first
paint is right and the resolved id is written back to localStorage. A retired
id in the backend's `ui_preferences` is resolved the same way and rewritten
once by the theme store; the backend validates only the id's shape.
`themeRegistry.test.ts` checks the two maps agree and that no theme list still
names a retired id; `themeStore.test.ts` runs the inline script against each.

**Pastel families.** Sorbet, Sky and Candy (each with a `-dark` sibling) lead
`VALID_THEMES` and the menu. They are clean, non-costume skins (Nunito body,
Outfit brand, Geist Mono code, with Fredoka (Sorbet), Outfit (Sky) or Baloo 2 (Candy) for display)
that carry personality through the liveliness tokens, not bespoke components.
Registering a theme touches every list that enumerates them: `VALID_THEMES`,
`app.html`'s inline `validThemes`, `ThemeSwitcher.svelte`, `TopBar.svelte`'s
`THEME_PAIRS`, the gallery's `plates.ts`, and `themeFonts.ts`;
`themeRegistry.test.ts` checks they agree.

## Landing appearance

The root marketing page owns the October brand's Day/Night pair locally under
`.lp-root[data-appearance]`. It registers no app theme and does not write the
visitor's saved preference. Its scoped tokens use Outfit, off-white/ink with
gold and pastel horizon washes in Day, and charcoal/light text/gold in Night.
`warmPalette.ts` mirrors its CSS; contrast tests cover both appearances.
See the [root landing contract](root-landing-native-route.md#october-brand-opening).

## The one rule

**Only use tokens that are defined for _every_ theme.** A `var(--token)` whose
token no theme defines silently falls back, to the CSS fallback you wrote
(`var(--token, #ccc)`) or to nothing, and renders a wrong value in dark themes.
This is the most common theming bug here.

Quick audit for a component's `<style>`:

```bash
# from ui/unified-ui — lists tokens a file references but app.css never defines
awk '/<style>/,/<\/style>/' src/path/Component.svelte \
  | grep -oE "var\(--[a-z0-9-]+" | sed 's/var(--//' | sort -u \
  | while read t; do [ "$(grep -cE "\-\-$t\s*:" src/app.css)" -eq 0 ] && echo "UNDEFINED: --$t"; done
```

## Action chip (`.mg-chip-action`)

One global class in `src/app.css` for small secondary actions in chat:
**Inspect run →** (`RequestActivityCard`), **Open task →** / **Inspect** /
**+N more** (chat-next `LiveTaskCard`), and every file action in
`ChatContentBlocks` (also `ComposerAskDock` and `TaskActSection`). Tokens only:
`--text-secondary` text, a border mixing 45% `--accent-secondary` into
`--border-soft`, `--radius-chip`, hover `--accent-secondary-soft` /
`--text-primary`, focus `--ring-focus`, transitions on `--dur-quick`. Callers
add placement only. The accent stays out of the text because
`--accent-secondary` text measured about 1.9:1 on white in the teal themes; fix
contrast in the token, not per caller.

## Composer radius

`--radius-composer` sets the floating composer's corners: a fixed 10px rounded
square in every theme.

## Radius scale: rounded squares

Corners read as rounded squares, never soft blobs or pills. The default and
pastel scales are `--radius-sm` 6px, `--radius-md` 8px, `--radius-lg` 10px,
`--radius-xl` 12px. Panels, dialogs and cards use the tokens, not hardcoded
radii. Buttons, chips and badges use `--radius-chip` (6px). `--radius-full` is
only for true circles and bars: dots, avatars, the presence orb, progress
tracks. Exceptions: the landing page's device mockups keep their own radii
(they draw hardware), and the October Bright Sidekick landing uses pill actions
and example selectors, scoped to the public root page.

## Canonical token families (defined in every theme)

- **Backgrounds**: `--bg-base`, `--bg-surface`, `--bg-elevated`, `--bg-card`,
  `--bg-soft`, `--bg-cream`, `--bg-warm`
- **Text**: `--text-primary`, `--text-secondary`, `--text-muted`,
  `--text-faint`, `--text-ink`, `--text-on-accent`, `--text-body`
- **Borders**: `--border-default`, `--border-soft`
- **Accent**: `--accent-primary` (+ `-hover`, `-soft`), `--accent-fill`
  (+ `-hover`), `--accent-secondary` (+ `-soft`), `--accent-tertiary`, named
  accents (`--accent-coral/plum/sage`)
- **Status**: `--color-danger`, `--color-error`, `--color-warning`,
  `--color-success`, `--color-info` (each with `-soft`); execution status
  `--status-{attention,completed,failed,paused,running}` (+ `-soft`)
- **Shadows**: `--shadow-sm`, `--shadow-md`, `--shadow-lg`, `--shadow-glow`

Use the canonical name for a semantic role (`--color-danger`, not `--danger`).

## The accent as a fill: `--accent-fill`

One accent, two roles:

- **`--accent-primary` / `--accent`**: the bright accent for anything drawn
  *on* a normal surface: accent text and icons, borders, outlines, focus rings,
  progress strokes, underline indicators, status dots.
- **`--accent-fill` / `--accent-fill-hover`**: the accent as a *fill* carrying
  `--text-on-accent` (or `--accent-contrast` / `--accent-on-primary` /
  `--button-primary-color`): primary buttons, the user's chat bubble, selected
  chips/tabs/segments, badges, send buttons, checked checkboxes, accent
  gradients behind a label. Hover and pressed fills use `--accent-fill-hover`.

Light themes: the fill **is** the accent (`--accent-fill: var(--accent-primary)`,
`--accent-fill-hover: var(--accent-primary-hover)`), declared once on
`:root, [data-theme]` *before* every theme block so a nested `data-theme` (the
gallery's plates) resolves its own accent. Dark themes declare both in their
own block: the accent's HSL hue and saturation with lightness lowered until
white reaches 4.5:1 (hover about 5.5:1), with `--text-on-accent: #ffffff`. The
fills match the cross-platform reference table (Android):

| Dark theme | `--accent-primary` | `--accent-fill` | `--accent-fill-hover` | white on fill |
|---|---|---|---|---|
| sorbet-dark | `#ff8a94` | `#ed0014` | `#d40012` | 4.55:1 |
| sky-dark | `#8e9aff` | `#5163ff` | `#3c51ff` | 4.59:1 |
| candy-dark | `#ff8fc7` | `#e60073` | `#cd0067` | 4.55:1 |
| longhand-dark | `#d8602e` | `#c35324` | `#ae4a21` | 4.58:1 |
| arcane-terminal | `#00d4aa` | `#00856b` | `#007760` | 4.60:1 |
| retro-16bit | `#ffb000` | `#9e6d00` | `#8c6000` | 4.52:1 |
| mario-8bit-dark | `#fbd000` | `#8b7300` | `#7d6700` | 4.62:1 |
| risograph-dark | `#ff48b0` | `#e40081` | `#cb0074` | 4.55:1 |
| mixtape-dark | `#ff5040` | `#eb1400` | `#d11200` | 4.53:1 |
| mono-dark | `#ffffff` | `#757575` | `#686868` | 4.61:1 |
| cartoon-dark | `#ff7eb6` | `#e90065` | `#cf005a` | 4.51:1 |
| jarvis | `#00d4ff` | `#00819c` | `#00728a` | 4.55:1 |

`--button-primary-bg` defaults to `var(--accent-fill)`; dark themes with a
button gradient run it through the fill shades (Arcane keeps its `#9d4edd` end
stop, 4.60:1 with white; Jarvis ends on `#008851`).

**`--text-on-bright`** is the dark ink for bright *non-accent* fills where
white fails in dark themes: error and warning pills and buttons
(`--color-error`, `--vibe-warning`) and the top-bar avatar on the logo
gradient. Dark themes declare it; `:root, [data-theme]` resets it to the
guaranteed-invalid value, so consumers write
`var(--text-on-bright, <previous foreground>)` and light themes paint as
before.

daisyUI's `--color-primary` is **not** wired to the theme accent. Its
unstyled consumers are the `btn-primary` buttons in `ChatBubble.svelte` and
`PreferenceChecksPanel.svelte`, left as is; `EnginesPanel.svelte` and
`TerminalGrantsPanel.svelte` restyle `.btn-primary` with `--accent-fill`.

## `--text-on-accent` and the contrast contract

`:root` matches the same `<html>` element as `[data-theme="…"]` at the same
specificity, so a theme that never declares a token silently **inherits
`:root`'s value**. Therefore **every theme declares its own
`--text-on-accent`**, the foreground for anything painted on
`--accent-primary`. `--accent-contrast` and `--accent-on-primary` alias it.
`--text-inverse` does not: it is painted on `--bg-inverse` (= `--text-primary`)
and resolves to `--bg-base`.

Two tokens, two contracts:

- **`--text-on-accent` (accent surfaces, chat bubbles), painted on
  `--accent-fill`.** Every dark theme is `#ffffff` on its deep fill (≥ 4.5:1).
  Light themes keep their own values: a saturated, non-light fill carries white
  when white reaches at least 3:1, so Sorbet, Sky, Candy, Risograph, 2D Cartoon
  and Jarvis Light sit between 3:1 and 4.5:1.
- **`--button-primary-color` (small-control labels).** The largest primary
  button label is `0.875rem`/600, below WCAG's large-text exemption, so this
  token must clear **4.5:1 against every stop** of `--button-primary-bg`. It
  defaults to `--text-on-accent` at `:root`; light themes with a bright button
  fill override it with dark ink. 35 call sites write
  `var(--button-primary-color, #fff)`, so it must stay defined at `:root`.
  **Judge a gradient at its worst stop** (ten themes use a gradient
  `--button-primary-bg`: Arcane, Jarvis, Sorbet, Sky and Candy, light and
  dark).

`ui/unified-ui/src/lib/magician/tasks/taskPanelPresentation.test.ts` sweeps
every theme in `app.css`, so a theme missing `--text-on-accent` fails. Its
"accent as a fill" block derives which themes are dark (from `--bg-base`),
requires each to declare a fill that keeps the accent's hue and saturation and
gives white ≥ 4.5:1, and pins every light theme's fill to its accent.
`arcane-terminal-light` is exempt (marked in `app.css` as owing an owner
decision): its gradient's middle stop `#00937a` admits no compliant foreground,
so the background must move. The test fails once the exemption is no longer
needed.

## Theme fonts are split in two, and the halves are one set

`app.html` requests only the seven Google families used by `:root`,
`longhand` / `longhand-dark` and the Sorbet / Sky pairs (Candy's Baloo 2 stays
costume). The costume families (Press Start 2P, Bungee Shade, Special Elite,
Bricolage Grotesque, Newsreader, Pixelify Sans, …) live in
`$lib/shared/themeFonts.ts` and load the first time such a theme is applied.
Geist Mono is self-hosted and declared once in `app.css`.

The hook is `themeStore`'s `syncTheme`, the single choke point for every theme
change (menu, `localStorage` restore, backend push). One catalog with every
family is ~16KB of CSS and hundreds of `@font-face` rules parsed on every
route. It is safe because the shell link is async and every stack has a system
fallback: it changes when a costume face arrives, not whether the page paints.

`BASE_ONLY_THEMES` is conservative: a theme missing from it gets the costume
sheet, so a new theme never silently loses its face. `themeFonts.test.ts`
asserts the halves union to the full set and that `app.html`'s href matches
`BASE_FONT_HREF` byte for byte.

## Compatibility aliases (backfill)

47 semantic tokens used in the app were never defined by any theme
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
alias its own value.

## Brand mark source

The "m" mark is one SVG path from `src/lib/shared/brand/mark.ts`
(`BRAND_MARK_PATH`, `BRAND_MARK_VIEWBOX`). The top bar and login card render it
inside their own coral tile; nothing renders a typographic "m". A surface adding
the mark imports the path and adds the `brand-mark` class for theme glows.

## Font voices

`--font-brand` is Outfit at `:root` and in every theme, so the wordmark is
identical everywhere. `--font-body` and `--font-data` alias the primary and mono
faces at `:root`. Every theme pairs a **distinct display face with a distinct
primary face**.

Longhand's three voices:

| Voice | Face | Tokens |
|---|---|---|
| What the product is: wordmark, headings, nav, section titles | Outfit | `--font-brand`, `--font-display` |
| How the product speaks: body copy, subtitles, labels, UI prose | Manrope | `--font-primary`, `--font-body` |
| What the machine is doing: code, logs, terminal, ids, timestamps, counters | Geist Mono | `--font-mono`, `--font-data` |

| Theme | Display | Primary | Mono |
|---|---|---|---|
| Longhand, Longhand Dark | Outfit | Manrope | Geist Mono |
| Arcane Terminal, Light | Fira Code | IBM Plex Mono | Fira Code |
| Retro Light, Retro Dark | IBM Plex Mono | JetBrains Mono | JetBrains Mono / IBM Plex Mono |
| Mario 8-bit, Underground | Press Start 2P | Pixelify Sans | Press Start 2P |
| Risograph, Dark | Bricolage Grotesque | Manrope | JetBrains Mono |
| Mixtape, Side B | Permanent Marker | Special Elite | IBM Plex Mono |
| Mono, Mono Dark | Space Grotesk | Inter | JetBrains Mono |
| 2D Cartoon, Night | Lilita One | Fredoka | JetBrains Mono |
| Jarvis, Jarvis Light | Rajdhani | Manrope | JetBrains Mono |

## Theme specimen sheet

`/dev/theme-gallery` renders every theme, light beside dark: the three voices,
a palette strip, the generative component library
(`src/lib/magician/components/generative/`: Button, Card, Input, Select,
Checkbox, RadioGroup, Toggle, Tabs, Badge, Progress, ProgressBar, Alert,
Table), a chart, a chat exchange with composer, a task card and a log block.

- **Each plate is its own document** (`?plate=<id>` in an iframe) that sets the
  theme on `<html>` before painting, so tokens, `body`-scoped overrides and
  daisyUI theme variables apply exactly as in the app.
- Plates use the real components, since several themes (Retro, Arcane Light)
  carry their look inside them. Plate chrome reads `--radius-*`. The chat
  exchange maps daisyUI chat classes to tokens as `ChatPanel.svelte` does.
- The root layout skips the theme store on this route (a plate's theme is never
  saved as the user's choice); iframes render only after mount; each plate
  reports its height via `postMessage` after switching to its plate.
- `.plate` is a CSS container: one column under 760px of plate width, two voice tiles per row
  under 560px; every `.row` wraps.
- It lives under `/dev` outside the `(app)` gate, makes no backend calls, loads
  the costume font sheet on mount, and is prerendered
  (`build/dev/theme-gallery.html`). `src/routes/dev/theme-gallery/page.test.ts`
  fails when a theme in `VALID_THEMES` has no plate and asserts the `muij-*`
  classes on every plate.
