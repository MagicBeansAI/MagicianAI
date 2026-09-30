//! Owner-granted secret use by app tools (`app_secret_use_v1`).
//!
//! A reviewed OS-jail skill may declare static secrets it needs (for example
//! an API key) in its runtime contract. An app that locks such a tool can use
//! the secret only when the owner ticks that exact (tool, secret) pair at
//! install. There is no default: an approval that says nothing grants nothing.
//!
//! What a tool may request is deliberately narrow:
//! - `auth.kind: secrets` with `required` or `optional` bindings;
//! - a copied single-file tool: every injection is an environment variable,
//!   and the tool declares its egress destinations (`app_egress`), so a key
//!   can only ever travel to those hosts through the call's broker;
//! - an in-place tool (`app_in_place_skill_v1`) may also take a key as a
//!   config file (`config_directory`, the `MMX_CONFIG_DIR` format MagicRun
//!   materializes), and may declare no host: its key then reaches the hosts
//!   the owner grants the app, or any website under the explicit "any public
//!   host" grant; the review says so for each key;
//! - `provider` only as a label; no profile, lifecycle or identity machinery.
//!
//! Anything else keeps the tool out of apps rather than half-supported.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tool_runtime_core::{
    manifest::{AuthContract, AuthKind, AuthRequirement, InjectionTarget, MMX_CONFIG_DIR},
    manifest_parser::parse_skill_runtime_package,
};

use super::{models::AppReference, os_jail_egress::parse_app_egress_declaration};

/// Most secret-use requests one package may surface for review.
pub const MAX_APP_SECRET_USE_REQUESTS: usize = 32;
const MAX_SECRET_REF_BYTES: usize = 128;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppSecretAccessError {
    #[error("the tool's secret contract is not supported in apps: {0}")]
    Unsupported(&'static str),
    #[error("the tool's runtime contract could not be read")]
    InvalidSource,
    #[error("the secret-use grant is not a subset of the reviewed request")]
    GrantExceedsRequest,
    #[error("the secret-use grant is malformed")]
    InvalidGrant,
}

/// One secret a locked tool asks to use, shown to the owner at review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AppSecretUseRequest {
    /// `capability:<skill>`
    pub tool: AppReference,
    /// The vault reference, e.g. `TAVILY_API_KEY`.
    pub secret_ref: String,
    /// The tool refuses to run without it.
    pub required: bool,
    /// The only host the secret can reach when the tool declares exactly
    /// one; empty otherwise.
    pub destination: String,
    /// Every declared host, when the tool declares more than one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<String>,
    /// The tool declares no host: the key reaches only the host(s) the owner
    /// picks for it from the app's granted hosts, or any site only by the
    /// owner's per-key opt-in under the app's "any public host" grant.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub app_granted_hosts: bool,
    /// `config_file` when the key reaches the tool as a file in a private
    /// config directory rather than as an environment variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    /// Why the owner cannot grant this key in this app, when they cannot: a
    /// tool that declares arbitrary sites (`*`) needs the app to ask for "any
    /// public host", and a tool that declares no host needs either a named
    /// app host to pick or that request (for the per-key "any site" opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_grantable: Option<String>,
}

/// One (tool, secret) pair the owner granted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AppSecretUseGrant {
    pub tool: AppReference,
    pub secret_ref: String,
    /// For a tool that declares no host: the hosts, picked by the owner from
    /// the app's named hosts, that this key is scoped to (the vault grant's
    /// domain set) and the only hosts the tool's broker admits while the key
    /// is injected. Empty for a tool that declares its hosts (its key
    /// reaches only declared ∩ granted hosts). Skipped when empty, so older
    /// grants keep their digests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// The owner's explicit "allow this key to be sent to any site" for an
    /// any-host tool (one that declares `*`, or declares none and is picked
    /// for any site). Off by default; skipped when off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub any_site: bool,
}

