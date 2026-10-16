//! Print what this machine actually has, and what that enables.
//!
//!     cargo run -p magician-components --features probe --example status
//!
//! The point is that the graph is checked against reality rather than against a
//! description of reality. Every path it probes comes from the same inventory
//! the shell health sweep uses, so a disagreement between this and
//! `scripts/install-verify.sh` is a bug in one of them.

use std::collections::BTreeMap;

use magician_components::{loader, probe, resolve, Availability, ComponentState, Surface};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let paths = loader::Paths::from_env();
    println!("data root: {}\n", paths.data_root);

    let graph = match loader::try_load(&paths) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("the shipped graph is malformed: {e}");
            std::process::exit(1);
        },
    };

    let observed = probe::observe_all(&graph.components).await;
    // Nothing declared: this is what a machine looks like to a registry that has
    // never been told anything, which is every install today.
    let report = resolve(&graph, &BTreeMap::new(), &observed);

    println!("Components");
    for component in &report.components {
        let (mark, note) = match &component.state {
            ComponentState::Present { detail } => ("ok  ", detail.clone()),
            ComponentState::Declined => ("--  ", "declined".to_string()),
            ComponentState::Missing { detail } => ("!!  ", detail.clone()),
            ComponentState::NotInstalled { detail } => ("    ", detail.clone()),
            ComponentState::BlockedBy { component } => ("!!  ", format!("blocked by {component}")),
            ComponentState::Unknown { detail } => ("?   ", detail.clone()),
        };
        println!("  {mark}{:<28} {note}", component.id);
    }

    for surface in [Surface::Web, Surface::Desktop, Surface::Ios] {
        println!("\n{surface:?}");
        for feature in report
            .features
            .iter()
            .filter(|f| f.surfaces.contains(&surface))
        {
            match &feature.availability {
                Availability::Available => println!("  ok   {}", feature.name),
                Availability::Unavailable { reason, .. } => {
                    println!("       {:<32} {reason}", feature.name)
                },
            }
        }
    }
}
