//! Provider-free catalog migration validation.
//! `make cargo ARGS="run -p magicllm --example inspect_model_catalog -- <config>"`

use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("pass a magician-config.yaml path")?;
    let path = Path::new(&path);
    let mut text = std::fs::read_to_string(path)?;
    let initial: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let initial_router = &initial["llm"]["router"];
    if initial_router.get("catalog").is_none()
        && initial_router.get("profiles").is_none()
        && initial_router.get("operation_mapping").is_none()
    {
        let sibling = path
            .parent()
            .unwrap_or(Path::new("."))
            .join("llm-router.yaml");
        let tables = std::fs::read_to_string(sibling)?;
        if !text.contains("  router:\n") {
            return Err("config has no llm.router key".into());
        }
        text = text.replacen("  router:\n", &format!("  router:\n{tables}\n"), 1);
    }
    let root: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let config: magicllm::LLMRouterConfig = serde_yaml::from_value(root["llm"]["router"].clone())?;
    if let Some(output) = std::env::args().nth(2) {
        let dump = serde_json::json!({"profiles": config.profiles, "operation_mapping": config.operation_mapping});
        std::fs::write(output, serde_json::to_vec(&dump)?)?;
    }
    let summary = serde_json::json!({
        "connections": config.catalog.connections.len(),
        "models": config.catalog.models.len(),
        "selectable_models": config.catalog.choices().len(),
        "assignments": config.catalog.assignments.len(),
        "legacy_aliases": 0,
        "resolved_routes": config.profiles.len(),
        "operations": config.operation_mapping.len()
    });
    magicllm::MultiLLMRouter::new(config)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
