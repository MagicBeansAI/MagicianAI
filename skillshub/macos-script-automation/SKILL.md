---
name: macos-script-automation
version: 0.2.0
description: Procedure skill — drive common macOS apps and system controls (Music, Notes, Reminders, Calendar, Safari, Finder, System Events, Spotlight, Shortcuts, clipboard) via the `macos_automation` tool, which runs AppleScript / JXA on the host Mac through the desktop app's host gateway — one path that works natively AND from a container. Pull this when the user asks to play music, capture a note, set volume / brightness / Do Not Disturb, open something in Spotlight, create a Reminder / Calendar event, control Safari (URL / tabs), peek at Finder, or run a saved Shortcut. To SEND an iMessage use the dedicated `imessage_send` tool. For multi-step GUI walks on Electron / non-scriptable apps (Slack, WhatsApp, Discord, Notion, Linear, VS Code, Cursor) or canvas surfaces (Figma, Photoshop) reach for the `macos-ui-automation` skill instead — this playbook is for one-shot, scriptable-app automations where an osascript one-liner wins.
metadata:
  magician:
    skill_type: procedure
    requires:
      host_gateway: true
---

# macOS Simple Automation Playbook

You're on macOS and the user wants the Mac to do something. Your engine is the
**`macos_automation` tool** — it runs an AppleScript (default) or JXA
(JavaScript-for-Automation) script on the host Mac and returns
`{status, stdout, stderr, exit_code}`. It routes through the desktop app's host
gateway, so the SAME tool works whether the backend runs natively or in a container
— there is no `osascript` on this process and no local `shell` path for Mac
control. For non-AppleScript shell utilities (`open`, `defaults`, `shortcuts`,
`pbcopy`/`pbpaste`, `mdfind`, `pmset`) wrap the command in `do shell script
"..."` inside the script you pass to `macos_automation`.

That covers ~90% of "automate my Mac" requests. This playbook tells you when to
reach for each idiom, the canonical scripts for the apps users actually use, and
how to recover from permission errors without burning turns guessing at syntax.

If the request is GUI-driven (clicking buttons in non-scriptable apps, multi-step
UI walks, "do what a human would do in Photoshop") — stop. That's computer-use
territory; use the `macos-ui-automation` skill. To SEND a message, use
`imessage_send` (below), never hand-rolled Messages AppleScript.

## The `macos_automation` tool contract

- `source` (required): the program text. Default language is AppleScript.
- `language: javascript`: run the `source` as JXA (returns structured data via
  `JSON.stringify` — prefer this whenever you need more than one field back).
- `timeout_secs`: host-side timeout, 1–120 (default 30).
- Returns `{status: "ok"|"error", stdout, stderr, exit_code}`. `status: error`
  with a transport reason means the **desktop app isn't running** (the host
  gateway is down) — tell the user to launch it. A non-zero `exit_code` with a
  permission message in `stderr` is a TCC grant problem (see Permissions).

You pass the SCRIPT BODY only — not an `osascript -e '...'` shell line. Where the
old playbook said `osascript -e 'SCRIPT'`, send `source: "SCRIPT"`; where it said
`osascript -l JavaScript -e 'SCRIPT'`, send `source: "SCRIPT", language: javascript`.

## Decision flow — pick the idiom

Don't write AppleScript if a `do shell script` one-liner works.

```
Is it just "open this file/app/URL"?
    → source: 'do shell script "open -a \"Music\""'   (or open /path, or open https://...)

Is it a system setting (defaults plist) without a UI?
    → source: 'do shell script "defaults write com.apple.<domain> <key> <value>"'
    → some changes need `killall <Process>` afterward (Dock, Finder, SystemUIServer)

Is the user asking for keyboard input / focus / mouse-y stuff?
    → AppleScript via "System Events" — but expect Accessibility-permission friction

Is it "tell App X to do Y" (Music, Notes, Reminders, Mail, Calendar, Safari, Finder, iTerm)?
    → source: 'tell application "X" to ...'

Do you need structured data back (arrays, dicts, anything multi-field)?
    → language: javascript (JXA) and `JSON.stringify(...)` the result.
       AppleScript output is human-formatted; JSON is parseable.

Did the user save a Shortcut in the Shortcuts app?
    → source: 'do shell script "shortcuts run \"Shortcut Name\""'
       Cleanest UX — the user controls the consent surface.

Sending an iMessage / SMS?
    → use the dedicated `imessage_send` tool (NOT raw Messages AppleScript).

Need a screenshot of the screen?
    → that's the `screen-observation` skill (it relays a host capture); don't
       shell out to `screencapture` here.

Clipboard?
    → source: 'do shell script "pbpaste"'  (read) / pipe text into pbcopy via
       `do shell script "pbcopy" with input ...` — or use the clipboard verbs of
       "System Events". For most cases a `tell application` script is cleaner.
```

