//! Project-owned checks and build caches.
//!
//! A project's checks are the owner's: set on the project (`checks`), or kept
//! in the repo (`.magician/checks.yaml`), or the project's own task runner
//! (`package.json` scripts, `Makefile` / `justfile` targets). Built-in presets
//! for common stacks are the last fallback, not the rule — a Java, Elixir or Go
//! project is as first-class as Rust.
//!
//! Build caches are the same idea for build output: the project lists the
//! environment variables its toolchain caches under (`CARGO_TARGET_DIR`,
//! `GRADLE_USER_HOME`, `MIX_BUILD_PATH`, `GOCACHE`, …). Magician points each at
//! a shared directory so every clone and shadow reuses it, applies them to
//! engine turns and checks, and empties them when the disk runs low.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::magician_v2::media_seam::dev_server::{detect_check_commands, CheckCommand};

/// The repo file a team keeps its checks in.
pub const REPO_CHECKS_FILE: &str = ".magician/checks.yaml";
/// Default per-check timeout when neither the check nor the preset sets one.
pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 240;
const MAX_CHECK_TIMEOUT_SECS: u64 = 3_600;
const MAX_CHECKS: usize = 16;

/// One check as the owner (or the repo file) writes it: a kind and a shell
/// command run from the project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VibeDevProjectCheck {
    /// Free text; `build`, `test`, `lint`, `typecheck` and `check` are the
    /// usual ones.
    pub kind: String,
    /// Run with `/bin/sh -c` in the project root.
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// A build cache: the environment variable the toolchain reads and, when not
/// left to Magician, the absolute directory it should point at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VibeDevBuildCache {
    pub env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Where a project's checks came from, for the settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    Project,
    RepoFile,
    TaskRunner,
    Preset,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedCheck {
    pub kind: String,
    /// What the UI shows and the owner would type.
    pub display: String,
    #[serde(skip)]
    pub command: CheckCommand,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedChecks {
    pub source: CheckSource,
    /// The file or preset the checks came from (`Makefile`, `Go preset`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub checks: Vec<ResolvedCheck>,
}

/// The checks for a project rooted at `dir`: the project's own list when set,
/// else the repo file, else the project's task runner, else a preset.
pub fn resolve_project_checks(
    configured: Option<&[VibeDevProjectCheck]>,
    dir: &Path,
) -> ResolvedChecks {
    if let Some(checks) = configured {
        return ResolvedChecks {
            source: CheckSource::Project,
            detail: None,
            checks: checks.iter().filter_map(shell_check).collect(),
        };
    }
    if let Some(checks) = repo_file_checks(dir) {
        return ResolvedChecks {
            source: CheckSource::RepoFile,
            detail: Some(REPO_CHECKS_FILE.to_string()),
            checks,
        };
    }
    // package.json scripts are the project's own commands, not a guess.
    if dir.join("package.json").is_file() {
        let checks = preset(detect_check_commands(dir), None);
        if !checks.is_empty() {
            return ResolvedChecks {
                source: CheckSource::TaskRunner,
                detail: Some("package.json".to_string()),
                checks,
            };
        }
    }
    for (file, runner) in [
        ("Makefile", "make"),
        ("justfile", "just"),
        ("Justfile", "just"),
    ] {
        let targets = task_runner_targets(&dir.join(file));
        if !targets.is_empty() {
            return ResolvedChecks {
                source: CheckSource::TaskRunner,
                detail: Some(file.to_string()),
                checks: targets
                    .into_iter()
                    .filter_map(|target| {
                        shell_check(&VibeDevProjectCheck {
                            kind: target.clone(),
                            run: format!("{runner} {target}"),
                            timeout_secs: None,
                        })
                    })
                    .collect(),
            };
        }
    }
    if let Some((label, checks)) = stack_preset(dir) {
        return ResolvedChecks {
            source: CheckSource::Preset,
            detail: Some(label),
            checks,
        };
    }
    ResolvedChecks {
        source: CheckSource::None,
        detail: None,
        checks: Vec::new(),
    }
}

/// Validate an owner-supplied list before it is stored.
pub fn validate_project_checks(checks: &[VibeDevProjectCheck]) -> Result<(), String> {
    if checks.len() > MAX_CHECKS {
        return Err(format!("at most {MAX_CHECKS} checks"));
    }
    for check in checks {
        let kind = check.kind.trim();
        if kind.is_empty() || kind.len() > 32 {
            return Err("each check needs a short kind (build, test, lint, …)".to_string());
        }
        if check.run.trim().is_empty() || check.run.len() > 2_000 {
            return Err(format!("check `{kind}` needs a command"));
        }
        if check
            .timeout_secs
            .is_some_and(|secs| secs == 0 || secs > MAX_CHECK_TIMEOUT_SECS)
        {
            return Err(format!(
                "check `{kind}` timeout must be 1–{MAX_CHECK_TIMEOUT_SECS} seconds"
            ));
        }
    }
    Ok(())
}

fn shell_check(check: &VibeDevProjectCheck) -> Option<ResolvedCheck> {
    let run = check.run.trim();
    let kind = check.kind.trim();
    if run.is_empty() || kind.is_empty() {
        return None;
    }
    Some(ResolvedCheck {
        kind: kind.to_string(),
        display: run.to_string(),
        command: CheckCommand {
            kind: kind.to_string(),
            // Absolute: the check runner sets env on the child, and a bare
            // program name would make Rust fork instead of posix_spawn.
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), run.to_string()],
            display: run.to_string(),
        },
        timeout_secs: check
            .timeout_secs
            .unwrap_or(DEFAULT_CHECK_TIMEOUT_SECS)
            .min(MAX_CHECK_TIMEOUT_SECS),
    })
}

