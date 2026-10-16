# magkindle — Magician thin client for Kindle Paperwhite 3

Targets a jailbroken **Kindle Paperwhite 3 (2015)**, i.MX6SL "Wario", armv7,
512 MB RAM, 1072x1448 8bpp E Ink.

This crate is **excluded from the root workspace** (see the `exclude` list in
the top-level `Cargo.toml`). It builds for a different architecture and has no
business in `make check-all`.

How to jailbreak a Kindle at all, and which firmware versions each method
applies to: [`docs/kindle-jailbreak.md`](../docs/kindle-jailbreak.md)

How the client will authenticate to magician over the network:
`docs/plans/2026-08-28-kindle-client-auth-and-transport-design.md`

Full bring-up detail for this device, including every dead end:
`docs/plans/2026-08-28-kindle-pw3-thin-client-plan.md`

## Build and run

    ./deploy.sh            # build, deploy, run "pattern"
    ./deploy.sh touch      # touch axis ranges + live events
    ./deploy.sh paint      # draw under the finger, partial refresh
    ./deploy.sh restore    # hand the screen back to the Kindle UI
    ./deploy.sh ribbon     # draw the DEV badge once
    ./deploy.sh build      # build only
    ./deploy.sh shell      # root shell on the device

Requires `ssh kindle` to work (`~/.ssh/config` entry + `~/.ssh/kindle_key`) and
the device to be in **DEV** mode.

Toolchain: `brew install zig` and `cargo install cargo-zigbuild`, target
`armv7-unknown-linux-musleabihf`. Static musl means the binary carries its own
libc, so a hard-float build runs fine on the Kindle's soft-float userland and
no `koxtoolchain` is needed.

## Dev mode

`device/magdev.sh` is a Kindle *scriptlet*: copy it to `/mnt/us/documents/` and
it appears in the library as **"Magician Dev Mode"**. One tap flips:

| | NORMAL | DEV |
| --- | --- | --- |
| USB | mass storage drive | RNDIS network gadget, SSH |
| Sleep | enabled | disabled |

Tap it **with the cable still plugged in**: it will ask you to unplug, wait for
you to do it, and then toggle automatically. One tap does the whole job.

That design is forced, not stylistic. **A scriptlet gets exactly one open.**
`KPPMainApp`, the framework process hosting scriptlets, dies on its second
launch and leaves the UI stuck on `Opening...` until a reboot — a two-line
scriptlet reproduces it, so it is a platform fault, not something a script can
avoid by behaving well. Never write a scriptlet that needs a second tap.

**In practice that means scriptlets are for one-shot actions only** — the dev
toggle, a probe you run once. Anything you would run repeatedly (rendering
demos, iterating on code) goes over SSH via `deploy.sh`. Shipping a
tap-to-render scriptlet looks convenient and wedges the UI the second time you
use it.

### Drawing owns the whole screen

The framework repaints only the regions it owns, so anything drawn outside them
survives as artifacts in the gaps. `magkindle restore` therefore clears to white
with a full GC16 *before* asking appmgrd to go home; asking appmgrd alone leaves
debris behind.

Entering DEV also starts a persistent **DEV badge** in the top-right corner, so
the device never looks like a stock Kindle while dev mode is live. Drawing
bypasses the framework, which will paint over the badge on its next repaint, so
`magkindle ribbon watch` polls the framebuffer and redraws when that happens.
Reading the framebuffer costs nothing, so an idle screen stays completely quiet.

Sleep is disabled in DEV mostly so the screen stays useful while working. It is
**not** what keeps the link alive — see below.

### The USB link goes quiet, and the fault is macOS

Run `./keepalive.sh &` for the duration of a dev session.

macOS stops delivering packets on the RNDIS/CDC-ECM interface once it idles.
The Kindle is not at fault. Instrumenting the device and staying completely
silent for 32 minutes recorded its state byte-identical throughout —

    up=1 ip=192.168.15.244 g_ether=2 dropbear=1 carrier=1 oper=up

— with no transition at all, while the Mac could not reach it. On the host,
`Opkts` climbed while `Ipkts` stayed at `0`: macOS transmits and receives
nothing back. Periodic traffic keeps its receive path awake, which is all
`keepalive.sh` does.

This was misdiagnosed for a long time as the Kindle sleeping. It is worth
stating plainly because the symptom — `Ipkts 0` with the gadget still
enumerated — looks exactly like a dead device from the host side.

Mac side, set once and it persists across re-enumeration:

    networksetup -setmanual "RNDIS/Ethernet Gadget" 192.168.15.201 255.255.255.0

## Gotchas that will bite

- **Stride is 1088, visible width is 1072.** Row arithmetic must use
  `line_length`, never `xres`, or the image shears into diagonals.
- **`mxcfb_update_data` is the 72-byte Lab126 variant**, with two extra
  `hist_*_waveform_mode` fields absent from the mainline i.MX struct.
- **`input_event` is 16 bytes.** Do not build it from `libc::timeval`: musl
  uses a 64-bit `time_t` on 32-bit targets, which would make it 24 and
  desynchronise the event stream into plausible-looking garbage.
- Both sizes are pinned with `const _: () = assert!(...)`.
- **Drawing bypasses the Kindle framework.** It will not repaint on its own, so
  every exit path must call `restore_ui()` or the device looks hung.

## Layout

    src/fb.rs      framebuffer mmap, damage tracking, EPDC refresh ioctl
    src/input.rs   touchscreen, multi-touch protocol B
    src/font.rs    TrueType (fontdue) antialiased text
    src/text.rs    5x7 bitmap font for the DEV badge
    src/main.rs    pattern / touch / paint / restore / ribbon / fonts / text
    device/        scriptlets that live on the Kindle itself
    deploy.sh      build -> verify -> scp -> run
