//! Runtime Dock identity for a raw `magician-desktop.bin` launch.
//!
//! App bundles obtain their icon from `icon.icns`. The repository's local
//! Makefile stages and launches the Cargo-built executable directly, outside an
//! `.app` bundle, so AppKit has no bundle icon to discover. Assign the same
//! Magican artwork explicitly and keep debug and packaged desktop identities in
//! sync.

use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::NSData;

const DOCK_ICON_PNG: &[u8] = include_bytes!("../icons/icon.png");

/// Install the embedded Magican icon on the process-wide AppKit application.
///
/// Tauri setup runs on the macOS main thread. Failing closed here would make a
/// cosmetic problem prevent the tray from starting, so callers log this error
/// and continue with AppKit's fallback icon.
pub fn install_runtime_dock_icon() -> Result<(), String> {
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| "runtime Dock icon setup must run on the macOS main thread".to_string())?;
    let data = NSData::with_bytes(DOCK_ICON_PNG);
    let icon = NSImage::initWithData(NSImage::alloc(), &data)
        .ok_or_else(|| "embedded Dock icon PNG could not be decoded by AppKit".to_string())?;
    let application = NSApplication::sharedApplication(main_thread);
    // SAFETY: `icon` is a valid retained NSImage decoded by AppKit. The setter
    // retains it for the lifetime of the process and is called on the main
    // thread during Tauri setup.
    unsafe { application.setApplicationIconImage(Some(&icon)) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::DOCK_ICON_PNG;

    #[test]
    fn embedded_runtime_dock_icon_is_a_nonempty_png() {
        assert!(DOCK_ICON_PNG.len() > 1_024);
        assert_eq!(&DOCK_ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
    }
}
