//! Owner-review surface for the `custom_surface` permission (plan 1.6).
//!
//! This module is the review half of the `custom_surfaces_v1` feature: it
//! turns a package's declared scripted entry points plus its verified
//! bundle members into the canonical "what code ships" inventory the owner
//! reviews, runs the static asset scan the platform owes that review, and
//! validates the owner's per-entry-point narrowing. It grants nothing by
//! itself: admission is the manifest kernel's coherence checks plus the
//! owner's explicit subset, and the technical boundary stays with the
//! isolation kernel, never with this review.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::models::{AppContractError, AppDigest};
use crate::apps::manifest::{
    member_has_custom_surface_executable_extension, AppBundlePath,
    AppManifestCustomSurfaceDeclaration, AppManifestPermission, AppPackageManifest,
};

/// Sandbox attribute every custom-surface host must put on the frame. This
/// is a kernel constant: no package, host page, or client configuration may
/// widen it, and widening it is a kernel code change plus a new
/// threat-model revision. `allow-scripts` only — `allow-same-origin` is
/// kernel-refused whenever scripts are enabled.
pub const CUSTOM_SURFACE_V1_SANDBOX: &str = "allow-scripts";

/// Fixed CSP body for scripted custom surfaces. `connect-src 'none'` is
/// absolute in V1: there is no egress channel at any allowlist size, and
/// the manifest has no field to request one. The frame-ancestors directive
/// is appended by [`custom_surface_v1_csp`] with the single admitted host
/// UI origin.
pub const CUSTOM_SURFACE_V1_CSP_BODY: &str = "default-src 'none'; script-src 'self'; style-src \
                                              'self' 'unsafe-inline'; img-src 'self' data:; \
                                              font-src 'self'; connect-src 'none'; frame-src \
                                              'none'; form-action 'none'; base-uri 'none'";

/// Compile the kernel-emitted CSP for one host UI origin. The origin must
/// be a single absolute `https`/`http` origin (scheme + host + optional
/// port; no path, query, fragment, credentials, wildcard, whitespace, or
/// control bytes). Multiple or wildcarded ancestors are refused — the
/// surface frame may be embedded by exactly the host UI that minted its
/// session.
pub fn custom_surface_v1_csp(
    frame_ancestors_origin: &str,
) -> Result<String, AppCustomSurfaceReviewError> {
    validate_frame_ancestors_origin(frame_ancestors_origin)?;
    Ok(format!(
        "{CUSTOM_SURFACE_V1_CSP_BODY}; frame-ancestors {frame_ancestors_origin}"
    ))
}

fn validate_frame_ancestors_origin(origin: &str) -> Result<(), AppCustomSurfaceReviewError> {
    let invalid = || AppCustomSurfaceReviewError::InvalidFrameAncestors(origin.to_owned());
    // Strict `scheme://host[:port]` grammar and nothing else: no
    // whitespace or control bytes (so a second origin can never ride a
    // space or tab past the single-ancestor rule), no wildcards, no
    // credential or path components, and printable ASCII only.
    if origin.len() > 253
        || origin.is_empty()
        || origin
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'*' | b','))
    {
        return Err(invalid());
    }
    let (scheme, rest) = origin.split_once("://").ok_or_else(invalid)?;
    if !matches!(scheme, "https" | "http") || rest.is_empty() {
        return Err(invalid());
    }
    if rest.contains('/') || rest.contains('?') || rest.contains('#') || rest.contains('@') {
        return Err(invalid());
    }
    // What remains may only be a host[:port] (brackets included for IPv6
    // literals): letters, digits, and the separators a host or port can
    // legitimately contain. Anything else fails closed.
    if !rest.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
    }) {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppCustomSurfaceReviewError {
    #[error("custom-surface frame-ancestors must be one absolute host origin: {0}")]
    InvalidFrameAncestors(String),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error("the custom-surface grant is invalid: {0}")]
    InvalidGrant(String),
}

