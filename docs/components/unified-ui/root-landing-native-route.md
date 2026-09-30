# Root Landing Native Route

## Current contract

This file is the living contract for the mounted `/` page. The manifesto +
landing plan
and design
are archived provenance; their earlier beats (MovieTrack, ReelSwitcher,
PrologueReel, Act 0, montage-as-opener, Geist display face) are superseded.

Copy follows
Personal Intelligence Under Your Control:
the manifesto acknowledges local-model limits and makes privacy, capability,
speed, cost and model choice explicit per operation; privacy boundaries and
permission before broader context sharing are stated as principles, not runtime
guarantees. HowTrack's local-first station pairs local memory with model choice;
the bounds station names privacy, capability, speed and spend. TrustReveal's
middle card is "Models you choose", pointing to operation selection in Settings
(see the [model routing contract](model-routing-panel.md)) — the page makes no
blanket "everything leaving the machine is minimised and redacted" promise.

### The shape

```
LandingChrome            magican (after the hero) · Manifesto (always, no underline)
LandingField             dusk-town video behind hero + how (still until the
                         loop is playing; landscape keeps the 16:9 plate,
                         taller-than-16:9 viewports cover the sticky stage)
  HeroSplit              magican (centred, weight 700, sunset sweep shimmer)
                         Superpowers for Work, Play and a cycling verb
  HowTrack               five stations (SuperApp, local first, security,
                         bounds, crew): synthetic screenshot from the left,
                         copy from the right, settle in the middle. Station one
                         is on when the track pins; markup reads the scrub
                         store. Each screenshot is a chrome window cycling
                         inner frames (Today / Chat / Tasks …) with a slow float.
DayTrack                 a day, eleven stations, PLAIN ground
LifeTrack                the montage, bracketed by the payoff lines
TrustReveal              who it answers to, three mechanisms
ManifestoExcerpt         Personal Intelligence is your asset → /manifesto
WhatItTakes              three requirements — qualifies the ask
lp-cta                   the ask
footer                   Manifesto → /manifesto · Privacy → /privacy · Terms → /terms
```

BrandReveal, the Lottie silhouette and `for the ___ in you` are unmounted.
`/`, `/manifesto`, `/privacy`, `/terms` are public marketing paths
(`isMarketingPath`): they skip the backend theme round-trip and stay readable on
a phone rather than hitting the mobile app gate. When Magician is reachable the
footer also links Today / Chat / Tasks / Crew.

**Off-device surfaces.** `/attention` (`isOffDeviceSurface`) and the `/login` it
bounces through (`isMobileAllowedPath`) are open on a phone without being
marketing paths — they use the backend and app theme, so they must not carry the
marketing shell class. A critical-request alert is delivered to a chat channel
the owner reads on their phone and links to `/attention`, so gating it would make
the link useless. The same rule relaxes the app shell's local-only host check in
`routes/(app)/+layout.ts` for that path; every other route still refuses any host
but `localhost` / `127.0.0.1` / `tauri.localhost`.

**The mobile gate is enforced twice and both halves ask the same question:** a
CSS media query (holds before hydration, without JS) and the `{#if}` that keeps
gated content out of the DOM. The media query is keyed on `is-mobile-open`
(`isMobileOpenPath`, pathname-only, SSR-safe); keying it off the narrower
marketing class paints "install the app" over routes the script allowed.
`MobileAppGate.contract.test.ts` pins both selectors and the host list.

Display+body is Outfit (linked from `app.html`); Geist Mono is the self-hosted
`--lp-mono`; non-mono Geist is not used here.

### The long-form documents

`/manifesto`, `/privacy`, `/terms` share `DocShell.svelte` (cream palette, 40rem
column, `Magican` back link, date line, h1 with hairline rule). With the `legal`
flag, policy typography is `:global` (slotted markup compiles in the route) but
namespaced under `.legal` so it cannot reach the manifesto's `prose`/`verse`
styles; body links are `p a`/`li a`, since bare `a` would outrank `.back` and
underline the back link. Policy prose lives in route markup (no second consumer);
manifesto copy is a data module because `ManifestoExcerpt` reuses its blocks.

