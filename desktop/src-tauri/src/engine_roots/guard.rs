//! Source guard: desktop must not grow new engine-root readers of engine-owned data.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    fn desktop_src() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Production source with every `#[cfg(test)]` item removed.
    ///
    /// This used to truncate at the first marker, which also discarded all
    /// production code that followed a mid-file test module. `main.rs` keeps
    /// `deep_link_tests` next to the function it covers, so the real
    /// `should_supervise_local_engine` call further down was invisible to the
    /// guard below.
    fn production_text(rel: &str) -> String {
        let path = desktop_src().join(rel);
        let whole = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
        strip_test_items(&whole)
    }

    /// Drop each `#[cfg(test)]` / `#[cfg(any(test, ..))]` item wherever it sits:
    /// a braced item is skipped to its matching close, a `;`-terminated one to
    /// its semicolon. Everything between test items is production code.
    fn strip_test_items(source: &str) -> String {
        const MARKERS: [&str; 2] = ["#[cfg(test)]", "#[cfg(any(test"];
        let mut out = String::with_capacity(source.len());
        let mut rest = source;
        while let Some(at) = MARKERS.iter().filter_map(|marker| rest.find(marker)).min() {
            out.push_str(&rest[..at]);
            let after = &rest[at..];
            let brace = after.find('{');
            let semi = after.find(';');
            match (brace, semi) {
                // `#[cfg(test)] use ..;` — a `;` before any brace ends the item.
                (Some(open), Some(end)) if end < open => rest = &after[end + 1..],
                (None, Some(end)) => rest = &after[end + 1..],
                // `#[cfg(test)] mod tests { .. }` — skip the balanced block.
                (Some(open), _) => {
                    let mut depth = 0usize;
                    let mut skip_to = after.len();
                    for (idx, ch) in after[open..].char_indices() {
                        if ch == '{' {
                            depth += 1;
                        } else if ch == '}' {
                            depth -= 1;
                            if depth == 0 {
                                skip_to = open + idx + 1;
                                break;
                            }
                        }
                    }
                    rest = &after[skip_to..];
                },
                (None, None) => return out,
            }
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn runtime_root_callers_are_classified() {
        let allowed = [
            "runtime_paths.rs",
            "engine_roots/mod.rs",
            "engine_roots/guard.rs",
            "engine_roots/characterization.rs",
            "engine_roots/storage_packet.rs",
            "container/mod.rs",
            "connect_route.rs",
            "setup.rs",
            "env_file.rs",
            "voice_wake.rs",
        ];
        let mut offenders = Vec::new();
        visit(&desktop_src(), &mut |rel, text| {
            if allowed.iter().any(|ok| rel == *ok) {
                return;
            }
            if text.contains("runtime_root_dir(") || text.contains("vosk_model_dir(") {
                offenders.push(rel);
            }
        });
        assert!(
            offenders.is_empty(),
            "new desktop engine-root readers must be classified in Task 16B: {offenders:?}"
        );
    }

    #[test]
    fn env_file_refuses_remote_engine_owned_reads() {
        let text = production_text("env_file.rs");
        assert!(
            text.contains("refuse_engine_owned_filesystem"),
            "env editor must refuse engine-owned files when the engine is remote"
        );
    }

    #[test]
    fn local_supervision_skips_remote_engine() {
        let text = production_text("main.rs");
        assert!(
            text.contains("should_supervise_local_engine"),
            "desktop must not spawn a local engine when is_remote_engine()"
        );
    }

    fn visit(dir: &std::path::Path, on_file: &mut impl FnMut(String, String)) {
        let entries = fs::read_dir(dir).unwrap();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, on_file);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let rel = path
                    .strip_prefix(desktop_src())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = production_text(&rel);
                on_file(rel, text);
            }
        }
    }
}