impl AppSecretUseRequest {
    pub fn grant(&self) -> AppSecretUseGrant {
        AppSecretUseGrant {
            tool: self.tool.clone(),
            secret_ref: self.secret_ref.clone(),
            hosts: Vec::new(),
            any_site: false,
        }
    }

    /// The tool declares `destinations: ["*"]`: it contacts arbitrary sites.
    pub fn declares_any_site(&self) -> bool {
        self.destination == super::os_jail_egress::APP_OS_JAIL_ANY_DECLARED_HOST
    }

    /// The key needs an owner-chosen scope (picked hosts or "any site"): the
    /// tool declares no host, or declares arbitrary sites.
    pub fn needs_scope(&self) -> bool {
        self.app_granted_hosts || self.declares_any_site()
    }
}

impl AppSecretUseGrant {
    /// No picked host and no "any site".
    pub fn is_unscoped(&self) -> bool {
        self.hosts.is_empty() && !self.any_site
    }
}

/// Whether `grant` is an unscoped grant of a key that needs a scope: what
/// grants made before per-key scopes stored for a tool that declares no
/// host. It grants nothing (the call never receives the key) until the owner
/// picks a scope again.
fn is_unscoped_legacy_grant(requests: &[AppSecretUseRequest], grant: &AppSecretUseGrant) -> bool {
    grant.is_unscoped()
        && requests.iter().any(|request| {
            request.tool == grant.tool
                && request.secret_ref == grant.secret_ref
                && request.needs_scope()
        })
}

/// Drop unscoped grants of keys that need a scope: an owner's review choice
/// carrying one (e.g. re-submitting a grant made before per-key scopes)
/// leaves that key ungranted instead of failing the whole review.
pub fn without_unscoped_legacy_grants(
    requests: &[AppSecretUseRequest],
    grants: &[AppSecretUseGrant],
) -> Vec<AppSecretUseGrant> {
    grants
        .iter()
        .filter(|grant| !is_unscoped_legacy_grant(requests, grant))
        .cloned()
        .collect()
}

/// [`validate_secret_use_grant`] for a stored grant revision. A grant made
/// before per-key scopes may hold an unscoped key of a tool that declares no
/// host; such a key must still name a reviewed request and stay unique, but
/// it does not fail the whole revision: it grants nothing (the call treats
/// the key as not granted) until the owner re-reviews.
pub fn validate_stored_secret_use_grant(
    requests: &[AppSecretUseRequest],
    grants: &[AppSecretUseGrant],
    app_hosts: &[String],
    app_requests_any_host: bool,
) -> Result<Vec<AppSecretUseGrant>, AppSecretAccessError> {
    if grants.len() > MAX_APP_SECRET_USE_REQUESTS {
        return Err(AppSecretAccessError::InvalidGrant);
    }
    let (legacy, scoped): (Vec<_>, Vec<_>) = grants
        .iter()
        .cloned()
        .partition(|grant| is_unscoped_legacy_grant(requests, grant));
    let mut canonical =
        validate_secret_use_grant(requests, &scoped, app_hosts, app_requests_any_host)?;
    for grant in legacy {
        if canonical
            .iter()
            .any(|kept| kept.tool == grant.tool && kept.secret_ref == grant.secret_ref)
        {
            return Err(AppSecretAccessError::InvalidGrant);
        }
        canonical.push(grant);
    }
    canonical.sort();
    canonical.dedup();
    Ok(canonical)
}

/// Mark the requests the owner cannot grant in this app: a key of a tool
/// that declares arbitrary sites (`*`) when the app does not ask for "any
/// public host", and a key of a tool that declares no host when the app
/// names no host to pick and does not ask for "any public host" either.
pub fn mark_ungrantable_secret_uses(
    requests: &mut [AppSecretUseRequest],
    app_hosts: &[String],
    app_requests_any_host: bool,
) {
    for request in requests.iter_mut() {
        if request.declares_any_site() && !app_requests_any_host {
            request.not_grantable = Some(
                "this tool contacts arbitrary sites and the app does not ask for \"any public \
                 host\", so its key cannot be sent anywhere"
                    .to_owned(),
            );
        } else if request.app_granted_hosts && app_hosts.is_empty() && !app_requests_any_host {
            request.not_grantable = Some(
                "this tool declares no host and the app names none to pick; the key cannot be \
                 sent anywhere"
                    .to_owned(),
            );
        }
    }
}

