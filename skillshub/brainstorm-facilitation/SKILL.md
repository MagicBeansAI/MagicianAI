---
name: brainstorm-facilitation
version: 0.2.0
description: |
  Procedure skill for facilitating a live Thinking Map. Read the complete
  supplied graph and active lineage, use relevant memory or tools only when
  they materially improve the next move, then return a deliberately tiny,
  diverse frontier: normally zero to two continuations, or up to four
  complementary directions for an explicit break-open request.
metadata:
  magician:
    skill_type: procedure
---

# Brainstorm facilitation

> Activate this procedure for every Thinking Map frontier request. Deactivate
> it immediately after returning the frontier. It is a turn discipline, not a
> personality that should leak into unrelated conversations.

## Job

Help the user think one useful step farther without taking ownership of the
idea. The map is a living conversation, not a framework generator, checklist,
or polished report.

Before proposing anything, read:

1. the map title and requested intent;
2. every supplied node and edge;
3. the complete active lineage, in order;
4. which nodes were captured from the user versus previously suggested;
5. the requested move budget and diversity policy.

Treat the graph as current working cognition, not verified truth. A node may be
tentative, contradictory, or intentionally unresolved.

One durable chat session represents exactly one idea. Multiple idea sessions
may remain active together in `#brainstorming`; sharing that thread is for
navigation, not permission to merge their histories. Use another idea only
when it arrives through relevant scoped memory and genuinely changes the
current frontier.

## Context and grounding

Agent-scoped and shared-user memory may be present in the prompt. Use a memory
only when it changes relevance, reveals a stable preference/constraint, or
connects this idea to an earlier user project. Never expose private memory in a
card merely to prove it was consulted. Do not turn a remembered fact into a
confirmed graph fact without the user's support.

The normal frontier turn is tool-free. Use a read/research tool only when the
active thought explicitly depends on an external fact and grounding would
materially change the next branch. Never browse to decorate an idea, and never
perform writes, create tasks, send messages, or take external action from this
procedure. If research is useful but would stall the thinking rhythm, propose a
small research action as one frontier move instead.

## Select the smallest useful frontier

For `continue_thinking`:

- return zero, one, or at most two moves;
- zero is correct when the user should keep talking;
- prefer one precise question or one strong connection over two weaker cards;
- do not complete the user's thought for them.

For `break_open`:

- return two to four moves, never more than the supplied `maxMoves`;
- make them consequential and complementary, not variations of one sentence;
- prefer at most one move of each kind (`question`, `idea`, `risk`, `decision`,
  `action`) unless a repeated kind is clearly more useful than forced variety;
- cover distinct lenses such as assumption, option, contradiction, consequence,
  connection, evidence, or experiment. Do not mechanically fill categories.

Across both modes:

- never repeat or paraphrase an existing node;
- never suggest a generic brainstorm ritual (SWOT, pros/cons, “define the
  audience”) unless this graph specifically makes it the missing move;
- each title is a selectable thought, not commentary about the model;
- title: at most 90 characters; detail: one sentence, at most 180 characters;
- the `reason` explains why this is useful *now*, not why brainstorming matters;
- prefer tension, leverage, or a testable next step over breadth for its own sake.

## Output contract

Return only one compact JSON object. No markdown fence, preamble, conclusion,
or hidden fields:

```json
{"moves":[{"kind":"question|idea|risk|decision|action","title":"short card title","detail":"one useful sentence","reason":"why this advances this map now"}]}
```

The caller validates, bounds, deduplicates, and presents these as provisional
cards. Never emit a complete replacement graph.

## Final self-check

Before returning, reject any move that fails one of these tests:

1. Could this have been suggested without reading this particular graph?
2. Does it duplicate a node or another proposed move?
3. Is it weaker than simply letting the user keep talking?
4. Does it assume remembered or external information is confirmed?
5. Would selecting it meaningfully change the next minute of thinking?

Return the survivors, even when that means an empty frontier. Then call
`deactivate_skill` in the same turn.
