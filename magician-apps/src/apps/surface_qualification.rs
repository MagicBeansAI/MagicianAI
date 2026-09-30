//! Phase 7 web/desktop host qualification.
//!
//! Proves the envelope a browser or desktop WebView would load: empty
//! sandbox, deny-all CSP, no general Iframe tokens, red-team tokens
//! denied, and scripted JS admitted only through the killable worker.
//! The Unified UI host is the desktop host; there is no second
//! scripted WebView.

use chrono::{TimeZone, Utc};

use super::{
    manifest::{
        build_app_package_candidate, tests::valid_skill_document, AppBundleMember, AppPackageLimits,
    },
    models::{AppDigest, AppInstallationId, AppReference, AppRevision},
    sandbox::{
        compile_custom_surface_isolation, AppCustomSurfaceMode, AppSandboxError,
        GENERAL_IFRAME_SANDBOX,
    },
    surface_runtime::{
        surface_admission_for_enabled_installation, AppCustomSurfaceRuntime,
        AppCustomSurfaceRuntimeError, AppCustomSurfaceWatchdog,
    },
    surface_worker::{package_has_javascript, package_has_wasm},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPhase7HostQualification {
    pub empty_sandbox: bool,
    pub deny_all_csp: bool,
    pub general_iframe_refused: bool,
    pub red_team_tokens_denied: bool,
    pub display_stays_no_script: bool,
    pub javascript_uses_killable_worker: bool,
    pub wasm_refused: bool,
}

impl AppPhase7HostQualification {
    pub fn passed(&self) -> bool {
        self.empty_sandbox
            && self.deny_all_csp
            && self.general_iframe_refused
            && self.red_team_tokens_denied
            && self.display_stays_no_script
            && self.javascript_uses_killable_worker
            && self.wasm_refused
    }
}

pub fn run_phase7_host_qualification() -> AppPhase7HostQualification {
    AppPhase7HostQualification {
        empty_sandbox: empty_sandbox_eval().is_ok(),
        deny_all_csp: deny_all_csp_eval().is_ok(),
        general_iframe_refused: general_iframe_eval().is_ok(),
        red_team_tokens_denied: red_team_eval().is_ok(),
        display_stays_no_script: display_no_script_eval().is_ok(),
        javascript_uses_killable_worker: worker_required_eval().is_ok(),
        wasm_refused: wasm_eval().is_ok(),
    }
}

fn time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0)
        .single()
        .unwrap()
}

fn reference(value: &str) -> AppReference {
    AppReference::parse(value).unwrap()
}

fn package_members(extra: Vec<AppBundleMember>) -> Vec<AppBundleMember> {
    let mut members = vec![
        AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes()).unwrap(),
        AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec()).unwrap(),
        AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
        AppBundleMember::regular_file(
            "vendor/skills/summarize/SKILL.md",
            b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
        )
        .unwrap(),
        AppBundleMember::regular_file(
            "vendor/skills/summarize/bin/summarize.py",
            b"print('summary')\n".to_vec(),
        )
        .unwrap(),
        AppBundleMember::regular_file(
            "surfaces/index.html",
            b"<html><body>plan</body></html>".to_vec(),
        )
        .unwrap(),
    ];
    members.extend(extra);
    members
}

fn empty_sandbox_eval() -> Result<(), String> {
    let isolation = compile_custom_surface_isolation(
        AppCustomSurfaceMode::DeclarativeNoScript,
        "",
        false,
        reference("package-revision:reading-list"),
    )
    .map_err(|error| error.to_string())?;
    if !isolation.sandbox.is_empty() {
        return Err("display sandbox must be empty".to_owned());
    }
    if isolation.allows_scripts || isolation.allows_same_origin || isolation.allows_network {
        return Err("display isolation opened a forbidden capability".to_owned());
    }
    Ok(())
}

fn deny_all_csp_eval() -> Result<(), String> {
    let isolation = compile_custom_surface_isolation(
        AppCustomSurfaceMode::DeclarativeNoScript,
        "",
        false,
        reference("package-revision:reading-list"),
    )
    .map_err(|error| error.to_string())?;
    for token in [
        "default-src 'none'",
        "connect-src 'none'",
        "script-src 'none'",
        "frame-ancestors 'none'",
    ] {
        if !isolation.csp.contains(token) {
            return Err(format!("csp missing {token}"));
        }
    }
    Ok(())
}

