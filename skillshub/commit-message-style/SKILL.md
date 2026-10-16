---
name: commit-message-style
version: 0.1.0
description: Write commit messages that are concise, present-tense, and scoped. Use when the user asks for a commit message, or when committing changes on their behalf. The output should focus on intent and reasoning rather than restating the diff.
license: MIT
---

# Commit message style

Write commit messages that read like a clear summary for someone scanning
the log a year from now.

## Rules

1. **Subject line**: imperative present-tense, ≤ 72 chars, no trailing period.
   - Good: `add retry on transient SMTP errors`
   - Bad: `Added retries.` (past tense, vague, period)

2. **Scope when relevant**: prefix with `area:` if the repo uses it
   (e.g. `auth: tighten cookie-domain matching`). If unsure, skip the prefix.

3. **Body**: only when the *why* isn't obvious from the diff. Don't restate
   the diff in prose.
   - Good context: a referenced incident, a constraint that motivated the
     fix, a deliberately rejected alternative.
   - Bad context: "this changes file X to do Y" — the diff already shows that.

4. **Wrap body at ~72 cols**. Blank line between subject and body.

5. **Don't pad with co-authors, signatures, or emoji** unless the user
   explicitly asked for them.

## When choosing between concise and detailed

- If the change is purely mechanical (rename, format, dep bump, generated
  output): subject line only.
- If the change is a bugfix: subject + 1-2 sentences naming the failure
  mode and the fix.
- If the change is a feature or refactor with a non-obvious motivation:
  subject + a few short paragraphs in the body explaining the *why*.

## Output format

When asked for a commit message, produce just the message text. No
markdown fences. No "Here's your commit message:" preamble. The user is
piping the output straight into `git commit -m "..."` or an editor.
