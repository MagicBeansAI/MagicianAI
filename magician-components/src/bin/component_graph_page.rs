//! Render the component graph as a standalone page.
//!
//!     make component-graph
//!
//! Generated from `graph.yaml` through the real [`loader`], so the page cannot
//! describe a graph the runtime would not accept: a declaration that fails to
//! parse or references an unknown id fails here first, before anything is
//! written.
//!
//! Only the structural fields are emitted. Probe *paths* are deliberately left
//! out — they expand to the generating machine's home and data root, which
//! would make the output differ per person and turn the drift gate into noise.
//! The probe *kind* is kept, because how an answer is obtained is part of what
//! the page explains.

use magician_components::{loader, Graph};
use serde::Serialize;

const TEMPLATE: &str = include_str!("graph_page.html");

#[derive(Serialize)]
struct PageComponent<'a> {
    id: &'a str,
    name: &'a str,
    core: bool,
    /// Not declinable, but still installed. The page has to say so, or it
    /// offers a switch that the wizard will refuse to honour.
    required: bool,
    provides: &'a [String],
    requires: &'a [String],
    cost: &'a str,
    /// "paid" / "free tier", or empty when it costs nothing — the page shows
    /// nothing rather than labelling the majority of the graph "free".
    #[serde(skip_serializing_if = "str::is_empty")]
    pricing: &'static str,
    probe: &'static str,
    /// The machine this needs, in one sentence, or empty when it runs anywhere.
    #[serde(skip_serializing_if = "str::is_empty")]
    host: String,
}

#[derive(Serialize)]
struct PageRequirement<'a> {
    id: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct PageNeed<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    component: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requirement: Option<&'a str>,
}

#[derive(Serialize)]
struct PageFeature<'a> {
    id: &'a str,
    name: &'a str,
    description: &'a str,
    needs: Vec<PageNeed<'a>>,
    surfaces: Vec<String>,
}

#[derive(Serialize)]
struct Page<'a> {
    requirements: Vec<PageRequirement<'a>>,
    components: Vec<PageComponent<'a>>,
    features: Vec<PageFeature<'a>>,
}

fn probe_kind(spec: &magician_components::ProbeSpec) -> &'static str {
    use magician_components::ProbeSpec::*;
    match spec {
        HttpOk { .. } => "http ok",
        HttpAnyResponse { .. } => "any http response",
        HttpBodyContains { .. } => "http body contains",
        ConfiguredOllamaModelPresent { .. } => "configured Ollama model present",
        FileExists { .. } => "file exists",
        FileReadable { .. } => "file readable",
        EnvKeyPresent { .. } => "any env key present",
        EnvKeysAllPresent { .. } => "all env keys present",
        Manual { .. } => "manual",
    }
}

/// The machine a component needs, phrased for a reader rather than a matcher.
/// Empty when it runs anywhere, so the page can simply omit the line.
fn host_line(host: Option<&magician_components::HostRequirement>) -> String {
    let Some(host) = host else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(gb) = host.min_memory_gb {
        parts.push(format!("{gb} GB of memory"));
    }
    if !host.arch.is_empty() {
        parts.push(host.arch.join(" or "));
    }
    if !host.os.is_empty() {
        parts.push(host.os.join(" or "));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("needs {}", parts.join(", "))
}

fn page(graph: &Graph) -> Page<'_> {
    Page {
        requirements: graph
            .requirements
            .iter()
            .map(|r| PageRequirement {
                id: &r.id,
                name: &r.name,
            })
            .collect(),
        components: graph
            .components
            .iter()
            .map(|c| PageComponent {
                id: &c.id,
                name: &c.name,
                core: c.core,
                required: c.required,
                provides: &c.provides,
                requires: &c.requires,
                cost: &c.cost,
                pricing: if c.pricing.costs_money() {
                    c.pricing.label()
                } else {
                    ""
                },
                probe: probe_kind(&c.probe),
                host: host_line(c.host.as_ref()),
            })
            .collect(),
        features: graph
            .features
            .iter()
            .map(|f| PageFeature {
                id: &f.id,
                name: &f.name,
                description: &f.description,
                needs: f
                    .needs
                    .iter()
                    .map(|n| match n {
                        magician_components::Need::Component(id) => PageNeed {
                            component: Some(id),
                            requirement: None,
                        },
                        magician_components::Need::Requirement(id) => PageNeed {
                            component: None,
                            requirement: Some(id),
                        },
                    })
                    .collect(),
                surfaces: f
                    .surfaces
                    .iter()
                    .map(|s| format!("{s:?}").to_lowercase())
                    .collect(),
            })
            .collect(),
    }
}

fn main() {
    // Neutral paths: nothing machine-specific reaches the output, so the page
    // is byte-identical whoever regenerates it.
    let paths = loader::Paths {
        data_root: String::new(),
        home: String::new(),
    };
    let graph = match loader::try_load(&paths) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("component graph is malformed: {e}");
            std::process::exit(1);
        },
    };
    let json = serde_json::to_string(&page(&graph)).expect("the page model must serialize");
    print!("{}", TEMPLATE.replace("__GRAPH_JSON__", &json));
}
