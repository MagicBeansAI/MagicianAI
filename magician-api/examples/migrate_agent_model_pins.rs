//! One-time, offline alias retirement through the agent definition owner.
//! Kept with the API examples so owner and API checks share a dependency graph.
//! Inventories/backups come from `prepare_llm_profile_retirement.py`.
//! No execution snapshots, journals, approvals or templates are rewritten.

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use magician::config::load_magician_config_from_path;
use magician::magician_v2::agents::{AgentDefinition, AgentDefinitionStore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    inventory: PathBuf,
    #[arg(long)]
    backup_dir: PathBuf,
    /// Full matched catalog config, including generated coding choices.
    #[arg(long)]
    config: PathBuf,
    /// Writes require the supervised Magician service to be stopped first.
    #[arg(long)]
    apply: bool,
    #[arg(long, default_value_t = 3002)]
    api_port: u16,
}

struct Targets {
    routes: HashSet<String>,
    coding: HashSet<String>,
}

struct Prepared {
    store: AgentDefinitionStore,
    source: PathBuf,
    expected_hash: String,
    original: AgentDefinition,
    replacement: AgentDefinition,
    already_applied: bool,
    changes: Vec<Value>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checked_file(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "invalid relative record path"
    );
    let file = root.join(relative);
    ensure!(
        file.canonicalize()?.starts_with(root.canonicalize()?),
        "record escaped its root"
    );
    let mut cursor = root.to_path_buf();
    for part in relative.components() {
        cursor.push(part);
        ensure!(
            !std::fs::symlink_metadata(&cursor)?.file_type().is_symlink(),
            "symlinked migration input"
        );
    }
    ensure!(file.is_file(), "migration input is not a file");
    Ok(file)
}

fn rewrite(
    definition: &AgentDefinition,
    map: &Value,
    targets: &Targets,
) -> Result<(AgentDefinition, Vec<Value>)> {
    let mut result = definition.clone();
    let mut changes = Vec::new();
    let Some(routing) = result.llm_routing.as_mut() else {
        return Ok((result, changes));
    };
    for (lane, endpoint) in [
        ("planning", &mut routing.planning),
        ("evaluation", &mut routing.evaluation),
        ("correction_extraction", &mut routing.correction_extraction),
        ("memory_consolidation", &mut routing.memory_consolidation),
    ] {
        if let Some(profile) = endpoint
            .as_mut()
            .and_then(|endpoint| endpoint.profile.as_mut())
        {
            rewrite_route(profile, lane, map, targets, &mut changes)?;
        }
    }
    for (operation, endpoint) in &mut routing.operations {
        if let Some(profile) = endpoint.profile.as_mut() {
            rewrite_route(
                profile,
                &format!("operations.{operation}"),
                map,
                targets,
                &mut changes,
            )?;
        }
    }
    if let Some(profile) = routing.coding_profile.as_mut() {
        if let Some(choice) = map["coding"].get(profile.as_str()) {
            ensure!(
                choice["engine"] == "pi" && choice["client_resolvable"] == true,
                "coding selection needs explicit replacement"
            );
            let model = choice["model_selection"]["model_id"]
                .as_str()
                .context("missing coding model")?;
            let thinking = choice["model_selection"]["thinking"]
                .as_str()
                .context("missing coding thinking")?;
            let target = format!("pi/model/{model}/{thinking}");
            ensure!(
                targets.coding.contains(&target),
                "coding target is not in the matched catalog"
            );
            changes.push(json!({"field":"coding_profile", "from":profile, "to":target}));
            *profile = target;
        } else {
            ensure!(
                targets.coding.contains(profile),
                "unknown coding pin; refresh the migration inventory"
            );
        }
    }
    Ok((result, changes))
}

fn rewrite_route(
    profile: &mut String,
    field: &str,
    map: &Value,
    targets: &Targets,
    changes: &mut Vec<Value>,
) -> Result<()> {
    let target = map["routes"]
        .get(profile.as_str())
        .and_then(|choice| choice["canonical_target"].as_str())
        .unwrap_or(profile.as_str())
        .to_owned();
    ensure!(
        targets.routes.contains(&target),
        "unknown assignment pin; refresh the migration inventory"
    );
    if target != *profile {
        changes.push(json!({"field":field,"from":profile,"to":target}));
        *profile = target;
    }
    Ok(())
}

