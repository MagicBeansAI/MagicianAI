# Magician anatomy

A break-open model of the Magician codebase, for demos. Every number it shows is read from the source by `data/extract.mjs`, never typed by hand.

| Page | What it is |
|---|---|
| `explorer.html` | The site's front page. One 3D canvas holding the whole system: system → processes → crates → modules → files → types. Click a box to break it open in place; the focused branch stays solid and the rest turns to wireframe. Orbit, pan, zoom, search, guided tour, four skins. |
| `media/` | Tour audio: `tour-music.m4a` (ambient bed) and `narration/*.mp3` (one clip per spoken line, `manifest.json` maps text → clip). |

## Tours

**Explore → Tour** walks one branching graph: the sealed case → where a request comes from (chat, task, voice, app, or how it is deployed) → every fork the code really has (which engine runs a turn, local or remote model, which provider, decision engine on or off, which voice engine, how a tool runs, …). Each step lands on a real file or type, with its `file:line` under the caption. Nerd mode adds detail steps; auto-pick takes the usual path at each fork.

| Part | Source |
|---|---|
| intro, setups, features | generated in `explorer/tours.mjs` from the data (`data/world.json`, `data/features-*.json`); a setup's route can carry a plain-language `say`, otherwise its step just names the hop |
| chat, brain (model calls + decision engine), task (+ quality, browser), voice, apps | `explorer/graph/<part>.json`, format in `explorer/graph/CONTRACT.md` |

**Explore → Features** lists the catalog in two groups: **What it does** (what a person uses) and **Under the hood** (the platform underneath: durable runs, live updates, storage, routing, remote access through Cloudflare, the apps runtime). A filter box at the top narrows the list by name, description or maturity, and Enter opens the first match. Picking one lights up every place it lives in the code, and "Play its tour" walks it. Previous/Next controls in the bottom strip follow the filtered list and disable at its boundaries. A feature's optional `group` field places it; without one it counts as "What it does". `data/features-a.json` and `features-b.json` hold the product features, and `features-c.json` the rest. Features and Tour share one dropdown with keyboard-accessible segmented tabs; Escape closes it and restores focus to Explore.

The header's theme-aware mascot links back to [magican.ai](https://magican.ai), beside the [public GitHub mirror](https://github.com/MagicBeansAI/MagicianAI) mark. Both retain 44px touch targets and stay visible beside Menu on mobile; header items share the same vertical center. A 44px search icon is available directly in the header on desktop and mobile. Clicking it or pressing `/` replaces the entire header with a focused, borderless search field and Cancel; focus accents the search icon without boxing the input. Cancel/Escape clears search and restores the normal header and icon focus; selecting a result also closes search. Enter opens the first match and Up/Down navigate results. Reduced motion disables the arrival animation.

**Overview** opens `/overview`, a plain HTML architecture and
feature directory generated from the same catalog, preserving its maturity
labels. Both pages carry unique canonical, Open Graph, Twitter and JSON-LD
metadata. Packaging adds `robots.txt`, `sitemap.xml`, an optional `llms.txt`
directory, the shared 1200×630 social image and a real noindex 404 page.
Preview deployments keep production canonicals and use noindex HTML plus an
`X-Robots-Tag` response header; their URLs remain publicly accessible.
See the [public-site contract](../../docs/components/unified-ui/public-site-metadata.md).

Controls: Space pauses, → next, ← previous step (or previous/next feature), 1–9 pick an option, M mutes, Esc stops. Tour rewind restores path, counters and call state, preserves pause, and cancels superseded timers/narration. During option narration it returns to that fork; it also works from the completed tour. `explorer.html?tour` starts on the first click.

To check every step against the source (file exists, fn defined there, type declared there):

```sh
node explorer/graph/validate.mjs         # report
node explorer/graph/validate.mjs --fix   # move lines to the fn/type definition, keeping the branch line in evidence
```

### Narration

One clip per spoken line, named after its step, in `media/narration/<part>/`:

```
media/narration/entry/entry.choose--where-does-the-request-come-from.mp3
media/narration/entry/entry.choose.option-1--a-chat-message.mp3
media/narration/llm/llm.where--local-or-remote-model.mp3
```

