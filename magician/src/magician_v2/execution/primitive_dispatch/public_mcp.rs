//! Configuration for explicitly anonymous remote MCP contracts. This never
//! serves as a fallback when a credential-bearing connection cannot sign in.

use magician_mcp_client::{McpClientConfig, McpTransportConfig, StreamableHttpTransportConfig};
use tool_runtime_core::{
    manifest::{AuthKind, AuthRequirement, McpTransport, RuntimeProtocol, SkillRuntimeContract},
    manifest_validation::validate_skill_runtime_contract,
};

pub(super) struct PublicMcpBinding {
    pub endpoint: String,
    pub alias: Option<String>,
    pub config: McpClientConfig,
}

pub(super) fn public_mcp_binding(
    contract: &SkillRuntimeContract,
    selected: Option<&str>,
) -> Result<PublicMcpBinding, String> {
    validate_skill_runtime_contract(contract)
        .map_err(|_| "anonymous remote MCP contract is invalid")?;
    let RuntimeProtocol::Mcp {
        transport: McpTransport::StreamableHttp { endpoint },
        discovery,
        limits,
    } = &contract.runtime
    else {
        return Err("anonymous remote MCP requires Streamable HTTP".into());
    };
    if contract.auth.kind != AuthKind::None
        || contract.auth.requirement != AuthRequirement::None
        || discovery.oauth.is_some()
    {
        return Err("anonymous remote MCP must not declare credentials or OAuth".into());
    }
    let (endpoint, alias) = if discovery.endpoint_aliases.is_empty() {
        if selected.is_some() {
            return Err("governed MCP endpoint alias is not declared".into());
        }
        (endpoint.clone(), None)
    } else {
        let alias = selected
            .or(discovery.default_endpoint_alias.as_deref())
            .ok_or("governed MCP endpoint alias is required")?;
        let endpoint = discovery
            .endpoint_aliases
            .get(alias)
            .ok_or("governed MCP endpoint alias is not declared")?;
        (endpoint.clone(), Some(alias.to_owned()))
    };
    let mut config = McpClientConfig::new(McpTransportConfig::StreamableHttp(
        StreamableHttpTransportConfig::new(endpoint.clone()),
    ));
    if let Some(timeout) = limits.timeout_secs {
        config.connect_timeout = std::time::Duration::from_secs(u64::from(timeout).min(120));
        config.request_timeout = std::time::Duration::from_secs(
            u64::from(timeout)
                .min(tool_runtime_core::manifest_validation::MAX_RUNTIME_TIMEOUT_SECS as u64),
        );
    }
    Ok(PublicMcpBinding {
        endpoint,
        alias,
        config,
    })
}