fn preflight(inventory: &Value, backup: &Path, targets: &Targets) -> Result<Vec<Prepared>> {
    ensure!(
        inventory["mode"] == "read_only_preparation",
        "not a retirement inventory"
    );
    let map = &inventory["selection_migration"];
    ensure!(
        map["schema_version"] == 1 && map["runtime_alias_resolver"] == false,
        "unsupported migration map"
    );
    let root = PathBuf::from(
        inventory["runtime"]["root"]
            .as_str()
            .context("runtime root missing")?,
    );
    ensure!(
        inventory["runtime"]["errors"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "refresh the inventory and resolve record read errors before migration"
    );
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for record in inventory["runtime"]["records_with_legacy_references"]
        .as_array()
        .context("runtime records missing")?
    {
        let owner = record["owner"]
            .as_str()
            .context("refresh the inventory to classify record ownership")?;
        if owner != "agent_definition_store" {
            continue;
        }
        let relative = PathBuf::from(record["path"].as_str().context("record path missing")?);
        let parts = relative
            .components()
            .map(|part| part.as_os_str())
            .collect::<Vec<_>>();
        ensure!(
            parts.len() == 7
                && parts[0] == "scopes"
                && parts[3] == "agent_runtime"
                && parts[4] == "agents"
                && parts[6] == "definition.agent.yaml",
            "record is not a current agent definition"
        );
        let source = checked_file(&root, &relative)?;
        ensure!(
            seen.insert(source.clone()),
            "duplicate agent record in migration inventory"
        );
        let snapshot = checked_file(backup, &Path::new("runtime").join(&relative))?;
        let expected_hash = record["sha256"]
            .as_str()
            .context("source hash missing")?
            .to_owned();
        let bytes = std::fs::read(snapshot)?;
        ensure!(
            hash(&bytes) == expected_hash,
            "backup does not match the inventoried definition"
        );
        let original: AgentDefinition = serde_yaml::from_slice(&bytes)?;
        original
            .version
            .checked_add(1)
            .context("version overflow")?;
        ensure!(
            Path::new(&original.agent_id).as_os_str() == parts[5],
            "definition identity mismatch"
        );
        let (replacement, changes) = rewrite(&original, map, targets)?;
        ensure!(
            !changes.is_empty(),
            "inventoried definition has no eligible pin changes"
        );
        let current = std::fs::read(&source)?;
        let already_applied = if hash(&current) == expected_hash {
            false
        } else {
            let mut applied = replacement.clone();
            applied.version = original
                .version
                .checked_add(1)
                .context("version overflow")?;
            ensure!(
                serde_yaml::from_slice::<AgentDefinition>(&current)? == applied,
                "definition changed after inventory; no files were written"
            );
            true
        };
        let storage_root = source
            .parent()
            .context("agent directory missing")?
            .parent()
            .context("agents directory missing")?
            .parent()
            .context("storage root missing")?;
        result.push(Prepared {
            store: AgentDefinitionStore::with_base_path(storage_root),
            source,
            expected_hash,
            original,
            replacement,
            already_applied,
            changes,
        });
    }
    Ok(result)
}

async fn commit(prepared: &mut [Prepared]) -> Result<()> {
    for item in prepared {
        if item.already_applied {
            continue;
        }
        ensure!(
            hash(&std::fs::read(&item.source)?) == item.expected_hash,
            "definition changed after preflight; remaining writes refused"
        );
        let updated = item
            .store
            .update_definition(
                &item.original.agent_id,
                item.replacement.clone(),
                item.original.version,
            )
            .await?;
        let mut expected = item.replacement.clone();
        expected.version = item
            .original
            .version
            .checked_add(1)
            .context("version overflow")?;
        ensure!(
            updated.definition == expected,
            "owner did not persist the expected definition"
        );
        item.already_applied = true;
    }
    Ok(())
}

fn require_offline(port: u16) -> Result<()> {
    for host in [
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
    ] {
        require_refused_connection(std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::new(host, port),
            Duration::from_secs(1),
        ))?;
    }
    Ok(())
}

