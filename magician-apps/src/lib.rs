//! The app-platform runtime surface: surface hosting/hydration/workers, the
//! entity lifecycle periphery (outbox, portability, retention, changes),
//! migration, sandboxing, installation review, and registry lifecycle — the
//! ~22k-line subset of the former `magician_v2::apps` module with zero
//! references from the magician lib. The shared app vocabulary, registry, and
//! workflows stay lib-side (`magician_v2::apps`) until the execution↔apps
//! cycle is inverted.

pub mod apps;

// The isolated scripted-surface worker respawns the current test binary with
// this exact filter. Keep the child entrypoint in the crate that owns both the
// worker and its tests so extraction does not make the child exit before its
// ready handshake.
#[cfg(test)]
#[test]
fn magician_surface_worker_child() {
    apps::surface_worker::run_test_child_if_requested();
}
