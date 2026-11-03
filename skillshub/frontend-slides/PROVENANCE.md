# Frontend Slides provenance

Source: https://github.com/zarazhangrui/frontend-slides
Revision: `9906a34d640d2111f724544cbc50f7f130569ae1` (retrieved 2026-07-17).
License: MIT; upstream copyright and permission notice retained in [LICENSE](LICENSE).

This is a curated instruction-only Magician procedure, not the complete upstream
plugin. SKILL.md adapts the upstream discovery, three-preview, fixed-stage,
generation and visual verification workflow to native governed actions.
STYLE_PRESETS.md, html-template.md, animation-patterns.md, viewport-base.css and
LICENSE are unmodified upstream files; SHA-256 digests are recorded in
[UPSTREAM.json](UPSTREAM.json) and checked offline by the packaging tests.

Excluded: Claude marketplace/plugin installers, duplicate plugin tree, the large
bold-template pack, PPTX extraction script, deployment script and auto-installing
PDF exporter. There is no new executable, runtime contract, provider key or agent
grant. HTML generation is supported; editable PPTX output is not. Conversion
requires separately extracted content; PDF export requires a separately governed
available tool. No setup or network activity occurs by reading the procedure.

## Scoped setup (separate owner approval)

From the repository root, invoke the native setup approval gate for:

```sh
make -C skillshub install-scope -o build SCOPE=anonymous/default NAMES=frontend-slides
```

`-o build` skips unrelated native builds. Installation still validates the skill
catalog and materializes the named package under
`$MAGICIAN_ROOT_DIR/scopes/anonymous/default/skills/frontend-slides/`.
On this host the runtime root is `/Volumes/SSD1/MagicianNotes`. This selects
principal `anonymous`, workspace `default`, not a global/system installation.
The next chat session discovers the installed procedure; no restart is required.
This integration does not itself perform installation.