fn preset(commands: Vec<CheckCommand>, timeout_secs: Option<u64>) -> Vec<ResolvedCheck> {
    commands
        .into_iter()
        .map(|command| ResolvedCheck {
            kind: command.kind.clone(),
            display: command.display.clone(),
            timeout_secs: timeout_secs.unwrap_or(DEFAULT_CHECK_TIMEOUT_SECS),
            command,
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RepoChecksFile {
    Wrapped { checks: Vec<VibeDevProjectCheck> },
    List(Vec<VibeDevProjectCheck>),
}

fn repo_file_checks(dir: &Path) -> Option<Vec<ResolvedCheck>> {
    let path = dir.join(REPO_CHECKS_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    let parsed: RepoChecksFile = match serde_yaml::from_str(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring unreadable checks file");
            return None;
        },
    };
    let list = match parsed {
        RepoChecksFile::Wrapped { checks } | RepoChecksFile::List(checks) => checks,
    };
    if let Err(error) = validate_project_checks(&list) {
        tracing::warn!(path = %path.display(), %error, "ignoring invalid checks file");
        return None;
    }
    let checks: Vec<ResolvedCheck> = list.iter().filter_map(shell_check).collect();
    (!checks.is_empty()).then_some(checks)
}

/// Targets of a Makefile / justfile that read as checks, in a stable order.
fn task_runner_targets(path: &Path) -> Vec<String> {
    const NAMES: &[&str] = &["check", "lint", "typecheck", "test", "build"];
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let defined: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with([' ', '\t', '#', '.']))
        .filter_map(|line| line.split(':').next())
        .map(|name| name.split_whitespace().next().unwrap_or(""))
        .collect();
    NAMES
        .iter()
        .filter(|name| defined.contains(name))
        .map(|name| name.to_string())
        .collect()
}

/// Built-in commands for common stacks, used only when the project and repo
/// say nothing. Rust, Python and Node come from the existing detector.
fn stack_preset(dir: &Path) -> Option<(String, Vec<ResolvedCheck>)> {
    let check = |kind: &str, run: &str| {
        shell_check(&VibeDevProjectCheck {
            kind: kind.to_string(),
            run: run.to_string(),
            timeout_secs: None,
        })
    };
    let collect = |items: Vec<Option<ResolvedCheck>>| items.into_iter().flatten().collect();
    if dir.join("go.mod").is_file() {
        return Some((
            "Go preset".to_string(),
            collect(vec![
                check("build", "go build ./..."),
                check("lint", "go vet ./..."),
                check("test", "go test ./..."),
            ]),
        ));
    }
    if dir.join("mix.exs").is_file() {
        return Some((
            "Elixir preset".to_string(),
            collect(vec![
                check("build", "mix compile --warnings-as-errors"),
                check("lint", "mix format --check-formatted"),
                check("test", "mix test"),
            ]),
        ));
    }
    let gradle = [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
    ]
    .iter()
    .any(|file| dir.join(file).is_file());
    if gradle {
        let runner = if dir.join("gradlew").is_file() {
            "./gradlew"
        } else {
            "gradle"
        };
        return Some((
            "Gradle preset".to_string(),
            collect(vec![
                check("build", &format!("{runner} assemble")),
                check("test", &format!("{runner} test")),
            ]),
        ));
    }
    if dir.join("pom.xml").is_file() {
        let runner = if dir.join("mvnw").is_file() {
            "./mvnw"
        } else {
            "mvn"
        };
        return Some((
            "Maven preset".to_string(),
            collect(vec![
                check("build", &format!("{runner} -q -DskipTests compile")),
                check("test", &format!("{runner} -q test")),
            ]),
        ));
    }
    let detected = preset(detect_check_commands(dir), None);
    if detected.is_empty() {
        return None;
    }
    let label = if dir.join("Cargo.toml").is_file() {
        "Rust preset"
    } else {
        "Python preset"
    };
    Some((label.to_string(), detected))
}

/// Environment names a project may not point at a cache directory.
fn reserved_env(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "HOME" | "SHELL" | "USER" | "TMPDIR" | "PWD" | "LANG" | "TERM"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || name.starts_with("MAGICIAN_")
}

/// Validate an owner-supplied cache list before it is stored.
pub fn validate_build_caches(caches: &[VibeDevBuildCache]) -> Result<(), String> {
    if caches.len() > MAX_CHECKS {
        return Err(format!("at most {MAX_CHECKS} build caches"));
    }
    for cache in caches {
        let env = cache.env.trim();
        let valid_name = !env.is_empty()
            && env.len() <= 64
            && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !env.starts_with(|c: char| c.is_ascii_digit());
        if !valid_name {
            return Err(format!("`{env}` is not an environment variable name"));
        }
        if reserved_env(env) {
            return Err(format!("`{env}` cannot be a build cache"));
        }
        if let Some(path) = cache
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if !Path::new(path).is_absolute() && !path.starts_with("~/") {
                return Err(format!("`{env}` path must be absolute"));
            }
            // Coding turns may write a cache folder, so it gets the same
            // limits as an owner-allowed writable folder.
            let expanded = PathBuf::from(shellexpand::tilde(path).into_owned());
            if crate::magician_v2::execution::coding_engine::write_denials::never_allow(
                &expanded,
                dirs::home_dir().as_deref(),
            ) || super::sandbox_approvals::holds_or_inside_magician(&expanded)
            {
                return Err(format!(
                    "`{env}` path `{path}` can never be made writable for coding runs"
                ));
            }
        }
    }
    Ok(())
}

/// The cache directories a turn or check runs with: `CARGO_TARGET_DIR` at
/// `cargo_dir` (the `coding.build_dir` setting) unless the project sets it,
/// then each project cache at its own path or a Magician-managed one under
/// `build_root`.
pub fn resolve_build_env(
    caches: &[VibeDevBuildCache],
    build_root: &Path,
    cargo_dir: &Path,
) -> Vec<(String, PathBuf)> {
    let mut env: Vec<(String, PathBuf)> = Vec::new();
    for cache in caches {
        let name = cache.env.trim().to_string();
        if name.is_empty() || reserved_env(&name) || env.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let path = match cache
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(path) => PathBuf::from(shellexpand::tilde(path).into_owned()),
            None if name == "CARGO_TARGET_DIR" => cargo_dir.to_path_buf(),
            None => build_root.join(name.to_ascii_lowercase().replace('_', "-")),
        };
        env.push((name, path));
    }
    if !env.iter().any(|(name, _)| name == "CARGO_TARGET_DIR") {
        env.insert(0, ("CARGO_TARGET_DIR".to_string(), cargo_dir.to_path_buf()));
    }
    env
}