## Common idioms — copy-paste catalog

Each entry is the `source` you pass to `macos_automation` (AppleScript unless it
says JXA → `language: javascript`). AppleScript strings use double-quotes;
because you're passing the body as a tool argument (not through a shell), you do
NOT add an outer `osascript -e '...'` wrapper.

For replace/rename/set-value tasks in scriptable known apps, treat the verified
final state as success. If source is absent but final is present, return
`OK|ALREADY_SATISFIED|...`; reserve `ERROR|TARGET_TEXT_NOT_FOUND` for cases where
neither source nor final is in the target.

### System controls

```applescript
-- Volume 0–100
set volume output volume 50
-- Mute / unmute
set volume with output muted
set volume without output muted
-- Brightness (no public AppleScript API): key codes via System Events
tell application "System Events" to key code 145   -- brightness down
tell application "System Events" to key code 144   -- brightness up
-- Sleep the display / lock the screen
do shell script "pmset displaysleepnow"
-- Show Notification Center
tell application "System Events" to keystroke "n" using {command down, fn down}
-- Toggle Do Not Disturb (Focus) — a user Shortcut is the reliable path:
do shell script "shortcuts run \"Toggle Do Not Disturb\""
```

### Music

```applescript
-- Play / pause / next / previous
tell application "Music" to playpause
tell application "Music" to next track
tell application "Music" to previous track
-- Play a specific playlist / track
tell application "Music" to play playlist "Chill"
tell application "Music" to play (first track whose name is "Bohemian Rhapsody")
```

```javascript
// Currently playing (JXA → clean JSON; language: javascript)
const m = Application("Music");
if (m.playerState() !== "playing") JSON.stringify({playing:false});
else JSON.stringify({playing:true, title:m.currentTrack.name(), artist:m.currentTrack.artist(), album:m.currentTrack.album()})
```

### Notes

```applescript
-- Create a new note in the default account
tell application "Notes" to make new note with properties {name:"Quick capture", body:"Today: shipped the bridge skeleton."}

-- Append to an existing note (find by name)
tell application "Notes"
  set theNote to first note whose name is "Daily log"
  set body of theNote to (body of theNote) & "<br>" & "New line: " & (current date as string)
end tell
```

### Reminders

```applescript
-- Add a reminder to a named list with optional due-date
tell application "Reminders" to tell list "Inbox" to make new reminder with properties {name:"Pay electricity bill", due date:date "Friday 18:00"}
```

```javascript
// List all open reminders (JXA → JSON; language: javascript)
const r = Application("Reminders");
JSON.stringify(r.lists().flatMap(l => l.reminders().filter(x=>!x.completed()).map(x=>({list:l.name(), name:x.name(), due:x.dueDate() ? x.dueDate().toISOString() : null}))));
```

### Messages (iMessage / SMS) — use the `imessage_send` tool

Do NOT hand-roll Messages AppleScript for sending. Call the dedicated
`imessage_send` tool: `{to: "<handle>", text: "<body>", service: "iMessage"|"SMS"}`.
It runs a fixed, escaped template through the same host gateway and is safer than
arbitrary script. Reading message history is the separate `imessage` tool (needs
Full Disk Access on the host).

WARNING: never send a message without explicit user confirmation. Always show the
user the exact text and recipient first. Sending is irreversible and reaches a
real person.

### Mail

```applescript
-- Compose (does not send — leaves draft open)
tell application "Mail"
  set newMsg to make new outgoing message with properties {subject:"Quick question", content:"Hey — got a sec?", visible:true}
  tell newMsg
    make new to recipient with properties {address:"someone@example.com"}
  end tell
end tell
```

Same WARNING as Messages — never call `send newMsg` from script without explicit
confirmation.

### Calendar

```applescript
-- Create an event in a named calendar
tell application "Calendar"
  tell calendar "Work"
    make new event with properties {summary:"1:1 with Daisy", start date:date "Friday 10:00 AM", end date:date "Friday 10:30 AM"}
  end tell
end tell
```

```javascript
// List today's events (JXA; language: javascript)
const c = Application("Calendar");
const start = new Date(); start.setHours(0,0,0,0);
const end = new Date(); end.setHours(23,59,59,999);
JSON.stringify(c.calendars().flatMap(cal => cal.events.whose({startDate:{_greaterThan:start}, startDate:{_lessThan:end}})().map(e=>({calendar:cal.name(), summary:e.summary(), start:e.startDate().toISOString(), end:e.endDate().toISOString()}))));
```

### Safari

```applescript
-- Current URL of front Safari window
tell application "Safari" to URL of current tab of front window
-- Open URL in front window (new tab)
tell application "Safari" to open location "https://example.com"
```

