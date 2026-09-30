---
name: form-filling-playbook
version: 0.1.0
description: Procedure skill — read a long authenticated form before you touch it, and decide the order you fill it in. Covers surveying the whole form first, conditional sections that appear only after an earlier answer, character and word limits, required-vs-optional, uploads, validation errors that are the server disagreeing with you, out-of-band verification codes, and the four things that mean STOP AND ASK rather than guess. Pull this whenever the task is completing an application, a CFP, a vendor-onboarding or KYC flow, a procurement portal, a grant, or any multi-page form behind a login. No tool calls — pure decision-making.
metadata:
  magician:
    skill_type: procedure
---

# Form Filling Decision Playbook

You are about to fill in a form somebody will read and act on. The tool part is
solved — `browser` already does `navigate, click, fill, type, select, press,
submit, upload`. What is not solved by a tool is the judgment: *what order, what
to type, and when to stop.*

This playbook is decision-making only. No tool calls in here. Work through it,
then run.

## The one framing that matters

**A half-filled form is worse than an unstarted one.** An unstarted application
costs you nothing. A half-filled one occupies a slot, may auto-submit on a
deadline, may email the reviewer, and — worst — is the state in which somebody
later assumes the work was done.

So the order of operations is: **survey → gather → fill → verify → hand over.**
Never fill as you read.

## 1. Survey — read the WHOLE form before typing anything

Walk every page. Do not fill a single field on the first pass. You are building
a list, and the list is what you check against later.

For each field record:

- its **label as written**, not your paraphrase;
- **required or optional** — and how the form says so (asterisk, "optional"
  suffix, or nothing at all, which usually means required);
- its **limit** — characters, words, file size, file type. Limits are the single
  most common cause of silent truncation;
- its **type** — free text, select, multi-select, date, upload, radio;
- whether it is **conditional**: does it only appear after some earlier answer?

### Conditional sections are why surveying is not optional

Many long forms reveal a whole section only after an earlier answer. "Are you
incorporated?" → yes → six more required fields. If you fill top-to-bottom you
will believe you are 80% done and be 40% done, and you will have committed to
the answer that opened the section before you knew what it cost.

**Answer the branching questions first, deliberately, and re-survey after each
one.** A branch answered to make the form shorter is a lie with a paper trail.

### Multi-page forms: find out what "Next" does

Before pressing it once, establish whether it **saves**. Three behaviours exist
and they are not distinguishable by looking:

- saves a draft server-side (safe to leave and resume);
- holds state in the browser session only (a lost tab loses everything);
- **submits the section irrevocably** (some portals do this with no warning).

If you cannot tell, treat it as the third. Draft every answer somewhere you own
BEFORE it goes in a box you cannot re-open.

## 2. Gather — every answer is grounded, or it is a gap

**Never answer from nothing.** An invented metric on a real application is
unrecoverable in a way a missed deadline is not: the deadline costs you one
cycle, the invention costs you the relationship and it is discoverable forever.

For each field, an answer comes from exactly one of three places:

1. **Retrieved evidence** — a real record you can cite. This is the normal case.
2. **A deliberately-worded set-piece** the owner has already written (the
   pitch, the one-liner, the why-now). Use it as written; do not improve it.
3. **A gap** — the evidence store cannot ground it. That goes to the owner as a
   question, and their reply becomes new evidence, so the gap is paid for once
   rather than every time.

A fourth place — your own plausible reconstruction — does not exist here.

### Sensitivity is checked on the source, not on your judgment

Some retrievable material is private. The filter belongs on the record, not on
your reading of the question: an agent deciding "this seems fine to share" is
exactly the failure the sensitivity marking exists to prevent. If a piece of
evidence is not cleared for outward use, it is a gap — the same as having no
evidence at all.

### Say the number, not around it

If the form asks for revenue and you have revenue, give it. Hedged non-answers
("growing steadily") read as evasion on a form and are usually worse than the
unflattering number. If the number is genuinely unflattering, that is an owner
decision, not a wording problem.

## 3. Fill — order, limits, and what the boxes do

**Fill in this order:**

1. branching questions (they change the form);
2. short factual fields (name, entity, dates, links) — cheap and they often
   auto-populate later ones;
3. uploads (they are slowest and most likely to fail);
4. long-form text last, and drafted outside the box first.

**On limits.** Write to the limit, then check the count yourself. Do not trust
the form's counter to be counting what you are counting — some count characters,
some words, some strip whitespace, some count the HTML. If a field truncates,
you want to have chosen where.

**On rich-text boxes.** Pasting formatted text into one usually produces markup
the reviewer sees. Type plain text.

**On selects and radios.** If none of the options is true, that is a gap, not a
"closest match". Picking the nearest wrong option is how a form ends up asserting
something nobody decided.

**On uploads.** Check the accepted types and size before generating the file, not
after. Name the file the way a human filing it would want it named — the reviewer
sees the filename.

## 4. Verify — the server is allowed to disagree with you

**A validation error is information, not an obstacle.** It means the server's
model of a valid answer differs from yours. Read what it actually says before
changing anything. Re-submitting the same value harder is the single most common
way an agent burns a rate limit and gets an account flagged.

If the same field rejects three different well-formed answers, stop. Something
about the field is not what you think it is, and the fourth attempt will not
discover it.

**Out-of-band verification.** Many flows email or message a code mid-way. That is
a normal step, not an exception: the code arrives in an inbox you can read, it is
usually short-lived, and it usually invalidates on a second request. Ask for it
**once**, then wait and read. Requesting a fresh code because the first has not
arrived yet is how you end up racing two codes and using the dead one.

**Re-survey before submitting.** Conditional sections may have appeared while you
were filling. Check your list from step 1 against the form as it now stands.

## 5. Hand over — you do not press Submit

Submission is a **submission-class act**: it is public, it is irreversible, and
it is not covered by any standing permission to work on the form. The agent
drafts the whole thing; a person presses send.

What you hand over is not "it's ready". It is:

- the completed draft, field by field;
- **every gap** still outstanding, as questions;
- anything you had to choose between, and what you chose;
- anything the form asserts that you could not ground;
- the limits you wrote to, so the owner knows what was cut.

## STOP AND ASK — the four cases

Not "flag it and continue". Stop.

1. **A field asks for something you cannot ground.** Every time. This is the
   common one and it is the one worth being boring about.
2. **A field commits the owner to something** — a legal declaration, a price, a
   date you would have to keep, a "we certify that…". Those are commitments, and
   an agent does not make them.
3. **The form asks for material marked private**, or for credentials, or for
   anything that would authenticate as somebody.
4. **You cannot tell what a control does** — an unlabelled button, a "Next" that
   might submit, a checkbox whose consequence is not stated.

## The failure modes, in one list

- Filling top-to-bottom and discovering a conditional section at 80%.
- Trusting the character counter.
- Answering a branch to make the form shorter.
- Re-submitting a rejected value unchanged.
- Requesting a second verification code before the first arrived.
- Choosing the nearest wrong option in a select.
- Pasting formatted text into a rich-text box.
- Generating an upload before reading the accepted types.
- Pressing Submit.
