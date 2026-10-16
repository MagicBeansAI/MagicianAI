# macOS media permissions (mic / camera) in the Tauri tray

Why the HUD composer's mic and realtime-call buttons need usage strings in
the process Info.plist, and how those strings reach WKWebView in dev and prod.

Without `NSMicrophoneUsageDescription` (and the camera key) in the resolved
Info.plist, macOS WKWebView silently refuses `navigator.mediaDevices`,
`MediaRecorder`, and `getUserMedia({ audio: true })`. The chat composer's
`MicCaptureButton.svelte` and `VoiceCallButton.svelte` both gate on those APIs
and show as unavailable when they are missing.

Canonical source: `desktop/src-tauri/Info.plist`.

## Dev — embed in MachO

Unbundled `cargo build` binaries have no `.app` and therefore no bundle
Info.plist. `desktop/src-tauri/build.rs` adds a macOS linker flag that embeds
the same file into the `__TEXT,__info_plist` MachO section:

```rust
#[cfg(target_os = "macos")]
{
    let info_plist = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("Info.plist");
    if info_plist.exists() {
        println!("cargo:rerun-if-changed={}", info_plist.display());
        println!(
            "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            info_plist.display()
        );
    }
}
```

macOS reads the embedded section for usage strings even when the binary is
not inside a bundle. Verify with:

```bash
otool -s __TEXT __info_plist /Volumes/build/magician/builds/debug/magician-desktop | head
```

## Prod — Tauri merges Info.plist

Tauri merges `desktop/src-tauri/Info.plist` into the packaged
`.app/Contents/Info.plist` before it signs and creates the DMG / updater
archive. `make release-desktop` and `make release-desktop-target` run
`pnpm tauri build` and list artifacts; they do not patch the bundle
afterwards.

`.github/workflows/desktop-app.yml` does the same: after `pnpm tauri build`
the macOS job is **Verify packaged macOS app**. It `plutil -extract`s
`NSMicrophoneUsageDescription` and `NSCameraUsageDescription` and, on tag
builds, `codesign --verify`s the `.app`. It never mutates the bundle.

`scripts/patch-macos-info-plist.sh` is a **legacy repair helper** for older
or hand-built `.app` trees. Production must not call it after `tauri build`:
that would edit only the unpacked `.app` after the DMG and updater archive
already exist.

## Entitlements

`desktop/src-tauri/Magician.entitlements` declares:

```xml
<key>com.apple.security.device.audio-input</key>
<true/>
<key>com.apple.security.device.camera</key>
<true/>
```

Referenced from `tauri.conf.json` → `bundle.macOS.entitlements`. Tauri's
bundle step applies these via `codesign --entitlements`. The legacy patch
script re-applies the same file only when it is used to repair a bundle.

## Verifying it works

```bash
make build-desktop-tray-debug
make stop-desktop-tray && make run-desktop-tray-debug
# Open HUD with a double-tap of Left Option. First open: macOS prompts for mic permission.
# After grant, the mic + realtime-call buttons in the composer should
# render in their normal (non-slashed) state.
```

If they still show as unavailable, open the HUD WebView's DevTools and check
for:

```
[MicCaptureButton] mic capture unavailable: { mediaDevices: …, getUserMedia: …, MediaRecorder: … }
```
