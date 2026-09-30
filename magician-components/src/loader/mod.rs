//! Loading the declared graph.
//!
//! The graph is `graph.yaml`, compiled in with `include_str!`. It is data so
//! that adding a capability is an entry rather than an edit to the resolver, and
//! so a reviewer can read what the system believes about itself in a diff. It is
//! compiled in rather than read at runtime because a missing or half-written
//! file at startup would leave the registry reporting an empty stack, which is
//! indistinguishable from a broken one.
//!
//! Probe paths carry placeholders because the data root and the home directory
//! are only known at run time. Expansion happens here, once, on the way out.

use serde::Deserialize;

use crate::{Component, Feature, Graph, InstallAction, Need, ProbeSpec, Requirement, Surface};

const GRAPH_YAML: &str = include_str!("../graph.yaml");

/// A need is written as `{requirement: model}` or `{component: magicutor}`,
/// which is friendlier to author than the model's tagged form.
///
/// A struct with two optional fields rather than an enum: serde_yaml encodes
/// externally-tagged enums as YAML tags (`!Requirement model`), not as maps, so
/// the readable form would not round-trip. Exactly one field must be set, and
/// saying which one is wrong beats a parse error pointing at a line number.
#[derive(Debug, Deserialize)]
struct RawNeed {
    #[serde(default)]
    component: Option<String>,
    #[serde(default)]
    requirement: Option<String>,
}

