# 24 design workflows

Modified for Magician: condensed instruction-only adaptations of Impeccable
skill-v4.8.0. See [provenance](../PROVENANCE.md). Read
[design and verification](design.md) for every workflow. Arguments below are
natural language, not executable argv. Ask once if two workflows plausibly fit.
Each workflow ends with evidence and limitations, not an invented success claim.

## Build

### craft
Deprecated alias for ordinary new-work requests. Establish user, task, surface
mode and a visual concept; preserve product facts. Plan a coherent new surface
and implement only approved scope. Verify the actual rendered result.

### shape
Plan before code: identify users, tasks, navigation, information hierarchy,
content, states and constraints. Explain trade-offs and acceptance criteria.
Deliver a UX/UI plan, not unsolicited implementation.

### init
Capture durable product context in PRODUCT.md from existing evidence and user
answers: audience, value, tasks, platform and constraints. Mark unknowns. Ask
only material missing questions. Propose the document via governed edits; resume
the requested task without a CLI context command.

### document
Inspect existing code, tokens and actual UI; document the observed design system
in DESIGN.md. Separate observed patterns from recommendations. Do not redesign
or overwrite conflicting user-authored guidance without review.

### extract
Identify repeated tokens and components; propose reusable boundaries preserving
behavior. Migrate scoped callers only after approval, and verify variants,
interaction and visual parity. Do not create speculative abstractions.

## Evaluate

### critique
Review hierarchy, task clarity, visual direction, density, consistency and
interaction. Cite concrete surfaces and evidence; rank issues by user impact.
Explain trade-offs and recommended changes without silently implementing them.

### audit
Review accessibility, responsive behavior, performance and UI states with
available tools. Record affected location, severity, evidence and remediation.
Separate measured failures from hypotheses. On native UI check platform semantics,
text scaling and input conventions. No detector is installed or implied.

## Refine

### polish
Inspect the complete target, then correct alignment, rhythm, type, color and
interaction inconsistencies. Preserve identity and functionality. Verify all
changed states, not just the first viewport.

### bolder
Identify where weak hierarchy or timid choices obscure intent. Strengthen one
coherent direction using type, contrast, composition or meaningful assets.
Preserve readability and task completion; boldness is not indiscriminate noise.

### quieter
Reduce competing emphasis, excessive motion, saturation and decoration. Preserve
primary actions, useful hierarchy and brand identity. Confirm the target does
not become low-contrast or ambiguously interactive.

### distill
Identify the core user task; remove redundant steps, chrome and repeated content
without deleting needed information or behavior. Explain what is removed and
why; verify the simplified path and edge cases.

### harden
Cover long/localized copy, empty/loading/error/success states, offline/permission
failures, input validation, keyboard use and screen-reader semantics. Preserve
backend contracts; do not fabricate successful network or authentication states.

### onboard
Map first-run questions to the shortest useful activation path. Design empty
states, guidance, progressive disclosure, recovery and returning-user behavior.
Avoid forced tours and misleading success; verify skip/back/resume paths.

## Enhance

### animate
Use motion to explain hierarchy, continuity or feedback. Specify duration,
easing and interruption behavior, honor reduced motion, and verify smoothness
and responsiveness without obscuring essential content.

### colorize
Introduce a coherent semantic palette tied to the brief and existing tokens.
Test contrast, color-independent states and light/dark themes. Avoid coloring
every element or adding arbitrary gradients.

### typeset
Improve scale, line height, measure, weight and content hierarchy. Reuse approved
fonts and verify long text, fallbacks, zoom, localization and narrow layouts.

### layout
Fix grouping, spacing, alignment, density and responsive composition. Prioritize
the user's scan path. Check overflow and real content at relevant breakpoints;
do not hide content to make a screenshot look clean.

### delight
Add small, purposeful moments of personality to an already usable interface.
Keep them optional, accessible and appropriate to the surface's mode. Verify
that delight does not delay input or compete with primary tasks.

### overdrive
Explore ambitious visual craft when explicitly requested and appropriate to the
brief. Choose one strong concept; establish performance/accessibility budgets.
Offer a simpler fallback and validate actual usability, not only spectacle.

## Fix

### clarify
Rewrite confusing labels, instructions and errors for the actual audience.
Preserve facts and intent; state next actions and recovery. Do not invent
product capabilities, legal claims or guarantees. Check layout with new copy.

### adapt
Identify target devices, sizes and input modes. Recompose layout and hierarchy
rather than merely shrinking them. Preserve task parity, native conventions,
touch/keyboard access and text scaling; verify representative sizes and states.

### optimize
Measure a UI bottleneck before changing code. Examine rendering, assets,
loading, layout shifts and interaction latency with available tools. Make the
smallest justified change, then compare measurements and regressions. Never
claim a speedup without measurement.

## Iterate

### live
Use an available governed browser to inspect the user's target and identify the
element to vary. Do not inject upstream picking scripts. Agree scope, propose
alternatives and compare rendered candidates only when tools and approval permit.
If no preview/browser exists, give clearly labeled proposals, not a live result.

### generate
For a named element and requested count, propose meaningfully distinct variants
of the specified action while holding product facts and constraints constant.
Render/compare through existing approved tooling if available; otherwise label
variants as proposals. Let the user choose; do not overwrite production UI or
install a variant gallery/browser integration without explicit approval.
