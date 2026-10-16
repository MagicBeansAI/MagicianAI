---
name: eli5-explainer
version: 0.1.0
description: |
  Procedure skill — write a high-quality "explain like I'm 5" (ELI5)
  rendering of a concept, finding, or topic for a non-expert reader. Pull
  this whenever the user asks "ELI5 X" / "explain X simply" / "I don't
  understand this — make it simple", AND whenever your delivery to a
  non-technical user would otherwise contain jargon that's load-bearing
  for comprehension. No tool calls — pure prose-writing discipline:
  audience calibration, single load-bearing analogy, concept ladder
  (concrete → abstract), honest analogy break-points, 7-lens self-critique,
  bounded iteration with explicit stop conditions.
metadata:
  magician:
    skill_type: procedure
---

# ELI5 — explain like I'm 5

> **Lifecycle (read first):** Activate at the moment you start writing the explanation — not during research. Call `deactivate_skill` the same turn you finish. Body re-injects every turn while active and costs prompt context.

You're about to explain something to someone who doesn't have your context.

## Audience calibration (do this first, in one sentence)

"ELI5" almost never means a literal 5-year-old. Pick one in your head before you start writing:

- **Curious kid (~10 years old):** uses words a 10-year-old would already know. Zero jargon. Very concrete examples. Rare; only when explicitly asked.
- **Smart teenager (~14):** one technical term per paragraph, defined inline. Most "make it simple" requests live here.
- **Intelligent generalist adult (default):** assumes general literacy, no domain expertise. Allows analogies that reference adult life (mortgages, software, businesses). Pick this unless the user signals otherwise.

If you're not sure, default to "intelligent generalist adult."

## The shape of a high-quality ELI5

Built around **one single load-bearing analogy** that does ~80% of the work. The trap to avoid: throwing 3-4 weak analogies hoping one sticks. Pick one, commit to it, build the whole explanation around it.

Structure:

1. **Lead with the headline insight** — one sentence that names the thing in plain language. If the reader stops here, what's the one idea they should walk away with?
2. **Introduce the analogy** — one sentence. "Think of X like Y."
3. **Build the concept on top of the analogy** — 2-4 short paragraphs. Each paragraph extends the analogy one step. Concrete → abstract direction always; never the other way.
4. **Flag where the analogy breaks** — one sentence at the end: "This analogy isn't perfect — in real life, X is also Z." Earns trust; prevents reader from over-extrapolating.
5. **(Optional) The "tell me more if curious"** — one closing sentence pointing at the next layer of depth, in case they want to go deeper.

Length: 150-400 words for the default audience. Shorter if a kid. Longer only if the concept genuinely requires it — and if so, break with subheadings.

## Concept ladder discipline

Always concrete → abstract. Bad:

> "Inflation is a sustained increase in the general price level. This means…"

Good:

> "Imagine you bought a coffee for $3 last year and it's $3.30 this year. The coffee didn't change. Money got weaker. That's inflation."

Start with a tiny specific thing the reader has handled (coffee, a key, a lunchbox, a Netflix subscription), then climb to the abstraction. Don't open with the abstraction and hope the reader catches up.

## Self-critique pass — 7 lenses

Before you publish, walk through these. If any fails, rewrite that part:

1. **Headline test:** would a non-expert reading only the first sentence walk away with the right idea?
2. **Vocab test:** every word common (top-5K English). Audience exception: teenager allows one defined technical term per paragraph; kid allows none. Flag words like heuristic, paradigm, abstraction, polymorphism, decentralized and replace with plainer ones.
3. **Hedge test:** strip "obviously," "simply," "just," "of course," "essentially." They patronize without informing.
4. **Single-analogy test:** exactly ONE analogy doing the lifting. If multiple, kill the weaker ones.
5. **Concrete-first test:** does the first noun in each paragraph refer to something everyday — physical or familiar abstract (money, time, memory)?
6. **Break-point test:** did you flag where the analogy fails?
7. **Re-explainability test:** could a non-expert who just read this re-explain the core idea in one sentence without looking back?

## Iteration loop with explicit stop

Don't say "iterate until good." Stop when ALL of these are true:

- All 7 self-critique lenses pass
- Length within 150-400 words (default audience)
- Sentence-length proxy for ≤ 8th-grade reading level: no sentence > 25 words, no sentence with > 1 comma on average, no compound-complex sentences (and/but/because joining ≥ 2 clauses)
- Exactly one analogy is load-bearing; analogy break-point is flagged

If after 2 full-pass rewrites the criteria still don't all pass, the topic is too big — narrow it. Pick the single most important sub-concept and explain THAT well, then say "there's more to it — ask if you want the next layer."

## Common failure modes

- **"Just X" syndrome:** "X is just Y." Reader thinks "if it's just Y, why does it have a fancy name?" — you've patronized them. Drop the "just."
- **Domain-adjacent analogies:** explaining git via "version control" or recursion via "stack." If they understood the analogy, they'd understand the original. Reach for analogies from a totally different domain (cooking, sports, household).
- **Over-completing:** explaining every edge case. ELI5 is not the spec. Cover the core; mention edges only if they're load-bearing.
- **Burying the lead:** opening with "to understand X, you first need to know about Y." Reorder: state X plainly first, then bring in Y only if needed.
- **Stacking analogies:** "X is like Y, which is like Z, which is like A." Each layer halves comprehension. Pick one and go deep.

## When no good analogy comes to mind

If after 30 seconds of thinking you don't have a single load-bearing analogy, that's a signal — either the concept is genuinely abstract (e.g. "consciousness", "infinity") or your own understanding is shallow. **Don't force a weak one.** Instead: explain the underlying mechanism in plain language using concrete components ("here's what actually happens, step by step") — sometimes a clear walkthrough beats a stretched analogy. Acknowledge if useful: "There's no perfect everyday parallel for this — but here's how it works mechanically."

## When the user says "still don't get it"

Don't repeat with a different analogy. Ask which part was unclear — the lead, the analogy, or the bridge between them. Then rewrite ONLY that section, keeping everything that did land. Switching analogies mid-conversation forces the reader to rebuild their mental model from scratch; surgical fixes preserve the parts they've already absorbed.

## Worked example — "what is a database index?" (audience: intelligent generalist adult)

> A database index is a shortcut your computer keeps so it can find things fast.
>
> Think of it like the index at the back of a book. The book itself is hundreds of pages of text. If you wanted to find every mention of "dragons," you could read the whole book front to back — slow. Or you could flip to the index, see "dragons — pages 14, 87, 203," and jump straight there. The index is a separate, much smaller list that just maps "what you're looking for" to "where to find it."
>
> A database index works the same way. The actual data — say, a million customer records — is the book. The index is a separate list the database keeps that says "Sarah Smith → row 4,521" so when you ask "find Sarah Smith," it doesn't read all million rows. It checks the index and goes straight there.
>
> The trade-off: the index takes extra space, and every time you add a new customer, the database has to update the index too. So you only index columns you'll search often — like names or email addresses, not random notes you'll never query.
>
> This analogy isn't perfect — real database indexes use tree structures (B-trees) that let them stay fast even with millions of entries, where a paper index would get unwieldy. But the core idea is identical: a small, fast lookup table that points at the data.

---

## After you finish writing

Call `deactivate_skill` THIS turn. The playbook body is re-injected every turn while active and costs prompt context; the work is done, free the slot.

If the explanation throws an error mid-render or the user changes the request, deactivate anyway before reporting failure — don't leave the playbook hanging.
