//! Phase 7 package-revision document and asset resolver.
//!
//! A custom surface may load only regular files under `surfaces/` from the
//! exact live package revision. Path escape, a stale digest, and a
//! disabled/quarantined/updated installation fail closed. The resolver
//! compiles isolation and mints a bridge session; it does not serve HTTP
//! or change the general Iframe host.

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::{
    lifecycle::AppInstallationStatus,
    manifest::{AppBundlePath, AppManifestError, AppValidatedBundleMember},
    models::{AppDigest, AppInstallationId, AppReference, AppRevision},
    records::AppSurfaceStatus,
    sandbox::{
        compile_custom_surface_isolation, mint_bridge_session, AppBridgeSession,
        AppCustomSurfaceIsolation, AppCustomSurfaceMode, AppSandboxError,
    },
};

const SURFACE_PREFIX: &str = "surfaces/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSurfaceAssetKind {
    Document,
    Asset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSurfaceAssetMember {
    path: AppBundlePath,
    content_digest: AppDigest,
    bytes: Vec<u8>,
}

impl AppSurfaceAssetMember {
    pub fn from_verified_bytes(
        path: impl AsRef<str>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, AppSurfaceAssetError> {
        let path = AppBundlePath::parse(path.as_ref())?;
        ensure_surface_path(&path)?;
        let bytes = bytes.into();
        Ok(Self {
            content_digest: AppDigest::blake3(&bytes),
            path,
            bytes,
        })
    }

    pub fn path(&self) -> &AppBundlePath {
        &self.path
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone)]
pub struct AppSurfaceAssetAdmission<'a> {
    pub package_revision_ref: &'a AppReference,
    pub live_package_revision_ref: &'a AppReference,
    pub bundle_digest: &'a AppDigest,
    pub live_bundle_digest: &'a AppDigest,
    pub installation_id: AppInstallationId,
    pub installation_status: AppInstallationStatus,
    pub surface_status: AppSurfaceStatus,
    pub surface_revision: AppRevision,
    pub grant_revision: AppRevision,
    pub host_session_ref: AppReference,
    pub session_ref: AppReference,
    pub nonce: AppReference,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AppResolvedSurfaceAsset {
    pub path: AppBundlePath,
    pub kind: AppSurfaceAssetKind,
    pub content_digest: AppDigest,
    pub bytes: Vec<u8>,
    pub isolation: AppCustomSurfaceIsolation,
    pub session: AppBridgeSession,
}

impl AppResolvedSurfaceAsset {
    pub fn media_type(&self) -> &'static str {
        media_type_for_surface_path(self.path.as_str())
    }
}

