//! Exercise the product's anonymous binding against the real runtime types,
//! without compiling the entire application or making purchases/network calls.
// Only part of this production file is exercised here; the rest is used by
// the runtime, not dead.
#[allow(dead_code)]
#[path = "../../magician/src/magician_v2/execution/primitive_dispatch/mcp_commerce.rs"]
mod mcp_commerce;
#[path = "../../magician/src/magician_v2/mcp_public_profile.rs"]
mod mcp_public_profile;
#[path = "../../magician/src/magician_v2/execution/primitive_dispatch/public_mcp.rs"]
mod public_mcp;

use tool_runtime_core::{
    manifest::{McpTransport, RuntimeProtocol},
    manifest_parser::{parse_skill_runtime_package, SkillRuntimePackage},
    manifest_validation::validate_skill_runtime_contract,
    mcp_catalog_policy::{McpCatalogPolicyContract, McpToolPublicationDecision},
};

fn package(source: &str) -> SkillRuntimePackage {
    parse_skill_runtime_package(source)
        .expect("parse skill")
        .expect("runtime package")
}

const SHOPIFY: &str = include_str!("../../skillshub/shopify-mcp/SKILL.md");
const ZOMATO: &str = include_str!("../../skillshub/zomato-mcp/SKILL.md");
const SWIGGY: &str = include_str!("../../skillshub/swiggy-mcp/SKILL.md");

fn shopify_profile() -> mcp_public_profile::McpPublicProfile {
    #[derive(serde::Deserialize)]
    struct Bindings {
        public_profile: mcp_public_profile::McpPublicProfile,
    }
    tool_runtime_core::manifest_parser::parse_skill_magician_extension::<Bindings>(
        SHOPIFY,
        "mcp_bindings",
    )
    .unwrap()
    .unwrap()
    .public_profile
}

#[test]
fn profile_is_owned_by_each_installation_and_persists_across_workers() {
    let profile = shopify_profile();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let url = profile
        .publish(first.path(), "https://first.example.com")
        .unwrap();
    let second_url = profile
        .publish(second.path(), "https://second.example.com")
        .unwrap();
    assert_ne!(url, second_url);
    assert_eq!(
        url,
        profile
            .publish(first.path(), "https://first.example.com/")
            .unwrap()
    );
    let id = url
        .rsplit('/')
        .next()
        .unwrap()
        .strip_suffix(".json")
        .unwrap();
    let stored = mcp_public_profile::read_public_profile(first.path(), id).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stored).unwrap(),
        profile.document
    );
    assert_eq!(
        stored,
        mcp_public_profile::read_public_profile(second.path(), id).unwrap()
    );
    assert_eq!(
        profile.document["ucp"]["capabilities"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    assert!(profile.document["ucp"]["capabilities"]
        .get("dev.ucp.shopping.checkout")
        .is_none());
}

#[test]
fn profile_binding_preserves_metadata_and_refuses_other_identities() {
    let profile = shopify_profile();
    let url = "https://installation.example.com/profile.json";
    let mut arguments =
        serde_json::json!({"meta":{"request_id":"keep"},"catalog":{"query":"shirts"}});
    profile.bind(&mut arguments, url).unwrap();
    profile.bind(&mut arguments, url).unwrap();
    assert_eq!(arguments["meta"]["request_id"], "keep");
    assert_eq!(arguments["meta"]["ucp-agent"]["profile"], url);
    assert_eq!(arguments["catalog"]["query"], "shirts");
    arguments["meta"]["ucp-agent"]["profile"] =
        serde_json::json!("https://someone-else.example/profile.json");
    assert!(profile.bind(&mut arguments, url).is_err());
    for meta in [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!("invalid"),
    ] {
        assert!(profile
            .bind(&mut serde_json::json!({"meta": meta}), url)
            .is_err());
    }
}

#[test]
fn public_profiles_reject_missing_hosting_and_malformed_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let mut profile = shopify_profile();
    for origin in [
        "http://127.0.0.1:3002",
        "http://public.example",
        "https://localhost",
        "https://127.0.0.1",
        "https://localhost.",
        "https://[::ffff:127.0.0.1]",
        "https://100.64.0.1",
        "https://10.0.0.5",
        "https://[::1]",
        "https://[fc00::1]",
        "https://computer.local",
        "https://user:secret@public.example",
        "https://public.example/path",
        "https://public.example/?secret=value",
        "https://public.example/#fragment",
    ] {
        assert!(
            profile.publish(directory.path(), origin).is_err(),
            "{origin}"
        );
    }
    for pointer in ["", "/", "relative", "/meta//profile", "/meta/~2profile"] {
        profile.argument_pointer = pointer.into();
        assert!(profile.validate().is_err(), "{pointer}");
    }
    profile.argument_pointer = "/meta/ucp-agent/profile".into();
    profile.document =
        serde_json::json!({"large": "x".repeat(mcp_public_profile::MAX_PUBLIC_PROFILE_BYTES)});
    assert!(profile.validate().is_err());
    profile.document = serde_json::json!(["not a document"]);
    assert!(profile.validate().is_err());
}

