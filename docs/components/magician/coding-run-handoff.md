# Coding run handoff (switching engine mid-run)

A VibeDev run is a chain of turns. Each coding engine keeps its own native
session, and a session only holds the turns that ran in it: Codex, Claude,
Grok and Antigravity never resume another engine's session
(`codex_lifecycle::resume_or_fresh` → `Fresh { CrossEngine }`), and Pi's
per-chain `--continue` session misses whatever another engine did in between.
Magician therefore keeps its own, engine-independent record of every turn and
hands the turns a session has not seen to the engine that runs next
(`execution/coding_engine/run_handoff.rs`).

## The record

`run_coding_task` writes one `HandoffTurn` per finished or failed turn to
`<scope>/coding_engine/run_handoffs/<root_task_id>.json` (a first turn is its
own root): task id, engine, coding profile and model, the engine session id,
the request, the engine's final answer (each up to 8,000 characters), the
proposal's changed files, the outcome, and — when the engine reports it (Pi) —
the session's context size. A retried task replaces its entry; at most 200
turns are kept.

## What a turn is handed

Before the engine starts, the resume decision is read off the request
(`handoff_resume_target`): an explicit resume id, Pi's most recent chain
session under `--continue`, or a fresh session.

| Resume | Turns handed over |
| --- | --- |
| The same session as the previous turn | none |
| A session that missed later turns (Pi → Claude → Pi) | the turns after that session's last turn |
| A fresh session (engine switched, or Codex/Claude/Grok/Antigravity with no prior session) | every earlier turn, plus the run's first request (up to 2,000 characters) as its goal |

The block is appended to the engine's prompt under
`## Earlier turns in this run (handed over by Magician)` and says whether the
engine starts fresh or resumes. The turn emits `coding.handoff` (turn counts,
source engines, budget, window), shown as a notice card in the cockpit spine.

## Budget against the target model

The handoff may use 25% of the target model's context window, at most 40,000
tokens (about 4 characters per token). The window comes from the coding
profile's LLM profile `context_window_tokens`, else from another router profile
on the same provider and model (`ResolvedCodingProfile::context_window_tokens`);
otherwise 128k for Pi and 200k for the engine CLIs, whose model the CLI picks.
Newest turns are rendered at length (request 3,000 / answer 4,000 characters)
while they fit 70% of the budget, then shortened (request 240 / answer 400 characters) up to 90%, then one line each;
anything older is counted as left out.

A Pi session whose last recorded context is at least 80% of the new model's
window — a switch to a smaller model — is not resumed: the turn starts a fresh
Pi session with the full handoff and emits `coding.session_too_large`.

## Continuation wording

The coordinator's follow-up prose (`vibedev_rail_follow_up_context` v1.1.0, the
cockpit block in `vibedev::run_service`) and `run_coding_task`'s continuation
note say that the same engine resumes its own session and a different engine
gets the handoff; neither claims a shared Pi session holds the parent's work.
