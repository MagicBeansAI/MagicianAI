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
guarantees. HowTrack's last station ("The models never see any of it") says
codes, passwords, cards and keys stay with MagicVault on your system and that you
decide what stays and what goes out. TrustReveal's
middle card is "Models you choose", pointing to operation selection in Settings
(see the [model routing contract](model-routing-panel.md)) — the page makes no
blanket "everything leaving the machine is minimised and redacted" promise.

### The shape

```
LandingChrome            sticky star + magican, How it works (→ explain.magican.ai), Manifesto,
                         GitHub public mirror, Day/Night toggle; capability-aware access action
                         appears only once the hero has scrolled past the sticky header
HeroSplit                Bright Sidekick: magican^(More You),
                         Life assistant for work, play, and everything else,
                         one access CTA, mascot, 36 cycling examples, explore bridge (→ #landing-proof);
                         beside the CTA (outside its nav) "Watch the film · 8 min"
                         with a silent 84 KB loop of the film → #film; mobile reads
                         "Film 8 min" and keeps both CTAs on one row at 320px and up
FilmDialog               the intro film (unlisted YouTube aCqym-rcVu0) full screen
                         whenever the address is #film: youtube-nocookie player
                         created on open and removed on close, true full screen
                         where the browser allows it (not iOS Safari)
TrustReveal              who it answers to, three mechanisms (the vault, models you choose,
                         your hand on the wheel), placed right after the first viewport
DayTrack                 a day, eleven stations, gold/lilac rope led by a star
LandingField             dusk-town video behind how only (still until the
                         loop is playing; portrait covers the sticky stage)
  HowTrack               seven stations, told in the intro film's own lines
                         (wherever you are, real apps, Today, Relentless, builds,
                         your agents, yours): an outcome title, the capability
                         line and one aside. Each has a HowCard: a 16:10 scene
                         with the web app at the back and the whole phone in
                         front, a silent loop plus poster in static/landing/how/
                         rendered with Remotion from the film's screen overlays
                         (magican_launch_video, src/landing/). Only the pinned,
                         on-screen card plays; reduced motion and Save-Data get
                         the poster.
LifeTrack                the montage with people living their lives while devices work untouched,
                         bracketed by the payoff lines ("Love what you do?" and "Go. Do what you love.");
                         LifeDevice draws the laptop's screen as today's app (star, Chat ·
                         Work · Observe · Today, the rail, Today's newspaper,
                         Work pills, the chat's live task card, VibeDev)
HowItWorks               a still of the opened anatomy model (Sorbet by day,
                         Night by night) → explain.magican.ai, Take the tour
                         opens ?tour in a new tab
ManifestoExcerpt         Personal Intelligence is your asset → /manifesto
WhatItTakes              two requirements — a Mac or Linux and a little trust
lp-cta                   the ask
footer                   GitHub public mirror · How it works → explain.magican.ai · Manifesto → /manifesto ·
                         Privacy → /privacy · Terms → /terms
```

Header and footer GitHub marks link to
`https://github.com/MagicBeansAI/MagicianAI` in a new tab, matching the explain
showcase. Header access visibility observes the actual hero boundary below the
sticky navigation and updates when header height changes. On narrow screens,
the wordmark yields to the mascot while the header access action is mounted,
including its exit transition. "How it works" stays available as a labelled
information icon on phones. The access button fades and slides down over 280ms
with the shared eased arrival transition, and fades/slides out over 180ms on
return. Both transitions become instant for reduced motion.

BrandReveal, the old Lottie silhouette and `for the ___ in you` are unmounted.
The October star mascot is mounted by `BrandMascot.svelte`.
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

### October brand opening

The approved **Bright Sidekick** direction uses the
[October brand board](../../assets/brand/board.png) and its
[author philosophy](../../assets/brand/README.md): Magican is the nerdy star
sidekick; the person is the hero. The hero copy opens with **magican** raised to the mathematical power of **More You** (`<sup class="hs-pow">More You</sup>`), supported by the proposition
**Life assistant for work, play, and everything else**. Access links follow immediately. The mascot has no promotional caption;
the cycling examples communicate work, play and side quests without another
slogan. The landing footer and manifesto sign-off preserve **More You**. The repo
README opens with **Powering Magican — Life assistant for work, play, and everything else**.
**More You** explicitly uses Outfit Medium (500), upright, in the footer
and manifesto sign-off. The footer tagline does not inherit the monospace face
used for its navigation and other footer text.
Outfit is the display and body face (linked from `app.html`); Geist Mono is the
self-hosted `--lp-mono`.

