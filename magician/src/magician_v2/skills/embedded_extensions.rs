//! Generic discovery for product metadata embedded in governed `SKILL.md`.
//!
//! A skill directory is the unit of precedence. The first `SKILL.md` for a
//! name shadows the entire lower-precedence skill, including extensions it
//! does not declare. Product subsystems deserialize only their strict owned
//! block through tool-runtime-core's bounded frontmatter parser.

use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use tool_runtime_core::manifest_parser::{
    parse_skill_magician_extension, MAX_SKILL_MARKDOWN_BYTES,
};

use super::path_rewrite::resolve_skill_path;

pub const SKILL_MARKDOWN_FILE: &str = "SKILL.md";
const MAX_SKILLS_PER_SCAN: usize = 4_096;
const MAX_DIRECTORIES_PER_ROOT: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiscoveryIssueClass {
    ScanFailed,
    BrokenMarker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryIssue {
    pub skill_name: String,
    pub path: PathBuf,
    pub class: SkillDiscoveryIssueClass,
    pub message: String,
}

/// Discover precedence-winning SKILL.md files beneath ordered roots. A root
/// may itself be one skill directory or a directory containing skills.
pub fn discover_skill_markdown_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let (paths, issues) = discover_skill_markdown_paths_isolated(roots);
    if let Some(issue) = issues.first() {
        bail!(
            "skill discovery failed for `{}` at `{}`: {}",
            issue.skill_name,
            issue.path.display(),
            issue.message
        );
    }
    Ok(paths)
}

/// Discover precedence-winning skills while isolating malformed installations.
/// A present but invalid higher-precedence SKILL.md still claims the whole skill
/// name, so breaking a scoped marker can never reactivate a lower package.
pub fn discover_skill_markdown_paths_isolated(
    roots: &[PathBuf],
) -> (Vec<PathBuf>, Vec<SkillDiscoveryIssue>) {
    let mut paths = Vec::new();
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        if seen.len() >= MAX_SKILLS_PER_SCAN {
            issues.push(scan_issue(
                root,
                format!("skill discovery exceeds {MAX_SKILLS_PER_SCAN} packages"),
            ));
            break;
        }
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_dir() => {},
            Ok(_) => {
                issues.push(scan_issue(
                    root,
                    "skill root must be a regular directory".to_owned(),
                ));
                continue;
            },
            Err(error) => {
                issues.push(scan_issue(
                    root,
                    format!("reading skill root metadata failed: {error}"),
                ));
                continue;
            },
        }

        if inspect_skill_directory(root, &mut seen, &mut paths, &mut issues) {
            continue;
        }

        let mut directories = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                issues.push(scan_issue(
                    root,
                    format!("reading skill root failed: {error}"),
                ));
                continue;
            },
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    issues.push(scan_issue(
                        root,
                        format!("reading directory entry failed: {error}"),
                    ));
                    continue;
                },
            };
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {
                    if directories.len() >= MAX_DIRECTORIES_PER_ROOT {
                        issues.push(scan_issue(
                            root,
                            format!("skill root exceeds {MAX_DIRECTORIES_PER_ROOT} directories"),
                        ));
                        break;
                    }
                    directories.push(entry.path());
                },
                Ok(_) => {},
                Err(error) => issues.push(scan_issue(
                    &entry.path(),
                    format!("reading directory entry type failed: {error}"),
                )),
            }
        }
        directories.sort();
        for directory in directories {
            if seen.len() >= MAX_SKILLS_PER_SCAN {
                issues.push(scan_issue(
                    &directory,
                    format!("skill discovery exceeds {MAX_SKILLS_PER_SCAN} packages"),
                ));
                break;
            }
            inspect_skill_directory(&directory, &mut seen, &mut paths, &mut issues);
        }
    }
    (paths, issues)
}

/// Read and strictly deserialize one optional `metadata.magician.<extension>`
/// block from a precedence-winning SKILL.md.
pub fn load_skill_magician_extension<T>(skill_path: &Path, extension: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let source = read_bounded_skill_markdown(skill_path)?;
    parse_skill_magician_extension(&source, extension).with_context(|| {
        format!(
            "decoding metadata.magician.{extension} from `{}`",
            skill_path.display()
        )
    })
}