pub fn media_type_for_surface_path(path: &str) -> &'static str {
    // Extension matching is case-insensitive (a `canvas.JS` member is
    // executable to the kernel and to the review cap, so it must also be
    // served as JavaScript, never as opaque octet-stream bytes).
    let extension = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "css" => "text/css; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppSurfaceAssetError {
    #[error(transparent)]
    Manifest(#[from] AppManifestError),
    #[error(transparent)]
    Isolation(#[from] AppSandboxError),
    #[error("custom-surface path is outside the surfaces/ tree")]
    NotASurfacePath,
    #[error("custom-surface path is not in the verified package revision")]
    MissingMember,
    #[error("custom-surface bytes do not match their content digest")]
    TamperedMember,
    #[error("requested package revision is not the live revision")]
    StalePackageRevision,
    #[error("installation is not enabled for custom-surface hosting")]
    InstallationNotEnabled,
    #[error("surface binding is not active")]
    SurfaceNotActive,
    #[error("no-script custom surfaces cannot load executable members")]
    ExecutableRefused,
    #[error("custom-surface script-capable documents must be declared entry points")]
    ScriptCapableDocumentRefused,
}

pub fn resolve_surface_asset_from_candidate(
    members: &[AppValidatedBundleMember],
    requested_path: &str,
    admission: AppSurfaceAssetAdmission<'_>,
    mode: AppCustomSurfaceMode,
    killable_boundary: bool,
) -> Result<AppResolvedSurfaceAsset, AppSurfaceAssetError> {
    let requested = AppBundlePath::parse(requested_path)?;
    let member = members
        .iter()
        .find(|member| member.path() == &requested)
        .ok_or(AppSurfaceAssetError::MissingMember)?;
    resolve_verified_member(
        member.path(),
        member.content_digest(),
        member.bytes(),
        &admission,
        mode,
        killable_boundary,
    )
}

pub fn resolve_surface_asset(
    members: &[AppSurfaceAssetMember],
    requested_path: &str,
    admission: AppSurfaceAssetAdmission<'_>,
    mode: AppCustomSurfaceMode,
    killable_boundary: bool,
) -> Result<AppResolvedSurfaceAsset, AppSurfaceAssetError> {
    let requested = AppBundlePath::parse(requested_path)?;
    let member = members
        .iter()
        .find(|member| member.path() == &requested)
        .ok_or(AppSurfaceAssetError::MissingMember)?;
    resolve_verified_member(
        member.path(),
        member.content_digest(),
        member.bytes(),
        &admission,
        mode,
        killable_boundary,
    )
}

fn resolve_verified_member(
    path: &AppBundlePath,
    declared_digest: &AppDigest,
    bytes: &[u8],
    admission: &AppSurfaceAssetAdmission<'_>,
    mode: AppCustomSurfaceMode,
    killable_boundary: bool,
) -> Result<AppResolvedSurfaceAsset, AppSurfaceAssetError> {
    ensure_surface_path(path)?;
    if admission.package_revision_ref != admission.live_package_revision_ref
        || admission.bundle_digest != admission.live_bundle_digest
    {
        return Err(AppSurfaceAssetError::StalePackageRevision);
    }
    if admission.installation_status != AppInstallationStatus::Enabled {
        return Err(AppSurfaceAssetError::InstallationNotEnabled);
    }
    if admission.surface_status != AppSurfaceStatus::Active {
        return Err(AppSurfaceAssetError::SurfaceNotActive);
    }
    let observed = AppDigest::blake3(bytes);
    if observed != *declared_digest {
        return Err(AppSurfaceAssetError::TamperedMember);
    }
    if surface_path_is_wasm(path.as_str())
        || (mode == AppCustomSurfaceMode::DeclarativeNoScript
            && surface_path_is_javascript(path.as_str()))
    {
        return Err(AppSurfaceAssetError::ExecutableRefused);
    }
    // Script-capable documents a browser will EXECUTE when navigated to
    // directly (SVG is the canonical case: served as `image/svg+xml` its
    // scripts run) may never be served in scripted mode unless they are a
    // declared entry document. Entry documents are always `.html` (the
    // manifest kernel refuses anything else), so any non-HTML
    // script-capable member is by construction NOT a declared entry
    // document and fails closed here; the HTML half of the rule is
    // enforced by the scripted resolver, which knows the declaration.
    if mode == AppCustomSurfaceMode::ScriptedRequiresKillableBoundary
        && surface_path_is_script_capable_document(path.as_str())
        && !path.as_str().to_ascii_lowercase().ends_with(".html")
    {
        return Err(AppSurfaceAssetError::ScriptCapableDocumentRefused);
    }
    let isolation = compile_custom_surface_isolation(
        mode,
        "",
        killable_boundary,
        admission.live_package_revision_ref.clone(),
    )?;
    let session = mint_bridge_session(
        admission.session_ref.clone(),
        admission.installation_id.clone(),
        admission.live_package_revision_ref.clone(),
        admission.surface_revision,
        admission.grant_revision,
        admission.host_session_ref.clone(),
        admission.nonce.clone(),
        admission.now,
    );
    Ok(AppResolvedSurfaceAsset {
        path: path.clone(),
        kind: surface_asset_kind(path),
        content_digest: observed,
        bytes: bytes.to_vec(),
        isolation,
        session,
    })
}

fn ensure_surface_path(path: &AppBundlePath) -> Result<(), AppSurfaceAssetError> {
    if path.as_str().starts_with(SURFACE_PREFIX) && path.as_str() != SURFACE_PREFIX {
        Ok(())
    } else {
        Err(AppSurfaceAssetError::NotASurfacePath)
    }
}

fn surface_asset_kind(path: &AppBundlePath) -> AppSurfaceAssetKind {
    let name = path.as_str().rsplit('/').next().unwrap_or("");
    if name.eq_ignore_ascii_case("index.html") || name.ends_with(".html") {
        AppSurfaceAssetKind::Document
    } else {
        AppSurfaceAssetKind::Asset
    }
}

pub fn surface_path_is_javascript(path: &str) -> bool {
    let name = path.to_ascii_lowercase();
    name.ends_with(".js") || name.ends_with(".mjs")
}

pub fn surface_path_is_wasm(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".wasm")
}

