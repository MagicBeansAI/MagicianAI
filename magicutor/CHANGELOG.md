# Magicutor Changelog

## Unreleased

- Extension 0.2.11: WhatsApp calls show the caller with Join, Listen, Ignore and Cancel on any tab; Observe can hide the bar, and Cancel hangs up even after Join or Listen.

### Fixed - a thread's dedicated window is checked before it is reused (Magicutor v0.1.92)

- Fixed: chat browser runs failed when a thread's dedicated Chrome window had been closed; a closed window is now detected and replaced.

_Shipped on another branch as v0.1.89 and renumbered on merge (2026-09-21)._

### Fixed - the automation panel and aurora end with the CDP session (Magicutor v0.1.91; extension v0.2.2)

- Fixed: the page's automation panel and border stayed after a run ended until the user pressed Stop. The proxy now tells the extension when a session ends, and the extension shows the real outcome on every tab the run touched.

### Changed - the debugger release is logged at info (Magicutor v0.1.90)

- Debugger detach on session end is logged at info.

### Changed - the extension is named "Magican" (extension v0.2.1)

- The extension is renamed "Magican".

### Fixed - the debugger is released when a CDP session ends (Magicutor v0.1.89; extension v0.2.1)

- Fixed: Chrome's "is being debugged" bar stayed after a run ended; the debugger is now detached from every tab the session attached.

---

Older entries: `docs/archive/changelogs/magicutor.md`