pub fn read_bounded_skill_markdown(skill_path: &Path) -> Result<String> {
    let resolved = resolve_skill_path(skill_path);
    let mut file = std::fs::File::open(&resolved)
        .with_context(|| format!("opening governed skill `{}`", skill_path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading governed skill metadata `{}`", skill_path.display()))?;
    if !metadata.is_file() {
        bail!(
            "governed skill must be a regular file: `{}`",
            skill_path.display()
        );
    }
    if metadata.len() > MAX_SKILL_MARKDOWN_BYTES as u64 {
        bail!(
            "governed skill `{}` exceeds {MAX_SKILL_MARKDOWN_BYTES} bytes",
            skill_path.display()
        );
    }
    let mut bytes =
        Vec::with_capacity(metadata.len().min(MAX_SKILL_MARKDOWN_BYTES as u64) as usize);
    Read::take(&mut file, MAX_SKILL_MARKDOWN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading governed skill `{}`", skill_path.display()))?;
    if bytes.len() > MAX_SKILL_MARKDOWN_BYTES {
        bail!(
            "governed skill `{}` grew beyond {MAX_SKILL_MARKDOWN_BYTES} bytes while being read",
            skill_path.display()
        );
    }
    String::from_utf8(bytes)
        .with_context(|| format!("governed skill `{}` is not UTF-8", skill_path.display()))
}

pub fn owning_skill_name(skill_path: &Path) -> Result<String> {
    skill_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("skill `{}` has no owning directory", skill_path.display()))
}

fn directory_name(directory: &Path) -> Result<String> {
    directory
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("skill directory `{}` has no name", directory.display()))
}

fn inspect_skill_directory(
    directory: &Path,
    seen: &mut BTreeSet<String>,
    paths: &mut Vec<PathBuf>,
    issues: &mut Vec<SkillDiscoveryIssue>,
) -> bool {
    let path = directory.join(SKILL_MARKDOWN_FILE);
    let marker_metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            let name = directory_name_lossy(directory);
            if seen.insert(name.clone()) {
                issues.push(SkillDiscoveryIssue {
                    skill_name: name,
                    path,
                    class: SkillDiscoveryIssueClass::ScanFailed,
                    message: format!("reading SKILL.md marker failed: {error}"),
                });
            }
            return true;
        },
    };
    let name = match directory_name(directory) {
        Ok(name) => name,
        Err(error) => {
            issues.push(scan_issue(directory, error.to_string()));
            return true;
        },
    };
    if !seen.insert(name.clone()) {
        return true;
    }

    let resolved = resolve_skill_path(&path);
    match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => paths.push(path),
        Ok(_) => issues.push(SkillDiscoveryIssue {
            skill_name: name,
            path,
            class: SkillDiscoveryIssueClass::ScanFailed,
            message: "SKILL.md marker is not a regular file".to_owned(),
        }),
        Err(error) => {
            let broken = marker_metadata.file_type().is_symlink()
                && error.kind() == std::io::ErrorKind::NotFound;
            issues.push(SkillDiscoveryIssue {
                skill_name: name,
                path,
                class: if broken {
                    SkillDiscoveryIssueClass::BrokenMarker
                } else {
                    SkillDiscoveryIssueClass::ScanFailed
                },
                message: if broken {
                    "installed SKILL.md symlink is broken".to_owned()
                } else {
                    format!("reading governed SKILL.md failed: {error}")
                },
            });
        },
    }
    true
}

fn scan_issue(path: &Path, message: String) -> SkillDiscoveryIssue {
    SkillDiscoveryIssue {
        skill_name: directory_name_lossy(path),
        path: path.to_path_buf(),
        class: SkillDiscoveryIssueClass::ScanFailed,
        message,
    }
}

fn directory_name_lossy(directory: &Path) -> String {
    directory
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-skill".to_owned())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Extension {
        value: String,
    }

    fn write_skill(root: &Path, name: &str, extension: Option<&str>) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let extension = extension
            .map(|value| format!("    content_reader:\n      value: {value}\n"))
            .unwrap_or_default();
        std::fs::write(
            directory.join(SKILL_MARKDOWN_FILE),
            format!(
                "---\nname: {name}\ndescription: fixture\nmetadata:\n  magician:\n{extension}    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {{bins: [fixture]}}\n      runtime: {{protocol: cli, command_prefix: []}}\n---\nBody.\n"
            ),
        )
        .unwrap();
        directory
    }

    #[test]
    fn higher_skill_without_extension_shadows_lower_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let higher = temporary.path().join("higher");
        let lower = temporary.path().join("lower");
        std::fs::create_dir_all(&higher).unwrap();
        std::fs::create_dir_all(&lower).unwrap();
        write_skill(&higher, "fixture", None);
        write_skill(&lower, "fixture", Some("lower"));

        let paths = discover_skill_markdown_paths(&[higher, lower]).unwrap();

        assert_eq!(paths.len(), 1);
        assert!(
            load_skill_magician_extension::<Extension>(&paths[0], "content_reader")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn direct_skill_root_loads_typed_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let skill = write_skill(temporary.path(), "fixture", Some("ready"));

        let paths = discover_skill_markdown_paths(&[skill]).unwrap();
        let extension = load_skill_magician_extension::<Extension>(&paths[0], "content_reader")
            .unwrap()
            .unwrap();

        assert_eq!(extension.value, "ready");
    }

    #[cfg(unix)]
    #[test]
    fn broken_higher_marker_shadows_lower_skill_fail_closed() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let higher = temporary.path().join("higher");
        let lower = temporary.path().join("lower");
        let broken = higher.join("fixture");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::create_dir_all(&lower).unwrap();
        symlink("missing-SKILL.md", broken.join(SKILL_MARKDOWN_FILE)).unwrap();
        write_skill(&lower, "fixture", Some("lower"));

        let (paths, issues) =
            discover_skill_markdown_paths_isolated(&[higher.clone(), lower.clone()]);

        assert!(paths.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].skill_name, "fixture");
        assert_eq!(issues[0].class, SkillDiscoveryIssueClass::BrokenMarker);
        assert!(discover_skill_markdown_paths(&[higher, lower]).is_err());
    }

    #[test]
    fn extension_loader_rejects_oversized_skill_before_reading_it() {
        use std::fs::OpenOptions;

        let temporary = tempfile::tempdir().unwrap();
        let skill = temporary.path().join("oversized");
        std::fs::create_dir_all(&skill).unwrap();
        let path = skill.join(SKILL_MARKDOWN_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_SKILL_MARKDOWN_BYTES as u64 + 1).unwrap();

        let error = load_skill_magician_extension::<Extension>(&path, "content_reader")
            .expect_err("oversized skill must fail before YAML parsing");

        assert!(error.to_string().contains("exceeds"));
    }
}