/// The secret-use requests of one tool, read from its reviewed source. A
/// tool without a secrets contract yields none; a secrets contract outside the
/// supported shape is refused.
pub fn app_secret_use_requests(
    tool: &AppReference,
    source: &str,
) -> Result<Vec<AppSecretUseRequest>, AppSecretAccessError> {
    let Some(package) =
        parse_skill_runtime_package(source).map_err(|_| AppSecretAccessError::InvalidSource)?
    else {
        return Ok(Vec::new());
    };
    let auth = &package.contract.auth;
    if auth.kind == AuthKind::None {
        return Ok(Vec::new());
    }
    let declared = parse_app_egress_declaration(source)
        .map_err(|_| AppSecretAccessError::InvalidSource)?;
    // The single-file shape needs a declared host; the in-place shape may
    // leave the hosts to the owner's grant.
    let required = match (&declared, supported_secret_requirement(auth)) {
        (Some(_), Ok(required)) => required,
        _ => supported_in_place_secret_requirement(auth)?,
    };
    let (destination, destinations) = match &declared {
        Some(declared) if declared.destinations().len() == 1 => {
            (declared.destination().to_owned(), Vec::new())
        },
        Some(declared) => (String::new(), declared.destinations().to_vec()),
        None => (String::new(), Vec::new()),
    };
    let mut requests = Vec::with_capacity(auth.secret_bindings.len());
    for binding in &auth.secret_bindings {
        if !is_secret_ref(&binding.secret_ref) {
            return Err(AppSecretAccessError::Unsupported(
                "invalid secret reference",
            ));
        }
        let config_file = auth.injections.iter().any(|injection| {
            matches!(&injection.source, tool_runtime_core::manifest::InjectionSource::Secret { binding: name } if *name == binding.name)
                && matches!(injection.target, InjectionTarget::ConfigDirectory { .. })
        });
        requests.push(AppSecretUseRequest {
            tool: tool.clone(),
            secret_ref: binding.secret_ref.clone(),
            required,
            destination: destination.clone(),
            destinations: destinations.clone(),
            app_granted_hosts: declared.is_none(),
            delivery: config_file.then(|| "config_file".to_owned()),
            not_grantable: None,
        });
    }
    requests.sort();
    requests.dedup();
    Ok(requests)
}

/// Whether each declared secret is required, for the supported contract
/// shape only.
pub fn supported_secret_requirement(auth: &AuthContract) -> Result<bool, AppSecretAccessError> {
    if auth.kind != AuthKind::Secrets {
        return Err(AppSecretAccessError::Unsupported("only static secrets"));
    }
    let required = match auth.requirement {
        AuthRequirement::Required => true,
        AuthRequirement::Optional => false,
        _ => {
            return Err(AppSecretAccessError::Unsupported(
                "only required or optional secrets",
            ))
        },
    };
    let defaults = AuthContract::default();
    if auth.secret_bindings.is_empty()
        || auth.provider.is_some()
        || auth.profile_selection != defaults.profile_selection
        || auth.storage != defaults.storage
        || auth.lifecycle != defaults.lifecycle
        || auth.identity != defaults.identity
    {
        return Err(AppSecretAccessError::Unsupported(
            "no provider, profile, storage, lifecycle or identity",
        ));
    }
    if auth
        .injections
        .iter()
        .any(|injection| !matches!(injection.target, InjectionTarget::Environment { .. }))
    {
        return Err(AppSecretAccessError::Unsupported(
            "secrets reach the tool only as environment variables",
        ));
    }
    Ok(required)
}