#[test]
fn public_profile_store_rejects_traversal_and_modified_files() {
    let directory = tempfile::tempdir().unwrap();
    let profile = shopify_profile();
    let url = profile
        .publish(directory.path(), "https://installation.example.com")
        .unwrap();
    let id = url
        .rsplit('/')
        .next()
        .unwrap()
        .strip_suffix(".json")
        .unwrap();
    for invalid in ["../magician-config", "", "abc", "../.env", &"f".repeat(65)] {
        assert!(mcp_public_profile::read_public_profile(directory.path(), invalid).is_err());
    }
    std::fs::write(
        directory
            .path()
            .join(format!("system/mcp-public-profiles/{id}.json")),
        b"{\"modified\":true}",
    )
    .unwrap();
    assert!(mcp_public_profile::read_public_profile(directory.path(), id).is_err());
    assert!(profile
        .publish(directory.path(), "https://installation.example.com")
        .is_err());
}

#[cfg(unix)]
#[test]
fn public_profile_store_never_follows_symlink_files_or_directories() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let profile = shopify_profile();
    let url = profile
        .publish(other.path(), "https://installation.example.com")
        .unwrap();
    let id = url
        .rsplit('/')
        .next()
        .unwrap()
        .strip_suffix(".json")
        .unwrap();
    std::fs::create_dir_all(directory.path().join("system/mcp-public-profiles")).unwrap();
    symlink(
        other
            .path()
            .join(format!("system/mcp-public-profiles/{id}.json")),
        directory
            .path()
            .join(format!("system/mcp-public-profiles/{id}.json")),
    )
    .unwrap();
    assert!(mcp_public_profile::read_public_profile(directory.path(), id).is_err());
    assert!(profile
        .publish(directory.path(), "https://installation.example.com")
        .is_err());
    let linked = tempfile::tempdir().unwrap();
    symlink(other.path().join("system"), linked.path().join("system")).unwrap();
    assert!(profile
        .publish(linked.path(), "https://installation.example.com")
        .is_err());
}

#[tokio::test]
#[ignore = "read-only live Shopify MCP discovery; requires internet"]
async fn shopify_public_catalog_discovery() {
    let p = package(SHOPIFY);
    let binding = public_mcp::public_mcp_binding(&p.contract, None).unwrap();
    let mut client = magician_mcp_client::McpClient::connect(binding.config)
        .await
        .unwrap();
    let descriptors = client.discover_tools().await.unwrap();
    let validated = validate_skill_runtime_contract(&p.contract).unwrap();
    let policy = McpCatalogPolicyContract::compile("shopify-mcp", validated).unwrap();
    let catalog = magician_mcp_client::project_mcp_catalog(descriptors, &policy).unwrap();
    let mut names = catalog
        .tools()
        .iter()
        .map(|t| t.remote_id().remote_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["get_product", "lookup_catalog", "search_catalog"]);
    client.close().await.unwrap();
}