fn general_iframe_eval() -> Result<(), String> {
    match compile_custom_surface_isolation(
        AppCustomSurfaceMode::DeclarativeNoScript,
        GENERAL_IFRAME_SANDBOX,
        false,
        reference("package-revision:reading-list"),
    ) {
        Err(AppSandboxError::GeneralIframeTokensRefused) => Ok(()),
        other => Err(format!("general iframe tokens: {other:?}")),
    }
}

fn red_team_eval() -> Result<(), String> {
    for token in [
        "allow-popups",
        "allow-top-navigation",
        "allow-forms",
        "allow-downloads",
        "allow-modals",
        "allow-presentation",
        "allow-storage-access-by-user-activation",
        "allow-scripts",
    ] {
        if compile_custom_surface_isolation(
            AppCustomSurfaceMode::DeclarativeNoScript,
            token,
            false,
            reference("package-revision:reading-list"),
        )
        .is_ok()
        {
            return Err(format!("red-team token admitted: {token}"));
        }
    }
    Ok(())
}

fn display_no_script_eval() -> Result<(), String> {
    let package = build_app_package_candidate(
        package_members(vec![AppBundleMember::regular_file(
            "surfaces/app.js",
            b"magician.render('x');".to_vec(),
        )
        .unwrap()]),
        &AppPackageLimits::default(),
    )
    .map_err(|error| error.to_string())?;
    if !package_has_javascript(&package) {
        return Err("fixture lost its javascript member".to_owned());
    }
    let package_ref = reference("package-revision:reading-list");
    let bundle = package.bundle_digest().clone();
    let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
    let envelope = runtime
        .open_no_script_host(
            &package,
            "surfaces/index.html",
            surface_admission_for_enabled_installation(
                &package_ref,
                &bundle,
                AppInstallationId::parse("install_1").unwrap(),
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                reference("host:session-1"),
                reference("bridge:session-1"),
                reference("nonce:1"),
                time(),
            ),
            time(),
        )
        .map_err(|error| error.to_string())?;
    if !envelope.sandbox.is_empty() || envelope.sandbox == GENERAL_IFRAME_SANDBOX {
        return Err("no-script host emitted iframe tokens".to_owned());
    }
    if envelope.srcdoc.contains("surfaces/app.js") {
        return Err("display srcdoc inlined executable javascript".to_owned());
    }
    let _ = AppDigest::blake3(envelope.srcdoc.as_bytes());
    Ok(())
}

fn worker_required_eval() -> Result<(), String> {
    let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
    match runtime.require_killable_worker_for_scripts() {
        Err(AppCustomSurfaceRuntimeError::KillableWorkerRequired) => Ok(()),
        other => Err(format!("default runtime leaked a worker: {other:?}")),
    }
}

fn wasm_eval() -> Result<(), String> {
    let package = build_app_package_candidate(
        package_members(vec![AppBundleMember::regular_file(
            "surfaces/app.wasm",
            b"\0asm".to_vec(),
        )
        .unwrap()]),
        &AppPackageLimits::default(),
    )
    .map_err(|error| error.to_string())?;
    if !package_has_wasm(&package) {
        return Err("fixture lost its wasm member".to_owned());
    }
    let runtime = AppCustomSurfaceRuntime::with_killable_worker();
    let package_ref = reference("package-revision:reading-list");
    let bundle = package.bundle_digest().clone();
    match runtime.open_scripted_host(
        &package,
        "surfaces/index.html",
        surface_admission_for_enabled_installation(
            &package_ref,
            &bundle,
            AppInstallationId::parse("install_1").unwrap(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            reference("host:session-1"),
            reference("bridge:session-wasm"),
            reference("nonce:1"),
            time(),
        ),
        time(),
    ) {
        Err(AppCustomSurfaceRuntimeError::WasmRefused) => Ok(()),
        other => Err(format!("wasm was admitted: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase7_web_desktop_host_qualification_passes() {
        let report = run_phase7_host_qualification();
        assert!(report.passed(), "{report:?}");
    }
}
