//! App-declared LLM operation lane over the operator trust policy (plan 1.4).
//!
//! This module owns the server half of the `llm_operations_v1` manifest
//! feature: namespacing, fail-closed admission, and profile-family
//! resolution. The trust boundary is two-sided, exactly like the rest of
//! the app platform:
//!
//! - the manifest DECLARES operation names (bounded review material only);
//! - the operator's `app_platform.llm_operations` admission policy
//!   must ADMIT each name, and the admitted name must route through
//!   `llm.router.operation_mapping` under the `app:` namespace with every
//!   selector arm naming a profile already trusted in
//!   `app_platform.processing.profiles`.
//!
//! Nothing here grants blanket LLM access: a name the policy has not
//! admitted, a missing namespaced mapping, or an arm outside the trusted
//! profile catalog fails closed. Packages declaring no operations are a
//! no-op.

use magicllm::config::{LLMRouterConfig, OperationProfileSelector, RequestShape};
use magicllm::LLMProfile;
use thiserror::Error;

use crate::config::{AppPlatformSettings, AppProcessingProfileTrust, AppProcessingTrustSettings};

use super::manifest::AppPackageManifest;

/// Namespace prefix for app-declared LLM operations in
/// `llm.router.operation_mapping`. Core lane operation keys are bare
/// `snake_case` words (see `LLMOperation::as_str`) and manifest operation
/// names use the `AppName` alphabet, so neither vocabulary can produce or
/// collide with an `app:`-prefixed key.
pub const APP_LLM_OPERATION_NAMESPACE_PREFIX: &str = "app:";

/// Maximum admitted/declared app LLM operation name length. Mirrors the
/// `AppName` byte ceiling so every manifest-declarable name fits the policy
/// key space.
pub const APP_LLM_OPERATION_MAX_NAME_BYTES: usize = 64;

/// Maximum entries in the operator's `app_platform.llm_operations` admission
/// policy, mirroring the trusted-profile catalog cap. This bound binds on the
/// config side, at load time (`enforce_app_llm_operation_admission_invariant`
/// in `magician/src/config.rs`); the manifest kernel does not apply it —
/// package-side `app.llm_operations` declarations are bounded only by the
/// contract's `max_collection_items` limit at manifest validation.
pub const APP_LLM_OPERATION_MAX_ADMITTED: usize = 256;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppLlmOperationError {
    #[error(
        "app LLM operation name `{0}` must be 1..={1} bytes, begin with an ASCII letter or digit, and contain only letters, digits, `_` or `-`"
    )]
    InvalidName(String, usize),
    #[error("app LLM operation `{0}` is not admitted by app_platform.llm_operations")]
    NotAdmitted(String),
    #[error(
        "app LLM operation `{0}` has no llm.router.operation_mapping entry for `{1}`; add the \
         namespaced mapping or drop the declaration"
    )]
    UnmappedOperation(String, String),
    #[error(
        "app LLM operation `{operation}` maps to profile `{profile}` outside the \
         app_platform.processing.profiles trust catalog"
    )]
    UntrustedProfile { operation: String, profile: String },
    #[error("app LLM operation `{0}` has no positive operator max_output_tokens ceiling")]
    MissingOutputTokenCeiling(String),
    #[error(
        "app LLM operation `{operation}` manifest max_tokens hint `{tokens}` exceeds the runtime token range"
    )]
    OutputTokenHintOutOfRange { operation: String, tokens: u64 },
}

/// Validate one app LLM operation name against the shared `AppName`
/// alphabet. Colon-containing or oversized names are rejected here, which
/// is what makes the `app:` namespace injection-proof: a manifest name can
/// never smuggle a second namespace segment or an arbitrary mapping key.
pub fn validate_app_llm_operation_name(operation: &str) -> Result<(), AppLlmOperationError> {
    let mut bytes = operation.bytes();
    if operation.is_empty()
        || operation.len() > APP_LLM_OPERATION_MAX_NAME_BYTES
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || bytes.any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    {
        return Err(AppLlmOperationError::InvalidName(
            operation.to_owned(),
            APP_LLM_OPERATION_MAX_NAME_BYTES,
        ));
    }
    Ok(())
}

/// Build the `llm.router.operation_mapping` key for one app operation name.
///
/// The two-segment `app:<operation>` form is the least-invasive scheme that
/// cannot collide with core operations: it needs one static, operator-owned
/// YAML entry per admitted name (finer per-installation keys would require
/// dynamic mapping entries the router config cannot declare), and it keeps
/// the core-lane model where an operation name is a shared routing intent
/// admitted by operator policy rather than a per-caller identity.
pub fn namespaced_app_llm_operation(operation: &str) -> Result<String, AppLlmOperationError> {
    validate_app_llm_operation_name(operation)?;
    Ok(format!("{APP_LLM_OPERATION_NAMESPACE_PREFIX}{operation}"))
}