#[test]
fn public_binding_never_downgrades_oauth_and_binds_reviewed_endpoint() {
    let p = package(SHOPIFY);
    let b = public_mcp::public_mcp_binding(&p.contract, None).unwrap();
    assert_eq!(b.endpoint, "https://catalog.shopify.com/api/ucp/mcp");
    assert!(b.alias.is_none());
    let magician_mcp_client::McpTransportConfig::StreamableHttp(http) = b.config.transport else {
        panic!("expected remote transport");
    };
    assert!(http.bearer_token.is_none());
    assert!(
        public_mcp::public_mcp_binding(&p.contract, Some("https://attacker.example/mcp")).is_err()
    );
    for source in [ZOMATO, SWIGGY] {
        assert!(public_mcp::public_mcp_binding(&package(source).contract, None).is_err());
    }
}

#[test]
fn public_aliases_only_resolve_declared_endpoints() {
    let mut p = package(SHOPIFY);
    let RuntimeProtocol::Mcp {
        transport: McpTransport::StreamableHttp { endpoint },
        discovery,
        ..
    } = &mut p.contract.runtime
    else {
        panic!()
    };
    discovery
        .endpoint_aliases
        .insert("catalog".into(), endpoint.clone());
    discovery.default_endpoint_alias = Some("catalog".into());
    let b = public_mcp::public_mcp_binding(&p.contract, None).unwrap();
    assert_eq!(b.alias.as_deref(), Some("catalog"));
    assert_eq!(b.endpoint, "https://catalog.shopify.com/api/ucp/mcp");
    assert!(public_mcp::public_mcp_binding(&p.contract, Some("unreviewed")).is_err());
    let RuntimeProtocol::Mcp { discovery, .. } = &mut p.contract.runtime else {
        panic!()
    };
    discovery.default_endpoint_alias = None;
    assert!(public_mcp::public_mcp_binding(&p.contract, None).is_err());
}

#[test]
fn commerce_packages_validate_and_preserve_checkout_boundaries() {
    for (name, source) in [
        ("shopify-mcp", SHOPIFY),
        ("zomato-mcp", ZOMATO),
        ("swiggy-mcp", SWIGGY),
    ] {
        let p = package(source);
        let validated = validate_skill_runtime_contract(&p.contract).expect(name);
        let policy = McpCatalogPolicyContract::compile(name, validated).expect(name);
        if name == "shopify-mcp" {
            assert!(matches!(
                policy.decision("search_catalog").unwrap(),
                McpToolPublicationDecision::Publish(_)
            ));
            assert!(matches!(
                policy.decision("complete_checkout").unwrap(),
                McpToolPublicationDecision::Denied
            ));
        }
        if name == "swiggy-mcp" {
            for tool in ["place_event_order", "confirm_order", "check_payment_status"] {
                assert!(matches!(
                    policy.decision(tool).unwrap(),
                    McpToolPublicationDecision::Publish(_)
                ));
            }
            let McpToolPublicationDecision::Publish(order) =
                policy.decision("place_event_order").unwrap()
            else {
                panic!()
            };
            assert_eq!(
                order.risk(),
                tool_runtime_core::manifest::McpToolRiskClass::Commerce
            );
            let McpToolPublicationDecision::Publish(status) =
                policy.decision("check_payment_status").unwrap()
            else {
                panic!()
            };
            assert_eq!(
                status.risk(),
                tool_runtime_core::manifest::McpToolRiskClass::ExternalSideEffect
            );
            assert!(matches!(
                policy.decision("discover_events").unwrap(),
                McpToolPublicationDecision::Publish(_)
            ));
            let RuntimeProtocol::Mcp { discovery, .. } = &p.contract.runtime else {
                panic!()
            };
            assert_eq!(
                discovery.endpoint_aliases["scenes"],
                "https://mcp.swiggy.com/scenes"
            );
        }
    }
}

#[test]
fn exact_token_fingerprints_bind_ephemeral_quotes_without_exposing_credentials() {
    let a = magician_mcp_client::BearerToken::new("test-credential-one");
    let b = magician_mcp_client::BearerToken::new("test-credential-two");
    assert_eq!(
        a.session_fingerprint(),
        magician_mcp_client::BearerToken::new("test-credential-one").session_fingerprint()
    );
    assert_ne!(a.session_fingerprint(), b.session_fingerprint());
    assert_eq!(a.session_fingerprint().len(), 64);
    assert!(!format!("{a:?}").contains("test-credential-one"));
}
