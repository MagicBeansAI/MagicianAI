---
name: email-etiquette
version: 0.1.0
description: Procedure skill — how to draft email replies that get read and answered. Tone matching, subject-line shape, length discipline, follow-up timing, when to thread vs new, escalation phrasing. Pull this when drafting any external email on the user's behalf.
metadata:
  magician:
    skill_type: procedure
---

# Email Etiquette

Drafting email on someone else's behalf has three jobs: say the thing, match the recipient's register, and make it easy to reply. This skill is the playbook.

## Tone matching — pick before you write

The single biggest failure mode is writing in your default voice instead of the sender's. Before drafting, identify three signals from the most recent thread or known contact preferences:

- **Register**: formal (full sentences, no contractions, "best regards") vs casual (lowercase, contractions, "thanks!") vs neutral (the default — concise, polite, no slang).
- **Length norm**: does this contact reply in two-line bullets or four-paragraph essays? Match within ±50%.
- **Salutation pattern**: "Hi Alice," vs "Hello Alice," vs "Alice," vs no salutation at all. Mirror what they used last.

If you have no prior thread, default to **neutral register, 3-6 sentences, "Hi {first name},"** — almost no one finds this wrong.

## Subject lines

A subject line is a one-line summary of what's inside, NOT a teaser. Two rules:

1. **Specific over clever**: `Pricing question — Acme Q3 plan` beats `Quick question`.
2. **Action-oriented when an action is requested**: `Please confirm Thursday demo time` beats `Demo on Thursday?`.

For replies, keep the original subject. Don't change it mid-thread — clients lose the thread when their filter rules break. Exception: if the topic genuinely changed, start a new email with a new subject and reference the prior one in the first sentence.

For cold outreach, lead with the relevant noun, not the verb: `Acme onboarding follow-up` beats `Following up on Acme onboarding`.

## Length

The shortest email that says everything is the right length. Most professional emails should be 3-6 sentences. If you find yourself writing a fourth paragraph, ask: can the rest go in a follow-up? Can it be a bulleted list? Can it move to a doc and link?

Cap your sentences at ~25 words. Long sentences read as evasive in email. Break into two.

## Structure — the four-part body

For most emails:

1. **Hook** (1 sentence): why this email exists. "Following up on the spec review we discussed Tuesday."
2. **Content** (1-3 sentences or a short bullet list): the thing itself.
3. **Ask** (1 sentence): what you want them to do, by when. "Could you send the updated draft by Friday?"
4. **Sign-off** (1 sentence): "Thanks!" or "Best," depending on register.

If you don't have an ask, say so explicitly: "No action needed — just looping you in."

## When to ask vs tell

- **Ask** when the recipient has authority over the answer or controls the timeline. "When works best for you on Thursday?"
- **Tell** when you control the answer and you're informing. "Thursday at 4pm IST works for me." Don't pseudo-ask ("Does Thursday work?") if you're going to push back on alternatives anyway — just propose.

## Follow-up timing

If you sent an email and got no response:

- **B2B / professional**: wait 3 business days before nudging. Reply to your own thread, don't start a new one. Keep the nudge to 1-2 sentences ("Bumping this — any thoughts?"). Cap at 3 follow-ups before going to a different channel or accepting the no-response as a signal.
- **Personal / casual**: 5-7 days is more humane. People aren't ignoring you; they're busy.
- **Time-sensitive**: 24 hours, and switch channel (WhatsApp/Slack/call) instead of email-nudging.

## Threading vs new email

Reply in-thread when the topic continues. Start a new email when:

- The recipient changes (don't drag old recipients into a new conversation).
- The topic changes meaningfully and the old subject would mislead a future searcher.
- More than 30 days have passed — assume the old thread is cold.

When threading, **do** trim the quoted history if it's >10 lines. Most clients show it collapsed by default but a clean reply reads better when forwarded.

## Escalation phrasing

When something is overdue or you need to push:

- **First escalation** (polite, defensive cover): "Want to make sure this didn't get lost — could you take a look when you have a chance?"
- **Second escalation** (firmer, accountable): "We need this resolved by EOD Thursday to avoid X. Can you confirm timing?"
- **Third escalation** (decisive, names stakes): "Without your input by Friday, we'll proceed with assumption Y. Let me know if that's not acceptable."

Never threaten more than once. After the third nudge, escalate to the recipient's manager or accept the answer.

## What to never do

- **Never apologize for "the long email"** — it advertises that the email is too long. If it's too long, shorten it.
- **Never write "as per my last email"** — passive-aggressive, breaks the relationship.
- **Never use exclamation points in formal email** — one is acceptable, two is excitable, three is a teenager.
- **Never send before you've reread it once** — typos and missing context are the two most-common avoidable errors.
- **Never CC someone's manager as a passive escalation** — either escalate openly with "+manager-name for visibility" or don't.
- **Never reply-all when reply suffices** — half the people on the thread don't need it.

## Multi-account routing

When the user has multiple email accounts (work, personal, side-project), pick the one that matches:

- The recipient's preferred channel from the `contacts` memory tier, if known.
- The most recent thread with this recipient — keep continuity on the same account.
- The topic — work topics on work account, personal on personal. When ambiguous, ask via task_state, don't guess.

## When NOT to use email

- **Real-time questions**: WhatsApp or iMessage gets a faster answer.
- **Document collaboration**: send a link, not an attachment.
- **Bad news**: phone call or in-person, never email-first.
- **Negotiation back-and-forth**: at most 2-3 email volleys, then switch to a call.

## Final pre-send checklist

Before declaring the draft ready and surfacing for send approval:

1. Recipient list is correct (no accidental reply-all, no missing CC).
2. Subject line stands alone — readable in an inbox preview.
3. Salutation matches the prior thread's register.
4. The ask is in the email, not buried in a paragraph.
5. Tone matches the recipient — read it aloud in their voice.
6. No typos in the recipient's name (single most-noticed mistake).
7. Attachments mentioned in the body are actually attached.
8. Signature is appropriate (formal vs personal account).
