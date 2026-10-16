# Theme

Matches the Magican app's palettes so notes and the app do not look like two
products. Tokens are copied from `ui/unified-ui/src/app.css`.

Switch with the command palette (`Ctrl/Cmd-/`) and type **Theme:** — each
command rewrites [[Theme/Active]] with that palette's tokens and reloads.

Two details are load-bearing. SilverBullet sets `data-theme` on `<html>` and its
own rules are `html[data-theme="dark"]`, so a plain `html` selector is
outspecified and silently loses — half the variables keep their defaults, which
looks like it partly worked. And the palette applies on **both** `data-theme`
states on purpose: an app theme already decides its own light/dark, so it must
not flip with SilverBullet's toggle.

## Mapping

The palette tokens live in [[Theme/Active]]; this maps them onto SilverBullet's
own variables, so switching rewrites ten lines rather than a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --ui-accent-color: var(--aurora-accent);
  --ui-accent-text-color: var(--aurora-accent);
  --ui-accent-contrast-color: var(--aurora-accent-contrast);

  --root-background-color: var(--aurora-bg-base);
  --root-color: var(--aurora-text-primary);

  --top-background-color: var(--aurora-bg-surface);
  --top-color: var(--aurora-text-primary);
  --top-border-color: var(--aurora-border);

  --editor-text-color: var(--aurora-text-primary);
  --editor-heading-color: var(--aurora-text-primary);
  --editor-heading-meta-color: var(--aurora-text-muted);
  --editor-meta-color: var(--aurora-text-muted);
  --editor-line-meta-color: var(--aurora-text-muted);
  --editor-link-color: var(--aurora-accent);
  --editor-naked-url-color: var(--aurora-accent);
  --editor-link-meta-color: var(--aurora-text-muted);
  --editor-caret-color: var(--aurora-accent);
  --editor-selection-background-color: var(--aurora-accent-soft);
  --editor-widget-background-color: var(--aurora-bg-soft);
  --editor-ruler-color: var(--aurora-border);
  --editor-list-bullet-color: var(--aurora-accent);
  --editor-task-marker-color: var(--aurora-accent);
  --editor-task-state-color: var(--aurora-secondary);

  --editor-wiki-link-page-color: var(--aurora-accent);
  --editor-wiki-link-page-background-color: transparent;
  --editor-wiki-link-page-missing-color: var(--aurora-text-muted);

  --editor-code-background-color: var(--aurora-bg-soft);
  --editor-code-color: var(--aurora-text-primary);
  --editor-code-comment-color: var(--aurora-text-muted);
  --editor-code-string-color: var(--aurora-secondary);

  --editor-blockquote-color: var(--aurora-text-secondary);
  --editor-blockquote-border-color: var(--aurora-accent);
  --editor-blockquote-background-color: var(--aurora-bg-soft);

  --editor-table-head-background-color: var(--aurora-bg-soft);
  --editor-table-head-color: var(--aurora-text-primary);
  --editor-table-even-background-color: var(--aurora-bg-surface);

  --editor-hashtag-color: var(--aurora-accent);
  --editor-hashtag-background-color: transparent;
  --editor-hashtag-border-color: var(--aurora-border);

  --action-button-color: var(--aurora-text-secondary);
  --action-button-hover-color: var(--aurora-accent);
  --action-button-active-color: var(--aurora-accent);
  --action-button-background-color: transparent;

  /* Header state text. .sb-saved is the page name; unmapped it inherits
     SilverBullet's white, which is invisible on a light header. */
  --top-saved-color: var(--aurora-text-primary);
  --top-unsaved-color: var(--aurora-text-muted);
  --top-loading-color: var(--aurora-text-muted);
  --top-sync-error-color: var(--aurora-accent-contrast);
  --top-sync-error-background-color: var(--aurora-danger);

  /* Frontmatter block. Unmapped it renders as a dark translucent slab
     regardless of palette. The --- delimiter lines are deliberately
     transparent in SilverBullet's own CSS; that is not ours to change. */
  --editor-frontmatter-background-color: var(--aurora-bg-soft);
  --editor-frontmatter-color: var(--aurora-text-secondary);
  --editor-frontmatter-marker-color: var(--aurora-text-muted);

  --editor-panels-bottom-background-color: var(--aurora-bg-surface);
  --editor-panels-bottom-color: var(--aurora-text-primary);
  --editor-panels-bottom-border-color: var(--aurora-border);
}
```

## Switcher

````space-lua
local themes = {
  {
    label = "Ambient Aurora",
    body = [==[
# Theme / Active

Active palette: **Ambient Aurora**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites a dozen lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #fdfcf8;
  --aurora-bg-surface: #fff8f2;
  --aurora-bg-soft: #f6f1e8;
  --aurora-text-primary: #2d3436;
  --aurora-text-secondary: #5f6668;
  --aurora-text-muted: #8f9799;
  --aurora-accent: #ff6b6b;
  --aurora-accent-soft: rgba(255, 107, 107, 0.18);
  --aurora-accent-contrast: #1a1a1a;
  --aurora-secondary: #4ecdc4;
  --aurora-danger: #ff6b6b;
  --aurora-danger-soft: rgba(255, 107, 107, 0.16);
  --aurora-border: rgba(45, 52, 54, 0.14);
}
```
]==],
  },
  {
    label = "Soft Machine Dark",
    body = [==[
# Theme / Active

Active palette: **Soft Machine Dark**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites a dozen lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #171b1d;
  --aurora-bg-surface: #1d2326;
  --aurora-bg-soft: #20272a;
  --aurora-text-primary: #f7f1e8;
  --aurora-text-secondary: #d7cfc2;
  --aurora-text-muted: #a79f95;
  --aurora-accent: #ff7b7b;
  --aurora-accent-soft: rgba(255, 123, 123, 0.18);
  --aurora-accent-contrast: #171b1d;
  --aurora-secondary: #54d3cb;
  --aurora-danger: #ff7b7b;
  --aurora-danger-soft: rgba(255, 123, 123, 0.16);
  --aurora-border: rgba(247, 241, 232, 0.14);
}
```
]==],
  },
  {
    label = "Soft Machine",
    body = [==[
# Theme / Active

Active palette: **Soft Machine**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites a dozen lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #fefdfb;
  --aurora-bg-surface: #f8f6f2;
  --aurora-bg-soft: #f3f0ea;
  --aurora-text-primary: #2d2a26;
  --aurora-text-secondary: #4a4540;
  --aurora-text-muted: #8a847a;
  --aurora-accent: #e85d5d;
  --aurora-accent-soft: rgba(232, 93, 93, 0.18);
  --aurora-accent-contrast: #000000;
  --aurora-secondary: #6b9080;
  --aurora-danger: #d4574a;
  --aurora-danger-soft: rgba(212, 87, 74, 0.16);
  --aurora-border: rgba(45, 42, 38, 0.14);
}
```
]==],
  },
  {
    label = "Arcane Terminal",
    body = [==[
# Theme / Active

Active palette: **Arcane Terminal**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites a dozen lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #0a0a0f;
  --aurora-bg-surface: #12121a;
  --aurora-bg-soft: #1a1a2e;
  --aurora-text-primary: #e0e0e0;
  --aurora-text-secondary: #b0b0b0;
  --aurora-text-muted: #6a6a7a;
  --aurora-accent: #00d4aa;
  --aurora-accent-soft: rgba(0, 212, 170, 0.18);
  --aurora-accent-contrast: #000000;
  --aurora-secondary: #9d4edd;
  --aurora-danger: #ff4757;
  --aurora-danger-soft: rgba(255, 71, 87, 0.16);
  --aurora-border: rgba(224, 224, 224, 0.14);
}
```
]==],
  },
  {
    label = "Arcane Terminal Light",
    body = [==[
# Theme / Active

Active palette: **Arcane Terminal Light**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites a dozen lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #f6f8fa;
  --aurora-bg-surface: #eef1f4;
  --aurora-bg-soft: #e2e7ec;
  --aurora-text-primary: #0a0a0f;
  --aurora-text-secondary: #2a2a35;
  --aurora-text-muted: #5a5a68;
  --aurora-accent: #007a66;
  --aurora-accent-soft: rgba(0, 122, 102, 0.18);
  --aurora-accent-contrast: #f6f8fa;
  --aurora-secondary: #6929c4;
  --aurora-danger: #b91c1c;
  --aurora-danger-soft: rgba(185, 28, 28, 0.16);
  --aurora-border: rgba(10, 10, 15, 0.14);
}
```
]==],
  },
}
```
]==],
  },
  {
    label = "Soft Machine Dark",
    body = [==[
# Theme / Active

Active palette: **Soft Machine Dark**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites ten lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #171b1d;
  --aurora-bg-surface: #1d2326;
  --aurora-bg-soft: #20272a;
  --aurora-text-primary: #f7f1e8;
  --aurora-text-secondary: #d7cfc2;
  --aurora-text-muted: #a79f95;
  --aurora-accent: #ff7b7b;
  --aurora-accent-soft: rgba(255, 123, 123, 0.18);
  --aurora-accent-contrast: #171b1d;
  --aurora-secondary: #54d3cb;
  --aurora-border: rgba(247, 241, 232, 0.14);
}
```
]==],
  },
  {
    label = "Soft Machine",
    body = [==[
# Theme / Active

Active palette: **Soft Machine**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites ten lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #fefdfb;
  --aurora-bg-surface: #f8f6f2;
  --aurora-bg-soft: #f3f0ea;
  --aurora-text-primary: #2d2a26;
  --aurora-text-secondary: #4a4540;
  --aurora-text-muted: #8a847a;
  --aurora-accent: #e85d5d;
  --aurora-accent-soft: rgba(232, 93, 93, 0.18);
  --aurora-accent-contrast: #000000;
  --aurora-secondary: #6b9080;
  --aurora-border: rgba(45, 42, 38, 0.14);
}
```
]==],
  },
  {
    label = "Arcane Terminal",
    body = [==[
# Theme / Active

Active palette: **Arcane Terminal**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites ten lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #0a0a0f;
  --aurora-bg-surface: #12121a;
  --aurora-bg-soft: #1a1a2e;
  --aurora-text-primary: #e0e0e0;
  --aurora-text-secondary: #b0b0b0;
  --aurora-text-muted: #6a6a7a;
  --aurora-accent: #00d4aa;
  --aurora-accent-soft: rgba(0, 212, 170, 0.18);
  --aurora-accent-contrast: #000000;
  --aurora-secondary: #9d4edd;
  --aurora-border: rgba(224, 224, 224, 0.14);
}
```
]==],
  },
  {
    label = "Arcane Terminal Light",
    body = [==[
# Theme / Active

Active palette: **Arcane Terminal Light**

Written by a `Theme:` command — edit [[Theme]] to change palettes, not this page.
Only the palette tokens live here; the mapping onto SilverBullet's own variables
stays in [[Theme]] so a switch rewrites ten lines instead of a hundred.

```space-style
html[data-theme], html:not([data-theme]) {
  --aurora-bg-base: #f6f8fa;
  --aurora-bg-surface: #eef1f4;
  --aurora-bg-soft: #e2e7ec;
  --aurora-text-primary: #0a0a0f;
  --aurora-text-secondary: #2a2a35;
  --aurora-text-muted: #5a5a68;
  --aurora-accent: #007a66;
  --aurora-accent-soft: rgba(0, 122, 102, 0.18);
  --aurora-accent-contrast: #f6f8fa;
  --aurora-secondary: #6929c4;
  --aurora-border: rgba(10, 10, 15, 0.14);
}
```
]==],
  },
}

for _, theme in ipairs(themes) do
  command.define {
    name = "Theme: " .. theme.label,
    run = function()
      space.writePage("Theme/Active", theme.body)
      editor.flashNotification("Theme: " .. theme.label)
      editor.reloadUI()
    end
  }
end
````
