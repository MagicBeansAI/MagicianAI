//! One shared construction of a governed MCP OAuth credential binding.
//!
//! Service-account Connect/status (`magician-api`) and governed MCP runs
//! (`primitive_dispatch::governed_mcp`) must derive the exact same
//! [`CredentialProfileKey`]: the vault binding id hashes the principal,
//! workspace, provider, profile alias, canonical resource URL and issuer, so a
//! run that picks a different endpoint or profile than Connect did cannot find
//! the saved login. Both callers build the key, and the OAuth coordinator on
//! top of it, only through this module.

use std::sync::Arc;

use magician_mcp_client::{McpOAuthClientIdentity, McpOAuthCoordinator, McpOAuthCoordinatorError};
use serde::Deserialize;
use tool_runtime_core::{
    credential_profiles::{
        CanonicalCredentialUrl, CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    },
    manifest::{AuthKind, McpTransport, ProfileSelection, RuntimeProtocol},
    manifest_parser::{parse_skill_magician_extension, SkillRuntimePackage},
};

use crate::magician_v2::{
    mcp_oauth::{McpOAuthApi, OAuthCallbackMode},
    secrets::{mcp_oauth_vault::SecretStoreMcpOAuthVault, SecretStore},
};

/// Client name presented during dynamic registration by every governed
/// MCP OAuth coordinator.
pub const MCP_OAUTH_CLIENT_NAME: &str = "Magican MCP Client";

/// Why a governed MCP OAuth binding could not be built from a reviewed
/// skill package. Callers map these to their own error surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthBindingError {
    NotRemoteMcp,
    NotStreamableHttp,
    ProviderMissing,
    OAuthPolicyMissing,
    CallbackPolicyInvalid,
    EndpointAliasRequired,
    EndpointAliasUndeclared,
    ProfileAliasRequired,
    ProfileImplicit,
    ResourceInvalid,
    IssuerInvalid,
    ProfileInvalid,
}

impl McpOAuthBindingError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotRemoteMcp => "governed MCP OAuth is available only for remote MCP skills",
            Self::NotStreamableHttp => "governed MCP OAuth requires a Streamable HTTP MCP endpoint",
            Self::ProviderMissing => "the skill OAuth provider is not declared",
            Self::OAuthPolicyMissing => "the skill's MCP OAuth policy is not declared",
            Self::CallbackPolicyInvalid => "the skill's browser sign-in callback policy is invalid",
            Self::EndpointAliasRequired => "the skill requires an MCP endpoint choice",
            Self::EndpointAliasUndeclared => "the MCP endpoint alias is not declared by the skill",
            Self::ProfileAliasRequired => "the skill requires an explicit OAuth profile",
            Self::ProfileImplicit => "governed MCP OAuth requires an explicit local profile",
            Self::ResourceInvalid => "the skill's MCP resource binding is invalid",
            Self::IssuerInvalid => "the skill's OAuth issuer binding is invalid",
            Self::ProfileInvalid => "the skill's OAuth profile binding is invalid",
        }
    }
}

/// The exact credential binding for one (scope, endpoint, profile) choice.
#[derive(Debug, Clone)]
pub struct McpOAuthBinding {
    pub profile: CredentialProfileKey,
    pub provider: String,
    pub profile_alias: String,
    pub endpoint_url: String,
    /// `None` for single-endpoint skills.
    pub endpoint_alias: Option<String>,
    pub scopes: Vec<String>,
    pub callback_mode: Option<OAuthCallbackMode>,
}

/// Reviewed product metadata, separate from SDK-owned OAuth protocol settings.
/// A skill selects a supported mode, never a callback URL, port or route.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillBrowserSignIn {
    callback_mode: OAuthCallbackMode,
}

/// Read the same bounded, strict declaration for catalog, setup and app routes.
pub fn mcp_oauth_callback_mode(
    source: &str,
    package: &SkillRuntimePackage,
) -> Result<Option<OAuthCallbackMode>, McpOAuthBindingError> {
    let declaration: Option<SkillBrowserSignIn> =
        parse_skill_magician_extension(source, "browser_signin")
            .map_err(|_| McpOAuthBindingError::CallbackPolicyInvalid)?;
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    if package.contract.auth.kind != AuthKind::OAuthSession
        || !matches!(
            &package.contract.runtime,
            RuntimeProtocol::Mcp {
                transport: McpTransport::StreamableHttp { .. },
                discovery,
                ..
            } if discovery.oauth.is_some()
        )
    {
        return Err(McpOAuthBindingError::CallbackPolicyInvalid);
    }
    Ok(Some(declaration.callback_mode))
}

