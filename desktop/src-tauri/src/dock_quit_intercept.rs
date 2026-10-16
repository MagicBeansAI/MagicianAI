//! Intercept macOS dock right-click → Quit so it hides the app
//! window instead of terminating the whole process (the menu-bar
//! tray must stay running).
//!
//! Tauri 2.10's runtime sends `RunEvent::Exit` DIRECTLY for the
//! macOS terminate signal — it never fires `RunEvent::ExitRequested`
//! first, so there's no Rust-side opportunity to call
//! `api.prevent_exit()`. The intercept has to happen at the Objective-C
//! / NSApplicationDelegate level via method swizzling.
//!
//! What we do:
//!  1. Get NSApp's current delegate (Tauri/tao's).
//!  2. Replace its `-applicationShouldTerminate:` method implementation
//!     with our own. The new impl returns `NSTerminateCancel` (0),
//!     telling macOS "don't quit," and instead hides the main app
//!     window + demotes the activation policy.
//!  3. The tray menu's "Quit Magician" still works because it calls
//!     `app.exit(0)` directly, which bypasses
//!     `applicationShouldTerminate:` and goes through Tauri's exit
//!     pipeline.

#![cfg(target_os = "macos")]

use std::sync::OnceLock;

use objc2::ffi::{class_getName, class_replaceMethod};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use tauri::{AppHandle, Manager};
use tracing::{info, warn};

/// Stash the AppHandle so the C function the swizzle installs can
/// reach back into Tauri to hide the window + demote activation
/// policy. Set exactly once at app startup.
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Install the swizzle. Call once during Tauri's `setup`.
pub fn install(app: &AppHandle) {
    let _ = APP_HANDLE.set(app.clone());

    unsafe {
        let nsapp_class = class!(NSApplication);
        let nsapp: *mut AnyObject = msg_send![nsapp_class, sharedApplication];
        let delegate: *mut AnyObject = msg_send![nsapp, delegate];
        if delegate.is_null() {
            warn!("dock_quit_intercept: NSApp has no delegate yet; skipping install");
            return;
        }
        let delegate_class: &AnyClass = msg_send![delegate, class];
        let class_name: *const std::os::raw::c_char =
            class_getName(delegate_class as *const _ as *const _);
        let class_name_str = std::ffi::CStr::from_ptr(class_name)
            .to_string_lossy()
            .into_owned();
        info!(
            "dock_quit_intercept: swizzling -applicationShouldTerminate: on {}",
            class_name_str
        );

        let sel: Sel = sel!(applicationShouldTerminate:);
        // Method signature: returns NSUInteger (Q), takes self (@),
        // SEL (:), and sender NSApplication * (@).
        let types = b"Q@:@\0".as_ptr() as *const std::os::raw::c_char;
        let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize =
            should_terminate_override;
        // class_replaceMethod returns the previous IMP, or null if
        // no previous one existed (and the method was added). We
        // ignore the return; either way the new IMP is in place.
        let result = class_replaceMethod(
            delegate_class as *const _ as *mut _,
            sel,
            std::mem::transmute::<_, objc2::runtime::Imp>(imp),
            types,
        );
        if result.is_none() {
            info!("dock_quit_intercept: added new -applicationShouldTerminate: implementation");
        } else {
            info!("dock_quit_intercept: replaced existing -applicationShouldTerminate: implementation");
        }
    }
}

/// New `-applicationShouldTerminate:` implementation installed on
/// the NSApp delegate's class via `class_replaceMethod`. Always
/// returns `NSTerminateCancel` (0), preventing the OS from
/// terminating the process. Before returning, hides the Attention
/// window and demotes activation policy back to Accessory so the
/// dock icon disappears and the tray returns to menu-bar-only mode.
unsafe extern "C-unwind" fn should_terminate_override(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> usize {
    info!("dock_quit_intercept: applicationShouldTerminate: intercepted — Cancel");
    if let Some(app) = APP_HANDLE.get() {
        if let Some(window) = app.get_webview_window(crate::tray::ATTENTION_WINDOW_LABEL) {
            let _ = window.hide();
        }
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    // NSTerminateCancel = 0.
    0
}
