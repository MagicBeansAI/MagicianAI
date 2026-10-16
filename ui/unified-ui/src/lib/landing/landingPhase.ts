import { writable } from 'svelte/store';

/**
 * Where the landing section is, in its own terms.
 *
 * `BrandReveal` owns one very tall track that carries BOTH the hero beats and
 * the promise road — one sticky stage that changes what it holds, rather than
 * two adjacent sticky sections that read as two places. That merge left two
 * other components unable to see what they used to read straight off the DOM:
 *
 *   · `LandingChrome` timed its corner hand-off off `.br-track`'s progress,
 *     which now runs to the end of the road rather than the end of the hero.
 *   · It also read `.rt-track` to know which act was showing, and that element
 *     is now absolutely positioned inside the pinned stage, so its rect no
 *     longer moves with scroll at all.
 *
 * Rather than have either of them re-derive the split from geometry they do
 * not own, the one component that already computes it publishes it here.
 *
 * `hero` is 0..1 across the hero's own share and pins at 1 for the rest of the
 * track; `road` is 0 until the road begins and 0..1 across it. Both are plain
 * numbers, written once per animation frame from the same `paint()` that
 * drives everything else, so nothing can drift out of step with the picture.
 */
export const heroPhase = writable(0);
export const roadPhase = writable(0);

/** True once the road has begun — the moment the corners are needed and the
 *  persona beat is finished. Kept as its own store so a consumer does not have
 *  to know which threshold means "started". */
export const roadActive = writable(false);

/**
 * Where, inside one act's local progress, the promise heading has finished
 * flying in from centre and settled into place above the stations.
 *
 * Both components need it and neither owns it: `LandingChrome` eases the
 * heading's scale and travel up to this point, and `BrandReveal` must not
 * reveal a single station card before it. Held here because the two had it as
 * separate literals and immediately disagreed — the cards began fading in at
 * 0.15 while the heading was still mid-flight at 1.36x.
 */
export const HEAD_SETTLED = 0.26;

/** Which promise act is showing (0..2), and how far through it (0..1).
 *
 *  Published rather than recomputed because the acts are NOT equal thirds of
 *  the road: station weights decide the boundaries, and the final beat carries
 *  half the weight of the other eight, so the real edges are 0, 6/17, 12/17, 1.
 *  Two components each dividing by three is how the camera ended up sailing
 *  past the last card entirely — act three was mapped onto [0.667, 1] when the
 *  road had put it at [0.706, 1]. */
export const actIndex = writable(-1);
export const actLocal = writable(0);

/** True while the landing section — and therefore the dusk backdrop — is still
 *  on screen.
 *
 *  Two things need it and neither can infer it from progress alone: the
 *  promise heading must stop labelling acts once the road is behind us (scroll
 *  progress pins at 1 and stays there, so "Belongs to you" simply kept showing
 *  over every later section), and the fixed wordmark must stop being gold —
 *  a palette picked to carry against a dark dusk sky is nearly invisible on
 *  the light ground the rest of the page uses. */
export const overBackdrop = writable(true);