/// Declared endpoint aliases of a multi-endpoint remote MCP skill, sorted.
/// Empty for single-endpoint skills and non-MCP packages.
pub fn mcp_endpoint_aliases(package: &SkillRuntimePackage) -> Vec<String> {
    match &package.contract.runtime {
        RuntimeProtocol::Mcp { discovery, .. } => {
            discovery.endpoint_aliases.keys().cloned().collect()
        },
        _ => Vec::new(),
    }
}

/// Build the governed MCP OAuth binding for `scope`.
///
/// `endpoint_alias` selects one declared alias of a multi-endpoint skill
/// (`None` = the manifest's default alias); it must be `None` for a
/// single-endpoint skill. `profile_alias` is honoured only for
/// `ProfileSelection::Selectable` (falling back to its default); fixed
/// profiles ignore it and `ProfileSelection::None` binds `"default"`.
pub fn mcp_oauth_binding(
    package: &SkillRuntimePackage,
    scope: CredentialScope,
    endpoint_alias: Option<&str>,
    profile_alias: Option<&str>,
    callback_mode: Option<OAuthCallbackMode>,
) -> Result<McpOAuthBinding, McpOAuthBindingError> {
    let RuntimeProtocol::Mcp {
        transport,
        discovery,
        ..
    } = &package.contract.runtime
    else {
        return Err(McpOAuthBindingError::NotRemoteMcp);
    };
    let McpTransport::StreamableHttp { endpoint: base } = transport else {
        return Err(McpOAuthBindingError::NotStreamableHttp);
    };
    let provider = package
        .contract
        .auth
        .provider
        .as_deref()
        .ok_or(McpOAuthBindingError::ProviderMissing)?;
    let oauth = discovery
        .oauth
        .as_ref()
        .ok_or(McpOAuthBindingError::OAuthPolicyMissing)?;
    let (endpoint_url, endpoint_alias) = if discovery.endpoint_aliases.is_empty() {
        if endpoint_alias.is_some() {
            return Err(McpOAuthBindingError::EndpointAliasUndeclared);
        }
        (base.clone(), None)
    } else {
        let selected = endpoint_alias
            .map(str::to_owned)
            .or_else(|| discovery.default_endpoint_alias.clone())
            .ok_or(McpOAuthBindingError::EndpointAliasRequired)?;
        let url = discovery
            .endpoint_aliases
            .get(&selected)
            .cloned()
            .ok_or(McpOAuthBindingError::EndpointAliasUndeclared)?;
        (url, Some(selected))
    };
    let profile_alias = match &package.contract.auth.profile_selection {
        ProfileSelection::Fixed { alias } => alias.clone(),
        ProfileSelection::Selectable { default } => profile_alias
            .map(str::to_owned)
            .or_else(|| default.clone())
            .ok_or(McpOAuthBindingError::ProfileAliasRequired)?,
        ProfileSelection::None => "default".to_owned(),
        ProfileSelection::Implicit => return Err(McpOAuthBindingError::ProfileImplicit),
    };
    let profile = CredentialProfileKey::new(
        scope,
        provider,
        profile_alias.clone(),
        CredentialProfileBinding::McpOauth {
            resource_url: CanonicalCredentialUrl::new(&endpoint_url)
                .map_err(|_| McpOAuthBindingError::ResourceInvalid)?,
            authorization_issuer: CanonicalCredentialUrl::new(&oauth.authorization_issuer)
                .map_err(|_| McpOAuthBindingError::IssuerInvalid)?,
        },
    )
    .map_err(|_| McpOAuthBindingError::ProfileInvalid)?;
    Ok(McpOAuthBinding {
        profile,
        provider: provider.to_owned(),
        profile_alias,
        endpoint_url,
        endpoint_alias,
        scopes: oauth.scopes.iter().cloned().collect(),
        callback_mode,
    })
}

