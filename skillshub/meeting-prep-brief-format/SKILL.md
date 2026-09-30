---
name: meeting-prep-brief-format
version: 0.1.0
description: Procedure skill — produce a one-page meeting prep brief that the user can scan in 60 seconds before walking in. Standard sections, sourcing rules, what to skip. Pull this when asked to "brief me on the X meeting" or "prep me for the call with Y".
metadata:
  magician:
    skill_type: procedure
---

# Meeting Prep Brief Format

A meeting brief is the user's one chance to look prepared without doing the prep themselves. The audience is the user, 60 seconds before they walk in. Not their assistant, not the other attendees. Brevity beats completeness.

## The 5-section structure

Every brief has these sections, in this order, on one page (or one short doc):

1. **Header** — who, when, where, why
2. **Attendees** — name, role, relationship signal
3. **Context** — the 2-3 sentences that frame the meeting
4. **Open items** — what's unresolved going in
5. **Talking points** — what the user should say or ask

Keep it tight: **<400 words total** for a 30-minute meeting; **<600 words** for an hour+ strategic meeting. If you're over, you're including detail that belongs in a follow-up doc, not a prep brief.

## Section 1 — Header

Three to five lines. Plain facts, no narrative.

```
Meeting:        1:1 with Alice Chen (Acme VP Product)
When:           Thu Mar 13, 4:00 PM IST (12 hr from now)
Where:          Zoom — link in calendar invite
Duration:       30 min
Topic line:     Q2 partnership scope discussion
Recurring?:     Yes (monthly cadence)
```

If `Recurring? Yes`, briefly note the cadence and what was discussed last time. This prevents the user from re-asking questions they covered three weeks ago.

## Section 2 — Attendees

For each non-user attendee, one line:

```
Alice Chen — Acme VP Product. Owns the integration roadmap on their side. Met twice; warm relationship; prefers data over narrative. Last interaction Feb 14 (slack).
```

The four pieces:

- **Name + role + company** (or department if internal).
- **What they actually own / decide** (not their title — their effective scope).
- **Relationship signal**: first meeting / warm / strained / new contact. Pull from `contacts` memory tier where available.
- **Last interaction**: when, where, and one-line gist.

If there are >5 attendees, group by team or seniority. Don't list each in detail.

Special-case: if this is the user's first meeting with someone, lead with that fact ("**First meeting** — context below"). The user will read accordingly.

## Section 3 — Context

Two to four sentences. The minimum the user needs to enter the room. Includes:

- **Why this meeting is happening now** (the trigger).
- **The current state of whatever-was-last-discussed**.
- **Any change since last contact** that the user might not know about.

Example:

> Quarterly partnership review. Last sync (Feb 14) ended with Alice agreeing to send the revised pricing draft by end of Feb — that hasn't arrived. Internally, you decided two weeks ago that we'd walk if they go above $X/user. They likely don't know that bottom line.

Do NOT include exhaustive history. The brief is "what do you need that you don't have," not "everything that's ever happened."

## Section 4 — Open items

A short bulleted list of things unresolved going in. Each one max 15 words. Source these from:

- Open email threads with this attendee (search inbox)
- Tasks tagged with their name (if `tasks` memory accessible)
- Calendar reminders for follow-ups

Example:

- Alice still owes revised pricing draft (overdue 11 days).
- Their legal team hasn't returned the MSA redlines (sent Feb 20).
- We need to confirm Q2 launch date by end of this meeting.
- They flagged a security question on Slack — not yet answered.

Cap at 5-6 items. If there are more, group them or move secondary ones into "deferred / FYI" at the bottom.

## Section 5 — Talking points

What the user should bring up, in priority order. Each item is what to say or ask, NOT a recap of what's open.

- Lead with: **The most important ask** the user needs to make in this meeting.
- Then: 2-3 supporting points or questions.
- End with: **Decisions needed before the call ends** — what can't slip to the next meeting.

Example:

- Ask for the pricing draft today, with a soft deadline of EOW; if not, ask why and time-box.
- Probe on their security question — likely a blocker for legal.
- Get a verbal commit on the Q2 launch date.
- **Decision needed**: confirm whether we proceed with the partnership or pause pending pricing — say so explicitly if you reach the pause threshold.

If the user asked for "talking points about X", lead with X. Otherwise lead with whatever decision the meeting is meant to produce.

## Sourcing rules

The brief is built from the user's existing data. Don't invent.

- **Calendar**: meeting time, location, duration, recurring status, attendee list. Source of truth for the header.
- **Email**: prior thread with each attendee — surface the 2-3 most recent items. Don't dump full bodies; summarize.
- **WhatsApp / iMessage / Telegram**: if the attendee uses any of these (`contacts` memory tier), pull the last interaction date and gist.
- **Tasks / projects**: open items tagged with the attendee, deadlines, blockers.
- **Memory tiers**: `contacts` (relationship notes, preferred channel, timezone), `routines` (recurring meeting patterns).
- **Web search**: ONLY for first-meeting context — public role, recent company news, LinkedIn-style background. Skip for recurring meetings; you should know the person already.

If you can't find something, say so explicitly ("No prior thread found" beats omitting).

## When to skip / shorten the brief

Some meetings don't need the full structure:

- **Recurring 1:1 with someone you talk to constantly**: skip attendees section, abbreviate context. Focus on open items + talking points only.
- **Casual catchup with no decisions needed**: 4-line brief — who, when, last interaction, optional topics. Done.
- **Very large meeting (>10 attendees)**: brief becomes "your role in this meeting" + "the one decision you need to drive" + "who matters". Skip per-attendee detail.

## What to NEVER put in a brief

- **Private/sensitive context the user already knows by heart** (don't recap their own org chart).
- **Speculation framed as fact** — say "likely" or "based on the Feb 14 thread" when you're inferring.
- **Names of attendees they've never met without flagging "new contact"**.
- **Calendar links / Zoom URLs** — those are in the calendar event itself; don't duplicate.

## Output format

Markdown with the 5 sections as `##` headings. Bold for key facts (names, dates, decisions). Code blocks for verbatim quotes from email. Keep the whole brief scannable — bullets and short paragraphs, never long prose blocks.

Filename: `brief-<attendee-or-topic>-<YYYY-MM-DD>.md`. Save to `~/briefs/` if filesystem access is available, otherwise return inline.

Surface the brief in the user's task_state output. Do NOT email the brief unless explicitly asked — it's for the user, not the attendees.