/// Script-capable document members: extensions a browser executes as a
/// DOCUMENT when navigated to directly. An `<img>`-loaded SVG never runs
/// scripts; a navigated-to SVG (or HTML/XHTML page) does — so these are
/// the members the scripted resolver must treat as documents, not assets.
pub fn surface_path_is_script_capable_document(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.ends_with(".svg")
        || lowered.ends_with(".html")
        || lowered.ends_with(".htm")
        || lowered.ends_with(".xhtml")
        || lowered.ends_with(".xht")
}

pub fn is_executable_surface_path(path: &AppBundlePath) -> bool {
    surface_path_is_javascript(path.as_str()) || surface_path_is_wasm(path.as_str())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 19, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn admission<'a>(
        package: &'a AppReference,
        live: &'a AppReference,
        bundle: &'a AppDigest,
        live_bundle: &'a AppDigest,
        status: AppInstallationStatus,
        surface: AppSurfaceStatus,
    ) -> AppSurfaceAssetAdmission<'a> {
        AppSurfaceAssetAdmission {
            package_revision_ref: package,
            live_package_revision_ref: live,
            bundle_digest: bundle,
            live_bundle_digest: live_bundle,
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_status: status,
            surface_status: surface,
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            host_session_ref: reference("host:session-1"),
            session_ref: reference("bridge:session-1"),
            nonce: reference("nonce:1"),
            now: time(1),
        }
    }

    fn package() -> AppReference {
        reference("package-revision:reading-list")
    }

    #[test]
    fn exact_live_revision_serves_a_surfaces_document() {
        let package = package();
        let bundle = digest("bundle");
        let members = [AppSurfaceAssetMember::from_verified_bytes(
            "surfaces/index.html",
            b"<html><body>ok</body></html>".to_vec(),
        )
        .unwrap()];
        let resolved = resolve_surface_asset(
            &members,
            "surfaces/index.html",
            admission(
                &package,
                &package,
                &bundle,
                &bundle,
                AppInstallationStatus::Enabled,
                AppSurfaceStatus::Active,
            ),
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        )
        .expect("document");
        assert_eq!(resolved.kind, AppSurfaceAssetKind::Document);
        assert_eq!(resolved.bytes, b"<html><body>ok</body></html>");
        assert!(!resolved.isolation.allows_scripts);
        assert_eq!(resolved.session.package_revision_ref, package);
    }

    #[test]
    fn path_escape_and_non_surface_members_fail_closed() {
        assert!(AppSurfaceAssetMember::from_verified_bytes("../secret.html", b"no").is_err());
        assert!(matches!(
            AppSurfaceAssetMember::from_verified_bytes("SKILL.md", b"no"),
            Err(AppSurfaceAssetError::NotASurfacePath)
        ));
        let package = package();
        let bundle = digest("bundle");
        let members =
            [
                AppSurfaceAssetMember::from_verified_bytes("surfaces/index.html", b"<html></html>")
                    .unwrap(),
            ];
        let error = resolve_surface_asset(
            &members,
            "surfaces/../SKILL.md",
            admission(
                &package,
                &package,
                &bundle,
                &bundle,
                AppInstallationStatus::Enabled,
                AppSurfaceStatus::Active,
            ),
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        )
        .expect_err("escape");
        assert!(matches!(
            error,
            AppSurfaceAssetError::Manifest(_) | AppSurfaceAssetError::NotASurfacePath
        ));
    }

    #[test]
    fn missing_and_tampered_members_fail_closed() {
        let package = package();
        let bundle = digest("bundle");
        let mut members =
            [
                AppSurfaceAssetMember::from_verified_bytes("surfaces/index.html", b"<html></html>")
                    .unwrap(),
            ];
        let missing = resolve_surface_asset(
            &members,
            "surfaces/missing.html",
            admission(
                &package,
                &package,
                &bundle,
                &bundle,
                AppInstallationStatus::Enabled,
                AppSurfaceStatus::Active,
            ),
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        )
        .expect_err("missing");
        assert_eq!(missing, AppSurfaceAssetError::MissingMember);

        members[0].bytes = b"<html>tampered</html>".to_vec();
        let tampered = resolve_surface_asset(
            &members,
            "surfaces/index.html",
            admission(
                &package,
                &package,
                &bundle,
                &bundle,
                AppInstallationStatus::Enabled,
                AppSurfaceStatus::Active,
            ),
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        )
        .expect_err("tamper");
        assert_eq!(tampered, AppSurfaceAssetError::TamperedMember);
    }

    #[test]
    fn stale_revision_and_non_enabled_installations_fail_closed() {
        let package = package();
        let other = reference("package-revision:other");
        let bundle = digest("bundle");
        let other_bundle = digest("other-bundle");
        let members =
            [
                AppSurfaceAssetMember::from_verified_bytes("surfaces/index.html", b"<html></html>")
                    .unwrap(),
            ];
        assert_eq!(
            resolve_surface_asset(
                &members,
                "surfaces/index.html",
                admission(
                    &package,
                    &other,
                    &bundle,
                    &bundle,
                    AppInstallationStatus::Enabled,
                    AppSurfaceStatus::Active,
                ),
                AppCustomSurfaceMode::DeclarativeNoScript,
                false,
            )
            .expect_err("stale ref"),
            AppSurfaceAssetError::StalePackageRevision
        );
        assert_eq!(
            resolve_surface_asset(
                &members,
                "surfaces/index.html",
                admission(
                    &package,
                    &package,
                    &bundle,
                    &other_bundle,
                    AppInstallationStatus::Enabled,
                    AppSurfaceStatus::Active,
                ),
                AppCustomSurfaceMode::DeclarativeNoScript,
                false,
            )
            .expect_err("stale digest"),
            AppSurfaceAssetError::StalePackageRevision
        );
        for status in [
            AppInstallationStatus::Disabled,
            AppInstallationStatus::Quarantined,
            AppInstallationStatus::UpdatePending,
            AppInstallationStatus::UninstalledRetained,
            AppInstallationStatus::ReadyForReview,
        ] {
            assert_eq!(
                resolve_surface_asset(
                    &members,
                    "surfaces/index.html",
                    admission(
                        &package,
                        &package,
                        &bundle,
                        &bundle,
                        status,
                        AppSurfaceStatus::Active,
                    ),
                    AppCustomSurfaceMode::DeclarativeNoScript,
                    false,
                )
                .expect_err("status"),
                AppSurfaceAssetError::InstallationNotEnabled
            );
        }
        assert_eq!(
            resolve_surface_asset(
                &members,
                "surfaces/index.html",
                admission(
                    &package,
                    &package,
                    &bundle,
                    &bundle,
                    AppInstallationStatus::Enabled,
                    AppSurfaceStatus::Disabled,
                ),
                AppCustomSurfaceMode::DeclarativeNoScript,
                false,
            )
            .expect_err("surface"),
            AppSurfaceAssetError::SurfaceNotActive
        );
    }

    #[test]
    fn no_script_mode_refuses_javascript_and_wasm() {
        let package = package();
        let bundle = digest("bundle");
        for path in ["surfaces/app.js", "surfaces/mod.mjs", "surfaces/app.wasm"] {
            let members = [AppSurfaceAssetMember::from_verified_bytes(path, b"code").unwrap()];
            assert_eq!(
                resolve_surface_asset(
                    &members,
                    path,
                    admission(
                        &package,
                        &package,
                        &bundle,
                        &bundle,
                        AppInstallationStatus::Enabled,
                        AppSurfaceStatus::Active,
                    ),
                    AppCustomSurfaceMode::DeclarativeNoScript,
                    false,
                )
                .expect_err(path),
                AppSurfaceAssetError::ExecutableRefused
            );
        }
    }

    #[test]
    fn scripted_mode_with_a_killable_boundary_allows_javascript_but_not_wasm() {
        let package = package();
        let bundle = digest("bundle");
        let js = resolve_surface_asset(
            &[AppSurfaceAssetMember::from_verified_bytes("surfaces/app.js", b"code").unwrap()],
            "surfaces/app.js",
            admission(
                &package,
                &package,
                &bundle,
                &bundle,
                AppInstallationStatus::Enabled,
                AppSurfaceStatus::Active,
            ),
            AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
            true,
        )
        .expect("js");
        assert_eq!(js.kind, AppSurfaceAssetKind::Asset);
        assert!(js.isolation.allows_scripts);
        assert!(!js.isolation.allows_network);
        assert_eq!(
            resolve_surface_asset(
                &[
                    AppSurfaceAssetMember::from_verified_bytes("surfaces/app.wasm", b"code")
                        .unwrap()
                ],
                "surfaces/app.wasm",
                admission(
                    &package,
                    &package,
                    &bundle,
                    &bundle,
                    AppInstallationStatus::Enabled,
                    AppSurfaceStatus::Active,
                ),
                AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
                true,
            )
            .expect_err("wasm"),
            AppSurfaceAssetError::ExecutableRefused
        );
    }
}
