# Design and verification

Modified for Magician: condensed instruction-only guidance adapted from
Impeccable skill-v4.8.0; see [provenance](../PROVENANCE.md).

## Before editing

Identify the actual target, user task and incumbent visual language. Inspect
components, tokens, typography, spacing, color, interaction patterns and native
platform conventions. Establish whether this is refinement or redesign. For a
new visual world choose an explicit direction grounded in the brief, not a
collection of fashionable effects. Reuse working project components and tokens.
Do not fabricate testimonials, metrics, brand assets, prices or product claims.

## Craft floor

- Typography has a deliberate scale, readable measure, clear hierarchy and
  appropriate weight/line height. Avoid arbitrary font proliferation.
- Layout uses consistent spacing, alignment, grouping and intentional density.
  Do not wrap every region in a redundant card or use decoration to hide weak
  hierarchy. Express brand through purposeful details, not generic gradients.
- Color supports semantics, readable contrast and coherent light/dark variants.
  Do not encode state only by color; test focus and disabled/error states.
- Controls have accessible names, visible focus, keyboard operation and adequate
  touch targets. Prefer semantic HTML or native controls over simulated ones.
- Motion explains state or relationships, respects reduced-motion preferences,
  avoids blocking input and does not make essential content inaccessible.
- Real content, long labels, localization, empty/loading/error/success states,
  permission denial and narrow viewports belong in the design, not as afterthoughts.
- Use relevant real imagery when appropriate and authorized; never fetch paid
  assets or add dependencies without the normal permissions.

## Verify honestly

Use the repository's documented build/test commands. Inspect rendered outcomes
at shipping viewport sizes and relevant themes; batch obvious corrections from
each inspection. Verify keyboard flow, labels, contrast, overflow, zoom, reduced
motion and changed states. For native UI use platform-appropriate verification.
Compare before/after against the brief and incumbent where refinement was asked.
Report test commands and actual observations, with unresolved issues and their
impact. Distinguish static inspection from runtime checks. If a preview, browser,
image tool or test runner is unavailable, state what was not checked; do not
claim visual success from compilation alone. No bundled detector is available.

Keep changes within scope. Findings do not authorize repairs: an audit/critique
returns evidence and prioritized recommendations unless implementation was asked
for. Never silently repair stale PRODUCT.md, DESIGN.md or unrelated source files.