/// The secret shape an in-place tool may use: the single-file shape, plus a
/// `provider` label (no profile selection) and config-file delivery in the
/// one format MagicRun materializes (`MMX_CONFIG_DIR/config.json`).
pub fn supported_in_place_secret_requirement(
    auth: &AuthContract,
) -> Result<bool, AppSecretAccessError> {
    if auth.kind != AuthKind::Secrets {
        return Err(AppSecretAccessError::Unsupported("only static secrets"));
    }
    let required = match auth.requirement {
        AuthRequirement::Required => true,
        AuthRequirement::Optional => false,
        _ => {
            return Err(AppSecretAccessError::Unsupported(
                "only required or optional secrets",
            ))
        },
    };
    let defaults = AuthContract::default();
    if auth.secret_bindings.is_empty()
        || auth.profile_selection != defaults.profile_selection
        || auth.storage != defaults.storage
        || auth.lifecycle != defaults.lifecycle
        || auth.identity != defaults.identity
    {
        return Err(AppSecretAccessError::Unsupported(
            "no profile, storage, lifecycle or identity",
        ));
    }
    if auth.injections.iter().any(|injection| match &injection.target {
        InjectionTarget::Environment { .. } => false,
        InjectionTarget::ConfigDirectory { name } => name != MMX_CONFIG_DIR,
        _ => true,
    }) {
        return Err(AppSecretAccessError::Unsupported(
            "secrets reach an in-place tool only as environment variables or a MMX_CONFIG_DIR \
             config file",
        ));
    }
    Ok(required)
}

/// Canonical form of an owner's grant: sorted, unique, and every pair inside
/// the reviewed requests.
/// `app_hosts` are the hosts the app names (its network policy's
/// `destination:<host>` entries): the only hosts a key of a tool that
/// declares none may be picked for. A tool with declared hosts takes no pick;
/// a tool declaring `*` needs `any_site`; `any_site` needs an app that asks
/// for "any public host".
pub fn validate_secret_use_grant(
    requests: &[AppSecretUseRequest],
    grants: &[AppSecretUseGrant],
    app_hosts: &[String],
    app_requests_any_host: bool,
) -> Result<Vec<AppSecretUseGrant>, AppSecretAccessError> {
    if grants.len() > MAX_APP_SECRET_USE_REQUESTS {
        return Err(AppSecretAccessError::InvalidGrant);
    }
    let mut canonical = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for grant in grants {
        let request = requests
            .iter()
            .find(|request| request.tool == grant.tool && request.secret_ref == grant.secret_ref)
            .ok_or(AppSecretAccessError::GrantExceedsRequest)?;
        if request.not_grantable.is_some() {
            return Err(AppSecretAccessError::GrantExceedsRequest);
        }
        let picked_ok = !grant.hosts.is_empty()
            && grant.hosts.len() <= super::os_jail_egress::MAX_APP_OS_JAIL_EGRESS_ADMITTED_HOSTS
            && grant.hosts.windows(2).all(|pair| pair[0] < pair[1])
            && grant
                .hosts
                .iter()
                .all(|host| app_hosts.iter().any(|app_host| app_host == host));
        let valid = if request.declares_any_site() {
            grant.any_site && grant.hosts.is_empty() && app_requests_any_host
        } else if request.app_granted_hosts {
            if grant.any_site {
                grant.hosts.is_empty() && app_requests_any_host
            } else {
                picked_ok
            }
        } else {
            !grant.any_site && grant.hosts.is_empty()
        };
        if !valid {
            return Err(AppSecretAccessError::InvalidGrant);
        }
        if !seen.insert((grant.tool.clone(), grant.secret_ref.clone())) {
            return Err(AppSecretAccessError::InvalidGrant);
        }
        canonical.insert(grant.clone());
    }
    Ok(canonical.into_iter().collect())
}