/// One reviewed entry point: the route segment it mounts at, the
/// `surfaces/` document the frame loads, and that document's verified
/// content digest from the immutable bundle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppReviewedCustomSurfaceEntryPoint {
    pub route: String,
    pub document: String,
    pub document_digest: AppDigest,
}

/// One executable member of the reviewed "what code ships" inventory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppReviewedCustomSurfaceExecutableMember {
    pub path: String,
    pub content_digest: AppDigest,
    pub byte_len: u64,
}

/// One static-scan finding. The scan is a review aid, never a boundary:
/// minification defeats it, and the boundary is the isolation kernel. A
/// finding never blocks admission and never widens it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppReviewedCustomSurfaceScanFinding {
    pub path: String,
    pub pattern: &'static str,
}

/// The review material behind one `custom_surface` permission request.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppReviewedCustomSurfaceRequest {
    pub entry_points: Vec<AppReviewedCustomSurfaceEntryPoint>,
    /// Every `.js`/`.mjs` member under `surfaces/` (extension matched
    /// case-insensitively, exactly like the manifest kernel's executable
    /// cap) with content digests — the canonical executable inventory,
    /// whether or not any entry document references it.
    pub executable_members: Vec<AppReviewedCustomSurfaceExecutableMember>,
    pub executable_bytes: u64,
    pub scan_findings: Vec<AppReviewedCustomSurfaceScanFinding>,
    /// Fixed posture the review displays, never chooses.
    pub sandbox: &'static str,
    pub csp: &'static str,
    /// Digest over the entry points and the full executable inventory, so
    /// a swapped script changes the review material digest.
    pub request_digest: AppDigest,
}

/// Owner-selected narrowing of the reviewed entry points. Like the
/// contribution-port grant request, each entry must name an exactly
/// reviewed (route, document) pair bound to the digest the owner saw.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceEntryGrantRequest {
    pub route: String,
    pub document: String,
    pub reviewed_request_digest: AppDigest,
}

/// Static patterns the platform owes the review (design section 7.5). The
/// list is deliberately short and pattern-shaped: it is an aid for the
/// owner's eye, not an admission boundary.
const SCAN_PATTERNS: &[&str] = &[
    "fetch",
    "xmlhttprequest",
    "websocket",
    "sendbeacon",
    "window.open",
    "formaction",
    "<form",
    "http://",
    "https://",
];

/// Hydrate the review request from the manifest declaration and the
/// verified bundle members. Returns `None` when the package declares no
/// `custom_surface` permission — the pre-1.6 baseline reviews nothing new.
pub fn reviewed_custom_surface_request(
    manifest: &AppPackageManifest,
    members: &[super::manifest::AppValidatedBundleMember],
) -> Result<Option<AppReviewedCustomSurfaceRequest>, AppCustomSurfaceReviewError> {
    if !manifest
        .app
        .permissions
        .contains(&AppManifestPermission::CustomSurface)
    {
        return Ok(None);
    }
    let Some(declaration) = manifest.app.custom_surface.as_ref() else {
        return Err(AppCustomSurfaceReviewError::InvalidGrant(
            "manifest declares the custom_surface permission without its declaration block"
                .to_owned(),
        ));
    };
    let request = compile_reviewed_request(declaration, members)?;
    Ok(Some(request))
}