fn require_refused_connection(connection: std::io::Result<std::net::TcpStream>) -> Result<()> {
    match connection {
        Ok(_) => bail!(
            "stop the supervised Magician service before applying an offline definition migration"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        Err(error) => {
            Err(error).context("cannot verify that Magician is stopped; migration refused")
        },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let inventory: Value = serde_json::from_slice(&std::fs::read(&args.inventory)?)?;
    let config = load_magician_config_from_path(&args.config)?;
    let router = config.router_config().context("catalog router missing")?;
    let targets = Targets {
        routes: router
            .catalog
            .assignments
            .keys()
            .cloned()
            .chain(
                router
                    .catalog
                    .choices()
                    .into_iter()
                    .flat_map(|choice| choice.choices.into_values()),
            )
            .collect(),
        coding: config
            .coding
            .profiles
            .iter()
            .filter(|profile| profile.id.starts_with("pi/model/"))
            .map(|profile| profile.id.clone())
            .collect(),
    };
    let mut prepared = preflight(&inventory, &args.backup_dir, &targets)?;
    if args.apply {
        require_offline(args.api_port)?;
        commit(&mut prepared).await?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mode":if args.apply {"applied"} else {"read_only_preview"},
            "definitions":prepared.iter().map(|item| json!({"agent_id":item.original.agent_id,
                "old_version":item.original.version, "new_version":item.original.version + 1,
                "already_applied":item.already_applied,"changes":item.changes})).collect::<Vec<_>>(),
            "execution_snapshots_modified":false,"templates_modified":false
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AgentDefinition {
        let mut definition: AgentDefinition = serde_yaml::from_str(include_str!(
            "../../magician_data_v3/system/agent_templates/agents/architect/definition.agent.yaml"
        ))
        .unwrap();
        definition.version = 1;
        definition.llm_routing.as_mut().unwrap().coding_profile = Some("old-coding".into());
        definition
    }

    fn map() -> Value {
        json!({"schema_version":1,"runtime_alias_resolver":false,"routes":{},"coding":{
            "old-coding":{"engine":"pi","client_resolvable":true,
                "model_selection":{"model_id":"openai/sol","thinking":"high"}}}})
    }

    fn targets() -> Targets {
        Targets {
            routes: HashSet::new(),
            coding: HashSet::from(["pi/model/openai/sol/high".into()]),
        }
    }

    #[test]
    fn changes_only_the_owned_pin_and_preserves_the_native_thinking_level() {
        let original = fixture();
        let (changed, changes) = rewrite(&original, &map(), &targets()).unwrap();
        let mut expected = original.clone();
        expected.llm_routing.as_mut().unwrap().coding_profile =
            Some("pi/model/openai/sol/high".into());
        assert_eq!(changed, expected);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            original.version, changed.version,
            "only the store advances versions"
        );
        assert_eq!(rewrite(&changed, &map(), &targets()).unwrap().1.len(), 0);
        assert!(rewrite(
            &original,
            &map(),
            &Targets {
                routes: HashSet::new(),
                coding: HashSet::new()
            }
        )
        .is_err());
    }

    #[test]
    fn refuses_offline_apply_while_the_service_port_is_listening() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert!(require_offline(listener.local_addr().unwrap().port()).is_err());
    }

    #[test]
    fn offline_apply_refuses_indeterminate_network_errors() {
        use std::io::{Error, ErrorKind};

        assert!(require_refused_connection(Err(Error::from(ErrorKind::ConnectionRefused))).is_ok());
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::TimedOut,
            ErrorKind::AddrNotAvailable,
        ] {
            assert!(require_refused_connection(Err(Error::from(kind))).is_err());
        }
    }

    #[tokio::test]
    async fn owner_update_archives_once_and_stale_sources_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("live");
        let relative = Path::new("scopes/p/w/agent_runtime/agents/architect/definition.agent.yaml");
        let source = root.join(relative);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        let original = fixture();
        let bytes = serde_yaml::to_string(&original).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let backup = directory.path().join("backup");
        let copy = backup.join("runtime").join(relative);
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(copy, &bytes).unwrap();
        let inventory = json!({"mode":"read_only_preparation","selection_migration":map(),
            "runtime":{"root":root,"errors":[],"records_with_legacy_references":[
                {"owner":"agent_definition_store","path":relative,"sha256":hash(bytes.as_bytes())},
                {"owner":"execution_snapshot_requires_lifecycle_audit","path":"never-open-execution-snapshot"}]}});
        let mut prepared = preflight(&inventory, &backup, &targets()).unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            bytes.as_bytes(),
            "preflight is read-only"
        );
        assert_eq!(prepared.len(), 1);
        commit(&mut prepared).await.unwrap();
        let archive = prepared[0]
            .store
            .storage()
            .agent_definition_version_path("architect", 1)
            .unwrap();
        let archived_bytes = std::fs::read(&archive).unwrap();
        assert_eq!(
            serde_yaml::from_slice::<AgentDefinition>(&archived_bytes).unwrap(),
            original
        );
        let after = std::fs::read(&source).unwrap();
        assert_eq!(
            serde_yaml::from_slice::<AgentDefinition>(&after)
                .unwrap()
                .version,
            2
        );
        let mut again = preflight(&inventory, &backup, &targets()).unwrap();
        assert!(again[0].already_applied);
        commit(&mut again).await.unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), after);
        assert_eq!(
            std::fs::read(&archive).unwrap(),
            archived_bytes,
            "the owner's original archive is untouched on rerun"
        );
        let mut stale = original;
        stale.name = "Owner edited this independently".into();
        std::fs::write(&source, serde_yaml::to_string(&stale).unwrap()).unwrap();
        assert!(preflight(&inventory, &backup, &targets()).is_err());
    }
}