/// The complete trusted profile family behind one admitted operation: every
/// selector arm the router could select for it, already verified to sit in
/// the `app_platform.processing.profiles` trust catalog at admission time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLlmOperationProfileFamily {
    default_profile: String,
    when_has_images: Option<String>,
    when_cloud: Option<String>,
}

impl AppLlmOperationProfileFamily {
    pub fn default_profile(&self) -> &str {
        &self.default_profile
    }

    pub fn when_has_images(&self) -> Option<&str> {
        self.when_has_images.as_deref()
    }

    pub fn when_cloud(&self) -> Option<&str> {
        self.when_cloud.as_deref()
    }
}

/// One manifest-declared operation fully admitted by the live trust policy.
/// Construction is only possible through [`admit_manifest_llm_operations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedAppLlmOperation {
    operation: String,
    namespaced_operation: String,
    reviewed_purpose: String,
    max_tokens: Option<u64>,
    profile_family: AppLlmOperationProfileFamily,
}

impl AdmittedAppLlmOperation {
    /// The manifest-declared operation name.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The `app:`-namespaced `llm.router.operation_mapping` key.
    pub fn namespaced_operation(&self) -> &str {
        &self.namespaced_operation
    }

    /// The operator's acknowledged purpose for the admitted name.
    pub fn reviewed_purpose(&self) -> &str {
        &self.reviewed_purpose
    }

    /// Package-declared token budget hint. The dispatcher intersects this
    /// with the required live operator ceiling and the selected profile.
    pub fn max_tokens(&self) -> Option<u64> {
        self.max_tokens
    }

    /// The trusted profile family the operation routes through.
    pub fn profile_family(&self) -> &AppLlmOperationProfileFamily {
        &self.profile_family
    }
}

/// Revalidate every manifest-declared LLM operation against the live trust
/// policy and router mapping — the installation-review-path revalidation
/// for the operation lane, mirroring how primitive bindings are revalidated
/// from exact reviewed bytes. Every execution-bound caller must cross this
/// seam; merely parsing or installing a declaration grants nothing. A manifest
/// declaring no operations admits to an empty list; a manifest declaring any
/// name the policy has not admitted fails closed here.
pub fn admit_manifest_llm_operations(
    policy: &AppPlatformSettings,
    router: &LLMRouterConfig,
    manifest: &AppPackageManifest,
) -> Result<Vec<AdmittedAppLlmOperation>, AppLlmOperationError> {
    let mut admitted = Vec::with_capacity(manifest.app.llm_operations.len());
    for (name, declaration) in &manifest.app.llm_operations {
        let namespaced = namespaced_app_llm_operation(name.as_str())?;
        let admission = policy
            .llm_operations
            .get(name.as_str())
            .ok_or_else(|| AppLlmOperationError::NotAdmitted(name.to_string()))?;
        let selector = router.operation_mapping.get(&namespaced).ok_or_else(|| {
            AppLlmOperationError::UnmappedOperation(name.to_string(), namespaced.clone())
        })?;
        let profile_family = trusted_profile_family(name.as_str(), selector, &policy.processing)?;
        admitted.push(AdmittedAppLlmOperation {
            operation: name.to_string(),
            namespaced_operation: namespaced,
            reviewed_purpose: admission.reviewed_purpose.clone(),
            max_tokens: declaration.max_tokens,
            profile_family,
        });
    }
    Ok(admitted)
}

