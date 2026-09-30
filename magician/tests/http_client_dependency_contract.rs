const MAGICIAN_MANIFEST: &str = include_str!("../Cargo.toml");
const MAGICLLM_MANIFEST: &str = include_str!("../../magicllm/Cargo.toml");
const VECTOR_INDEX_MANIFEST: &str = include_str!("../../magician-vector-index/Cargo.toml");
const MAGIC_SUPERVISOR_MANIFEST: &str = include_str!("../../magic-supervisor/Cargo.toml");

fn reqwest_dependency(manifest: &str) -> &str {
    manifest
        .lines()
        .find(|line| line.trim_start().starts_with("reqwest = "))
        .expect("workspace HTTP client crate must declare reqwest")
}

#[test]
fn workspace_http_clients_keep_the_headless_safe_proxy_stack() {
    // reqwest 0.11 automatically reached system-configuration 0.5.1 here.
    // That release wrapped a null SCDynamicStore pointer and panicked before
    // callers could handle ClientBuilder::build as a Result.
    for (crate_name, manifest) in [
        ("magician", MAGICIAN_MANIFEST),
        ("magicllm", MAGICLLM_MANIFEST),
        ("magician-vector-index", VECTOR_INDEX_MANIFEST),
        ("magic-supervisor", MAGIC_SUPERVISOR_MANIFEST),
    ] {
        let dependency = reqwest_dependency(manifest);
        assert!(
            dependency.contains("version = \"0.12.28\""),
            "{crate_name} must require reqwest 0.12.28 or update this safety contract after auditing its proxy stack: {dependency}"
        );
        assert!(
            dependency.contains("\"system-proxy\""),
            "{crate_name} must preserve environment and platform proxy discovery: {dependency}"
        );
    }
}
