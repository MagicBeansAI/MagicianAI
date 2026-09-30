//! Default startup must not run backup, restore, or GC.
//! magician-bin must not gain s3 / state / migration startup dependencies.

#[test]
fn magician_bin_does_not_start_recovery() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../magician-bin/src/main.rs");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        !text.contains("snapshot_representative_scope")
            && !text.contains("collect_unreferenced")
            && !text.contains("restore_representative_scope")
            && !text.contains("SignedSnapshot"),
        "magician-bin must not invoke Task 17 recovery at startup"
    );
}

#[test]
fn magician_bin_does_not_depend_on_remote_storage_crates() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../magician-bin/Cargo.toml");
    let text = std::fs::read_to_string(path).unwrap();
    for crate_name in [
        "magician-storage-s3",
        "magician-storage-state",
        "magician-storage-migration",
    ] {
        assert!(
            !text.contains(crate_name),
            "magician-bin must not depend on {crate_name}"
        );
    }
}