/// Resolve the concrete physical profile for one admitted operation the
/// same way core lanes resolve: locality selects the arm family
/// (`when_cloud` under cloud mode), the request shape picks within it, and
/// the selected profile must still sit in the live trust catalog. The live
/// `app_platform.llm_operations` admission list is re-checked first, so an
/// `AdmittedAppLlmOperation` minted before an operator removed the name
/// fails closed instead of riding the stale mint. Returns the profile
/// together with its live trust declaration so callers mint attestations
/// through the existing processing boundary.
pub fn resolve_admitted_app_llm_operation_profile<'a>(
    admitted: &AdmittedAppLlmOperation,
    policy: &'a AppPlatformSettings,
    router: &'a LLMRouterConfig,
    shape: &RequestShape,
) -> Result<ResolvedAdmittedAppLlmOperation<'a>, AppLlmOperationError> {
    // Fail closed when the live admission list no longer carries the name:
    // every decision here is re-derived from live policy, exactly like the
    // mapping and trust-catalog checks below (the load-time invariant in
    // `magician/src/config.rs` duplicates these checks so misconfiguration
    // is diagnosed at load, but only this re-check survives a policy
    // change between admission and resolve).
    let admission = policy
        .llm_operations
        .get(admitted.operation.as_str())
        .ok_or_else(|| AppLlmOperationError::NotAdmitted(admitted.operation.clone()))?;
    let operator_max_output_tokens = admission
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| {
            AppLlmOperationError::MissingOutputTokenCeiling(admitted.operation.clone())
        })?;
    let selector = router
        .operation_mapping
        .get(admitted.namespaced_operation.as_str())
        .ok_or_else(|| {
            AppLlmOperationError::UnmappedOperation(
                admitted.operation.clone(),
                admitted.namespaced_operation.clone(),
            )
        })?;
    let profile_name = selector.profile_for_locality(shape, router.locality);
    let untrusted = || AppLlmOperationError::UntrustedProfile {
        operation: admitted.operation.clone(),
        profile: profile_name.to_owned(),
    };
    let profile = router.profiles.get(profile_name).ok_or_else(untrusted)?;
    let trust_declaration = policy
        .processing
        .profiles
        .get(profile_name)
        .ok_or_else(untrusted)?;
    let manifest_max_output_tokens = admitted
        .max_tokens
        .map(|tokens| {
            u32::try_from(tokens).map_err(|_| AppLlmOperationError::OutputTokenHintOutOfRange {
                operation: admitted.operation.clone(),
                tokens,
            })
        })
        .transpose()?;
    let max_output_tokens = manifest_max_output_tokens.map_or(operator_max_output_tokens, |hint| {
        operator_max_output_tokens.min(hint)
    });
    let max_output_tokens = profile
        .max_output_tokens
        .map_or(max_output_tokens, |profile_limit| {
            max_output_tokens.min(profile_limit)
        });
    if max_output_tokens == 0 {
        return Err(AppLlmOperationError::MissingOutputTokenCeiling(
            admitted.operation.clone(),
        ));
    }
    Ok(ResolvedAdmittedAppLlmOperation {
        profile_name,
        profile,
        trust_declaration,
        max_output_tokens,
    })
}

/// One live operation decision. Borrowing the exact policy/router snapshot
/// keeps profile identity and the effective token intersection inseparable.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedAdmittedAppLlmOperation<'a> {
    profile_name: &'a str,
    profile: &'a LLMProfile,
    trust_declaration: &'a AppProcessingProfileTrust,
    max_output_tokens: u32,
}

impl<'a> ResolvedAdmittedAppLlmOperation<'a> {
    pub fn profile_name(self) -> &'a str {
        self.profile_name
    }

    pub fn profile(self) -> &'a LLMProfile {
        self.profile
    }

    pub fn trust_declaration(self) -> &'a AppProcessingProfileTrust {
        self.trust_declaration
    }

    pub fn max_output_tokens(self) -> u32 {
        self.max_output_tokens
    }
}

/// Verify every selectable arm of one operation's mapping sits in the trust
/// catalog, and capture the family. Arms outside
/// `app_platform.processing.profiles` are rejected: an app operation may
/// only ride physically reviewed app-processing profiles.
fn trusted_profile_family(
    operation: &str,
    selector: &OperationProfileSelector,
    processing: &AppProcessingTrustSettings,
) -> Result<AppLlmOperationProfileFamily, AppLlmOperationError> {
    let family = match selector {
        OperationProfileSelector::Simple(default) => AppLlmOperationProfileFamily {
            default_profile: default.clone(),
            when_has_images: None,
            when_cloud: None,
        },
        OperationProfileSelector::Conditional {
            default,
            when_has_images,
            when_cloud,
            ..
        } => AppLlmOperationProfileFamily {
            default_profile: default.clone(),
            when_has_images: when_has_images.clone(),
            when_cloud: when_cloud.clone(),
        },
    };
    for arm in [
        Some(family.default_profile.as_str()),
        family.when_has_images.as_deref(),
        family.when_cloud.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !processing.profiles.contains_key(arm) {
            return Err(AppLlmOperationError::UntrustedProfile {
                operation: operation.to_owned(),
                profile: arm.to_owned(),
            });
        }
    }
    Ok(family)
}

#[cfg(test)]
mod tests {
    use magicllm::config::ProcessingLocality;
    use magicllm::LLMProviderKind;