impl RawNeed {
    fn into_need(self) -> Result<Need, String> {
        match (self.component, self.requirement) {
            (Some(id), None) => Ok(Need::Component(id)),
            (None, Some(id)) => Ok(Need::Requirement(id)),
            (Some(c), Some(r)) => Err(format!(
                "a need sets both component '{c}' and requirement '{r}'"
            )),
            (None, None) => Err("a need sets neither component nor requirement".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawComponent {
    id: String,
    name: String,
    #[serde(default)]
    core: bool,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    cost: String,
    #[serde(default)]
    pricing: crate::Pricing,
    install: InstallAction,
    probe: ProbeSpec,
    #[serde(default)]
    setup: Option<crate::setup::SetupBinding>,
    #[serde(default)]
    host: Option<crate::HostRequirement>,
}

#[derive(Debug, Deserialize)]
struct RawFeature {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    needs: Vec<RawNeed>,
    #[serde(default)]
    surfaces: Vec<Surface>,
}

#[derive(Debug, Deserialize)]
struct RawGraph {
    #[serde(default)]
    requirements: Vec<Requirement>,
    #[serde(default)]
    components: Vec<RawComponent>,
    #[serde(default)]
    features: Vec<RawFeature>,
}

/// Where the runtime keeps its data, and the user's home. Both appear in probe
/// paths and neither is known until the process runs.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_root: String,
    pub home: String,
}

impl Paths {
    /// Resolve from the environment the way every other consumer does:
    /// `MAGICIAN_ROOT_DIR`, else `$HOME/MagicianNotes`.
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let data_root =
            std::env::var("MAGICIAN_ROOT_DIR").unwrap_or_else(|_| format!("{home}/MagicianNotes"));
        Paths { data_root, home }
    }

    fn expand(&self, s: &str) -> String {
        s.replace("{data_root}", &self.data_root)
            .replace("{home}", &self.home)
    }
}

/// Parse the compiled-in graph and expand its paths.
///
/// Returns the parse error rather than panicking so a caller can report a bad
/// graph as a bad graph. [`load`] is the panicking convenience for callers that
/// cannot proceed without one.
pub fn try_load(paths: &Paths) -> Result<Graph, String> {
    let raw: RawGraph = serde_yaml::from_str(GRAPH_YAML).map_err(|e| e.to_string())?;

    let components = raw
        .components
        .into_iter()
        .map(|c| {
            let setup = c
                .setup
                .as_ref()
                .map(crate::setup::resolve)
                .transpose()
                .map_err(|error| format!("component '{}': {error}", c.id))?;
            Ok(Component {
                id: c.id,
                name: c.name,
                core: c.core,
                required: c.required,
                provides: c.provides,
                requires: c.requires,
                cost: c.cost,
                pricing: c.pricing,
                install: expand_install(c.install, paths),
                probe: expand_probe(c.probe, paths),
                setup,
                host: c.host,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut features = Vec::with_capacity(raw.features.len());
    for f in raw.features {
        let needs = f
            .needs
            .into_iter()
            .map(RawNeed::into_need)
            .collect::<Result<Vec<_>, String>>()
            .map_err(|e| format!("feature '{}': {e}", f.id))?;
        features.push(Feature {
            id: f.id,
            name: f.name,
            description: f.description,
            needs,
            surfaces: f.surfaces,
        });
    }

    let graph = Graph {
        components,
        requirements: raw.requirements,
        features,
    };

    // A reference to something that does not exist would otherwise surface as a
    // confusing "unavailable" at runtime instead of a loud error at load.
    let dangling = graph.dangling();
    if !dangling.is_empty() {
        return Err(format!(
            "graph.yaml references unknown ids: {}",
            dangling.join("; ")
        ));
    }
    Ok(graph)
}

/// The compiled-in graph, expanded for these paths.
///
/// Panics on a malformed graph. That is deliberate: `graph.yaml` ships inside
/// the binary, so a failure here is a build that should never have been
/// released, not a runtime condition anyone can recover from. A test parses it
/// on every run, so this cannot reach a user without failing CI first.
pub fn load(paths: &Paths) -> Graph {
    try_load(paths).expect("the compiled-in component graph must parse")
}

/// Instructions name real paths too — "add a key to {data_root}/.env" is only
/// useful once that is the path the reader actually has.
fn expand_install(action: InstallAction, paths: &Paths) -> InstallAction {
    match action {
        InstallAction::Manual {
            steps,
            open,
            secrets,
        } => InstallAction::Manual {
            steps: steps.iter().map(|s| paths.expand(s)).collect(),
            open,
            secrets,
        },
        other => other,
    }
}

fn expand_probe(probe: ProbeSpec, paths: &Paths) -> ProbeSpec {
    match probe {
        ProbeSpec::HttpOk { url, timeout_ms } => ProbeSpec::HttpOk {
            url: paths.expand(&url),
            timeout_ms,
        },
        ProbeSpec::HttpAnyResponse { url, timeout_ms } => ProbeSpec::HttpAnyResponse {
            url: paths.expand(&url),
            timeout_ms,
        },
        ProbeSpec::HttpBodyContains {
            url,
            needle,
            timeout_ms,
        } => ProbeSpec::HttpBodyContains {
            url: paths.expand(&url),
            needle,
            timeout_ms,
        },
        ProbeSpec::ConfiguredOllamaModelPresent {
            url,
            config_path,
            key_path,
            timeout_ms,
        } => ProbeSpec::ConfiguredOllamaModelPresent {
            url: paths.expand(&url),
            config_path: paths.expand(&config_path),
            key_path,
            timeout_ms,
        },
        ProbeSpec::FileExists { path } => ProbeSpec::FileExists {
            path: paths.expand(&path),
        },
        ProbeSpec::FileReadable { path, because } => ProbeSpec::FileReadable {
            path: paths.expand(&path),
            because,
        },
        ProbeSpec::EnvKeyPresent { keys, files } => ProbeSpec::EnvKeyPresent {
            keys,
            files: files.iter().map(|f| paths.expand(f)).collect(),
        },
        ProbeSpec::EnvKeysAllPresent { keys, files } => ProbeSpec::EnvKeysAllPresent {
            keys,
            files: files.iter().map(|f| paths.expand(f)).collect(),
        },
        ProbeSpec::Manual { hint } => ProbeSpec::Manual { hint },
    }
}

#[cfg(test)]
mod tests;
