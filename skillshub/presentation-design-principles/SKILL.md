---
name: presentation-design-principles
version: 0.1.0
description: Procedure skill — how to design slide decks that hold attention and land the point. Slide count, visual hierarchy, narrative arc, when to use prose vs bullets vs charts vs images. Pull when authoring or critiquing a deck (HTML slides or static).
metadata:
  magician:
    skill_type: procedure
---

# Presentation Design Principles

A deck is a delivery vehicle for one or two ideas. Most decks fail because the author was thinking like a writer (paragraphs of detail) instead of a presenter (sequenced beats). This skill is the playbook for getting it right.

## The one-question test

Before you make a single slide, answer: **what is the ONE thing the audience should remember after the presentation ends?**

Write that thing in a single sentence. Every slide that doesn't support that sentence is a cut. If you have two such sentences, you have two presentations — pick one or split.

## Slide count

For a live talk, **target ~1 slide per minute, never more than 2 minutes per slide**. A 10-minute talk = 7-12 slides. A 30-minute pitch = 15-25 slides.

For a **leave-behind deck** (read async without a presenter), you can afford more density — 30-50 slides for a comprehensive briefing — but each slide should still be one idea.

Decks longer than 25 live-talk slides are almost always too long. Cut.

## The narrative arc

A good deck follows a story shape, not an outline shape. The classic working arc:

1. **Stakes** (1-2 slides): why this matters now. The problem in the user's voice.
2. **Tension** (2-4 slides): what's hard, what's been tried, what the gap is.
3. **Insight** (1 slide): the new idea / the missing piece.
4. **Proof** (3-6 slides): evidence, data, examples. The body.
5. **Ask** (1 slide): what you want from the audience.

This works for pitches, design reviews, post-mortems, status updates — any deck where you're trying to land a conclusion. Decks that skip the stakes ("here's what we did") read as bureaucratic and lose the room in the first 30 seconds.

## Visual hierarchy on a single slide

Every slide should have one dominant element. The eye lands somewhere; design where.

- **Title is not the headline**: titles like "Q3 Results" are filing labels. Headlines like "Q3 revenue grew 40% despite a 12% pricing cut" are headlines. Use headlines.
- **One chart, one point**: if a chart needs three sentences of explanation, it's the wrong chart. Strip axes you don't need, annotate the data point that matters, use color to highlight one thing.
- **Three-deep rule**: at most three levels of visual hierarchy on a slide. Headline → key visual or data → supporting note. Anything more becomes noise.

## When to use prose vs bullets vs charts vs images

| Content type | Use | Avoid |
|---|---|---|
| A single argument | Full-sentence headline + supporting visual | Bullets that fragment the argument |
| A comparison | Side-by-side or table | Two separate slides — proximity matters |
| Numerical trend | Line chart or bar | Tables of numbers (use only for precise lookup decks) |
| Step-by-step process | Numbered diagram or sequence | Bulleted "1. 2. 3." prose |
| Architecture / system | Diagram with labeled components | Prose description of components |
| Emotional beat (problem framing) | Single large image or quote | A bulleted "Problems we face" slide |
| Detailed reference data | Appendix slide or linked doc | Tiny-font cramming |

Bullets are not the default. They became the default because slideware made them easy. Resist the reflex.

## The bullet test

If you're using bullets, every bullet must:

1. Be parallel in grammatical structure (all start with a verb, or all start with a noun — don't mix).
2. Stand on its own without the other bullets.
3. Be no more than ~12 words.
4. Number 3-5 max. Six bullets is two slides. Two bullets is one slide with the second bullet removed.

If any bullet is two sentences, it's not a bullet, it's a paragraph in disguise. Convert to prose or split into a separate slide.

## Typography

- Use **two fonts max** (one for headlines, one for body — or one font with two weights).
- Headline body sizes: 32-48pt headline, 18-24pt body. If you need 14pt, you have too much content.
- Avoid italics for body text — hard to read on projectors.
- Left-align body text — center alignment reads as ceremonial and is harder to scan.

## Color

- **One accent color** for emphasis. Everything else neutral (grayscale, off-white background, dark gray text).
- **Charts**: gray for non-focal series, accent color for the one series that matters.
- Avoid red/green as the only differentiator — colorblind-unsafe and looks like a stoplight chart.
- Dark mode decks read fine on screens but project poorly in lit rooms. Default light.

## Numbers and data

- Round aggressively. "$1.4M" beats "$1,437,892". Precision implies false precision.
- Always include the unit. "40% growth" with no time window is meaningless.
- Show the comparison. A single number without context is unreadable. "$1.4M ARR (up from $400k last year)" beats "$1.4M ARR".
- For percentages, also show absolute numbers when scale matters. "+200% growth" might be 2 customers to 6.

## What to leave OUT

- **Agenda slide** for a <15-minute talk — wastes a slide. Useful only for 30-min+ briefings.
- **Bio slide** — unless required by format, your audience knows who you are by the time you're presenting.
- **"Thank you" final slide with no content** — make the final slide useful: the ask, your contact, the QR code, the next-step CTA. Don't waste the last slide.
- **Logos of customers / partners** — only if relevant to the argument. Otherwise it's wallpaper.

## The 10/20/30 rule (for sales decks)

Guy Kawasaki's heuristic, useful as a sanity check:

- **No more than 10 slides** for a sales pitch.
- **No longer than 20 minutes** of presentation time.
- **No smaller than 30pt font** anywhere on the slide.

Adjust for context, but treat each violation as a defense burden — why is this deck different?

## Reviewing someone else's deck

When asked to critique:

1. **Read the headlines alone** (skip the body of each slide). Do they tell the story? If headlines alone don't make the argument, the deck is broken — body content is hiding the structure.
2. **Skip every slide that doesn't advance the argument** — what would be lost? Often nothing. Cut those.
3. **Find the dominant element on each slide**. If you can't, the slide has no focus.
4. **Spot the cognitive cliffs**: slides that introduce too many new ideas at once. Split them.
5. **Check the ending**: does the deck stop, or does it land?

## Output format when producing a deck

For the `comic_strip` skill, prose markdown, or HTML slides:

- Generate as HTML if richer layout matters (one `<section>` per slide).
- Generate as markdown with a separator (`---`) between slides for plain text decks.
- Always lead each slide with the headline, even in markdown.
- Cite sources at slide bottom with smaller font / muted color — credibility without distraction.
- Number slides bottom-right — useful for audience reference during Q&A.