fn compile_reviewed_request(
    declaration: &AppManifestCustomSurfaceDeclaration,
    members: &[super::manifest::AppValidatedBundleMember],
) -> Result<AppReviewedCustomSurfaceRequest, AppCustomSurfaceReviewError> {
    let mut member_by_path = members
        .iter()
        .map(|member| (member.path().as_str(), member))
        .collect::<Vec<_>>();
    member_by_path.sort_by(|left, right| left.0.cmp(right.0));
    let mut entry_points = Vec::with_capacity(declaration.entry_points.len());
    for entry in &declaration.entry_points {
        let document = entry.document.as_str();
        let member = members
            .iter()
            .find(|member| member.path().as_str() == document)
            .ok_or_else(|| {
                AppCustomSurfaceReviewError::InvalidGrant(format!(
                    "declared entry document is not a verified bundle member: {document}"
                ))
            })?;
        entry_points.push(AppReviewedCustomSurfaceEntryPoint {
            route: entry.route.as_str().to_owned(),
            document: document.to_owned(),
            document_digest: member.content_digest().clone(),
        });
    }
    let mut executable_members = Vec::new();
    let mut executable_bytes = 0u64;
    let mut scan_findings = Vec::new();
    for (path, member) in member_by_path {
        // The SAME case-insensitive predicate the manifest kernel's 8 MiB
        // cap uses: a `.JS` member counts toward the cap, so it must count
        // here too — into the inventory, the scan, and the digest.
        if !path.starts_with("surfaces/") || !member_has_custom_surface_executable_extension(path) {
            continue;
        }
        let byte_len = member.bytes().len() as u64;
        executable_members.push(AppReviewedCustomSurfaceExecutableMember {
            path: path.to_owned(),
            content_digest: member.content_digest().clone(),
            byte_len,
        });
        executable_bytes = executable_bytes.saturating_add(byte_len);
        scan_findings.extend(scan_executable_member(path, member.bytes()));
    }
    let request_digest = AppDigest::blake3(
        format!(
            "{}|{}",
            serde_json::to_string(&entry_points)
                .map_err(|error| AppCustomSurfaceReviewError::InvalidGrant(error.to_string()))?,
            serde_json::to_string(&executable_members)
                .map_err(|error| AppCustomSurfaceReviewError::InvalidGrant(error.to_string()))?,
        )
        .as_bytes(),
    );
    Ok(AppReviewedCustomSurfaceRequest {
        entry_points,
        executable_members,
        executable_bytes,
        scan_findings,
        sandbox: CUSTOM_SURFACE_V1_SANDBOX,
        csp: CUSTOM_SURFACE_V1_CSP_BODY,
        request_digest,
    })
}

fn scan_executable_member(path: &str, bytes: &[u8]) -> Vec<AppReviewedCustomSurfaceScanFinding> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        // Non-UTF-8 executable members are themselves a finding: the
        // review should not have to wonder what they are.
        return vec![AppReviewedCustomSurfaceScanFinding {
            path: path.to_owned(),
            pattern: "non-utf8",
        }];
    };
    let lowered = text.to_ascii_lowercase();
    let mut findings = Vec::new();
    for pattern in SCAN_PATTERNS {
        if lowered.contains(pattern) {
            findings.push(AppReviewedCustomSurfaceScanFinding {
                path: path.to_owned(),
                pattern,
            });
        }
    }
    // postMessage toward parent/top is the frame-reachability pattern this
    // review exists to flag; a same-window postMessage is not.
    for probe in [
        ".parent.postmessage",
        ".top.postmessage",
        "parent.postmessage",
        "top.postmessage",
    ] {
        if lowered.contains(probe) {
            findings.push(AppReviewedCustomSurfaceScanFinding {
                path: path.to_owned(),
                pattern: "postmessage-parent-top",
            });
            break;
        }
    }
    findings
}