```javascript
// List all open tabs (JXA; language: javascript)
const s = Application("Safari");
JSON.stringify(s.windows().flatMap(w => w.tabs().map(t => ({window:w.id(), title:t.name(), url:t.url()}))));
```

For Chrome / Arc / Brave use `tell application "Google Chrome"` / `"Arc"` /
`"Brave Browser"` — same shape, different app name.

### Finder

```applescript
-- Reveal a path in Finder
tell application "Finder" to reveal POSIX file "/Users/me/Documents/foo.pdf"
-- Get the current Finder selection (returns paths)
tell application "Finder" to get POSIX path of (selection as alias list)
-- Move to trash
tell application "Finder" to delete POSIX file "/path/to/file.txt"
```

### Spotlight (mdfind via do shell script)

```applescript
do shell script "mdfind -name invoice"
do shell script "open \"$(mdfind -count 0 -literal 'kMDItemDisplayName == \"invoice*\"' | head -1)\""
```

### Shortcuts (when available — best UX)

```applescript
do shell script "shortcuts run \"Morning brief\""
do shell script "shortcuts list"
```

## Output discipline

AppleScript's default output is human-y (`{name:"Notes", id:"x-coredata://..."}`)
— that is not JSON, and parsing it eats turns. **Default to JXA + `JSON.stringify`
whenever you need >1 field back** (`language: javascript`). For a single string
return value (URL, name, count), plain AppleScript is fine — trim the trailing
newline from `stdout`.

## Permissions — the #1 source of confusing failures

The first script against a new app triggers a TCC consent dialog on the host that
the user must approve. The **controller is the desktop app** (it owns the host
gateway that runs `osascript`), not Terminal/iTerm — so grants are made to the
Magican Desktop app. **If you see one of these error codes, it's a permissions
problem, not a syntax problem** (read them from `stderr`):

| Error code | What it means | Tell the user |
| --- | --- | --- |
| `-1743` | Not authorized to send Apple events to <App> | Grant Automation: System Settings → Privacy & Security → Automation → enable **Magican Desktop** for <App> |
| `-1719` | Object not found / scripting bridge denied | Same as above — also check the app is running |
| `-25211` | Accessibility permission needed | System Settings → Privacy & Security → Accessibility → enable **Magican Desktop** |
| `Operation not permitted` (reading ~/Library/Mail or ~/Library/Messages) | Full Disk Access needed | System Settings → Privacy & Security → Full Disk Access → enable **Magican Desktop** |

Three permission scopes exist and you cannot conflate them:
- **Automation** — needed for `tell application "X"`. Pairwise: controller × target.
- **Accessibility** — needed for `tell application "System Events" to keystroke / key code / click ...`.
- **Full Disk Access** — needed to read protected SQLite stores (`chat.db`, `Envelope Index`).

**Don't ask for blanket permissions.** Identify the specific scope your script
needs and ask for that one. If `status: error` is a transport failure (not a TCC
code), the desktop app itself isn't running — that's the fix, not a permission.

## Failure modes catalog

| Code | Meaning | Recovery |
| --- | --- | --- |
| `-128` | User canceled | User clicked Cancel on a `display dialog` — accept gracefully, don't retry |
| `-1712` | Apple event timed out | App busy/hung. Raise `timeout_secs`, or wrap in `with timeout of 60 seconds ... end timeout` |
| `-1728` | Can't get <object> | Object reference wrong. Verify it exists; check name/id |
| `-2700` | Script error | Syntax error. Show stderr to the user; don't blindly retry |
| `0 (silent failure)` | Script ran but did nothing | Often a `whose` clause matched nothing. Add a sanity check before the action |

Wrap risky calls in `try ... on error errMsg number errNum ... end try` when you
need to handle failure gracefully rather than propagate.

## Things this skill is NOT for

- **Web automation** — use the `browser` tool, not Safari AppleScript. Safari
  scripting is reflection-only (read tabs, open URLs); you cannot click into pages.
- **Sending messages** — use `imessage_send`, not hand-rolled Messages script.
- **Complex multi-step GUI walks** — that's `macos-ui-automation` (CUA).
- **Long-running monitoring** — a script blocks the call. For "watch this folder"
  use `fswatch` / `launchd` / `cron` instead.
- **Cross-user / privileged automation** — assume the current user. Don't use
  `do shell script "..." with administrator privileges`; surface the need for
  sudo back to the user explicitly.

## Style notes

- Always escape user-supplied strings before interpolating into AppleScript
  (`"`, `\`, `«` break the parse). JXA is much safer for strings (template
  literals / `JSON.stringify`).
- Echo the exact script you're about to run before invoking it when the operation
  is user-visible (creating a Calendar event, modifying Notes). That's your audit
  trail and the user's "are you sure" prompt.
- Prefer one-shot scripts. Don't write a multi-step AppleScript that assumes the
  app stays in a fixed state — apps switch focus / change state mid-script.
