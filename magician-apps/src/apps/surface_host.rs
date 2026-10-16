//! Phase 7 no-script host envelope.
//!
//! Turns a resolved `surfaces/*.html` document into the payload a host
//! may load: `srcdoc`, empty sandbox tokens, and a deny-all CSP. This
//! is not the general Iframe component and must not emit
//! `allow-scripts allow-same-origin`. Binding to a staged package
//! candidate reads only live `surfaces/` members. It does not open a
//! frame or serve HTTP.

use thiserror::Error;

use super::{
    manifest::{AppBundlePath, AppPackageCandidate},
    models::{AppDigest, AppReference},
    sandbox::{
        AppBridgeSession, AppCustomSurfaceIsolation, AppCustomSurfaceMode, GENERAL_IFRAME_SANDBOX,
    },
    surface_assets::{
        resolve_surface_asset_from_candidate, AppResolvedSurfaceAsset, AppSurfaceAssetAdmission,
        AppSurfaceAssetError, AppSurfaceAssetKind,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppNoScriptHostEnvelope {
    pub srcdoc: String,
    pub sandbox: String,
    pub csp: String,
    pub allowed_assets: Vec<AppBundlePath>,
    pub isolation: AppCustomSurfaceIsolation,
    pub session: AppBridgeSession,
    pub envelope_digest: AppDigest,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppSurfaceHostError {
    #[error("no-script host requires an HTML document, not an asset")]
    NotADocument,
    #[error("no-script host cannot load a scripted isolation plan")]
    ScriptedIsolation,
    #[error("no-script host cannot emit the general iframe sandbox")]
    GeneralIframeTokensRefused,
    #[error("no-script host document is not UTF-8")]
    DocumentNotUtf8,
    #[error("admitted asset does not belong to this host session")]
    AssetSessionMismatch,
    #[error("admitted host member is not a non-executable asset")]
    InvalidAdmittedAsset,
    #[error(transparent)]
    Asset(#[from] AppSurfaceAssetError),
}

pub fn compile_no_script_host_envelope(
    document: &AppResolvedSurfaceAsset,
    admitted_assets: &[AppResolvedSurfaceAsset],
) -> Result<AppNoScriptHostEnvelope, AppSurfaceHostError> {
    if document.kind != AppSurfaceAssetKind::Document {
        return Err(AppSurfaceHostError::NotADocument);
    }
    refuse_scripted_or_general_iframe(&document.isolation)?;
    let document_html = String::from_utf8(document.bytes.clone())
        .map_err(|_| AppSurfaceHostError::DocumentNotUtf8)?;
    let mut allowed_assets = Vec::with_capacity(admitted_assets.len());
    for asset in admitted_assets {
        if asset.kind != AppSurfaceAssetKind::Asset {
            return Err(AppSurfaceHostError::InvalidAdmittedAsset);
        }
        refuse_scripted_or_general_iframe(&asset.isolation)?;
        if !same_host_binding(document, asset) {
            return Err(AppSurfaceHostError::AssetSessionMismatch);
        }
        allowed_assets.push(asset.path.clone());
    }
    allowed_assets.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let srcdoc = inline_admitted_styles(document_html, admitted_assets);
    let sandbox = document.isolation.sandbox.clone();
    let csp = document.isolation.csp.clone();
    let envelope_digest = digest_envelope(
        &srcdoc,
        &sandbox,
        &csp,
        &allowed_assets,
        &document.session.session_ref,
        &document.isolation.package_revision_ref,
    );
    Ok(AppNoScriptHostEnvelope {
        srcdoc,
        sandbox,
        csp,
        allowed_assets,
        isolation: document.isolation.clone(),
        session: document.session.clone(),
        envelope_digest,
    })
}

pub fn compile_no_script_host_from_package(
    candidate: &AppPackageCandidate,
    document_path: &str,
    admission: AppSurfaceAssetAdmission<'_>,
) -> Result<AppNoScriptHostEnvelope, AppSurfaceHostError> {
    if admission.live_bundle_digest != candidate.bundle_digest()
        || admission.bundle_digest != candidate.bundle_digest()
    {
        return Err(AppSurfaceAssetError::StalePackageRevision.into());
    }
    let document = resolve_surface_asset_from_candidate(
        candidate.members(),
        document_path,
        clone_admission(&admission),
        AppCustomSurfaceMode::DeclarativeNoScript,
        false,
    )?;
    let mut admitted_assets = Vec::new();
    for member in candidate.members() {
        let path = member.path().as_str();
        if path == document_path || !path.starts_with("surfaces/") {
            continue;
        }
        match resolve_surface_asset_from_candidate(
            candidate.members(),
            path,
            clone_admission(&admission),
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        ) {
            Ok(asset) if asset.kind == AppSurfaceAssetKind::Asset => {
                admitted_assets.push(asset);
            },
            Ok(_) => {},
            Err(AppSurfaceAssetError::ExecutableRefused) => {},
            Err(error) => return Err(error.into()),
        }
    }
    compile_no_script_host_envelope(&document, &admitted_assets)
}

fn clone_admission<'a>(admission: &AppSurfaceAssetAdmission<'a>) -> AppSurfaceAssetAdmission<'a> {
    AppSurfaceAssetAdmission {
        package_revision_ref: admission.package_revision_ref,
        live_package_revision_ref: admission.live_package_revision_ref,
        bundle_digest: admission.bundle_digest,
        live_bundle_digest: admission.live_bundle_digest,
        installation_id: admission.installation_id.clone(),
        installation_status: admission.installation_status,
        surface_status: admission.surface_status,
        surface_revision: admission.surface_revision,
        grant_revision: admission.grant_revision,
        host_session_ref: admission.host_session_ref.clone(),
        session_ref: admission.session_ref.clone(),
        nonce: admission.nonce.clone(),
        now: admission.now,
    }
}

fn refuse_scripted_or_general_iframe(
    isolation: &AppCustomSurfaceIsolation,
) -> Result<(), AppSurfaceHostError> {
    if isolation.mode != AppCustomSurfaceMode::DeclarativeNoScript
        || isolation.allows_scripts
        || isolation.allows_same_origin
        || isolation.allows_network
    {
        return Err(AppSurfaceHostError::ScriptedIsolation);
    }
    if isolation.sandbox.split_whitespace().any(|token| {
        GENERAL_IFRAME_SANDBOX
            .split_whitespace()
            .any(|forbidden| forbidden == token)
    }) {
        return Err(AppSurfaceHostError::GeneralIframeTokensRefused);
    }
    if isolation.sandbox == GENERAL_IFRAME_SANDBOX {
        return Err(AppSurfaceHostError::GeneralIframeTokensRefused);
    }
    Ok(())
}

fn same_host_binding(document: &AppResolvedSurfaceAsset, asset: &AppResolvedSurfaceAsset) -> bool {
    document.session.session_ref == asset.session.session_ref
        && document.session.installation_id == asset.session.installation_id
        && document.session.package_revision_ref == asset.session.package_revision_ref
        && document.session.nonce == asset.session.nonce
        && document.isolation.package_revision_ref == asset.isolation.package_revision_ref
}

fn inline_admitted_styles(srcdoc: String, assets: &[AppResolvedSurfaceAsset]) -> String {
    let mut styles = String::new();
    for asset in assets {
        if !asset.path.as_str().ends_with(".css") {
            continue;
        }
        let Ok(css) = std::str::from_utf8(&asset.bytes) else {
            continue;
        };
        styles.push_str("<style>");
        styles.push_str(css);
        styles.push_str("</style>");
    }
    if styles.is_empty() {
        return srcdoc;
    }
    let lower = srcdoc.to_ascii_lowercase();
    if let Some(idx) = lower.find("</head>") {
        let mut out = String::with_capacity(srcdoc.len() + styles.len());
        out.push_str(&srcdoc[..idx]);
        out.push_str(&styles);
        out.push_str(&srcdoc[idx..]);
        out
    } else if let Some(idx) = lower.find("<head>") {
        let insert_at = idx + "<head>".len();
        let mut out = String::with_capacity(srcdoc.len() + styles.len());
        out.push_str(&srcdoc[..insert_at]);
        out.push_str(&styles);
        out.push_str(&srcdoc[insert_at..]);
        out
    } else if let Some(idx) = lower.find("<html>") {
        let insert_at = idx + "<html>".len();
        let mut out = String::with_capacity(srcdoc.len() + styles.len());
        out.push_str(&srcdoc[..insert_at]);
        out.push_str("<head>");
        out.push_str(&styles);
        out.push_str("</head>");
        out.push_str(&srcdoc[insert_at..]);
        out
    } else {
        format!("{styles}{srcdoc}")
    }
}

fn digest_envelope(
    srcdoc: &str,
    sandbox: &str,
    csp: &str,
    allowed_assets: &[AppBundlePath],
    session_ref: &AppReference,
    package_revision_ref: &AppReference,
) -> AppDigest {
    let mut material = String::new();
    material.push_str(srcdoc);
    material.push('\n');
    material.push_str(sandbox);
    material.push('\n');
    material.push_str(csp);
    material.push('\n');
    material.push_str(session_ref.as_str());
    material.push('\n');
    material.push_str(package_revision_ref.as_str());
    for path in allowed_assets {
        material.push('\n');
        material.push_str(path.as_str());
    }
    AppDigest::blake3(material.as_bytes())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};

    use super::*;
    use crate::apps::surface_assets::{
        resolve_surface_asset, AppSurfaceAssetAdmission, AppSurfaceAssetMember,
    };
    use magician::magician_v2::apps::{
        lifecycle::AppInstallationStatus,
        manifest::{
            build_app_package_candidate, tests::valid_skill_document, AppBundleMember,
            AppPackageLimits,
        },
        models::{AppInstallationId, AppRevision},
        records::AppSurfaceStatus,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 20, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn resolve(path: &str, bytes: &[u8]) -> AppResolvedSurfaceAsset {
        let package = reference("package-revision:reading-list");
        let bundle = AppDigest::blake3(b"bundle");
        let members = [AppSurfaceAssetMember::from_verified_bytes(path, bytes.to_vec()).unwrap()];
        let admission = AppSurfaceAssetAdmission {
            package_revision_ref: &package,
            live_package_revision_ref: &package,
            bundle_digest: &bundle,
            live_bundle_digest: &bundle,
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_status: AppInstallationStatus::Enabled,
            surface_status: AppSurfaceStatus::Active,
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            host_session_ref: reference("host:session-1"),
            session_ref: reference("bridge:session-1"),
            nonce: reference("nonce:1"),
            now: time(1),
        };
        resolve_surface_asset(
            &members,
            path,
            admission,
            AppCustomSurfaceMode::DeclarativeNoScript,
            false,
        )
        .expect("resolve")
    }

    #[test]
    fn html_document_becomes_a_no_script_srcdoc_envelope() {
        let document = resolve("surfaces/index.html", b"<html><body>ok</body></html>");
        let style = resolve("surfaces/theme.css", b"body{color:black}");
        let envelope = compile_no_script_host_envelope(&document, &[style]).expect("envelope");
        assert!(envelope.srcdoc.contains("<style>body{color:black}</style>"));
        assert!(envelope.srcdoc.contains("<body>ok</body>"));
        assert!(envelope.sandbox.is_empty());
        assert_ne!(envelope.sandbox, GENERAL_IFRAME_SANDBOX);
        assert!(envelope.csp.contains("default-src 'none'"));
        assert!(envelope.csp.contains("style-src 'unsafe-inline'"));
        assert_eq!(envelope.allowed_assets.len(), 1);
        assert_eq!(envelope.allowed_assets[0].as_str(), "surfaces/theme.css");
        assert!(!envelope.isolation.allows_scripts);
    }

    #[test]
    fn general_iframe_tokens_never_appear_on_the_envelope() {
        let document = resolve("surfaces/index.html", b"<html></html>");
        let envelope = compile_no_script_host_envelope(&document, &[]).expect("envelope");
        assert!(!envelope.sandbox.contains("allow-scripts"));
        assert!(!envelope.sandbox.contains("allow-same-origin"));
        assert_ne!(envelope.sandbox, GENERAL_IFRAME_SANDBOX);
    }

    #[test]
    fn an_asset_cannot_be_the_host_document() {
        let asset = resolve("surfaces/theme.css", b"body{}");
        let error = compile_no_script_host_envelope(&asset, &[]).expect_err("not a document");
        assert_eq!(error, AppSurfaceHostError::NotADocument);
    }

    #[test]
    fn scripted_isolation_cannot_compile_a_no_script_host() {
        let mut document = resolve("surfaces/index.html", b"<html></html>");
        document.isolation.mode = AppCustomSurfaceMode::ScriptedRequiresKillableBoundary;
        document.isolation.allows_scripts = true;
        document.isolation.sandbox = "allow-scripts".to_owned();
        let error = compile_no_script_host_envelope(&document, &[]).expect_err("scripted");
        assert_eq!(error, AppSurfaceHostError::ScriptedIsolation);
    }

    #[test]
    fn forged_general_iframe_sandbox_on_isolation_is_refused() {
        let mut document = resolve("surfaces/index.html", b"<html></html>");
        document.isolation.sandbox = GENERAL_IFRAME_SANDBOX.to_owned();
        let error = compile_no_script_host_envelope(&document, &[]).expect_err("iframe tokens");
        assert_eq!(error, AppSurfaceHostError::GeneralIframeTokensRefused);
    }

    #[test]
    fn admitted_assets_must_share_the_document_session() {
        let document = resolve("surfaces/index.html", b"<html></html>");
        let mut other = resolve("surfaces/theme.css", b"body{}");
        other.session.session_ref = reference("bridge:other");
        let error = compile_no_script_host_envelope(&document, &[other]).expect_err("mismatch");
        assert_eq!(error, AppSurfaceHostError::AssetSessionMismatch);
    }

    #[test]
    fn envelope_digest_changes_when_srcdoc_changes() {
        let first = compile_no_script_host_envelope(
            &resolve("surfaces/index.html", b"<html><body>one</body></html>"),
            &[],
        )
        .unwrap();
        let second = compile_no_script_host_envelope(
            &resolve("surfaces/index.html", b"<html><body>two</body></html>"),
            &[],
        )
        .unwrap();
        assert_ne!(first.envelope_digest, second.envelope_digest);
    }

    fn staged_candidate() -> magician::magician_v2::apps::manifest::AppPackageCandidate {
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
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
                AppBundleMember::regular_file("surfaces/theme.css", b"body{color:navy}".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("surfaces/app.js", b"alert(1)".to_vec()).unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    fn package_admission<'a>(
        package: &'a AppReference,
        bundle: &'a AppDigest,
        live_bundle: &'a AppDigest,
    ) -> AppSurfaceAssetAdmission<'a> {
        AppSurfaceAssetAdmission {
            package_revision_ref: package,
            live_package_revision_ref: package,
            bundle_digest: bundle,
            live_bundle_digest: live_bundle,
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_status: AppInstallationStatus::Enabled,
            surface_status: AppSurfaceStatus::Active,
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            host_session_ref: reference("host:session-1"),
            session_ref: reference("bridge:session-1"),
            nonce: reference("nonce:1"),
            now: time(1),
        }
    }

    #[test]
    fn staged_package_hosts_only_live_surfaces_members() {
        let candidate = staged_candidate();
        let package = reference("package-revision:reading-list");
        let bundle = candidate.bundle_digest().clone();
        let envelope = compile_no_script_host_from_package(
            &candidate,
            "surfaces/index.html",
            package_admission(&package, &bundle, &bundle),
        )
        .expect("hosted");
        assert!(envelope.srcdoc.contains("<style>body{color:navy}</style>"));
        assert!(envelope.srcdoc.contains("<body>plan</body>"));
        assert_eq!(
            envelope
                .allowed_assets
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>(),
            vec!["surfaces/theme.css"]
        );
        assert!(envelope.sandbox.is_empty());
    }

    #[test]
    fn staged_package_refuses_non_surface_and_stale_digest() {
        let candidate = staged_candidate();
        let package = reference("package-revision:reading-list");
        let bundle = candidate.bundle_digest().clone();
        let skill = compile_no_script_host_from_package(
            &candidate,
            "SKILL.md",
            package_admission(&package, &bundle, &bundle),
        )
        .expect_err("skill");
        assert!(matches!(
            skill,
            AppSurfaceHostError::Asset(AppSurfaceAssetError::NotASurfacePath)
                | AppSurfaceHostError::Asset(AppSurfaceAssetError::Manifest(_))
        ));
        let icon = compile_no_script_host_from_package(
            &candidate,
            "assets/icon.svg",
            package_admission(&package, &bundle, &bundle),
        )
        .expect_err("icon");
        assert_eq!(
            icon,
            AppSurfaceHostError::Asset(AppSurfaceAssetError::NotASurfacePath)
        );
        let stale = AppDigest::blake3(b"not-the-candidate");
        let mismatch = compile_no_script_host_from_package(
            &candidate,
            "surfaces/index.html",
            package_admission(&package, &bundle, &stale),
        )
        .expect_err("stale");
        assert_eq!(
            mismatch,
            AppSurfaceHostError::Asset(AppSurfaceAssetError::StalePackageRevision)
        );
    }
}
