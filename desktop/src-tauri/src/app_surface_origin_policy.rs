//! Scripted custom-surface host constraints for the desktop client
//! (plan 1.6, `custom_surfaces_v1`).
//!
//! The desktop app hosts the unified-ui bundle in its system WebView, so
//! the web instantiation carries over with two added constraints, both
//! enforced as constants here and mirrored in the web host
//! (`appScriptedSurface.ts`):
//!
//! (a) the surface iframe must never load from the Tauri app's own
//!     origin or the dev-server origins that serve the host page — those
//!     are host-page sources, never asset sources. The only admitted
//!     frame sources are kernel-issued, digest-keyed relative API paths;
//! (b) the sandbox attribute is the kernel constant `allow-scripts` and
//!     must never be widened — a cross-origin sandboxed frame cannot
//!     reach Tauri IPC, and it must stay that way.
//!
//! This module's decision functions are deliberately pure.
//! [`admits_webview_navigation`] is the wired form the desktop shell
//! consults: the Attention window's navigation policy (the one
//! desktop-managed webview rendering unified-ui routes at the shared UI
//! origin) calls it for every navigation action, so a scripted surface
//! frame can never load there no matter what the embedded page does.
//! Any future native surface window must satisfy the same rules through
//! the same function.

/// The single admitted sandbox attribute for a scripted custom-surface
/// frame (kernel constant; `allow-same-origin` is kernel-refused whenever
/// scripts are enabled).
pub const CUSTOM_SURFACE_SANDBOX: &str = "allow-scripts";

/// URL prefixes that identify the desktop host page's own origins. A
/// surface frame source that resolves to any of these is refused: the
/// frame would share the host document's authority.
pub const FORBIDDEN_SURFACE_SOURCE_PREFIXES: [&str; 7] = [
    "tauri://",
    "https://tauri.localhost",
    "http://tauri.localhost",
    "http://localhost:5173/",
    "http://127.0.0.1:5173/",
    "http://localhost:3002/",
    "http://127.0.0.1:3002/",
];

/// Admit one candidate frame source for a scripted custom surface. The
/// forbidden host origins are matched by name first — the Tauri origin and
/// the dev-server origins are host-page sources, never asset sources — and
/// then only relative, non-escaping, non-percent-encoded paths under the
/// digest-keyed custom-surface API route are admitted; every other
/// absolute URL is refused by the relative-path requirement. A
/// protocol-relative source (`//evil.example/x`) is refused outright: it
/// resolves against the host page's own scheme to a cross-origin absolute
/// URL, so it must never pass the relative-path requirement that a bare
/// `/` prefix alone would satisfy.
pub fn surface_frame_source_is_admitted(source: &str) -> bool {
    let lowered = source.to_ascii_lowercase();
    if FORBIDDEN_SURFACE_SOURCE_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return false;
    }
    if !source.starts_with('/') {
        return false;
    }
    if source.starts_with("//") {
        return false;
    }
    if source.contains("..") || source.contains('\\') || source.contains('%') {
        return false;
    }
    true
}

/// The sandbox attribute a surface frame must carry. Returns `false` for
/// every widening, including the general iframe token set the kernel
/// reserves for host-owned embeds.
pub fn surface_sandbox_is_admitted(sandbox: &str) -> bool {
    let mut tokens = sandbox.split_whitespace();
    let admitted = tokens.next() == Some(CUSTOM_SURFACE_SANDBOX) && tokens.next().is_none();
    admitted && !sandbox.contains("allow-same-origin")
}

/// True when the URL addresses a scripted custom-surface asset: a member
/// of the digest-keyed `custom-surface-v1/assets/` route. Only those
/// URLs are the origin policy's business; every other navigation is
/// admitted unchanged.
fn is_custom_surface_asset_route(url: &str) -> bool {
    url.contains("/custom-surface-v1/assets/")
}