impl AppReviewedCustomSurfaceRequest {
    /// Validate the owner's selected subset against this exact reviewed
    /// request. `None` and empty both mean no surfaces — an absent grant is
    /// never an implicit grant of everything.
    pub fn narrow(
        &self,
        selected: Option<&[AppCustomSurfaceEntryGrantRequest]>,
    ) -> Result<Vec<AppReviewedCustomSurfaceEntryPoint>, AppCustomSurfaceReviewError> {
        let Some(selected) = selected else {
            return Ok(Vec::new());
        };
        if selected.len() > self.entry_points.len() {
            return Err(AppCustomSurfaceReviewError::InvalidGrant(
                "granted custom-surface entry-point set exceeds the reviewed request".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut granted = Vec::with_capacity(selected.len());
        for entry in selected {
            let key = (entry.route.clone(), entry.document.clone());
            if !seen.insert(key.clone()) {
                return Err(AppCustomSurfaceReviewError::InvalidGrant(format!(
                    "custom-surface entry point `{}/{}` is granted more than once",
                    entry.route, entry.document
                )));
            }
            let reviewed = self
                .entry_points
                .iter()
                .find(|candidate| {
                    candidate.route == entry.route && candidate.document == entry.document
                })
                .ok_or_else(|| {
                    AppCustomSurfaceReviewError::InvalidGrant(format!(
                        "custom-surface entry point `{}/{}` was not requested",
                        entry.route, entry.document
                    ))
                })?;
            if entry.reviewed_request_digest != self.request_digest {
                return Err(AppCustomSurfaceReviewError::InvalidGrant(format!(
                    "custom-surface entry point `{}/{}` binds stale review material; refresh the \
                     installation review",
                    entry.route, entry.document
                )));
            }
            granted.push(reviewed.clone());
        }
        Ok(granted)
    }
}

/// Parse an owner-supplied entry document path against the manifest
/// kernel's bundle-path rules. Review DTOs never trust raw wire strings.
pub fn parse_reviewed_document(raw: &str) -> Result<AppBundlePath, AppContractError> {
    AppBundlePath::parse(raw).map_err(|error| {
        AppContractError::invalid(
            "document",
            &format!("entry document is not a canonical bundle path: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::manifest::{
        build_app_package_candidate, tests::valid_skill_document, AppBundleMember, AppPackageLimits,
    };

    fn custom_surface_package(script: &str) -> crate::apps::manifest::AppPackageCandidate {
        let entry_points = "      - route: /canvas\n        document: surfaces/canvas.html\n";
        let manifest = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{entry_points}"
                ),
                1,
            );
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", manifest.into_bytes()).unwrap(),
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
                    "surfaces/canvas.html",
                    b"<html><body>canvas</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/canvas.js", script.as_bytes().to_vec())
                    .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    fn request_for(script: &str) -> AppReviewedCustomSurfaceRequest {
        let candidate = custom_surface_package(script);
        reviewed_custom_surface_request(candidate.manifest().manifest(), candidate.members())
            .expect("hydrate")
            .expect("declared")
    }

    #[test]
    fn packages_without_the_permission_review_nothing_new() {
        let candidate = build_app_package_candidate(
            {
                let mut members = vec![
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
                ];
                members.push(
                    AppBundleMember::regular_file("surfaces/index.html", b"<html></html>".to_vec())
                        .unwrap(),
                );
                members
            },
            &AppPackageLimits::default(),
        )
        .expect("baseline candidate");
        assert!(reviewed_custom_surface_request(
            candidate.manifest().manifest(),
            candidate.members()
        )
        .expect("hydrate")
        .is_none());
    }

    #[test]
    fn review_inventory_lists_every_executable_member_with_digests() {
        let request = request_for("console.log('canvas');");
        assert_eq!(request.entry_points.len(), 1);
        assert_eq!(request.entry_points[0].route, "/canvas");
        assert_eq!(request.entry_points[0].document, "surfaces/canvas.html");
        assert_eq!(request.executable_members.len(), 1);
        assert_eq!(request.executable_members[0].path, "surfaces/canvas.js");
        assert!(request.executable_bytes > 0);
        assert_eq!(request.sandbox, "allow-scripts");
        assert!(request.csp.contains("connect-src 'none'"));
        assert!(request.csp.contains("script-src 'self'"));
        // Swapping the script changes the review-material digest even
        // though the entry points are unchanged.
        let swapped = request_for("console.log('evil');");
        assert_ne!(request.request_digest, swapped.request_digest);
        assert!(request.scan_findings.is_empty());
    }

    #[test]
    fn inventory_counts_swapped_case_executable_members_like_the_cap() {
        // The manifest kernel's 8 MiB executable cap lowercases extensions,
        // so `surfaces/canvas.JS` counts against it. The inventory must use
        // the same predicate or a swapped-case script rides into the bundle
        // under the cap while escaping the inventory, scan, and digest.
        let request = swapped_case_request("console.log('case');");
        assert_eq!(request.executable_members.len(), 1);
        assert_eq!(request.executable_members[0].path, "surfaces/canvas.JS");
        assert!(request.executable_bytes > 0);
        // The swapped-case script is in the digest: changing it must move
        // the review-material digest.
        let other = swapped_case_request("console.log('evil');");
        assert_ne!(request.request_digest, other.request_digest);
    }

    fn swapped_case_request(script: &str) -> AppReviewedCustomSurfaceRequest {
        let entry_points = "      - route: /canvas\n        document: surfaces/canvas.html\n";
        let manifest = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{entry_points}"
                ),
                1,
            );
        let candidate = build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", manifest.into_bytes()).unwrap(),
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
                    "surfaces/canvas.html",
                    b"<html><body>canvas</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/canvas.JS", script.as_bytes().to_vec())
                    .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate");
        reviewed_custom_surface_request(candidate.manifest().manifest(), candidate.members())
            .expect("hydrate")
            .expect("declared")
    }

    #[test]
    fn scan_reports_forbidden_references_without_blocking() {
        let request = request_for(
            "fetch('http://evil.example/x');\nwindow.open('https://evil.example');\n\
             parent.postMessage('hi', '*');\nnew WebSocket('wss://evil.example');\n",
        );
        let patterns = request
            .scan_findings
            .iter()
            .map(|finding| finding.pattern)
            .collect::<Vec<_>>();
        for expected in [
            "fetch",
            "window.open",
            "http://",
            "postmessage-parent-top",
            "websocket",
        ] {
            assert!(patterns.contains(&expected), "missing {expected}");
        }
        // The scan is an aid: findings never refuse the request.
        assert_eq!(request.entry_points.len(), 1);
    }

    #[test]
    fn narrowing_never_grants_more_than_was_reviewed() {
        let request = request_for("console.log('canvas');");
        let good = AppCustomSurfaceEntryGrantRequest {
            route: "/canvas".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            reviewed_request_digest: request.request_digest.clone(),
        };
        assert_eq!(
            request.narrow(Some(&[good.clone()])).expect("subset"),
            request.entry_points
        );
        // Absent and empty grants both mean no surfaces.
        assert!(request.narrow(None).unwrap().is_empty());
        assert!(request.narrow(Some(&[])).unwrap().is_empty());
        // Unknown entry, duplicate entry, and stale digest all fail closed.
        let unknown = AppCustomSurfaceEntryGrantRequest {
            route: "/other".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            reviewed_request_digest: request.request_digest.clone(),
        };
        assert!(request.narrow(Some(&[unknown])).is_err());
        assert!(request.narrow(Some(&[good.clone(), good])).is_err());
        let stale = AppCustomSurfaceEntryGrantRequest {
            route: "/canvas".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            reviewed_request_digest: AppDigest::blake3(b"stale"),
        };
        assert!(request.narrow(Some(&[stale])).is_err());
        // Unknown wire fields are refused.
        assert!(
            serde_json::from_value::<AppCustomSurfaceEntryGrantRequest>(serde_json::json!({
                "route": "/canvas",
                "document": "surfaces/canvas.html",
                "reviewed_request_digest": "blake3:abc",
                "network": "egress-please"
            }))
            .is_err()
        );
    }

    #[test]
    fn frame_ancestors_admit_exactly_one_host_origin() {
        assert!(custom_surface_v1_csp("https://home.magicbeans.ai")
            .expect("absolute origin")
            .contains("frame-ancestors https://home.magicbeans.ai"));
        assert!(custom_surface_v1_csp("http://localhost:5173").is_ok());
        for hostile in [
            "*",
            "https://a.example https://b.example",
            "https://a.example,https://b.example",
            "null",
            "https://user:pass@a.example",
            "https://a.example/path",
            "file://localhost",
            "",
        ] {
            assert!(custom_surface_v1_csp(hostile).is_err(), "{hostile}");
        }
    }

    /// Repo seed location of the plan-4 Brainstorm custom-surface reference
    /// package (see `authoring.rs` for the admission/lock twin of this test).
    fn brainstorm_canvas_package_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician_data_v3/system/thinking_map/app")
    }