/// File Magician holds a shared lock on inside a cache directory while a turn
/// or check uses it; [`prune_build_caches_when_low`] needs the exclusive lock.
const IN_USE_LOCK: &str = ".magician-in-use";

/// Held for as long as a turn or check builds into the caches.
pub struct BuildCacheLease {
    _locks: Vec<std::fs::File>,
}

/// Create the cache directories and take a shared in-use lock on each.
pub fn lease_build_caches(env: &[(String, PathBuf)]) -> BuildCacheLease {
    let mut locks = Vec::new();
    for (_, dir) in env {
        if std::fs::create_dir_all(dir).is_err() {
            continue;
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(IN_USE_LOCK))
        {
            if file.lock_shared().is_ok() {
                locks.push(file);
            }
        }
    }
    BuildCacheLease { _locks: locks }
}

/// Empty every cache directory whose disk is below `min_free_bytes` and that
/// no turn or check holds. A cargo target also keeps cargo's own lock check
/// ([`prune_build_dir_when_low`]). Returns the directories emptied.
///
/// [`prune_build_dir_when_low`]: crate::magician_v2::execution::coding_engine::prune_build_dir_when_low
pub fn prune_build_caches_when_low(env: &[(String, PathBuf)], min_free_bytes: u64) -> Vec<PathBuf> {
    use fs2::FileExt;
    let mut pruned = Vec::new();
    if min_free_bytes == 0 {
        return pruned;
    }
    for (_, dir) in env {
        if !dir.is_dir() {
            continue;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(IN_USE_LOCK));
        let Ok(lock) = lock else { continue };
        if lock.try_lock_exclusive().is_err() {
            continue;
        }
        match crate::magician_v2::execution::coding_engine::prune_build_dir_when_low(
            dir,
            min_free_bytes,
        ) {
            Ok(true) => pruned.push(dir.clone()),
            Ok(false) => {},
            Err(error) => tracing::warn!(dir = %dir.display(), %error, "build cache prune failed"),
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn kinds(resolved: &ResolvedChecks) -> Vec<(&str, &str)> {
        resolved
            .checks
            .iter()
            .map(|check| (check.kind.as_str(), check.display.as_str()))
            .collect()
    }

    #[test]
    fn the_project_list_wins_over_everything_in_the_repo() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\nname='x'\n");
        write(
            tmp.path(),
            REPO_CHECKS_FILE,
            "checks:\n  - kind: test\n    run: echo repo\n",
        );
        let configured = vec![VibeDevProjectCheck {
            kind: "test".into(),
            run: "mix test".into(),
            timeout_secs: Some(600),
        }];
        let resolved = resolve_project_checks(Some(&configured), tmp.path());
        assert_eq!(resolved.source, CheckSource::Project);
        assert_eq!(kinds(&resolved), vec![("test", "mix test")]);
        assert_eq!(resolved.checks[0].timeout_secs, 600);
        assert_eq!(resolved.checks[0].command.program, "/bin/sh");
        // An empty owner list means "no checks", not "detect".
        let none = resolve_project_checks(Some(&[]), tmp.path());
        assert_eq!(none.source, CheckSource::Project);
        assert!(none.checks.is_empty());
    }

    #[test]
    fn a_repo_file_then_a_task_runner_then_a_preset() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "go.mod", "module x\n");
        let preset = resolve_project_checks(None, tmp.path());
        assert_eq!(preset.source, CheckSource::Preset);
        assert_eq!(preset.detail.as_deref(), Some("Go preset"));
        assert!(kinds(&preset).contains(&("test", "go test ./...")));

        write(tmp.path(), "Makefile", ".PHONY: test\ntest:\n\tgo test ./...\nlint: vet\n\tgolangci-lint run\nrelease:\n\t./ship\n");
        let runner = resolve_project_checks(None, tmp.path());
        assert_eq!(runner.source, CheckSource::TaskRunner);
        assert_eq!(runner.detail.as_deref(), Some("Makefile"));
        assert_eq!(
            kinds(&runner),
            vec![("lint", "make lint"), ("test", "make test")]
        );

        write(
            tmp.path(),
            REPO_CHECKS_FILE,
            "- kind: test\n  run: ./ci/test.sh\n  timeout_secs: 900\n",
        );
        let file = resolve_project_checks(None, tmp.path());
        assert_eq!(file.source, CheckSource::RepoFile);
        assert_eq!(kinds(&file), vec![("test", "./ci/test.sh")]);
        assert_eq!(file.checks[0].timeout_secs, 900);
    }

    #[test]
    fn presets_cover_elixir_gradle_and_maven_and_unknown_has_none() {
        let elixir = tempfile::tempdir().unwrap();
        write(elixir.path(), "mix.exs", "defmodule X do end\n");
        assert!(kinds(&resolve_project_checks(None, elixir.path())).contains(&("test", "mix test")));

        let gradle = tempfile::tempdir().unwrap();
        write(gradle.path(), "build.gradle.kts", "");
        write(gradle.path(), "gradlew", "");
        assert!(kinds(&resolve_project_checks(None, gradle.path()))
            .contains(&("test", "./gradlew test")));

        let maven = tempfile::tempdir().unwrap();
        write(maven.path(), "pom.xml", "<project/>");
        assert!(
            kinds(&resolve_project_checks(None, maven.path())).contains(&("test", "mvn -q test"))
        );

        let unknown = tempfile::tempdir().unwrap();
        let none = resolve_project_checks(None, unknown.path());
        assert_eq!(none.source, CheckSource::None);
        assert!(none.checks.is_empty());
    }

    #[test]
    fn owner_lists_are_validated() {
        let ok = VibeDevProjectCheck {
            kind: "test".into(),
            run: "mix test".into(),
            timeout_secs: None,
        };
        assert!(validate_project_checks(&[ok.clone()]).is_ok());
        let no_run = VibeDevProjectCheck {
            run: "  ".into(),
            ..ok.clone()
        };
        assert!(validate_project_checks(&[no_run]).is_err());
        let slow = VibeDevProjectCheck {
            timeout_secs: Some(99_999),
            ..ok
        };
        assert!(validate_project_checks(&[slow]).is_err());

        let cache = |env: &str, path: Option<&str>| VibeDevBuildCache {
            env: env.into(),
            path: path.map(str::to_string),
        };
        assert!(validate_build_caches(&[cache("GRADLE_USER_HOME", None)]).is_ok());
        assert!(validate_build_caches(&[cache("PATH", None)]).is_err());
        assert!(validate_build_caches(&[cache("DYLD_LIBRARY_PATH", None)]).is_err());
        assert!(validate_build_caches(&[cache("npm_config_cache", None)]).is_ok());
        assert!(validate_build_caches(&[cache("HAS SPACE", None)]).is_err());
        assert!(validate_build_caches(&[cache("MIX_BUILD_PATH", Some("relative"))]).is_err());
    }

    #[test]
    fn build_env_keeps_cargo_and_adds_project_caches() {
        let root = Path::new("/caches");
        let cargo = Path::new("/caches/cargo-target");
        let env = resolve_build_env(
            &[
                VibeDevBuildCache {
                    env: "GRADLE_USER_HOME".into(),
                    path: None,
                },
                VibeDevBuildCache {
                    env: "MIX_BUILD_PATH".into(),
                    path: Some("/srv/mix".into()),
                },
            ],
            root,
            cargo,
        );
        assert_eq!(
            env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), cargo.to_path_buf()),
                (
                    "GRADLE_USER_HOME".to_string(),
                    root.join("gradle-user-home")
                ),
                ("MIX_BUILD_PATH".to_string(), PathBuf::from("/srv/mix")),
            ]
        );
        let custom_cargo = resolve_build_env(
            &[VibeDevBuildCache {
                env: "CARGO_TARGET_DIR".into(),
                path: Some("/fast/target".into()),
            }],
            root,
            cargo,
        );
        assert_eq!(
            custom_cargo,
            vec![(
                "CARGO_TARGET_DIR".to_string(),
                PathBuf::from("/fast/target")
            )]
        );
    }

    #[test]
    fn a_leased_cache_is_never_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("gradle");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("artifact.bin"), b"x").unwrap();
        let env = vec![("GRADLE_USER_HOME".to_string(), dir.clone())];
        {
            let _lease = lease_build_caches(&env);
            assert!(prune_build_caches_when_low(&env, u64::MAX).is_empty());
            assert!(dir.join("artifact.bin").is_file());
        }
        assert_eq!(
            prune_build_caches_when_low(&env, u64::MAX),
            vec![dir.clone()]
        );
        assert!(!dir.join("artifact.bin").exists());
    }
}