impl McpOAuthBinding {
    /// The SDK coordinator for this exact binding over a scoped vault.
    pub fn coordinator(
        &self,
        store: Arc<SecretStore>,
        oauth_api: &McpOAuthApi,
    ) -> Result<McpOAuthCoordinator, McpOAuthCoordinatorError> {
        McpOAuthCoordinator::new(
            self.profile.clone(),
            Arc::new(SecretStoreMcpOAuthVault::new(store)),
            oauth_api.callback_base_url_for(self.callback_mode),
            McpOAuthClientIdentity::DynamicPublic,
        )
        .and_then(|coordinator| coordinator.with_client_name(MCP_OAUTH_CLIENT_NAME))
        .and_then(|coordinator| coordinator.with_scopes(self.scopes.iter().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swiggy() -> SkillRuntimePackage {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../skillshub/swiggy-mcp/SKILL.md"),
        )
        .expect("read the reviewed Swiggy skill");
        tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .expect("parse the Swiggy runtime package")
            .expect("Swiggy declares a governed runtime")
    }

    fn source_and_package(name: &str) -> (String, SkillRuntimePackage) {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../skillshub")
                .join(name)
                .join("SKILL.md"),
        )
        .expect("reviewed skill");
        let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .expect("runtime package")
            .expect("governed skill");
        (source, package)
    }

    #[test]
    fn tool_callback_declaration_works_for_a_new_provider_without_engine_changes() {
        let (source, _) = source_and_package("zepto-mcp");
        let source = source.replace("zepto-mcp", "callback-fixture");
        let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .expect("runtime package")
            .expect("governed skill");
        let mode = mcp_oauth_callback_mode(&source, &package).expect("callback declaration");
        let binding = mcp_oauth_binding(&package, scope(), None, None, mode).expect("binding");
        assert_eq!(binding.provider, "callback-fixture");
        assert_eq!(
            binding.callback_mode,
            Some(OAuthCallbackMode::LoopbackLocalhost)
        );
        let api = McpOAuthApi::new_with_callback_port(
            Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(4)),
            4317,
        );
        assert_eq!(
            api.callback_base_url_for(binding.callback_mode),
            "http://localhost:4317/api/magician/v2/auth/mcp/oauth/callback"
        );
    }

    #[test]
    fn callback_declarations_reject_urls_unknown_modes_and_unknown_fields() {
        let (source, package) = source_and_package("zepto-mcp");
        for invalid in [
            "callback_mode: https://attacker.example/callback",
            "callback_mode: unreviewed_mode",
            "callback_mode: loopback_localhost\n      callback_url: https://attacker.example/callback",
            "callback_mode: loopback_localhost\n      port: 80",
        ] {
            let invalid_source = source.replace("callback_mode: loopback_localhost", invalid);
            assert_eq!(
                mcp_oauth_callback_mode(&invalid_source, &package).err(),
                Some(McpOAuthBindingError::CallbackPolicyInvalid)
            );
        }
    }

    #[test]
    fn missing_callback_declaration_preserves_generic_fallback_and_binding_identity() {
        let (source, package) = source_and_package("zepto-mcp");
        let source = source.replace(
            "    browser_signin:\n      callback_mode: loopback_localhost\n",
            "",
        );
        assert_eq!(
            mcp_oauth_callback_mode(&source, &package).expect("optional policy"),
            None
        );
        let default =
            mcp_oauth_binding(&package, scope(), None, None, None).expect("default binding");
        let localhost = mcp_oauth_binding(
            &package,
            scope(),
            None,
            None,
            Some(OAuthCallbackMode::LoopbackLocalhost),
        )
        .expect("localhost binding");
        assert_eq!(
            default.profile, localhost.profile,
            "callback preferences must not change the vault scope"
        );
    }

    #[test]
    fn callback_declaration_is_refused_without_an_oauth_connection() {
        let (source, mut package) = source_and_package("zepto-mcp");
        if let RuntimeProtocol::Mcp { discovery, .. } = &mut package.contract.runtime {
            discovery.oauth = None;
        }
        assert_eq!(
            mcp_oauth_callback_mode(&source, &package).err(),
            Some(McpOAuthBindingError::CallbackPolicyInvalid)
        );
    }

    fn scope() -> CredentialScope {
        CredentialScope::new("owner", "default").expect("scope")
    }

    #[test]
    fn each_declared_alias_is_its_own_binding_and_undeclared_aliases_are_refused() {
        let package = swiggy();
        assert_eq!(
            mcp_endpoint_aliases(&package),
            vec![
                "dineout".to_owned(),
                "food".to_owned(),
                "instamart".to_owned(),
                "scenes".to_owned()
            ]
        );
        let default =
            mcp_oauth_binding(&package, scope(), None, None, None).expect("default binding");
        assert_eq!(default.endpoint_alias.as_deref(), Some("food"));
        assert_eq!(default.endpoint_url, "https://mcp.swiggy.com/food");
        let instamart = mcp_oauth_binding(&package, scope(), Some("instamart"), None, None)
            .expect("alias binding");
        assert_eq!(instamart.endpoint_url, "https://mcp.swiggy.com/im");
        assert_eq!(instamart.profile_alias, default.profile_alias);
        assert_ne!(instamart.profile, default.profile);
        assert_eq!(
            mcp_oauth_binding(&package, scope(), Some("https://attacker.test"), None, None).err(),
            Some(McpOAuthBindingError::EndpointAliasUndeclared)
        );
    }
}