At desktop widths (768px and up), the complete hero is a three-row grid sized to
`100svh - --landing-nav-height`. The copy/mascot row takes the remaining space;
the example strip and bottom bridge stay inside that viewport budget. Type,
spacing and mascot size respond to viewport height, with tighter rules below
740px tall. The example copy reserves room to keep cycling from shifting rows.
At 767px and below, the hero stacks with smaller type, tighter spacing and a
mascot bounded to 8.5–13rem and 20svh. Actions and example controls keep touch
targets of at least 44px. Phones and windows at most 540px tall use natural
content height so short screens and landscape views can scroll without clipping.

Day starts on off-white `#faf8f5`, ink `#1a1612`, gold `#ffc93c`, with slow
lilac, peach and gold color bands across the hero. `HeroBackground.svelte`
progressively enhances the CSS background using the MIT-licensed
[Shaders.com FlowingGradient](https://shaders.com/docs/components/flowinggradient)
from the pinned `shaders` npm package. The player loads only when the hero is
visible and motion is enabled. A veil behind the copy preserves readability;
Night shifts the colors to charcoal, muted lilac and warm gold.
The WebGPU canvas renders at a reduced size for this soft effect, pauses offscreen
and in hidden tabs, and releases resources on unmount. Telemetry is disabled.
Without WebGPU or after an initialization failure, the CSS bands still animate.
Reduced motion keeps a still colored background and skips the initial GPU load.
On devices with a mouse and hover support, `BrandBling.svelte` paints a crisp,
short-lived trail of little gold stars and the mascot's companion dot with
three dash marks. The leading star carries that same companion; the dot/dashes
use ink on Day and gold accents on Night. Passive listeners on the hero inject
sparks every 22px of pointer travel, capped at 48; the trail fades within 1.1s
and stops requesting frames when empty. Leaving the hero fades the leading
mark. The 2D effect works with the CSS background even when WebGPU is unavailable.
Coarse pointers, reduced motion, offscreen and hidden-tab states clear the
trail. Listeners, resize observation and animation frames are cleaned up on
unmount. Touch scrolling keeps the normal background animation. Decorative
layers never receive pointer events or change the hero's viewport sizing.
`warmPalette.ts` mirrors the scoped CSS and tests its text contrast. These are
landing appearances, not entries in the application's theme registry. The
subtle sun/moon icon button toggles only `.lp-root[data-appearance]` in local
component state;
no app theme attribute, saved preference or backend UI setting is written.
Each new page load starts on Day.
The header access action keeps its contrasting text color on hover in both
appearances; only the Manifesto link changes to the primary text color.
Activating the header's **Magican home** logo records `#landing-hero` using
SvelteKit's shallow history API, instantly jumps to the top of the page and
focuses the hero without scrolling again. This synchronous return skips the
intervening scroll animations and avoids deferred route scroll adjustments.
Modified clicks retain native link behavior.

Normal app builds and development use the same bounded `probeLandingBackend`
result for header and hero actions:
**Try Magican** links to `/chat` with a real backend; **Request setup** links to
`mailto:reach.magican@gmail.com` on the public site. The header, hero and closing
**Request setup** links share an editable email draft with the subject
**Magican setup request**, a greeting requesting help setting up Magican, and
placeholders for intended uses, devices and name. Subject and body are URL-encoded;
the body uses CRLF line breaks. The visitor sends the email themselves.
While checking, the access action links to `#landing-cta`.
The dedicated `VITE_MARKETING_BUILD=true` build prerenders the public root and
legal pages, skips that backend probe and renders working **Request setup**
links in the initial HTML. Leave this flag unset for the personal app and normal
Vite development. The root layout's `PublicSiteHead` owns public metadata; see
[public-site metadata](public-site-metadata.md) for canonical, crawler and preview
contracts. The film trigger itself owns `id="film"`, so its hash link resolves
before the dialog is mounted.
This is the hero's only call-to-action button.
The hero's **Explore Magican** bridge links to `#landing-proof`,
the beginning of HowTrack. `heroExamples.ts` holds 36 illustrative requests,
12 each for Work, Play, and Side quests. Every six seconds the hero advances
through an interleaved Work → Play → Side quests sequence, visiting each example
before wrapping. Category buttons jump to that category's first example and
restart the reading time. Previous/next and Pause/Resume controls let visitors
browse at their pace. The example count identifies the current category and
position. These examples never submit tasks; “Try asking” describes an intended
result, not a completed live run.

Automatic rotation pauses while the example strip is off-screen, the document
is hidden, or a visitor hovers with a mouse or focuses its controls by keyboard.
Touch selection does not latch the hover/focus pause. Reduced motion disables
automatic rotation and fades; category and previous/next controls still work.
The copy reserves space for longer prompts so changes do not move the following
section. A separate live region announces only deliberate navigation, keeping
automatic updates silent for screen readers. The timer, observer, and document
listener are cleaned up on unmount.

`BrandMascot` continuously loops the greeting until the first example change.
Work loops `working`, Play loops `on-it`, and Side quests loops the new
coin-collecting `side-quest`; Night selects the corresponding `-dark.json`.
Pausing the example carousel leaves the current mascot loop playing. An SVG pose
covers SSR, initial loading/failure and reduced motion. With reduced motion the
animation player and JSON are not requested. Mood/appearance changes retain the
current loop until the incoming SVG is ready, then cross-fade over 360ms with
eased opacity. Two player slots bound the handover: the outgoing player is
destroyed after the fade, and rapid selections wait for the current handover
before reusing its slot. Stale requests and pending players are cancelled; a
failed replacement leaves the current animation visible. Both sides pause when
the mascot leaves the viewport or the document becomes hidden. Reduced motion
and unmount clear pending requests, players and the handover timer. The static SVGs are
exports of frame 15 from `hi.json` / `hi-dark.json`, preserving the companion
dot and dashes. Export them from the supplied animation preview if the drawing
changes. `lottie-web` is a lazy chunk and an explicit Vite `optimizeDeps.include`
entry so discovering it cannot invalidate other dev-session imports.

### The magical rope

DayTrack's existing scroll-driven path carries the hero's gold/lilac palette
through the eleven day-in-the-life stations. Its soft wake, moving color
shimmer and two fine braided strands stay alive while a visitor reads. Seven
tiny sparks travel along the body; a small five-point gold star with a light
center leads the rope (18px across on desktop, 14px on phones). Its glow is
bounded to keep surrounding copy clear. The rope and star share the same
depth feather, so both pass behind scene objects and emerge together. The
existing orb/phone handoffs and receipt's closing loop retain their scroll
timing. A local appearance observer re-probes the ground when Day/Night changes,
without changing the app theme. The animation stops offscreen; reduced motion
omits the decorative canvases and stacks the same stations as readable content.

The walkthrough background uses the same gold, lilac and peach washes as the
hero, with lower opacity on charcoal at Night. Station glows use the bright
brand colors; colored labels use darker gold/lilac ink on Day and lighter
colors on Night. The proof's town backdrop is lightly desaturated and sits on
the same washes. The lock-screen wallpaper and overnight deck also carry the
gold/lilac/charcoal palette. Accessible text/button accents remain independent
of decorative colors.

The right-side chapter rail uses lilac unvisited markers, a gold completed path,
and a small gold star playhead with a bounded glow. Its visible markers retain
24px button targets and lilac keyboard-focus rings. Chapter buttons derive
scroll positions from the mounted DayTrack element and its scene bounds.
The rail hides on phones and in the reduced-motion document layout.

### The long-form documents

`/manifesto`, `/privacy`, `/terms` share `DocShell.svelte` (cream palette, 40rem
column, `Magican` back link, date line, h1 with hairline rule). With the `legal`
flag, policy typography is `:global` (slotted markup compiles in the route) but
namespaced under `.legal` so it cannot reach the manifesto's `prose`/`verse`
styles; body links are `p a`/`li a`, since bare `a` would outrank `.back` and
underline the back link. Policy prose lives in route markup (no second consumer);
manifesto copy is a data module because `ManifestoExcerpt` reuses its blocks.

Manifesto copy is `prose` and `verse` blocks (short lines tight, paragraphs
spaced), structured in `manifesto.ts`. The opening and timing sequence are
shared on `ManifestoExcerpt` (grounding personal responsibility and work timing
before the link to the full document), while `/manifesto` continues through life
continuity, computing friction, hybrid local/cloud model pragmatism under user
control, and the founder's purpose. The activity cycle
in unmounted legacy components
opens on `singing` (36 verbs + "all your side quests"). Its closer remains
coupled to `lifePortrait.ts` and `MANIFESTO_TAGLINE`. The manifesto sign-off,
mounted hero, page metadata and footer use the October brand copy, with
**More You** as the single tagline.

### The rules that are load-bearing

- **The hero is a normal section.** It precedes LandingField; the town is
  reserved for HowTrack. The sticky header is present from first paint.
- **Proof anchors account for navigation.** HowTrack's sticky stage and
  `scroll-margin-top` use `--landing-nav-height`; the screenshot does not sit
  beneath the header when an introduction link is followed.
- **Legacy phases remain shared.** `landingPhase.ts` still serves the unmounted
  road/reveal components. The new header no longer depends on their phases.
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
  Day/Night also scopes its filter and opacity so proof copy stays readable.
- **HowTrack cards are rendered scenes; they keep their 16:10 frame.** On
  desktop the card takes most of the row (`1.7fr` beside `0.72fr` of copy). At
  `max-width: 820px` copy stacks above and the card is full width (capped at
  `80svh`), `--ht-slide` drops from 46% to 10%. At `max-height: 560px` (phone
  landscape) card and copy share the row and the card is capped by height.
- **`LandingAskComposer` mounts only after `probeLandingBackend` proves Magician
  is reachable** (design §1.2); the static public site has no backend, so
  visitors get the mailto card.

### Asset loading

The first visible mascot is `/brand/mascot-hi-light.svg` (about 19 KB).
Motion-enabled visits lazily load the Lottie player and `/brand/animations/hi.json`
(about 13 KB); other moods load on selection. Reduced-motion visits use only the
static pose. The town loop and poster belong to the following HowTrack section;
`prologue/reel-life/` loads lazily as LifeTrack approaches and the laptop later
in the traverse. Archived `reel`, `reel-diorama`, `pilot`, `prologue/src` and
`contact-sheet` remain unreachable; `make build-marketing-site` strips them
without deleting the tracked source assets.

### Unmounted code

Still on disk with live tests, pending one deletion pass: `LandingHero`,
`RoleCycle`, `Declaration`, `Greeting`, `MovieTrack`, `worldTrack`, `MoteField`,
`PrologueReel`, `ReelSwitcher`, `LifeDevice`'s standalone use, `PathFork`,
`PathLifecycle`, `PathDay`, `PathActSign`, `BrandReveal`, `pathFork.ts`.
`PathFork.component.test.ts` is `describe.skip` (reason inline): a standalone road
now builds nine stations, not twelve.

### Tests

`make test-ui`. Key contracts: `HeroSplit.component.test.ts` (copy, access links,
full example cycle, manual controls, pause/resume, visibility, reduced motion,
and cleanup);
`BrandMascot.component.test.ts` (readiness-gated handover, asset failures, rapid
selection races, hidden-page playback, reduced motion and cleanup);
`LandingChrome.component.test.ts` (navigation and local appearance);
`warmPalette.test.ts` (Day/Night AA contrast and CSS parity);
`LandingNarrative.contract.test.ts` (mount order
HeroSplit → TrustReveal → DayTrack → HowTrack → LifeTrack → HowItWorks → ManifestoExcerpt →
WhatItTakes → `#landing-cta`; no `BrandReveal`, `Greeting`, `LandingHero`,
`Declaration`, ProofSwitcher); `howTrack.test.ts` (seven stations, swipe math, mobile
contract); `HowCard.component.test.ts` (nothing loads until a station is pinned and on screen, then its loop; poster only with reduced motion); `coldLoad.contract.test.ts` (no non-mono Geist preload;
`GeistMonoVariable.woff2` self-hosted with `font-display: swap`);
`TrustReveal.component.test.ts` (the three mechanisms: The vault, Models you
choose, Your hand on the wheel).