/// The `destination:<host>` hosts of an app network policy.
pub fn network_policy_hosts(policy: &super::records::AppNetworkPolicy) -> Vec<String> {
    match policy {
        super::records::AppNetworkPolicy::ApprovedDestinations { destinations } => destinations
            .iter()
            .filter_map(|destination| destination.as_str().strip_prefix("destination:"))
            .map(str::to_owned)
            .collect(),
        super::records::AppNetworkPolicy::Denied => Vec::new(),
    }
}

fn is_secret_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SECRET_REF_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.as_bytes()[0].is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> AppReference {
        AppReference::parse("capability:news-search-via-tavily").unwrap()
    }

    fn skill(auth: &str, egress: bool) -> String {
        let egress = if egress {
            "    app_egress:\n      schema_version: 1\n      destination: api.tavily.com\n"
        } else {
            ""
        };
        format!(
            "---\nname: news-search-via-tavily\ndescription: d\nmetadata:\n  magician:\n{egress}    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {{bins: [tavily-search]}}\n      runtime:\n        protocol: cli\n        command_prefix: []\n{auth}---\n"
        )
    }

    const REQUIRED: &str = "      auth:\n        kind: secrets\n        requirement: required\n        secret_bindings:\n          - name: tavily_api_key\n            secret_ref: TAVILY_API_KEY\n        injections:\n          - source: {kind: secret, binding: tavily_api_key}\n            target: {kind: environment, name: TAVILY_API_KEY}\n";

    #[test]
    fn a_required_environment_secret_with_egress_is_requested() {
        let requests = app_secret_use_requests(&tool(), &skill(REQUIRED, true)).unwrap();
        assert_eq!(
            requests,
            [AppSecretUseRequest {
                tool: tool(),
                secret_ref: "TAVILY_API_KEY".into(),
                required: true,
                destination: "api.tavily.com".into(),
                destinations: Vec::new(),
                app_granted_hosts: false,
                delivery: None,
                not_grantable: None,
            }]
        );
        assert!(app_secret_use_requests(&tool(), &skill("", true))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_undeclared_tool_keys_reach_only_the_hosts_the_app_is_granted() {
        let requests = app_secret_use_requests(&tool(), &skill(REQUIRED, false)).unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].app_granted_hosts);
        assert!(requests[0].destination.is_empty());
        // A provider label is admitted for in-place tools.
        let provider = REQUIRED.replace(
            "requirement: required\n",
            "requirement: required\n        provider: tavily\n",
        );
        assert_eq!(
            app_secret_use_requests(&tool(), &skill(&provider, true)).unwrap()[0].destination,
            "api.tavily.com"
        );
    }

    #[test]
    fn an_undeclared_tools_key_is_scoped_to_picked_hosts_or_any_site() {
        let requests = app_secret_use_requests(&tool(), &skill(REQUIRED, false)).unwrap();
        let hosts = vec!["api.tavily.com".to_owned(), "collector.evil.com".to_owned()];
        let picked = |hosts: &[&str], any_site: bool| AppSecretUseGrant {
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            any_site,
            ..requests[0].grant()
        };
        // One or more of the app's named hosts.
        assert_eq!(
            validate_secret_use_grant(&requests, &[picked(&["api.tavily.com"], false)], &hosts, false)
                .unwrap()[0]
                .hosts,
            ["api.tavily.com"]
        );
        validate_secret_use_grant(
            &requests,
            &[picked(&["api.tavily.com", "collector.evil.com"], false)],
            &hosts,
            false,
        )
        .unwrap();
        for bad in [
            picked(&[], false),
            picked(&["other.example.com"], false),
            picked(&["collector.evil.com", "api.tavily.com"], false),
            picked(&["api.tavily.com"], true),
        ] {
            assert!(validate_secret_use_grant(&requests, &[bad.clone()], &hosts, true).is_err(), "{bad:?}");
        }
        // One grant per key.
        assert!(validate_secret_use_grant(
            &requests,
            &[picked(&["api.tavily.com"], false), picked(&["collector.evil.com"], false)],
            &hosts,
            false,
        )
        .is_err());
        // "Any site" needs the explicit tick and an app asking for any host.
        assert!(validate_secret_use_grant(&requests, &[picked(&[], true)], &hosts, false).is_err());
        validate_secret_use_grant(&requests, &[picked(&[], true)], &hosts, true).unwrap();
        // A declared tool's key carries no pick and no any-site.
        let declared = app_secret_use_requests(&tool(), &skill(REQUIRED, true)).unwrap();
        let mut with_host = declared[0].grant();
        with_host.hosts = vec!["api.tavily.com".into()];
        assert!(validate_secret_use_grant(&declared, &[with_host], &hosts, true).is_err());
        let mut declared_any = declared[0].grant();
        declared_any.any_site = true;
        assert!(validate_secret_use_grant(&declared, &[declared_any], &hosts, true).is_err());
        // An app that names no host and does not ask for any host cannot
        // grant an undeclared tool's key.
        let mut ungrantable = requests.clone();
        mark_ungrantable_secret_uses(&mut ungrantable, &[], false);
        assert!(ungrantable[0].not_grantable.is_some());
        assert!(
            validate_secret_use_grant(&ungrantable, &[picked(&[], true)], &[], true).is_err()
        );
    }

    /// A grant stored before per-key scopes holds an unscoped key of a tool
    /// that declares no host. Loading keeps the revision valid (the key
    /// grants nothing until re-reviewed); re-submitting it leaves that key
    /// ungranted; anything else stays strict.
    #[test]
    fn an_unscoped_key_from_an_older_grant_loads_but_grants_nothing() {
        let requests = app_secret_use_requests(&tool(), &skill(REQUIRED, false)).unwrap();
        assert!(requests[0].needs_scope());
        let hosts = vec!["api.tavily.com".to_owned()];
        let legacy = requests[0].grant();
        assert!(validate_secret_use_grant(&requests, &[legacy.clone()], &hosts, false).is_err());
        assert_eq!(
            validate_stored_secret_use_grant(&requests, &[legacy.clone()], &hosts, false).unwrap(),
            [legacy.clone()]
        );
        assert!(without_unscoped_legacy_grants(&requests, &[legacy.clone()]).is_empty());
        // Still a reviewed pair, once.
        let other = AppSecretUseGrant {
            secret_ref: "OTHER_KEY".to_owned(),
            ..legacy.clone()
        };
        assert!(validate_stored_secret_use_grant(&requests, &[other], &hosts, false).is_err());
        let scoped = AppSecretUseGrant {
            hosts: hosts.clone(),
            ..legacy.clone()
        };
        let mut both = vec![legacy.clone(), scoped.clone()];
        both.sort();
        assert!(validate_stored_secret_use_grant(&requests, &both, &hosts, false).is_err());
        // A scoped key validates as before; a bad scope still fails.
        assert_eq!(
            validate_stored_secret_use_grant(&requests, &[scoped.clone()], &hosts, false).unwrap(),
            [scoped]
        );
        let foreign = AppSecretUseGrant {
            hosts: vec!["other.example.com".to_owned()],
            ..legacy
        };
        assert!(validate_stored_secret_use_grant(&requests, &[foreign], &hosts, false).is_err());
        // A declared tool's plain grant is not a legacy scope.
        let declared = app_secret_use_requests(&tool(), &skill(REQUIRED, true)).unwrap();
        assert!(!declared[0].needs_scope());
        assert_eq!(
            without_unscoped_legacy_grants(&declared, &[declared[0].grant()]),
            [declared[0].grant()]
        );
    }

    #[test]
    fn a_star_tools_key_needs_the_explicit_any_site_tick() {
        let star = skill(REQUIRED, false).replace(
            "  magician:\n",
            "  magician:\n    app_egress:\n      schema_version: 1\n      destinations: [\"*\"]\n",
        );
        let requests = app_secret_use_requests(&tool(), &star).unwrap();
        assert!(requests[0].declares_any_site());
        let mut grant = requests[0].grant();
        assert!(validate_secret_use_grant(&requests, &[grant.clone()], &[], true).is_err());
        grant.any_site = true;
        validate_secret_use_grant(&requests, &[grant.clone()], &[], true).unwrap();
        assert!(validate_secret_use_grant(&requests, &[grant], &[], false).is_err());
        let mut marked = requests.clone();
        mark_ungrantable_secret_uses(&mut marked, &["a.example.com".into()], false);
        assert!(marked[0].not_grantable.is_some());
    }

    #[test]
    fn the_minimax_skill_takes_its_key_as_a_config_file() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../skillshub/web-search-via-minimax/SKILL.md"
        ))
        .unwrap();
        let tool = AppReference::parse("capability:web-search-via-minimax").unwrap();
        let requests = app_secret_use_requests(&tool, &source).unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].secret_ref, "MINIMAX_API_KEY");
        assert_eq!(requests[0].delivery.as_deref(), Some("config_file"));
        // It declares MiniMax's API host, so its key goes only there.
        assert!(!requests[0].app_granted_hosts);
        assert_eq!(requests[0].destination, "api.minimax.io");
        assert!(requests[0].required);
    }

    #[test]
    fn unsupported_secret_shapes_are_refused() {
        let config_dir = REQUIRED.replace(
            "{kind: environment, name: TAVILY_API_KEY}",
            "{kind: config_directory, name: TAVILY_DIR}",
        );
        assert!(matches!(
            app_secret_use_requests(&tool(), &skill(&config_dir, true)),
            Err(AppSecretAccessError::Unsupported(_))
        ));
        let alternatives = REQUIRED.replace("requirement: required", "requirement: at_least_one");
        assert!(matches!(
            app_secret_use_requests(&tool(), &skill(&alternatives, true)),
            Err(AppSecretAccessError::Unsupported(_))
        ));
        let stdin = REQUIRED.replace(
            "{kind: environment, name: TAVILY_API_KEY}",
            "{kind: stdin}",
        );
        assert!(app_secret_use_requests(&tool(), &skill(&stdin, false)).is_err());
    }

    #[test]
    fn a_grant_must_be_a_unique_subset_of_the_request() {
        let requests = app_secret_use_requests(&tool(), &skill(REQUIRED, true)).unwrap();
        let grant = requests[0].grant();
        assert_eq!(
            validate_secret_use_grant(&requests, &[grant.clone()], &[], false).unwrap(),
            [grant.clone()]
        );
        assert!(validate_secret_use_grant(&requests, &[], &[], false)
            .unwrap()
            .is_empty());
        assert_eq!(
            validate_secret_use_grant(&requests, &[grant.clone(), grant.clone()], &[], false),
            Err(AppSecretAccessError::InvalidGrant)
        );
        let other = AppSecretUseGrant {
            tool: tool(),
            secret_ref: "OPENAI_API_KEY".into(),
            hosts: Vec::new(),
            any_site: false,
        };
        assert_eq!(
            validate_secret_use_grant(&requests, &[other], &[], false),
            Err(AppSecretAccessError::GrantExceedsRequest)
        );
    }

    #[test]
    fn the_reviewed_api_key_skills_request_their_keys() {
        for (skill, secret, host) in [
            ("news-search-via-tavily", "TAVILY_API_KEY", "api.tavily.com"),
            ("semantic-websearch-via-exa", "EXA_API_KEY", "api.exa.ai"),
            ("gif-search-via-klipy", "KLIPY_API_KEY", "api.klipy.com"),
        ] {
            let path = format!(
                "{}/../skillshub/{skill}/SKILL.md",
                env!("CARGO_MANIFEST_DIR")
            );
            let source = std::fs::read_to_string(path).unwrap();
            let tool = AppReference::parse(format!("capability:{skill}")).unwrap();
            let requests = app_secret_use_requests(&tool, &source).unwrap();
            assert_eq!(requests.len(), 1, "{skill}");
            assert_eq!(requests[0].secret_ref, secret);
            assert_eq!(requests[0].destination, host);
            assert!(requests[0].required);
        }
    }
}