`media/narration/manifest.json` maps each step id to its file and the exact text and voice it was made with, so editing a line's text re-voices only that line.

```sh
node explorer/narrate.mjs                      # voice whatever is new or changed (needs Voicebox running)
node explorer/narrate.mjs --redo chat.engine   # voice again every line whose id starts with this (repeatable)
node explorer/narrate.mjs --list               # id → file → text
node explorer/narrate.mjs --prune              # delete clips no step uses any more
VOICE_PROFILE=<voicebox profile id> node explorer/narrate.mjs   # another voice
node explorer/build.mjs                        # then rebuild so the page picks the clips up
```

Spoken text goes through a pronunciation map (`speakable()` in `tours.mjs`), so code names read naturally while captions keep the real names.

## Keeping it current

```sh
make showcase-refresh                  # re-read the code, fix moved pointers, rebuild, print the drift report
make showcase-refresh ARGS=--narrate   # also voice new or changed lines (Voicebox must be running)
make showcase-refresh ARGS="--min-lines 800"   # widen the uncovered-modules list
```

`refresh.mjs` does the steps below in order:

1. Re-runs both extractors.
2. Runs `explorer/graph/validate.mjs --fix`. A tour step whose function moved gets its new line; the old line stays in its evidence.
3. Checks every feature location and tour step in `data/features-*.json`:
   - the file exists;
   - its `fn` is still defined there (Rust, Swift, Kotlin, TypeScript and Svelte forms);
   - its `type` is still declared there.

   A pointer that only moved is updated to the definition's new line.
4. Rebuilds `explorer.html`.
5. Prints a drift report:
   - tour or feature pointers that no longer match the code;
   - modules that appeared since the last refresh;
   - the biggest modules no feature or tour step mentions (the backlog for new features);
   - narration lines not voiced yet.

What it cannot do is write meaning. A new feature (its places, tour and narration) is still a hand edit in `data/features-*.json`; the report says where one is missing.

**Provenance.** The hover card on every box ends with where it came from:
- the crate version (crates only);
- the newest commit touching any file beneath the box, with its date and time;
- `+ uncommitted edits` when a file beneath it is dirty;
- the build time and the commit the page was built from.

The bottom strip shows the build time too. `build.mjs` reads this from a single `git log --name-only` pass, so a rebuild stamps it; nothing is typed by hand.

## Rebuild

```sh
node showcase/anatomy/data/extract.mjs        # source → data/magician.json (cargo metadata + scan)
node showcase/anatomy/explorer/build.mjs      # → explorer.html
node showcase/anatomy/explorer/narrate.mjs    # → media/narration (needs Voicebox running), then build again
node showcase/anatomy/explorer/record.mjs --skin sorbet --out showcase/anatomy/media/tour.mp4   # needs Chrome + ffmpeg
node skillshub/anatomy/scripts/drive.mjs "file://$PWD/showcase/anatomy/explorer.html" --actions steps.json   # headless checks and screenshots
node showcase/anatomy/explorer/site.mjs [--no-media | --no-narration]   # → showcase/anatomy/dist/: the deployable site (gitignored); the explorer is its index.html
make publish-showcase-preview          # deploy to a preview URL on Cloudflare Pages (explain.magican.ai untouched)
make publish-showcase                  # deploy to explain.magican.ai; ARGS=--no-narration ships no voice, ARGS=--dry-run uploads nothing
```