Manifesto copy is `prose` and `verse` blocks (short lines tight, paragraphs
spaced), grouped identically on `/manifesto` and the excerpt. The activity cycle
opens on `singing` (36 verbs + "all your side quests"). **The closer is a
coupled constant:** `lifePortrait.ts` switches on its literal for a distinct icon
and `manifesto.ts` repeats it as `MANIFESTO_TAGLINE`, so renaming it means
changing the constant, that switch case, page title and meta description, footer
tag and manifesto constant together — a partial rename silently falls through to
the default portrait, which no test covers.

### The rules that are load-bearing

- **One pinned section, not two.** Hero beats and the promise road share one
  track and one sticky stage (two adjacent sticky sections read as two places).
  `RoadTrack` takes an optional `progress` prop and, when driven, owns no scroll.
- **Phases are published, never recomputed.** `landingPhase.ts` carries
  `heroPhase`, `roadPhase`, `actIndex`, `actLocal`, `roadActive`, `overBackdrop`,
  `HEAD_SETTLED`; each exists because two components once disagreed. Read them.
- **Act boundaries are weight-derived:** `roadBounds` = 0, 6/17, 12/17, 1 (the
  final beat weighs half); thirds skip the ninth station.
- **A driven road builds no sign stations** (`roadStations(3, 3, false)`) —
  `LandingChrome` titles the acts, and reserved sign stops leave the camera
  dwelling on empty positions.
- **Scroll geometry is measured, never read** — verify with
  `getBoundingClientRect`; collapsed stages and mis-centred cards are invisible
  in source.
- **Portrait dusk-town covers, not letterboxes:** at `max-aspect-ratio: 16 / 9`
  `LandingField` uses `object-fit: cover` / `object-position: center` to fill the
  100svh sticky stage; landscape/ultrawide keep the 16:9 plate.
- **The backdrop is never a video player:** the still renders beneath the
  muted, inline, looping, unfocusable, pointer-inert loop, which stays
  transparent until `playing`; WebKit media-control pseudo-elements are
  suppressed too, so blocked iPhone autoplay never shows a play overlay.
- **HowTrack screens scale; they do not reflow.** The chrome is designed at
  30 × 25.2rem; reflowing it clips the inner UI. At `max-width: 820px` it keeps
  that size and scales by `100cqi / 30rem`, copy stacks above, `--ht-slide`
  drops from 46% to 10%. At `max-height: 560px` (phone landscape) it scales by
  `min(cqi, cqb)` and keeps screen | copy.
- **`LandingAskComposer` mounts only after `probeLandingBackend` proves Magician
  is reachable** (design §1.2); the static public site has no backend, so
  visitors get the mailto card.

### Asset loading

On load the page fetches only `landing/dusk-town-loop.mp4` (~0.7 MB) and the
`dusk-town.webp` poster. `prologue/reel-life/` (the montage) loads lazily as
`LifeTrack` approaches, and `landing/laptop.png` later in the traverse. `reel`,
`reel-diorama`, `pilot`, `prologue/src` and `contact-sheet` are unreachable but
`static/` ships wholesale, so `make build-marketing-site` strips them (as it does
`vosk`) rather than deleting them — the reels are tracked and costly to
regenerate.

### Unmounted code

Still on disk with live tests, pending one deletion pass: `LandingHero`,
`RoleCycle`, `Declaration`, `Greeting`, `MovieTrack`, `worldTrack`, `MoteField`,
`PrologueReel`, `ReelSwitcher`, `LifeDevice`'s standalone use, `PathFork`,
`PathLifecycle`, `PathDay`, `PathActSign`, `BrandReveal`, `pathFork.ts`.
`PathFork.component.test.ts` is `describe.skip` (reason inline): a standalone road
now builds nine stations, not twelve.

### Tests

`make test-ui`. Key contracts: `LandingNarrative.contract.test.ts` (mount order
HeroSplit → HowTrack → DayTrack → LifeTrack → TrustReveal → ManifestoExcerpt →
WhatItTakes → `#landing-cta`; no `BrandReveal`, `Greeting`, `LandingHero`,
`Declaration`, ProofSwitcher); `howTrack.test.ts` (copy, swipe math, mobile
contract); `coldLoad.contract.test.ts` (no non-mono Geist preload;
`GeistMonoVariable.woff2` self-hosted with `font-display: swap`);
`TrustReveal.component.test.ts` (the three mechanisms: The vault, Models you
choose, Your hand on the wheel).