    fn copy_package_tree(source: &std::path::Path, destination: &std::path::Path) {
        std::fs::create_dir_all(destination).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_package_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// The reference consumer's full review request: one declared entry
    /// point bound to the shipped entry document's digest, the separate
    /// script member as the whole executable inventory, and the bridge's
    /// postMessage surfaced as scan material (the frame's only channel —
    /// the scan is an aid, the isolation kernel is the boundary).
    #[test]
    fn brainstorm_canvas_reference_package_reviews_its_full_surface_request() {
        let temporary = tempfile::tempdir().unwrap();
        let package_root = temporary.path().join("brainstorm-canvas");
        copy_package_tree(&brainstorm_canvas_package_root(), &package_root);
        let package_root = package_root.canonicalize().unwrap();
        let candidate = crate::apps::package_staging::admit_package_directory(&package_root)
            .expect("the seed package admits");

        let request =
            reviewed_custom_surface_request(candidate.manifest().manifest(), candidate.members())
                .expect("hydrate")
                .expect("the package declares the custom_surface permission");
        assert_eq!(request.entry_points.len(), 1);
        assert_eq!(request.entry_points[0].route, "/canvas");
        assert_eq!(request.entry_points[0].document, "surfaces/canvas.html");
        let entry_bytes = candidate
            .member(&AppBundlePath::parse("surfaces/canvas.html").unwrap())
            .expect("entry document member")
            .bytes();
        assert_eq!(
            request.entry_points[0].document_digest,
            AppDigest::blake3(entry_bytes),
            "the reviewed entry-point digest binds the exact shipped document"
        );

        // The executable inventory is exactly the canvas script member.
        assert_eq!(request.executable_members.len(), 1);
        assert_eq!(request.executable_members[0].path, "surfaces/canvas.js");
        let script_bytes = candidate
            .member(&AppBundlePath::parse("surfaces/canvas.js").unwrap())
            .expect("script member")
            .bytes();
        assert_eq!(
            request.executable_members[0].content_digest,
            AppDigest::blake3(script_bytes)
        );
        assert_eq!(
            request.executable_members[0].byte_len,
            script_bytes.len() as u64
        );
        assert_eq!(request.executable_bytes, script_bytes.len() as u64);
        assert!(
            request.executable_bytes < 1024 * 1024,
            "the reference consumer stays far under the 8 MiB executable cap"
        );

        // The bridge is the only outbound shape the script uses, and the
        // static scan surfaces it as review material; the SVG namespace
        // constant is the other benign finding.
        assert!(request
            .scan_findings
            .iter()
            .any(|finding| finding.path == "surfaces/canvas.js"
                && finding.pattern == "postmessage-parent-top"));
        assert!(request
            .scan_findings
            .iter()
            .any(|finding| finding.path == "surfaces/canvas.js" && finding.pattern == "http://"));
        assert!(
            request
                .scan_findings
                .iter()
                .all(|finding| finding.path == "surfaces/canvas.js"),
            "no other member carries executable findings"
        );

        // The owner's narrowing round-trips the reviewed request digest.
        let grant = AppCustomSurfaceEntryGrantRequest {
            route: "/canvas".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            reviewed_request_digest: request.request_digest.clone(),
        };
        assert_eq!(
            request.narrow(Some(&[grant])).expect("subset"),
            request.entry_points
        );
    }
}