    use crate::config::{
        AppPlatformSettings, AppProcessingEndpointClass, AppProcessingLlmOperationTrust,
        AppProcessingProfileTrust, AppProviderRetentionPosture,
    };

    use super::super::manifest::{
        parse_app_manifest_frontmatter, tests::llm_operation_bundle, tests::valid_skill_document,
        AppPackageLimits, AppPackageManifest,
    };
    use super::*;

    fn operation_policy() -> AppPlatformSettings {
        let mut policy = AppPlatformSettings::default();
        policy.processing.profiles.insert(
            "op-app-workflow-local".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::LoopbackManaged,
                local_processing_eligible: true,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );
        policy.processing.profiles.insert(
            "op-app-workflow-remote".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::External,
                local_processing_eligible: false,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );
        policy.llm_operations.insert(
            "summarize_record".to_owned(),
            AppProcessingLlmOperationTrust {
                reviewed_purpose: "Summarize one admitted record.".to_owned(),
                max_output_tokens: Some(4_096),
            },
        );
        policy
    }

    fn router_config(locality: ProcessingLocality) -> LLMRouterConfig {
        let mut router = LLMRouterConfig::default();
        router.locality = locality;
        for (name, model) in [
            ("op-app-workflow-local", "local-model"),
            ("op-app-workflow-remote", "remote-model"),
        ] {
            router.profiles.insert(
                name.to_owned(),
                LLMProfile {
                    provider: LLMProviderKind::Ollama,
                    model: model.to_owned(),
                    api_key_env: None,
                    api_base_url: Some(format!("http://127.0.0.1:11434/{name}")),
                    temperature: None,
                    max_output_tokens: Some(1_024),
                    default_modality: None,
                    reasoning: None,
                    metadata: None,
                    supports_vision: Some(false),
                    supports_reasoning: Some(false),
                    supports_tool_calling: Some(true),
                    supports_computer_use: Some(false),
                    timeout_secs: None,
                    context_window_tokens: Some(32_768),
                    chunking: None,
                },
            );
        }
        router.operation_mapping.insert(
            namespaced_app_llm_operation("summarize_record").unwrap(),
            OperationProfileSelector::Conditional {
                default: "op-app-workflow-local".to_owned(),
                when_has_images: None,
                when_cloud: Some("op-app-workflow-remote".to_owned()),
                description: None,
                group: None,
                engine: None,
            },
        );
        router
    }

    fn llm_operation_manifest() -> AppPackageManifest {
        let mut bundle = llm_operation_bundle();
        let skill = bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member");
        parse_app_manifest_frontmatter(&skill.bytes, &AppPackageLimits::default())
            .expect("llm operation fixture parses")
            .manifest()
            .clone()
    }

    #[test]
    fn namespacing_cannot_collide_with_core_operations_or_inject_segments() {
        assert_eq!(
            namespaced_app_llm_operation("summarize_record").unwrap(),
            "app:summarize_record"
        );
        // Core lane operation keys are bare snake_case words; none carries
        // the `app:` prefix, and manifest names cannot contain a colon.
        for core_operation in [
            "chat_completion",
            "task_decomposition",
            "memory_entity_extraction",
        ] {
            let namespaced = namespaced_app_llm_operation(core_operation).unwrap();
            assert_ne!(namespaced, core_operation);
            assert!(namespaced.starts_with(APP_LLM_OPERATION_NAMESPACE_PREFIX));
        }
        for invalid in [
            "",
            "summarize:record",
            "summarize record",
            "summarize/record",
        ] {
            assert!(
                namespaced_app_llm_operation(invalid).is_err(),
                "`{invalid}` must not produce a mapping key"
            );
        }
        let oversized = "a".repeat(APP_LLM_OPERATION_MAX_NAME_BYTES + 1);
        assert!(namespaced_app_llm_operation(&oversized).is_err());
    }

    #[test]
    fn undeclared_and_unmapped_operations_fail_closed() {
        let manifest = llm_operation_manifest();
        let mut policy = operation_policy();
        let router = router_config(ProcessingLocality::Local);

        assert!(
            admit_manifest_llm_operations(&policy, &router, &manifest).is_ok(),
            "a fully declared lane admits"
        );

        policy.llm_operations.clear();
        let error = admit_manifest_llm_operations(&policy, &router, &manifest).unwrap_err();
        assert!(error.to_string().contains("not admitted"));

        let mut unconfigured_router = router_config(ProcessingLocality::Local);
        unconfigured_router.operation_mapping.clear();
        let error =
            admit_manifest_llm_operations(&operation_policy(), &unconfigured_router, &manifest)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("no llm.router.operation_mapping"));