/// Consult the scripted custom-surface origin policy for one desktop
/// webview navigation action. Wired into the Attention window's
/// `on_navigation` — the one desktop-managed webview that renders
/// unified-ui routes at the shared UI origin, and the only desktop
/// surface a scripted custom-surface frame could therefore try to load
/// in. Subframe visibility is platform-scoped: macOS (WKWebView) and
/// Linux (WebKitGTK) report iframe loads through the same policy
/// callback, so the guard sees surface frames there; Windows (WebView2)
/// raises NavigationStarting for top-level documents only, so iframe
/// loads there are NOT seen and rely on the web host's own
/// sandbox/CSP — the load-denial invariant is macOS/Linux-only until
/// wry wires FrameNavigationStarting. Later `navigate` calls pass the
/// same delegate).
///
/// The guard is inert unless a scripted custom-surface asset route is
/// actually requested: general routes open in the system browser, where
/// the web host enforces the same kernel constants. When the URL does
/// target the asset route, two things must hold:
///
/// - the candidate frame source must be admitted by
///   [`surface_frame_source_is_admitted`]: only kernel-issued relative
///   digest-keyed API paths are asset sources, so every absolute URL is
///   refused — the Tauri origin and the dev-server origins by name, any
///   other origin by the relative-path requirement. A scripted surface
///   frame therefore never loads inside a desktop-managed webview;
/// - the kernel sandbox constant must still be the single admitted token
///   set ([`surface_sandbox_is_admitted`]). A navigation action carries
///   no sandbox attribute to inspect, so the load-bearing desktop form
///   of the never-widen rule is fail-closed drift protection: if the
///   kernel constant is ever widened, this guard denies every
///   custom-surface navigation rather than admitting a relaxed posture.
pub fn admits_webview_navigation(url: &str) -> bool {
    if !is_custom_surface_asset_route(url) {
        return true;
    }
    surface_sandbox_is_admitted(CUSTOM_SURFACE_SANDBOX) && surface_frame_source_is_admitted(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kernel_issued_relative_digest_paths_are_admitted() {
        assert!(surface_frame_source_is_admitted(
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/\
             surfaces/canvas.html"
        ));
        // The kernel mints the entry address WITH its live session path segment
        // (the asset route's required credential — a served frame's opaque
        // origin carries no headers); the mirrored admission keeps
        // admitting it.
        assert!(surface_frame_source_is_admitted(
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/\
             bridge-scripted:install_1:1/blake3:abc/surfaces/canvas.html"
        ));
        assert!(surface_frame_source_is_admitted(
            "/api/magician/v2/apps/installations/install_1/surfaces/canvas"
        ));
    }

    #[test]
    fn the_surface_frame_never_loads_from_the_host_or_dev_origins() {
        for source in [
            "tauri://localhost/index.html",
            "https://tauri.localhost/surfaces/canvas.html",
            "http://tauri.localhost/surfaces/canvas.html",
            "http://localhost:5173/surfaces/canvas.html",
            "http://127.0.0.1:5173/surfaces/canvas.html",
            "http://localhost:3002/surfaces/canvas.html",
            "http://127.0.0.1:3002/surfaces/canvas.html",
            "https://evil.example/canvas.html",
            "//evil.example/canvas.html",
            "//evil.example/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "/api/../../etc/passwd",
            "/api/%2e%2e/secret",
        ] {
            assert!(!surface_frame_source_is_admitted(source), "{source}");
        }
        // The forbidden origins are matched before the relative-path
        // requirement, so an absolute forbidden origin is refused by name —
        // in any letter case.
        for source in [
            "TAURI://localhost/index.html",
            "https://TAURI.LOCALHOST/index.html",
        ] {
            assert!(!surface_frame_source_is_admitted(source), "{source}");
        }
    }

    #[test]
    fn the_sandbox_attribute_is_never_widened() {
        assert!(surface_sandbox_is_admitted("allow-scripts"));
        for widened in [
            "",
            "allow-scripts allow-same-origin",
            "allow-same-origin",
            "allow-scripts allow-popups",
            "allow-scripts allow-downloads",
            "allow-scripts allow-forms",
            "allow-scripts allow-modals",
            "allow-scripts allow-top-navigation",
        ] {
            assert!(!surface_sandbox_is_admitted(widened), "{widened}");
        }
    }

    #[test]
    fn webview_navigation_is_admitted_except_for_surface_asset_routes() {
        // Every other navigation is admitted unchanged — the guard is
        // inert unless a scripted custom-surface asset is requested.
        for url in [
            "tauri://localhost/attention",
            "http://localhost:5173/attention?attention=1",
            "http://localhost:5173/apps/install_1/canvas",
            "https://home.example/api/magician/v2/apps/directory",
        ] {
            assert!(admits_webview_navigation(url), "{url}");
        }
    }

    #[test]
    fn webview_navigation_denies_every_absolute_surface_asset_source() {
        // The Attention window serves the host page from the shared UI
        // origin, so any surface-asset URL it could navigate to is
        // absolute — refused against the forbidden origins by name and
        // against every other origin by the relative-path requirement.
        for url in [
            "tauri://localhost/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "https://tauri.localhost/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "http://localhost:5173/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "http://127.0.0.1:3002/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "https://home.example/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "https://evil.example/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
            "//evil.example/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html",
        ] {
            assert!(!admits_webview_navigation(url), "{url}");
        }
    }

    #[test]
    fn webview_navigation_keeps_admitting_the_kernel_issued_relative_form() {
        // The pure frame-source policy stays the single decision surface:
        // the exact kernel-issued relative digest path remains admitted,
        // so the navigation guard cannot grow a second, driftier rule.
        assert!(admits_webview_navigation(
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/\
             blake3:abc/surfaces/canvas.html"
        ));
    }
}
