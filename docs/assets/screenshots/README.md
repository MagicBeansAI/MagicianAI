# README screenshots

Installer previews captured on 2026-10-15:

- `installer-gui.png` renders the shipped `desktop/src/lib/Setup.svelte` and
  Desktop stylesheet in Chromium with the existing Sorbet light theme. It
  resolves the 21 production theme-sync tokens from the app stylesheet and
  applies them through Desktop's normal CSS-variable contract. The native
  boundary supplies the current
  placement labels/descriptions from `desktop/src-tauri/src/setup.rs`, a
  placeholder data folder and an empty permission list. No installation,
  sign-in or permission request is performed.
- `installer-shell.png` captures the actual source installer's
  `--dry-run --phase prepare` output against an isolated temporary checkout
  scaffold, prefix and data folder. It does not run the printed commands.
- `installer-tui.png` captures the real `magician-setup --dry-run` welcome
  screen in a 110-column terminal, with an isolated data root. The wizard is
  exited before continuing. Component counts and readiness are local probe
  results, not installation guarantees.

Terminal captures replay the original ANSI output in xterm.js; GUI captures
render the actual component. Screenshots contain no credentials or live
Cloudflare/Clarity configuration. Refresh previews when their source screens
change; keep the linked full-size images readable.