        let mut untrusted_arms = router_config(ProcessingLocality::Local);
        untrusted_arms.operation_mapping.insert(
            namespaced_app_llm_operation("summarize_record").unwrap(),
            OperationProfileSelector::Simple("unreviewed-profile".to_owned()),
        );
        let error = admit_manifest_llm_operations(&operation_policy(), &untrusted_arms, &manifest)
            .unwrap_err();
        assert!(error.to_string().contains("outside the"));

        // Packages that declare no operations are an additive no-op.
        let quiet = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap()
        .manifest()
        .clone();
        let admitted = admit_manifest_llm_operations(
            &operation_policy(),
            &router_config(ProcessingLocality::Local),
            &quiet,
        )
        .unwrap();
        assert!(admitted.is_empty());
    }

    #[test]
    fn admitted_operations_resolve_through_the_trusted_profile_family() {
        let manifest = llm_operation_manifest();
        let policy = operation_policy();

        let local_router = router_config(ProcessingLocality::Local);
        let admitted = admit_manifest_llm_operations(&policy, &local_router, &manifest).unwrap();
        assert_eq!(admitted.len(), 1);
        let admitted = &admitted[0];
        assert_eq!(admitted.operation(), "summarize_record");
        assert_eq!(admitted.namespaced_operation(), "app:summarize_record");
        assert_eq!(admitted.max_tokens(), Some(2_048));
        assert_eq!(
            admitted.reviewed_purpose(),
            "Summarize one admitted record."
        );
        assert_eq!(
            admitted.profile_family().default_profile(),
            "op-app-workflow-local"
        );
        let resolved = resolve_admitted_app_llm_operation_profile(
            admitted,
            &policy,
            &local_router,
            &RequestShape::NONE,
        )
        .unwrap();
        assert_eq!(resolved.profile().model, "local-model");
        assert!(resolved.trust_declaration().local_processing_eligible);
        // The intersection of THREE ceilings -- operator admission, the
        // manifest hint (2048 above), and the physical profile. This fixture's
        // profiles declare `max_output_tokens: Some(1_024)`, so the profile is
        // the binding constraint. Asserting the manifest hint here would say a
        // reviewed operation may exceed the physical profile it rides.
        assert_eq!(resolved.max_output_tokens(), 1_024);

        let cloud_router = router_config(ProcessingLocality::Cloud);
        let resolved = resolve_admitted_app_llm_operation_profile(
            admitted,
            &policy,
            &cloud_router,
            &RequestShape::NONE,
        )
        .unwrap();
        assert_eq!(resolved.profile().model, "remote-model");
        assert!(!resolved.trust_declaration().local_processing_eligible);

        // A mapping rewired to an unreviewed arm at runtime fails closed
        // even for an admission already produced.
        let mut drifted = router_config(ProcessingLocality::Local);
        drifted.operation_mapping.insert(
            namespaced_app_llm_operation("summarize_record").unwrap(),
            OperationProfileSelector::Simple("unreviewed-profile".to_owned()),
        );
        let error = resolve_admitted_app_llm_operation_profile(
            admitted,
            &policy,
            &drifted,
            &RequestShape::NONE,
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside the"));
    }

    /// The resolver re-checks the live `app_platform.llm_operations`
    /// admission list before anything else: an `AdmittedAppLlmOperation`
    /// minted while the name was admitted must stop resolving the moment
    /// an operator removes that name, instead of riding the stale mint.
    #[test]
    fn resolver_rechecks_the_live_admission_list_before_resolving() {
        let manifest = llm_operation_manifest();
        let policy = operation_policy();
        let router = router_config(ProcessingLocality::Local);

        // Admission present: the admitted operation resolves.
        let admitted = admit_manifest_llm_operations(&policy, &router, &manifest).unwrap();
        let admitted = &admitted[0];
        assert!(
            resolve_admitted_app_llm_operation_profile(
                admitted,
                &policy,
                &router,
                &RequestShape::NONE
            )
            .is_ok(),
            "a live admission resolves"
        );

        // The same admission, resolved after the operator removed the
        // name, fails closed with the admission error.
        let mut revoked = operation_policy();
        revoked.llm_operations.clear();
        let error = resolve_admitted_app_llm_operation_profile(
            admitted,
            &revoked,
            &router,
            &RequestShape::NONE,
        )
        .unwrap_err();
        assert!(
            matches!(error, AppLlmOperationError::NotAdmitted(_)),
            "a removed admission must fail closed, got: {error}"
        );
        assert!(error.to_string().contains("not admitted"));
    }
}