`explorer.html?tour` starts the tour, `?skin=sorbet|night|blueprint|paper` picks a skin (Sorbet is the app's default theme; `brand`, its earlier name, still works), `?clean` hides the UI for recording, and `?ortho` switches back to a parallel (orthographic) projection.

The camera is a long lens (an 18° field of view), not a parallel projection. A parallel projection draws a box's far edge exactly as long as its near edge; the eye expects the far edge to be shorter, so it reads equal edges as the far one being longer, and lids look like rhombuses. The lens shrinks far edges slightly and keeps the drawing flat. Framing works in "scale" (pixels per world unit at the point the camera looks at): the orthographic camera's zoom, or the lens's distance. `fitView` measures the real projected outline, so a box fills the space between the bar and the strip the same way under either camera.

## Phones, tablets and desktops

- **Compact layout.** It applies when the screen is narrower than 760 px, shorter than 500 px (a phone on its side), or a touch screen with a phone-sized short side.
  - The bar is one row: mascot, breadcrumb, GitHub, search icon and **Menu** holding Overview, Explore (Features/Tour), the sound icon, Expand/Seal, Reset view and the skin. Search replaces that row and closes other menus. Switching tabs or configuring a tour keeps the menu open; selecting a feature or starting a tour closes it.
  - The bottom strip is one line; a tap shows the rest.
  - The narration folds to its title and choices with a tap.
  - Notch and home-bar safe areas are respected.
- **Framing.** The view angle is picked to fill the screen. On tall screens a steeper view turned toward the model's long side fills it clearly better, and turning the device picks again. The bar and strip are measured rather than assumed, so framing leaves exactly their space.
- **Touch.** A first tap lights a box or cable and shows its tooltip; a second tap opens or follows it. Drag turns the view, pinch zooms and two fingers pan.
- **Labels.** In tour paths and feature spotlights, labels are kept inside the screen, and one that would overlap an earlier label shrinks to its number. Elsewhere, a label that would sit on a bigger box's label is hidden; hover or tap still names that box.
- **Cost.**
  - The model draws only while something changes (input, a tween, a tour, cable beads) and sleeps otherwise. Its separate ambient shader animates only while the case is sealed and the tab is visible.
  - Movement is measured with a tolerance, because OrbitControls' damping never reports zero.
  - Phones get 1.5× pixel density, lighter cable geometry and no idle beads.
  - The drawing buffer is kept only for recording (`?clean`).
- **Audio.** Served over http(s), the music bed runs through a Web Audio gain node, because iOS Safari ignores `HTMLMediaElement.volume`. Opened from a file it uses the element's own volume.
- **Tour controls.** While a tour plays, the right end of the narration strip holds previous step (←), pause/resume (Space), next step (→), stop (Esc), and separate voice (V) and music (B) switches. Pausing also fades the music. The voice and music choices are remembered in the browser; the top bar's speaker icon (M) silences both without changing them, with a crossed-out muted state and action tooltip/accessibility label. With the voice off, each step waits as long as its caption takes to read.
- **Quiet chrome.** Two seconds after the page opens, and whenever you press in the scene, the top bar and bottom strip fade to 14% and return when the pointer nears them, when they hold focus, or after a click selects something. So the faded top bar stays discoverable, a 100px band travels across it every 12 seconds and only what is inside the band shows at full strength. The bottom strip just stays faded, since the pointer is near it most of the time. It is a CSS mask, so the scene does not redraw. Phones keep their chrome at 72%, and reduced motion gets a steady 32% top bar instead of the band.
- **Keys.** `?` opens the shortcut list.
- **Deploy build.** `explorer/site.mjs` writes the explorer as `dist/index.html`, so the site opens straight into it. A `_redirects` file sends `/explorer` and the retired figure and landing URLs to `/`. It also copies the voiced clips. It vendors three.js and the addons the explorer imports, so there is no CDN at runtime; the import map is rewritten to point at them. It also writes Cloudflare Pages `_headers`: immutable `vendor/`, a day for `media/`, five minutes for pages.

## Ambient backgrounds

`explorer/ambient.mjs` supplies theme-specific backgrounds using the existing
Three.js dependency. `build.mjs` embeds its factory into the standalone HTML;
there is no extra runtime module or CDN request. Paper uses magnetic ink
filaments with traveling gold charges, Sorbet flowing iridescent ribbons, and
Blueprint sparse electric-blue particle currents with soft tails. The light
themes bend their flow around the pointer and reveal stronger ink/color there;
these effects are owner-accepted and frozen during the dark-theme revisions.
Night uses teal fluid light: a small feedback texture advects cursor velocity
and dye, leaving a curling wake that fades after movement. Both dark themes
remain quiet away from the pointer. The rejected pipes, grid/dials and polygon
forms have been removed. Leaving the page or window clears the pointer halo;
the fluid wake dissipates. Theme transitions blend palettes and drawings.

The backdrop has its own antialiased canvas, capped at 30 draws/second and
2× CSS resolution on desktop, or 18 draws/second and 1× on lightweight
clients. Night uses one feedback pass and one display pass, with two byte-format
render targets capped at 512 pixels on their longest side (256 on compact
clients); this is a decorative advection effect, not a full fluid solver.
Blueprint batches 580 independent particles (180 on compact clients) into one
draw call. The light themes use a single shader quad. The main canvas is
transparent, so the model can keep its existing idle behavior. Opening any box
immediately starts a 180ms backdrop fade and stops shader drawing; sealing the
whole case restores it after the closing animation. Hidden tabs stop drawing,
reduced motion uses a static image, and clean/record capture excludes the
backdrop and retains the original opaque model canvas. Unsupported background
contexts/shaders fall back to the solid theme color. Pointer/visibility/media
listeners, render targets and GPU resources have explicit disposal.
`window.__explorer.ambient` exposes draw/simulation counts for qualification.

Build with `node showcase/anatomy/explorer/build.mjs`, package with
`node showcase/anatomy/explorer/site.mjs`, then serve `showcase/anatomy/dist/`
locally. Publish the reviewed build with `make publish-showcase-preview` and
`make publish-showcase`. Rollout status and qualification evidence are in the
shader background plan.

## What the data means

- **Crates in a process:** the workspace crates linked into that binary (transitive `cargo metadata` dependencies).
- **Crate wires:** direct Cargo dependencies, with the edges that follow from others removed.
- **Module wires:** `use crate::…` / `super::…` references between sibling modules, the 40 strongest per level.
- **Cross-crate wires:** `other_crate::…` paths, counted per (module, target crate, target module) and kept only where Cargo declares the dependency (`crossLinks` in `magician.json`). Select or open a module or file inside a crate to see its heaviest six out (accent) and in (second colour).
- **Client wires:** from the clients research (`data/world.json`). A fifth link field names the real end inside the server (`p:magician` → `magician-api`), so the cable continues there once those boxes are open.

### How wires are drawn

- **Routing.** Each open box routes its cables with A* over a grid of its floor. Turns cost extra, and cells another cable already uses cost less, so cables heading the same way merge into bundles. Levels laid out by hand (the world, the server) run cables on the floor, from jacks on the box sides. Packed treemap levels hang them in a tray above the tallest box, still following the aisles, and drop them into sockets in the lids.
- **Crossing levels.** A cable whose end lies deeper goes over the rim, or down from the tray, of every open box on the way.
- **Thickness is not to scale.** A cable is sized for its own level, so one from an outer level seen from deep inside would be a pipe across the screen. Each frame, the shader pulls every cable in toward its centre line so it is never drawn wider than 3 px in radius, measured at its nearest point to the camera, and the jacks shrink with it. Cables are only ever thinned, so they still fit their aisles; at a level's own framing (1–2.5 px) nothing changes.
- **Reading a cable.**
  - Beads flow from caller to callee.
  - Thickness grows with the reference count.
  - Solid means calls or uses; dashed means starts and supervises; dotted means planned; purple means hosts.
- **Interaction.**
  - Only the focused level's cables show.
  - Hovering or selecting a box lights its cables and fades the rest. An x-ray copy shows a lit cable through boxes that hide it.
  - Hovering a cable shows what it carries. Clicking a cable goes to its far end.
  - In a tour's path view and in a feature spotlight, only the cables joining the lit parts show. They run heavier and take each part's legend colour: any link whose two ends each hold a lit part, followed down to it, plus cross-crate references between lit parts.
- **Types:** `struct` (with field count), `enum` (variants), `trait` (methods). Types inside `#[cfg(test)]` modules or test files are marked as test doubles.
- **Sizes:** box area grows with the square root of lines of code; height grows with log lines.
- **Scope:** the scan reads the working tree; when it differs from `HEAD`, the label says "+ uncommitted changes".
