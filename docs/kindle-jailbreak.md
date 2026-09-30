# Jailbreaking a Kindle (and which versions this applies to)

Written 2026-08-28 from an actual bring-up of a **Kindle Paperwhite 3 (2015)**
on firmware **5.16.2.1.1**. The device-specific findings are in
`docs/plans/2026-08-28-kindle-pw3-thin-client-plan.md`;
this page is the general "how and whether" answer.

> **This is a moving target.** Jailbreaks are exploits: Amazon patches them and
> the community finds new ones. The table below was accurate on 2026-08-28.
> Before starting, check the authoritative
> [Jailbreak Wizard](https://kindlemodding.org/jailbreak-wizard.html), which
> asks for your exact model and firmware and picks the current method. Do not
> trust this page over the wizard.

## Step 0: find your firmware

**Settings → Device Options → Device Info**, or read
`system/version.txt` at the root of the USB drive:

    $ cat /Volumes/Kindle/system/version.txt
    Kindle 5.16.2.1.1 (409747 002)

The firmware version — not the model — decides the method.

## Which jailbreak for which firmware

| Firmware | Method |
| --- | --- |
| below 5.18.1 | **WinterBreak** |
| 5.16.4 – 5.18.3 | **Sanctuary** (runs in the Kindle's own browser, no PC needed) |
| 5.18.1 – 5.18.5.x | **AdBreak** |
| 5.18.6 | **Nosebleed** |
| 5.19+ | **SpringBreak** |

Ranges overlap: on 5.16.4–5.18.0 both WinterBreak and Sanctuary apply, and
Sanctuary is far less fiddly since it needs no cable or PC. Our PW3 sat at
5.16.2.1.1 — *below* Sanctuary's floor — so WinterBreak was the only option.

Current-generation hardware (11th/12th gen, Colorsoft, Scribe) has been
shipping on 5.19.x since around July 2026, which puts it in SpringBreak
territory.

## Before you start

1. **Check the firmware first.** Everything else depends on it.
2. **Stop it updating.** Delete any file at the USB root ending in `.bin` or
   named `update.bin.tmp.partial`, then fill storage leaving 50–90 MB free
   (the `Kindle-Filler-Disk` script). An update landing mid-process ruins the
   jailbreak.
   *Exception:* on an EOL device already on its final firmware there is
   nothing left to download, so this is precautionary rather than critical.
3. **The device must be registered** to an Amazon account for the methods that
   go through the Kindle Store.

## WinterBreak, concretely

What we actually ran (v2.1.0, single asset `WinterBreak.tar.gz`):

1. Airplane mode **on**, then reboot.
2. Extract the release and copy the **contents** to the Kindle's USB root.
3. **On macOS, verify the hidden `.active_content_sandbox` folder copied.**
   Finder silently skips it and the jailbreak then does nothing. Use `ditto`
   rather than drag-and-drop, and confirm afterwards. Note it must *merge*
   with the existing folder, not replace it.
4. Eject, open the Kindle Store, accept the prompt to disable Airplane mode,
   tap **WinterBreak** inside Mesquito, wait ~30s for the restart.
5. Success leaves `documents/JAILBROKEN.txt` on the device.

WinterBreak also installs the hotfix, so the separate "disable OTA" step in
older guides can be skipped.

## What a jailbreak actually gets you

Not a different operating system. Kindle OS keeps running exactly as before;
you gain the ability to run your own code beside it:

- **Scriptlets** — any `.sh` in `/mnt/us/documents` appears in the library and
  runs **as root** when tapped. This is the most useful and least appreciated
  capability, and it is the one that unblocked our entire bring-up.
- **KUAL** — a launcher for extensions.
- **USBNetwork** — SSH over USB.
- **KOReader**, alternative readers, and a full Alpine chroot if wanted.

There is **no dual boot** in the usual sense. Nothing here replaces Kindle OS,
and a reboot always returns a working Kindle.

## Traps worth knowing

- **The rootfs is mounted read-only.** Writes to `/usr`, `/usr/local` and
  `/etc/upstart` fail with `Read-only file system`, and the stock installers
  swallow the error and report success. Run `mntroot rw` first. On our device
  this one fact explained every single install failure.
- **On some devices "Update Your Kindle" is inert** — greyed out for any valid
  `.bin` at the root, including correctly signed ones. Do not assume it works.
- **`;` search-bar debug commands may not dispatch.** `;log mrpi` returned
  "no results found" on our PW3 and MRPI never ran.
- **Match soft-float vs hard-float builds.** Firmware below 5.16.3 is
  soft-float; 5.16.3+ is hard-float. Installing prebuilt extensions from the
  wrong side is a common cause of things that install cleanly then fail.
- **`/mnt/us` is a FUSE mount (`fsp`).** Repeated appends across separate opens
  are unreliable and truncate logs. Accumulate in `/tmp` and write once.

## Risk, honestly

Low, but not zero.

- The jailbreak itself is reversible and survives reboots; a factory reset
  removes it.
- The real hazard is **writing to the userstore while the device is also
  mounted over USB** — concurrent writes can corrupt the filesystem. Never
  have both at once.
- A factory reset *while jailbroken* can cause update locks needing recovery.
- Older EOL devices are the safest to experiment on: Amazon ships them nothing
  new, so a working jailbreak stays working.

## Sources

- [Jailbreak Wizard](https://kindlemodding.org/jailbreak-wizard.html) — authoritative, start here
- [KindleModding](https://kindlemodding.org/) · [WinterBreak](https://kindlemodding.org/jailbreaking/WinterBreak/) · [FAQ](https://kindlemodding.org/jailbreaking/jailbreak-faq.html)
- [MobileRead Kindle Serial Numbers](https://wiki.mobileread.com/wiki/Kindle_Serial_Numbers) — identify the model from its serial
- [Snapshots of NiLuJe's hacks](https://www.mobileread.com/forums/showthread.php?t=225030) — KUAL, MRPI, USBNetwork
