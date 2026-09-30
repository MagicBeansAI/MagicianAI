//! In-place app skills (`app_in_place_skill_v1`).
//!
//! Most skills cannot be copied into a single private file: a Python wrapper
//! spawns a Node CLI from `node_modules`, a script imports packages beside
//! it, a CLI needs its runtime's libraries. Such a skill runs **in place**, in
//! MagicRun's exec-roots jail (`GovernedProcessJail::with_exec_roots`).
//!
//! Trust model (the owner's decision): installed skills and their runtimes
//! are trusted; the untrusted party is the app, which chooses the arguments.
//! The jail therefore confines authority and data flow, not the skill's code:
//!
//! - it reads and execs only the roots derived here (the skill package less
//!   its `config/`, the shared `node_modules`, and the install prefix of every
//!   runtime the skill names), plus `/bin` and `/usr/bin`;
//! - it writes only its fresh private workdir;
//! - it reaches only the hosts the owner granted the app, through the call's
//!   broker;
//! - it receives only the keys the owner ticked for this (app, tool) pair.
//!
//! Everything is derived from the skill package; there is no per-skill code.
//! The Magician data root, the home directory, `~/.ssh`, `~/Library`,
//! `~/Documents`, `~/Desktop`, `~/Downloads` and every other skill's directory
//! are forbidden as roots (MagicRun refuses a root equal to, containing or
//! inside any of them).
//!
//! Nothing is copied or pinned beyond the program's identity: the lock binds
//! the MagicRun exec-roots profile identity (roots, exclusions, `PATH`) and a
//! fingerprint of the skill package. A skill that changes after approval is
//! refused until the app is re-approved.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

use serde::Serialize;
use tool_runtime_core::governed_process_jail::{
    governed_process_jail_exec_roots_profile_identity, GovernedJailExecRoot,
    GovernedJailExecRoots, GovernedJailInterpreter, GovernedJailInterpreterKind,
    GovernedJailInterpreterVersion, GovernedProcessJailNetwork, GovernedProcessJailPlatform,
    MAX_GOVERNED_JAIL_EXEC_ROOTS, MAX_GOVERNED_JAIL_EXEC_SEARCH_PATH,
};

use super::models::{AppDigest, AppReference};

/// Lock reference prefix of an in-place artifact:
/// `artifact:app-os-jail-in-place:v1:<fingerprint-hex>:<identity-hex>`.
pub(crate) const APP_OS_JAIL_IN_PLACE_REF_PREFIX: &str = "artifact:app-os-jail-in-place:v1:";
pub(crate) const APP_OS_JAIL_IN_PLACE_PROFILE_V1: &str = "magician.app-os-jail-in-place.v1";
const FINGERPRINT_SCHEMA: &[u8] = b"magician.app-os-jail-skill-fingerprint.v1\0";
/// Bounds of the fingerprint walk. A skill package past them is refused
/// rather than fingerprinted partially.
pub(crate) const MAX_IN_PLACE_FINGERPRINT_FILES: usize = 4096;
pub(crate) const MAX_IN_PLACE_FINGERPRINT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_IN_PLACE_FINGERPRINT_DEPTH: usize = 32;
const MAX_IN_PLACE_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SIBLING_SKILLS: usize = 4096;
/// Excluded from the fingerprint wherever they appear. `__pycache__` is
/// never read in the jail: in-place runs set `PYTHONPYCACHEPREFIX` to a
/// directory in the private workdir, so a stale `.pyc` in the skill is not
/// imported. A package-local `node_modules`, `tests/` and `fixtures/` are
/// fingerprinted (the shared `skillshub/node_modules` is a runtime and is
/// not).
const FINGERPRINT_EXCLUDED_ANYWHERE: [&str; 3] = ["__pycache__", ".git", ".DS_Store"];
/// Excluded from the fingerprint at the package's top level only.
const FINGERPRINT_EXCLUDED_TOP_LEVEL: [&str; 1] = ["config"];
/// Where an in-place Python run keeps (and looks for) bytecode: relative to
/// the private workdir, never the skill's own `__pycache__`.
pub(crate) const IN_PLACE_PYCACHE_PREFIX: &str = ".magician-pycache";
/// The skill's own configuration directory: excluded from the jail's view of
/// the skill (it can hold the skill's `.env`).
pub(crate) const IN_PLACE_CONFIG_DIRECTORY: &str = "config";
/// Sensitive directories of the home directory that no root may be, contain
/// or lie inside.
/// Every other dot-directory directly in the home directory is fenced too.
const HOME_FORBIDDEN: [&str; 14] = [
    ".ssh",
    ".aws",
    ".config",
    ".gnupg",
    ".docker",
    ".kube",
    ".local/share",
    ".local/state",
    ".cargo",
    ".grok",
    "Library",
    "Documents",
    "Desktop",
    "Downloads",
];
const MAX_HOME_DOT_DIRECTORIES: usize = 1024;
/// The shared runtime directories the governed runtime puts on a skill's
/// `PATH`, relative to the directory that holds the skills
/// (`skillshub/node_modules/.bin`, `skillshub/.venv/bin`,
/// `skillshub/.node/bin`), searched before the host `PATH`.
const SHARED_RUNTIME_PATH: [&str; 3] = ["node_modules/.bin", ".venv/bin", ".node/bin"];
const SHARED_NODE_MODULES: &str = "node_modules";
const SYSTEM_EXEC_DIRECTORIES: [&str; 2] = ["/usr/bin", "/bin"];
const PYTHON3_SHEBANGS: [&str; 2] = ["#!/usr/bin/env python3", "#!/usr/bin/python3"];

/// Why an in-place skill cannot run. The text is shown to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppInPlaceSkillError {
    /// The skill package changed since the app was approved.
    Changed,
    /// The skill cannot run in place on this host, and why.
    Unavailable(String),
}

impl std::fmt::Display for AppInPlaceSkillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => formatter.write_str(
                "the skill changed since the app was approved; re-approve the app to use it",
            ),
            Self::Unavailable(reason) => formatter.write_str(reason),
        }
    }
}

fn unavailable(reason: impl Into<String>) -> AppInPlaceSkillError {
    AppInPlaceSkillError::Unavailable(reason.into())
}

/// Host facts the derivation depends on. Production reads them once per
/// derivation; tests supply temp fixtures.
#[derive(Debug, Clone)]
pub(crate) struct AppInPlaceHost {
    /// Magician data roots (the configured root and the default
    /// `~/MagicianNotes`). Never a root, never inside or around one.
    pub(crate) data_roots: Vec<PathBuf>,
    pub(crate) home: Option<PathBuf>,
    /// The host `PATH`, searched after the shared runtime directories.
    pub(crate) host_path: Vec<PathBuf>,
}

impl AppInPlaceHost {
    pub(crate) fn current() -> Self {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let mut data_roots =
            vec![crate::magician_v2::process_storage::runtime_root()];
        if let Some(home) = &home {
            data_roots.push(home.join("MagicianNotes"));
        }
        let host_path = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.is_absolute())
            .collect();
        Self {
            data_roots,
            home,
            host_path,
        }
    }
}

/// How the skill's entry point is launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppInPlaceLaunch {
    /// A native executable, exec'd in place.
    Native,
    /// A `#!` script whose interpreter is found inside the roots (or in
    /// `/usr/bin`, `/bin`); the kernel launches it.
    Script,
    /// An exact `python3` script, run under the host's pinned trusted
    /// interpreter as `<python3> -s -B <script>` so the packages beside it load.
    PinnedPython3,
}

/// One derived root, before MagicRun validates the whole declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppInPlaceRoot {
    pub(crate) path: PathBuf,
    pub(crate) excluded: Vec<PathBuf>,
    /// What the root is for, for the owner: `skill`, `node_modules`, or the
    /// companion it serves.
    pub(crate) purpose: String,
    /// A recognised versioned-runtime directory under the home directory
    /// (`~/.nvm/versions/node/X`, a uv Python install): the one kind of root
    /// allowed inside a fenced home directory.
    pub(crate) recognised_runtime: bool,
}

/// A validated in-place derivation for one action executable.
#[derive(Debug, Clone)]
pub(crate) struct AppInPlaceSkill {
    executable: String,
    #[cfg_attr(not(test), allow(dead_code))]
    package_root: PathBuf,
    program_directory: PathBuf,
    launch: AppInPlaceLaunch,
    roots: Vec<AppInPlaceRoot>,
    search_path: Vec<PathBuf>,
    forbidden: Vec<PathBuf>,
    fingerprint: AppDigest,
    program_digest: AppDigest,
    identity: AppDigest,
}

impl AppInPlaceSkill {
    #[cfg(test)]
    pub(crate) fn package_root(&self) -> &Path {
        &self.package_root
    }

    /// The skill's `bin/`, the only directory the governed contract `PATH`
    /// resolves the program in.
    pub(crate) fn program_directory(&self) -> &Path {
        &self.program_directory
    }

    pub(crate) fn launch(&self) -> AppInPlaceLaunch {
        self.launch
    }

    #[cfg(test)]
    pub(crate) fn roots(&self) -> &[AppInPlaceRoot] {
        &self.roots
    }

    #[cfg(test)]
    pub(crate) fn search_path(&self) -> &[PathBuf] {
        &self.search_path
    }

    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> &AppDigest {
        &self.fingerprint
    }

    /// BLAKE3 of the program bytes: the governed expected-executable digest.
    pub(crate) fn program_digest(&self) -> &AppDigest {
        &self.program_digest
    }

    #[cfg(test)]
    pub(crate) fn identity(&self) -> &AppDigest {
        &self.identity
    }

    /// The lock reference: the fingerprint (so a changed skill is told apart
    /// from a changed runtime) and the whole identity.
    pub(crate) fn revision_ref(&self) -> Option<AppReference> {
        AppReference::parse(format!(
            "{APP_OS_JAIL_IN_PLACE_REF_PREFIX}{}:{}",
            digest_hex(&self.fingerprint)?,
            digest_hex(&self.identity)?
        ))
        .ok()
    }

    /// The validated MagicRun declaration, optionally with one more root: a
    /// fresh private directory holding this call's credential files. That
    /// root is per call and is not part of the locked identity.
    pub(crate) fn exec_roots(
        &self,
        private_root: Option<&Path>,
    ) -> Result<GovernedJailExecRoots, AppInPlaceSkillError> {
        let mut roots = self
            .roots
            .iter()
            .map(|root| {
                root.excluded
                    .iter()
                    .fold(GovernedJailExecRoot::new(&root.path), |declared, excluded| {
                        declared.excluding(excluded)
                    })
            })
            .collect::<Vec<_>>();
        if let Some(private_root) = private_root {
            roots.push(GovernedJailExecRoot::new(private_root));
        }
        GovernedJailExecRoots::new_with_forbidden(
            roots,
            self.search_path.clone(),
            self.forbidden.clone(),
        )
        .map_err(|_| unavailable("the skill's exec roots were refused by the jail"))
    }
}

/// `(fingerprint hex, identity hex)` of an in-place lock reference.
pub(crate) fn parse_in_place_revision_ref(reference: &AppReference) -> Option<(&str, &str)> {
    let rest = reference
        .as_str()
        .strip_prefix(APP_OS_JAIL_IN_PLACE_REF_PREFIX)?;
    let (fingerprint, identity) = rest.split_once(':')?;
    (is_hex64(fingerprint) && is_hex64(identity)).then_some((fingerprint, identity))
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn digest_hex(digest: &AppDigest) -> Option<&str> {
    digest.as_str().strip_prefix("blake3:").filter(|hex| is_hex64(hex))
}

/// The package directory of an installed skill: the directory holding the
/// real `SKILL.md`. A runtime install is often a directory of links into the
/// package (with `config` linked into the data root); the package, not the
/// install, is what runs and what is fingerprinted.
pub(crate) fn skill_package_root(source_directory: &Path) -> Result<PathBuf, AppInPlaceSkillError> {
    let document = fs::canonicalize(source_directory.join("SKILL.md"))
        .map_err(|_| unavailable("the skill's SKILL.md could not be resolved"))?;
    document
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| unavailable("the skill has no package directory"))
}

/// Whether `bin/<executable>` of the installed skill is a native executable
/// or an exact `python3` script that the existing single-file kinds already
/// cover, or needs to run in place. `None` when there is no such file.
pub(crate) fn read_entry_head(source_directory: &Path, executable: &str) -> Option<Vec<u8>> {
    let program = fs::canonicalize(source_directory.join("bin").join(executable)).ok()?;
    let mut file = fs::File::open(program).ok()?;
    let mut head = vec![0_u8; 256];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return None,
        }
    }
    head.truncate(filled);
    Some(head)
}

/// The companions a skill names beyond its entry point: `requires.bins` less
/// the executable itself and `python3` (which the pinned interpreter
/// provides). A skill with none, whose entry point is one native file or one
/// exact `python3` script, keeps the existing single-file kinds.
pub(crate) fn declared_companions<'a>(
    requires_bins: &'a BTreeSet<String>,
    executable: &str,
) -> BTreeSet<&'a str> {
    requires_bins
        .iter()
        .map(String::as_str)
        .filter(|name| *name != executable && *name != "python3")
        .collect()
}

/// Derive, validate and identify the in-place run of `bin/<executable>`.
/// Creates the skill package's `config/` (0700) when absent, because MagicRun
/// excludes only existing real directories.
pub(crate) fn derive_in_place_skill(
    source_directory: &Path,
    executable: &str,
    requires_bins: &BTreeSet<String>,
    host: &AppInPlaceHost,
) -> Result<AppInPlaceSkill, AppInPlaceSkillError> {
    let platform = jail_platform()?;
    let package_root = skill_package_root(source_directory)?;
    let skills_root = package_root
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| unavailable("the skill package has no parent directory"))?;
    if host
        .home
        .as_deref()
        .is_some_and(|home| package_root == home || home.starts_with(&package_root))
    {
        return Err(unavailable("the skill package is the home directory"));
    }
    let program = fs::canonicalize(source_directory.join("bin").join(executable))
        .map_err(|_| unavailable(format!("the skill has no `bin/{executable}`")))?;
    if !program.starts_with(&package_root) {
        return Err(unavailable(format!(
            "`bin/{executable}` resolves outside the skill package"
        )));
    }
    let metadata = fs::metadata(&program)
        .map_err(|_| unavailable(format!("`bin/{executable}` is unreadable")))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IN_PLACE_PROGRAM_BYTES {
        return Err(unavailable(format!(
            "`bin/{executable}` is not a bounded regular file"
        )));
    }
    #[cfg(unix)]
    if metadata.mode() & 0o111 == 0 {
        return Err(unavailable(format!("`bin/{executable}` is not executable")));
    }
    let program_directory = program
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| unavailable("the program has no directory"))?;
    let head = read_entry_head(source_directory, executable)
        .ok_or_else(|| unavailable(format!("`bin/{executable}` is unreadable")))?;
    let (launch, script_interpreter) = entry_launch(&head)?;
    let interpreter = match launch {
        AppInPlaceLaunch::PinnedPython3 => {
            let pinned = GovernedJailInterpreter::python3_for_host().map_err(|_| {
                unavailable("no trusted host Python 3 is available to run this script skill")
            })?;
            Some((pinned.kind(), pinned.version()))
        },
        AppInPlaceLaunch::Native | AppInPlaceLaunch::Script => None,
    };

    // Everything the child may need by name: the companions and the
    // interpreter of a `#!` entry point, each resolved exactly as the
    // governed runtime resolves a skill's `PATH`.
    let mut wanted = declared_companions(requires_bins, executable)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(interpreter) = script_interpreter {
        if !wanted.contains(&interpreter) {
            wanted.push(interpreter);
        }
    }
    let home = host
        .home
        .as_deref()
        .map(|home| fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()));
    let mut runtime_path = SHARED_RUNTIME_PATH
        .iter()
        .map(|relative| skills_root.join(relative))
        .collect::<Vec<_>>();
    runtime_path.extend(host.host_path.iter().cloned());

    let mut roots = vec![AppInPlaceRoot {
        path: package_root.clone(),
        excluded: vec![PathBuf::from(IN_PLACE_CONFIG_DIRECTORY)],
        purpose: "skill".to_owned(),
        recognised_runtime: false,
    }];
    let mut search_path = Vec::new();
    let skill_bin = package_root.join("bin");
    if skill_bin.is_dir() {
        search_path.push(skill_bin);
    }
    let node_modules = skills_root.join(SHARED_NODE_MODULES);
    let mut include_node_modules = package_root.join("package.json").is_file();
    let mut runtimes: Vec<(AppInPlaceRoot, PathBuf)> = Vec::new();
    for name in &wanted {
        let Some((found_in, resolved)) = resolve_on_path(name, &runtime_path) else {
            // A companion this host does not carry adds nothing to reach;
            // the tool reports its own absence when it needs it.
            continue;
        };
        if SYSTEM_EXEC_DIRECTORIES
            .iter()
            .any(|system| resolved.starts_with(system))
        {
            continue;
        }
        if resolved.starts_with(&node_modules) {
            include_node_modules = true;
            continue;
        }
        // Found through a shared runtime directory (`skillshub/.venv/bin`)
        // but installed elsewhere (the venv's `python` links into a uv
        // Python store): both are roots, and `PATH` names the shared one.
        let found_canonical = fs::canonicalize(&found_in).unwrap_or_else(|_| found_in.clone());
        if found_canonical.starts_with(&skills_root)
            && !resolved.starts_with(&skills_root)
            && !found_canonical.starts_with(&package_root)
            && !found_canonical.starts_with(&node_modules)
        {
            if let Some(shared) = runtime_root_for(
                name,
                &found_canonical,
                &found_canonical.join(name),
                &skills_root,
                &package_root,
                home.as_deref(),
            )? {
                runtimes.push(shared);
            }
        }
        if let Some(runtime) = runtime_root_for(
            name,
            &found_in,
            &resolved,
            &skills_root,
            &package_root,
            home.as_deref(),
        )? {
            runtimes.push(runtime);
        }
    }
    // A Python venv among the runtimes needs the install its interpreter
    // links into (a uv or framework Python).
    let venv_interpreters = runtimes
        .iter()
        .filter(|(root, _)| root.path.join("pyvenv.cfg").is_file())
        .filter_map(|(root, _)| {
            ["python3", "python"].iter().find_map(|interpreter| {
                let link = root.path.join("bin").join(interpreter);
                let resolved = fs::canonicalize(&link).ok()?;
                (!resolved.starts_with(&root.path)).then(|| (root.path.join("bin"), resolved))
            })
        })
        .collect::<Vec<_>>();
    for (found_in, resolved) in venv_interpreters {
        if SYSTEM_EXEC_DIRECTORIES
            .iter()
            .any(|system| resolved.starts_with(system))
        {
            continue;
        }
        if let Some((root, _)) = runtime_root_for(
            "python",
            &found_in,
            &resolved,
            &skills_root,
            &package_root,
            home.as_deref(),
        )? {
            // The venv's own `bin` stays the PATH entry: the install root
            // (the whole uv store, say) need not have a `bin` of its own.
            runtimes.push((root, found_in));
        }
    }
    if include_node_modules {
        let node_modules = fs::canonicalize(&node_modules).map_err(|_| {
            unavailable("the skill needs Node packages but the shared node_modules is missing")
        })?;
        let bin = node_modules.join(".bin");
        roots.push(AppInPlaceRoot {
            path: node_modules,
            excluded: Vec::new(),
            purpose: SHARED_NODE_MODULES.to_owned(),
            recognised_runtime: false,
        });
        if bin.is_dir() {
            search_path.push(bin);
        }
    }
    for (root, entry) in runtimes {
        if let Some(existing) = roots
            .iter_mut()
            .find(|existing| root.path.starts_with(&existing.path))
        {
            // A runtime inside an existing root (a second companion of the
            // same install) needs only its PATH entry.
            if existing.path == package_root && root.path != package_root {
                return Err(unavailable(format!(
                    "the runtime for `{}` lies inside the skill package",
                    root.purpose
                )));
            }
        } else if roots
            .iter()
            .any(|existing| existing.path.starts_with(&root.path))
        {
            return Err(unavailable(format!(
                "the runtime `{}` for `{}` would contain the skill or another runtime",
                root.path.display(),
                root.purpose
            )));
        } else {
            roots.push(root);
        }
        if entry.starts_with(&package_root) {
            continue;
        }
        if !search_path.contains(&entry) {
            search_path.push(entry);
        }
    }
    if roots.len() > MAX_GOVERNED_JAIL_EXEC_ROOTS
        || search_path.len() > MAX_GOVERNED_JAIL_EXEC_SEARCH_PATH
    {
        return Err(unavailable("the skill needs more runtime roots than a jail admits"));
    }
    ensure_config_directory(&package_root)?;
    let forbidden = forbidden_paths(host, &skills_root, &package_root)?;
    let forbidden = fence_roots(&roots, &forbidden, home.as_deref())?;
    let declared = |roots: &[AppInPlaceRoot], search_path: Vec<PathBuf>| {
        GovernedJailExecRoots::new_with_forbidden(
            roots.iter().map(|root| {
                root.excluded
                    .iter()
                    .fold(GovernedJailExecRoot::new(&root.path), |declared, excluded| {
                        declared.excluding(excluded)
                    })
            }),
            search_path,
            forbidden.clone(),
        )
    };
    let declaration = match declared(&roots, search_path.clone()) {
        Ok(declaration) => declaration,
        Err(_) => return Err(diagnose_refusal(&roots, &search_path, &declared)),
    };
    let fingerprint = skill_fingerprint(&package_root)?;
    let program_digest = file_digest(&program)?;
    let identity = in_place_identity(
        executable,
        launch,
        interpreter,
        &fingerprint,
        &program_digest,
        platform,
        &declaration,
    )?;
    Ok(AppInPlaceSkill {
        executable: executable.to_owned(),
        package_root,
        program_directory,
        launch,
        roots,
        search_path,
        forbidden,
        fingerprint,
        program_digest,
        identity,
    })
}

/// Re-derive at run time and require the locked identity. A changed skill
/// package is `Changed`; a changed runtime, root or interpreter is
/// `Unavailable` with the reason.
pub(crate) fn verify_in_place_skill(
    source_directory: &Path,
    executable: &str,
    requires_bins: &BTreeSet<String>,
    expected_ref: &AppReference,
    expected_program_digest: &AppDigest,
    host: &AppInPlaceHost,
) -> Result<AppInPlaceSkill, AppInPlaceSkillError> {
    let (fingerprint, identity) = parse_in_place_revision_ref(expected_ref)
        .ok_or_else(|| unavailable("the locked in-place reference is malformed"))?;
    let derived = derive_in_place_skill(source_directory, executable, requires_bins, host)?;
    if digest_hex(&derived.fingerprint) != Some(fingerprint)
        || &derived.program_digest != expected_program_digest
    {
        return Err(AppInPlaceSkillError::Changed);
    }
    if digest_hex(&derived.identity) != Some(identity) || derived.executable != executable {
        return Err(unavailable(
            "the skill's runtime, roots or interpreter changed since the app was approved; \
             re-approve the app",
        ));
    }
    Ok(derived)
}

fn jail_platform() -> Result<GovernedProcessJailPlatform, AppInPlaceSkillError> {
    if cfg!(target_os = "macos") {
        Ok(GovernedProcessJailPlatform::MacosSandboxExec)
    } else if cfg!(target_os = "linux") {
        Ok(GovernedProcessJailPlatform::LinuxBubblewrap)
    } else {
        Err(unavailable("in-place skills need a macOS or Linux jail"))
    }
}

/// The launch of an entry point and, for a `#!` script, the interpreter it
/// names (a bare name after `/usr/bin/env`, or an absolute path).
fn entry_launch(head: &[u8]) -> Result<(AppInPlaceLaunch, Option<String>), AppInPlaceSkillError> {
    if head
        .get(..4)
        .and_then(|magic| <[u8; 4]>::try_from(magic).ok())
        .is_some_and(native_magic)
    {
        return Ok((AppInPlaceLaunch::Native, None));
    }
    if !head.starts_with(b"#!") {
        return Err(unavailable(
            "the entry point is neither a native executable nor a `#!` script",
        ));
    }
    let line_end = head
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| unavailable("the entry point's `#!` line is too long"))?;
    let line = std::str::from_utf8(&head[..line_end])
        .map_err(|_| unavailable("the entry point's `#!` line is not text"))?
        .trim_end_matches('\r');
    if PYTHON3_SHEBANGS.contains(&line) {
        return Ok((AppInPlaceLaunch::PinnedPython3, None));
    }
    let words = line[2..].split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["/usr/bin/env", name] if is_plain_name(name) && !name.starts_with('-') => {
            Ok((AppInPlaceLaunch::Script, Some((*name).to_owned())))
        },
        [path] | [path, _] if path.starts_with('/') && !path.starts_with("/usr/bin/env") => {
            Ok((AppInPlaceLaunch::Script, Some((*path).to_owned())))
        },
        _ => Err(unavailable(format!(
            "the entry point's `#!` line (`{line}`) is not a plain interpreter"
        ))),
    }
}

fn native_magic(magic: [u8; 4]) -> bool {
    magic == [0x7f, b'E', b'L', b'F']
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
}

fn is_plain_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// First `<dir>/<name>` on `path` that resolves to a non-empty executable
/// regular file: `(dir, canonical file)`. An absolute interpreter path is
/// taken as is.
fn resolve_on_path(name: &str, path: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    let executable = |candidate: &Path| -> Option<PathBuf> {
        let resolved = fs::canonicalize(candidate).ok()?;
        let metadata = fs::metadata(&resolved).ok()?;
        #[cfg(unix)]
        let runnable = metadata.mode() & 0o111 != 0;
        #[cfg(not(unix))]
        let runnable = true;
        (metadata.is_file() && metadata.len() > 0 && runnable).then_some(resolved)
    };
    if name.starts_with('/') {
        let candidate = Path::new(name);
        let resolved = executable(candidate)?;
        return Some((candidate.parent()?.to_path_buf(), resolved));
    }
    path.iter().find_map(|directory| {
        let resolved = executable(&directory.join(name))?;
        Some((directory.clone(), resolved))
    })
}

/// Versioned-runtime layouts under the home directory whose one version
/// directory (or, for uv's Python store, the store) may be a root even though
/// its parent is fenced off: each holds runtime installs, never a tool's data
/// or credentials.
const HOME_RUNTIME_LAYOUTS: [&[&str]; 7] = [
    &[".nvm", "versions", "node", "*"],
    &[".pyenv", "versions", "*"],
    &[".asdf", "installs", "*", "*"],
    // The uv Python store as a whole: a venv's interpreter links through the
    // store's minor-version alias (`cpython-3.12-…` → `cpython-3.12.13-…`),
    // and the sandbox must read that alias to reach the install. The store
    // holds only Python installs.
    &[".local", "share", "uv", "python"],
    &[".local", "share", "uv", "tools", "*"],
    &[".local", "share", "pipx", "venvs", "*"],
    &[".local", "pipx", "venvs", "*"],
];

/// The recognised versioned-runtime directory under `home` holding `path`.
fn home_runtime_layout(home: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(home).ok()?;
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    HOME_RUNTIME_LAYOUTS.iter().find_map(|layout| {
        (components.len() > layout.len()
            && layout
                .iter()
                .zip(&components)
                .all(|(expected, actual)| *expected == "*" || expected == actual))
        .then(|| home.join(components[..layout.len()].iter().collect::<PathBuf>()))
    })
}

/// An install prefix root, less the data (`var`) and configuration (`etc`)
/// directories it holds.
fn prefix_root(path: PathBuf, purpose: &str, recognised: bool) -> AppInPlaceRoot {
    let excluded = ["var", "etc"]
        .into_iter()
        .filter(|relative| {
            fs::symlink_metadata(path.join(relative))
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        })
        .map(PathBuf::from)
        .collect();
    AppInPlaceRoot {
        path,
        excluded,
        purpose: purpose.to_owned(),
        recognised_runtime: recognised,
    }
}

/// The install root of one resolved runtime and the `PATH` entry the child
/// finds it by. `Ok(None)` when the file is part of the skill itself.
fn runtime_root_for(
    name: &str,
    found_in: &Path,
    resolved: &Path,
    skills_root: &Path,
    package_root: &Path,
    home: Option<&Path>,
) -> Result<Option<(AppInPlaceRoot, PathBuf)>, AppInPlaceSkillError> {
    if resolved.starts_with(package_root) {
        return Ok(None);
    }
    // Never another skill: a runtime with any skill (`SKILL.md`) above it
    // would expose that skill's files, including a linked `config/`.
    if let Some(skill) = resolved
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.join("SKILL.md").exists())
    {
        return Err(unavailable(format!(
            "`{name}` lives in another skill (`{}`); a skill may not reach another skill's files",
            skill.display()
        )));
    }
    let found_in = fs::canonicalize(found_in).unwrap_or_else(|_| found_in.to_path_buf());
    let parent = resolved
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| unavailable(format!("`{name}` has no directory")))?;
    let entry_inside = |root: &Path| {
        if found_in.starts_with(root) {
            found_in.clone()
        } else if root.join("bin").is_dir() && parent.starts_with(root) {
            root.join("bin")
        } else {
            parent.clone()
        }
    };
    // A shared runtime directory beside the skills (`.node`, `.venv`).
    if let Ok(relative) = resolved.strip_prefix(skills_root) {
        let top = relative
            .components()
            .next()
            .map(|component| skills_root.join(component))
            .ok_or_else(|| unavailable(format!("`{name}` resolves to the skills directory")))?;
        let entry = entry_inside(&top);
        return Ok(Some((prefix_root(top, name, false), entry)));
    }
    // Under the home directory only a recognised versioned-runtime layout is
    // a prefix; anything else is at most the file's own directory, and
    // never the home directory or one of its direct children.
    if let Some(home) = home.filter(|home| resolved.starts_with(home)) {
        if let Some(layout) = home_runtime_layout(home, resolved) {
            let entry = entry_inside(&layout);
            return Ok(Some((prefix_root(layout, name, true), entry)));
        }
        if parent == home || parent.parent() == Some(home) {
            return Err(unavailable(format!(
                "`{name}` is installed directly in `{}`, which cannot be exposed to the jail",
                parent.display()
            )));
        }
        let entry = parent.clone();
        return Ok(Some((
            AppInPlaceRoot {
                path: parent,
                excluded: Vec::new(),
                purpose: name.to_owned(),
                recognised_runtime: false,
            },
            entry,
        )));
    }
    // Homebrew: kegs link each other through `opt/`, so a keg is not usable
    // alone; the prefix is the root, less its data (`var`) and configuration
    // (`etc`). MagicRun's trust checks decide whether that prefix is safe.
    if let Some(prefix) = resolved
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.join("Cellar").is_dir() && ancestor.join("bin").is_dir())
    {
        return Ok(Some((
            prefix_root(prefix.to_path_buf(), name, false),
            prefix.join("bin"),
        )));
    }
    // A macOS framework install (python.org): `…/X.framework/Versions/N`.
    if let Some(version) = resolved.ancestors().skip(1).find(|ancestor| {
        ancestor
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "Versions"))
            && ancestor
                .parent()
                .and_then(Path::parent)
                .and_then(Path::extension)
                .is_some_and(|extension| extension == "framework")
    }) {
        return Ok(Some((
            prefix_root(version.to_path_buf(), name, false),
            version.join("bin"),
        )));
    }
    // Any other install prefix (`/usr/local`, `/opt/tool`): `<prefix>` of
    // `<prefix>/bin/<name>` less its `etc` and `var`, or the file's own
    // directory.
    let (root, entry) = if parent.file_name().is_some_and(|name| name == "bin") {
        (
            parent
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| parent.clone()),
            parent.clone(),
        )
    } else {
        (parent.clone(), parent.clone())
    };
    Ok(Some((prefix_root(root, name, false), entry)))
}

#[cfg(unix)]
fn ensure_config_directory(package_root: &Path) -> Result<(), AppInPlaceSkillError> {
    let config = package_root.join(IN_PLACE_CONFIG_DIRECTORY);
    match fs::symlink_metadata(&config) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(unavailable(
            "the skill's `config` is not a real directory, so it cannot be kept out of the jail",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
            .mode(0o700)
            .create(&config)
            .map_err(|_| unavailable("the skill's `config` directory could not be created")),
        Err(_) => Err(unavailable("the skill's `config` directory is unreadable")),
    }
}

#[cfg(not(unix))]
fn ensure_config_directory(_package_root: &Path) -> Result<(), AppInPlaceSkillError> {
    Err(unavailable("in-place skills need a unix host"))
}

fn forbidden_paths(
    host: &AppInPlaceHost,
    skills_root: &Path,
    package_root: &Path,
) -> Result<Vec<PathBuf>, AppInPlaceSkillError> {
    let mut forbidden = host.data_roots.clone();
    if let Some(home) = &host.home {
        forbidden.extend(HOME_FORBIDDEN.iter().map(|relative| home.join(relative)));
        // Every dot-directory directly in the home directory holds some
        // tool's state or credentials (`.claude`, `.grok`, `.codex`, …).
        // `.local` itself holds runtimes; its data and state are listed above.
        if let Ok(entries) = fs::read_dir(home) {
            for entry in entries.flatten().take(MAX_HOME_DOT_DIRECTORIES) {
                let name = entry.file_name();
                let is_dot = name.to_str().is_some_and(|name| name.starts_with('.') && name != ".local");
                if is_dot && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let path = home.join(name);
                    if !forbidden.contains(&path) {
                        forbidden.push(path);
                    }
                }
            }
        }
    }
    let entries = fs::read_dir(skills_root)
        .map_err(|_| unavailable("the skills directory is unreadable"))?;
    let mut siblings = 0_usize;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.join("SKILL.md").exists()
            && fs::canonicalize(&path).ok().as_deref() != Some(package_root)
        {
            siblings += 1;
            if siblings > MAX_SIBLING_SKILLS {
                return Err(unavailable("too many skills beside this one to fence off"));
            }
            forbidden.push(path);
        }
    }
    Ok(forbidden)
}

/// Magician's own fence before MagicRun's: no root may be, contain or lie
/// inside a forbidden path or be the home directory or one of its direct
/// children, except a recognised versioned-runtime directory inside a
/// fenced home directory (`~/.nvm/versions/node/X`, a uv Python install).
/// Returns the forbidden list for MagicRun, less the entries only such a
/// runtime lies inside.
fn fence_roots(
    roots: &[AppInPlaceRoot],
    forbidden: &[PathBuf],
    home: Option<&Path>,
) -> Result<Vec<PathBuf>, AppInPlaceSkillError> {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut passed = Vec::with_capacity(forbidden.len());
    for fence in forbidden {
        let fence_path = canonical(fence);
        let mut excused = false;
        for root in roots {
            let inside = root.path.starts_with(&fence_path);
            if inside && root.recognised_runtime {
                excused = true;
                continue;
            }
            if inside || fence_path.starts_with(&root.path) {
                return Err(unavailable(format!(
                    "`{}` (for {}) is, contains or lies inside `{}`, which the jail may never \
                     read",
                    root.path.display(),
                    root.purpose,
                    fence.display()
                )));
            }
        }
        if !excused {
            passed.push(fence.clone());
        }
    }
    if let Some(home) = home {
        for root in roots {
            if root.path == home || root.path.parent() == Some(home) || home.starts_with(&root.path)
            {
                return Err(unavailable(format!(
                    "`{}` (for {}) is the home directory or directly inside it",
                    root.path.display(),
                    root.purpose
                )));
            }
        }
    }
    Ok(passed)
}

/// Name the root or `PATH` entry MagicRun refused, for the owner.
fn diagnose_refusal(
    roots: &[AppInPlaceRoot],
    search_path: &[PathBuf],
    declared: &impl Fn(
        &[AppInPlaceRoot],
        Vec<PathBuf>,
    ) -> Result<GovernedJailExecRoots, tool_runtime_core::governed_process_jail::GovernedProcessJailError>,
) -> AppInPlaceSkillError {
    for root in roots {
        if declared(std::slice::from_ref(root), Vec::new()).is_err() {
            return unavailable(format!(
                "`{}` (for {}) fails the jail's exec-root checks: a root must be an existing \
                 directory owned by root or you and not writable by group or others, with \
                 trusted parent directories, never your home, data or another skill's \
                 directory, and its exclusions must be real directories",
                root.path.display(),
                root.purpose
            ));
        }
    }
    for entry in search_path {
        let Some(root) = roots.iter().find(|root| entry.starts_with(&root.path)) else {
            continue;
        };
        if declared(std::slice::from_ref(root), vec![entry.clone()]).is_err() {
            return unavailable(format!(
                "`{}` (for {}) is writable by group or others, so another user could plant a \
                 program there; the jail refuses it",
                entry.display(),
                root.purpose
            ));
        }
    }
    unavailable("the skill's exec roots overlap or exceed the jail's bounds")
}

#[derive(Serialize)]
struct InPlaceIdentity<'a> {
    profile: &'static str,
    executable: &'a str,
    launch: AppInPlaceLaunch,
    interpreter: Option<String>,
    fingerprint: &'a AppDigest,
    program_digest: &'a AppDigest,
    exec_roots_denied: String,
    exec_roots_brokered: String,
}

fn in_place_identity(
    executable: &str,
    launch: AppInPlaceLaunch,
    interpreter: Option<(GovernedJailInterpreterKind, GovernedJailInterpreterVersion)>,
    fingerprint: &AppDigest,
    program_digest: &AppDigest,
    platform: GovernedProcessJailPlatform,
    declaration: &GovernedJailExecRoots,
) -> Result<AppDigest, AppInPlaceSkillError> {
    let profile = |network| {
        governed_process_jail_exec_roots_profile_identity(platform, network, interpreter, declaration)
            .to_string()
    };
    let identity = serde_json::to_value(InPlaceIdentity {
        profile: APP_OS_JAIL_IN_PLACE_PROFILE_V1,
        executable,
        launch,
        interpreter: interpreter.map(|(kind, version)| format!("{kind:?}:{version}")),
        fingerprint,
        program_digest,
        exec_roots_denied: profile(GovernedProcessJailNetwork::Denied),
        exec_roots_brokered: profile(GovernedProcessJailNetwork::BrokeredEgress),
    })
    .map_err(|_| unavailable("the in-place identity could not be computed"))?;
    AppDigest::blake3_canonical_json(&identity)
        .map_err(|_| unavailable("the in-place identity could not be computed"))
}

fn file_digest(path: &Path) -> Result<AppDigest, AppInPlaceSkillError> {
    let file = fs::File::open(path).map_err(|_| unavailable("the program is unreadable"))?;
    let mut hasher = blake3::Hasher::new();
    let copied = std::io::copy(&mut file.take(MAX_IN_PLACE_PROGRAM_BYTES + 1), &mut hasher)
        .map_err(|_| unavailable("the program is unreadable"))?;
    if copied > MAX_IN_PLACE_PROGRAM_BYTES {
        return Err(unavailable("the program is too large"));
    }
    AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map_err(|_| unavailable("the program digest is invalid"))
}

/// One fingerprinted entry's cheap identity: what changes when its bytes,
/// size, mode or link target change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FingerprintStat {
    relative: String,
    kind: u8,
    size: u64,
    mode: u32,
    modified_ns: i128,
    changed_ns: i128,
    inode: u64,
    device: u64,
}

type FingerprintCache = BTreeMap<PathBuf, (Vec<FingerprintStat>, AppDigest)>;

fn fingerprint_cache() -> &'static Mutex<FingerprintCache> {
    static CACHE: OnceLock<Mutex<FingerprintCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

const MAX_FINGERPRINT_CACHE_ENTRIES: usize = 256;

/// Digest over the skill package: every file's relative path, size, mode
/// and content hash, and every link's target, excluding only the top-level
/// `config/`, `__pycache__` (never read in the jail), `.git` and `.DS_Store`.
/// A package-local `node_modules`, `tests/` and `fixtures/` count. Bounded in
/// files and bytes. Re-hashing is skipped while every entry's (path, size,
/// mode, mtime, ctime, inode, device) is unchanged.
pub(crate) fn skill_fingerprint(package_root: &Path) -> Result<AppDigest, AppInPlaceSkillError> {
    let stats = fingerprint_stats(package_root)?;
    if let Some((cached, digest)) = fingerprint_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(package_root)
    {
        if cached == &stats {
            return Ok(digest.clone());
        }
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_SCHEMA);
    let mut part = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    };
    for stat in &stats {
        let path = package_root.join(&stat.relative);
        part(stat.relative.as_bytes());
        part(&[stat.kind]);
        part(&stat.size.to_be_bytes());
        part(&stat.mode.to_be_bytes());
        match stat.kind {
            b'f' => {
                let file = fs::File::open(&path)
                    .map_err(|_| unavailable("a skill file changed while it was fingerprinted"))?;
                let mut content = blake3::Hasher::new();
                let copied = std::io::copy(&mut file.take(stat.size + 1), &mut content)
                    .map_err(|_| unavailable("a skill file is unreadable"))?;
                if copied != stat.size {
                    return Err(unavailable("a skill file changed while it was fingerprinted"));
                }
                part(content.finalize().as_bytes());
            },
            _ => {
                let target = fs::read_link(&path)
                    .map_err(|_| unavailable("a skill link changed while it was fingerprinted"))?;
                part(target.as_os_str().as_encoded_bytes());
            },
        }
    }
    let digest = AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map_err(|_| unavailable("the skill fingerprint is invalid"))?;
    if fingerprint_stats(package_root)? != stats {
        return Err(unavailable("the skill changed while it was fingerprinted"));
    }
    let mut cache = fingerprint_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if cache.len() >= MAX_FINGERPRINT_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(package_root.to_path_buf(), (stats, digest.clone()));
    Ok(digest)
}

#[cfg(unix)]
fn fingerprint_stats(package_root: &Path) -> Result<Vec<FingerprintStat>, AppInPlaceSkillError> {
    let mut stats = Vec::new();
    let mut total = 0_u64;
    let mut pending = vec![(package_root.to_path_buf(), String::new(), 0_usize)];
    while let Some((directory, relative, depth)) = pending.pop() {
        if depth > MAX_IN_PLACE_FINGERPRINT_DEPTH {
            return Err(unavailable("the skill package is nested too deeply to fingerprint"));
        }
        let mut entries = fs::read_dir(&directory)
            .map_err(|_| unavailable("the skill package is unreadable"))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| unavailable("the skill package is unreadable"))?;
        entries.sort();
        for name in entries {
            let name = name
                .into_string()
                .map_err(|_| unavailable("a skill file name is not UTF-8"))?;
            if FINGERPRINT_EXCLUDED_ANYWHERE.contains(&name.as_str())
                || (depth == 0 && FINGERPRINT_EXCLUDED_TOP_LEVEL.contains(&name.as_str()))
            {
                continue;
            }
            let entry_relative = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let path = directory.join(&name);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| unavailable("a skill file is unreadable"))?;
            let kind = if metadata.file_type().is_symlink() {
                b'l'
            } else if metadata.is_dir() {
                pending.push((path, entry_relative, depth + 1));
                continue;
            } else if metadata.is_file() {
                b'f'
            } else {
                return Err(unavailable(format!(
                    "`{entry_relative}` in the skill is neither a file, a directory nor a link"
                )));
            };
            total = total.saturating_add(metadata.len());
            if stats.len() >= MAX_IN_PLACE_FINGERPRINT_FILES || total > MAX_IN_PLACE_FINGERPRINT_BYTES
            {
                return Err(unavailable(format!(
                    "the skill package exceeds {MAX_IN_PLACE_FINGERPRINT_FILES} files or \
                     {MAX_IN_PLACE_FINGERPRINT_BYTES} bytes, so it cannot be fingerprinted"
                )));
            }
            stats.push(FingerprintStat {
                relative: entry_relative,
                kind,
                size: metadata.len(),
                mode: metadata.mode() & 0o7777,
                modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                    + i128::from(metadata.mtime_nsec()),
                changed_ns: i128::from(metadata.ctime()) * 1_000_000_000
                    + i128::from(metadata.ctime_nsec()),
                inode: metadata.ino(),
                device: metadata.dev(),
            });
        }
    }
    stats.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(stats)
}

#[cfg(not(unix))]
fn fingerprint_stats(_package_root: &Path) -> Result<Vec<FingerprintStat>, AppInPlaceSkillError> {
    Err(unavailable("in-place skills need a unix host"))
}

/// A fresh 0700 per-call directory for credential files a tool reads from a
/// config directory. It is granted to the jail read-only as one extra root
/// and removed with the call.
const CREDENTIAL_ROOT_PREFIX: &str = "magician-app-credentials-";

pub(crate) struct AppPrivateCredentialRoot {
    path: PathBuf,
}

impl AppPrivateCredentialRoot {
    #[cfg(unix)]
    pub(crate) fn create() -> Result<Self, AppInPlaceSkillError> {
        let base = fs::canonicalize(std::env::temp_dir())
            .map_err(|_| unavailable("the private credential directory could not be created"))?;
        let path = base.join(format!(
            "{CREDENTIAL_ROOT_PREFIX}{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| unavailable("the private credential directory could not be created"))?;
        Ok(Self { path })
    }

    #[cfg(not(unix))]
    pub(crate) fn create() -> Result<Self, AppInPlaceSkillError> {
        Err(unavailable("in-place skills need a unix host"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Boot: remove private credential directories a Magician process that is no
/// longer running left behind (it died mid-call, so a key file may still be
/// there). Only this user's `magician-app-credentials-<pid>-*` directories in
/// the temp directory whose owning process is gone are touched. Returns how
/// many were removed.
pub fn sweep_stale_credential_roots() -> usize {
    let Ok(base) = fs::canonicalize(std::env::temp_dir()) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(&base) else {
        return 0;
    };
    #[cfg(unix)]
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let mut removed = 0;
    for entry in entries.flatten() {
        let owner = entry.file_name().to_str().and_then(|name| {
            name.strip_prefix(CREDENTIAL_ROOT_PREFIX)?
                .split_once('-')?
                .0
                .parse::<i32>()
                .ok()
        });
        // SAFETY: signal 0 only probes whether the process exists.
        let is_ours = owner.is_some_and(|pid| {
            pid > 0
                && u32::try_from(pid).ok() != Some(std::process::id())
                && unsafe { libc::kill(pid, 0) } != 0
        });
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        #[cfg(unix)]
        let owned = metadata.uid() == euid;
        #[cfg(not(unix))]
        let owned = true;
        if is_ours && owned && metadata.is_dir() && !metadata.file_type().is_symlink() {
            if fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

impl Drop for AppPrivateCredentialRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

    const NODE_SCRIPT: &str = "#!/usr/bin/env node\nconsole.log('hi')\n";
    const PYTHON_SCRIPT: &str = "#!/usr/bin/env python3\nprint('hi')\n";

    struct Fixture {
        _directory: tempfile::TempDir,
        root: PathBuf,
        skills: PathBuf,
        host: AppInPlaceHost,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let skills = root.join("skillshub");
        fs::create_dir_all(&skills).unwrap();
        let data = root.join("data");
        fs::create_dir_all(&data).unwrap();
        Fixture {
            _directory: directory,
            root: root.clone(),
            skills,
            host: AppInPlaceHost {
                data_roots: vec![data],
                home: None,
                host_path: Vec::new(),
            },
        }
    }

    fn write_executable(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn skill(fixture: &Fixture, name: &str, entry: &str) -> PathBuf {
        let package = fixture.skills.join(name);
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        write_executable(&package.join("bin").join(name), entry);
        package
    }

    fn bins(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_node_skill_gets_its_package_node_modules_and_vendored_runtime() {
        let fixture = fixture();
        let package = skill(&fixture, "web", NODE_SCRIPT);
        fs::write(package.join("package.json"), "{}").unwrap();
        write_executable(&fixture.skills.join(".node/bin/node"), "#!/bin/sh\n");
        write_executable(
            &fixture.skills.join("node_modules/cli/dist/cli.mjs"),
            NODE_SCRIPT,
        );
        fs::create_dir_all(fixture.skills.join("node_modules/.bin")).unwrap();
        symlink(
            "../cli/dist/cli.mjs",
            fixture.skills.join("node_modules/.bin/cli"),
        )
        .unwrap();
        // Another skill beside it stays fenced off.
        skill(&fixture, "other", NODE_SCRIPT);

        let derived =
            derive_in_place_skill(&package, "web", &bins(&["web", "cli", "node"]), &fixture.host)
                .unwrap();
        let roots = derived
            .roots()
            .iter()
            .map(|root| (root.path.clone(), root.excluded.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            roots,
            [
                (package.clone(), vec![PathBuf::from("config")]),
                (fixture.skills.join("node_modules"), Vec::new()),
                (fixture.skills.join(".node"), Vec::new()),
            ]
        );
        assert_eq!(
            derived.search_path(),
            [
                package.join("bin"),
                fixture.skills.join("node_modules/.bin"),
                fixture.skills.join(".node/bin"),
            ]
        );
        assert_eq!(derived.launch(), AppInPlaceLaunch::Script);
        assert!(package.join("config").is_dir(), "config/ is created to be excluded");
        assert_eq!(
            fs::metadata(package.join("config")).unwrap().mode() & 0o777,
            0o700
        );
        let reference = derived.revision_ref().unwrap();
        let (fingerprint, identity) = parse_in_place_revision_ref(&reference).unwrap();
        assert_eq!(Some(fingerprint), digest_hex(derived.fingerprint()));
        assert_eq!(Some(identity), digest_hex(derived.identity()));
    }

    #[test]
    fn a_homebrew_runtime_is_its_prefix_less_var_and_etc() {
        let fixture = fixture();
        let package = skill(&fixture, "brewed", NODE_SCRIPT);
        let prefix = fixture.root.join("homebrew");
        write_executable(&prefix.join("Cellar/node/25.6.0/bin/node"), "#!/bin/sh\n");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::create_dir_all(prefix.join("var/db")).unwrap();
        fs::create_dir_all(prefix.join("etc")).unwrap();
        symlink("../Cellar/node/25.6.0/bin/node", prefix.join("bin/node")).unwrap();
        let host = AppInPlaceHost {
            host_path: vec![prefix.join("bin")],
            ..fixture.host.clone()
        };
        let derived = derive_in_place_skill(&package, "brewed", &bins(&["brewed"]), &host).unwrap();
        let brew = derived
            .roots()
            .iter()
            .find(|root| root.path == prefix)
            .expect("the prefix is the root, not the keg");
        assert_eq!(brew.excluded, [PathBuf::from("var"), PathBuf::from("etc")]);
        assert!(derived.search_path().contains(&prefix.join("bin")));
    }

    #[test]
    fn a_group_writable_homebrew_prefix_is_refused_with_its_reason() {
        let fixture = fixture();
        let package = skill(&fixture, "brewed", NODE_SCRIPT);
        let prefix = fixture.root.join("homebrew");
        write_executable(&prefix.join("Cellar/node/25.6.0/bin/node"), "#!/bin/sh\n");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        symlink("../Cellar/node/25.6.0/bin/node", prefix.join("bin/node")).unwrap();
        // Homebrew's own layout: `bin/` is admin-group-writable.
        fs::set_permissions(prefix.join("bin"), fs::Permissions::from_mode(0o775)).unwrap();
        let host = AppInPlaceHost {
            host_path: vec![prefix.join("bin")],
            ..fixture.host.clone()
        };
        let error = derive_in_place_skill(&package, "brewed", &bins(&["brewed"]), &host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("writable by group or others"), "{error}");
        assert!(error.contains("homebrew/bin"), "{error}");
    }

    #[test]
    fn a_python_org_framework_runtime_is_its_version_directory() {
        let fixture = fixture();
        let package = skill(&fixture, "framework", "#!/usr/bin/env python3.14\nprint(1)\n");
        let version = fixture
            .root
            .join("Library/Frameworks/Python.framework/Versions/3.14");
        write_executable(&version.join("bin/python3.14"), "#!/bin/sh\n");
        let host = AppInPlaceHost {
            host_path: vec![version.join("bin")],
            ..fixture.host.clone()
        };
        let derived =
            derive_in_place_skill(&package, "framework", &bins(&["framework"]), &host).unwrap();
        assert!(derived.roots().iter().any(|root| root.path == version));
        assert!(derived.search_path().contains(&version.join("bin")));
    }

    #[test]
    fn data_roots_home_directories_and_other_skills_are_never_roots() {
        let fixture = fixture();
        // A companion inside another skill.
        let package = skill(&fixture, "reacher", NODE_SCRIPT);
        let other = skill(&fixture, "victim", NODE_SCRIPT);
        let host = AppInPlaceHost {
            host_path: vec![other.join("bin")],
            ..fixture.host.clone()
        };
        let error = derive_in_place_skill(&package, "reacher", &bins(&["reacher", "victim"]), &host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("another skill"), "{error}");

        // A skill installed inside the Magician data root.
        let data_skills = fixture.host.data_roots[0].join("skills");
        fs::create_dir_all(&data_skills).unwrap();
        let inside = data_skills.join("inside");
        fs::create_dir_all(&inside).unwrap();
        fs::write(inside.join("SKILL.md"), "---\nname: inside\n---\n").unwrap();
        write_executable(&inside.join("bin/inside"), NODE_SCRIPT);
        let error = derive_in_place_skill(&inside, "inside", &bins(&["inside"]), &fixture.host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("may never read"), "{error}");

        // A runtime whose prefix is a sensitive home directory.
        let home = fixture.root.join("home");
        let documents = home.join("Documents/tools");
        write_executable(&documents.join("bin/tool"), "#!/bin/sh\n");
        let host = AppInPlaceHost {
            home: Some(home.clone()),
            host_path: vec![documents.join("bin")],
            ..fixture.host.clone()
        };
        let error = derive_in_place_skill(&package, "reacher", &bins(&["reacher", "tool"]), &host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Documents"), "{error}");
    }

    /// A fixture host with a home directory holding tool state.
    fn with_home(fixture: &Fixture) -> (PathBuf, AppInPlaceHost) {
        let home = fixture.root.join("home");
        for state in [".grok/auth", ".claude", ".cargo/registry", ".local/share/opencode"] {
            fs::create_dir_all(home.join(state)).unwrap();
        }
        let host = AppInPlaceHost {
            home: Some(home.clone()),
            ..fixture.host.clone()
        };
        (home, host)
    }

    fn roots_of(derived: &AppInPlaceSkill) -> Vec<PathBuf> {
        derived.roots().iter().map(|root| root.path.clone()).collect()
    }

    #[test]
    fn a_tool_in_local_bin_exposes_only_that_directory() {
        let fixture = fixture();
        let (home, host) = with_home(&fixture);
        write_executable(&home.join(".local/bin/tool"), "#!/bin/sh\n");
        let package = skill(&fixture, "local", NODE_SCRIPT.replace("node", "tool").as_str());
        let host = AppInPlaceHost {
            host_path: vec![home.join(".local/bin")],
            ..host
        };
        let derived = derive_in_place_skill(&package, "local", &bins(&["local"]), &host).unwrap();
        assert!(roots_of(&derived).contains(&home.join(".local/bin")));
        assert!(!roots_of(&derived).contains(&home.join(".local")));
    }

    #[test]
    fn a_tool_under_a_fenced_home_directory_is_refused() {
        let fixture = fixture();
        let (home, host) = with_home(&fixture);
        for (bin, name) in [(".grok/bin", "grok"), (".cargo/bin", "cargo-tool"), ("bin", "mine")] {
            write_executable(&home.join(bin).join(name), "#!/bin/sh\n");
            let package = skill(&fixture, &format!("uses-{name}"), NODE_SCRIPT);
            let host = AppInPlaceHost {
                host_path: vec![home.join(bin)],
                ..host.clone()
            };
            let skill_name = format!("uses-{name}");
            let error = derive_in_place_skill(
                &package,
                &skill_name,
                &bins(&[skill_name.as_str(), name]),
                &host,
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("may never read") || error.contains("directly in"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn a_plain_prefix_is_a_root_less_its_etc_and_var() {
        let fixture = fixture();
        let prefix = fixture.root.join("usr-local");
        write_executable(&prefix.join("bin/tool"), "#!/bin/sh\n");
        fs::create_dir_all(prefix.join("etc")).unwrap();
        fs::create_dir_all(prefix.join("var/db")).unwrap();
        let package = skill(&fixture, "prefixed", NODE_SCRIPT);
        let host = AppInPlaceHost {
            host_path: vec![prefix.join("bin")],
            ..fixture.host.clone()
        };
        let derived =
            derive_in_place_skill(&package, "prefixed", &bins(&["prefixed", "tool"]), &host)
                .unwrap();
        let root = derived
            .roots()
            .iter()
            .find(|root| root.path == prefix)
            .unwrap();
        assert_eq!(root.excluded, [PathBuf::from("var"), PathBuf::from("etc")]);
    }

    #[test]
    fn a_versioned_runtime_under_home_is_its_version_directory() {
        let fixture = fixture();
        let (home, host) = with_home(&fixture);
        let version = home.join(".nvm/versions/node/v25.2.0");
        write_executable(&version.join("bin/node"), "#!/bin/sh\n");
        let package = skill(&fixture, "nvm", NODE_SCRIPT);
        let host = AppInPlaceHost {
            host_path: vec![version.join("bin")],
            ..host
        };
        let derived = derive_in_place_skill(&package, "nvm", &bins(&["nvm"]), &host).unwrap();
        assert!(roots_of(&derived).contains(&version));
        assert!(!roots_of(&derived).contains(&home.join(".nvm")));
    }

    #[test]
    fn a_runtime_below_any_skill_is_refused() {
        let fixture = fixture();
        let farm = fixture.root.join("farm/other-skill");
        fs::create_dir_all(&farm).unwrap();
        fs::write(farm.join("SKILL.md"), "---\nname: other\n---\n").unwrap();
        write_executable(&farm.join("bin/tool"), "#!/bin/sh\n");
        let package = skill(&fixture, "farmed", NODE_SCRIPT);
        let host = AppInPlaceHost {
            host_path: vec![farm.join("bin")],
            ..fixture.host.clone()
        };
        let error = derive_in_place_skill(&package, "farmed", &bins(&["farmed", "tool"]), &host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("another skill"), "{error}");
    }

    #[test]
    fn a_skillshub_venv_and_its_uv_python_are_roots() {
        let fixture = fixture();
        let (home, host) = with_home(&fixture);
        let install = home.join(".local/share/uv/python/cpython-3.12.4-macos-aarch64-none");
        write_executable(&install.join("bin/python3.12"), "#!/bin/sh\n");
        fs::create_dir_all(install.join("lib/python3.12")).unwrap();
        let venv = fixture.skills.join(".venv");
        fs::create_dir_all(venv.join("lib/python3.12/site-packages/yt_dlp")).unwrap();
        fs::write(venv.join("pyvenv.cfg"), "home = x\n").unwrap();
        symlink(install.join("bin/python3.12"), venv.join("bin/python")).unwrap_or_else(|_| {
            fs::create_dir_all(venv.join("bin")).unwrap();
            symlink(install.join("bin/python3.12"), venv.join("bin/python")).unwrap();
        });
        write_executable(&venv.join("bin/yt-dlp"), "#!/usr/bin/env python\n");
        let package = skill(&fixture, "video", PYTHON_SCRIPT);
        let derived = match derive_in_place_skill(
            &package,
            "video",
            &bins(&["video", "python3", "yt-dlp"]),
            &host,
        ) {
            Ok(derived) => derived,
            Err(error) => {
                assert!(error.to_string().contains("Python 3"), "{error}");
                return;
            },
        };
        let roots = roots_of(&derived);
        assert!(roots.contains(&venv), "{roots:?}");
        // uv's Python store (its version aliases link into the install).
        assert!(roots.contains(&home.join(".local/share/uv/python")), "{roots:?}");
        assert!(install.starts_with(home.join(".local/share/uv/python")));
        assert!(derived.search_path().contains(&venv.join("bin")));
        // The store has no `bin`; the venv's `bin` is the PATH entry.
        assert!(!derived.search_path().contains(&home.join(".local/share/uv/python/bin")));
        // The rest of ~/.local/share stays fenced.
        assert!(!roots.iter().any(|root| root == &home.join(".local/share")));
    }

    #[test]
    fn an_installed_link_farm_runs_from_its_package_directory() {
        let fixture = fixture();
        let package = skill(&fixture, "linked", NODE_SCRIPT);
        let install = fixture.host.data_roots[0].join("scopes/skills/linked");
        fs::create_dir_all(install.join("bin")).unwrap();
        symlink(package.join("SKILL.md"), install.join("SKILL.md")).unwrap();
        symlink(package.join("bin/linked"), install.join("bin/linked")).unwrap();
        let derived =
            derive_in_place_skill(&install, "linked", &bins(&["linked"]), &fixture.host).unwrap();
        assert_eq!(derived.package_root(), package);
        assert_eq!(derived.program_directory(), package.join("bin"));
    }

    #[test]
    fn a_changed_skill_is_refused_until_re_approved() {
        let fixture = fixture();
        let package = skill(&fixture, "tool", PYTHON_SCRIPT);
        fs::create_dir_all(package.join("config")).unwrap();
        fs::write(package.join("config/.env"), "SECRET=1").unwrap();
        fs::create_dir_all(package.join("tests")).unwrap();
        let derived = match derive_in_place_skill(&package, "tool", &bins(&["tool"]), &fixture.host)
        {
            Ok(derived) => derived,
            // No trusted host Python on this machine: the reason says so.
            Err(error) => {
                assert!(error.to_string().contains("Python 3"), "{error}");
                return;
            },
        };
        assert_eq!(derived.launch(), AppInPlaceLaunch::PinnedPython3);
        let reference = derived.revision_ref().unwrap();
        let verify = || {
            verify_in_place_skill(
                &package,
                "tool",
                &bins(&["tool"]),
                &reference,
                derived.program_digest(),
                &fixture.host,
            )
        };
        verify().expect("unchanged");
        // Excluded paths never change the fingerprint.
        fs::write(package.join("config/.env"), "SECRET=2").unwrap();
        fs::create_dir_all(package.join("bin/__pycache__")).unwrap();
        fs::write(package.join("bin/__pycache__/x.pyc"), "stale").unwrap();
        verify().expect("config and __pycache__ are not fingerprinted");
        // A package's own tests, fixtures and node_modules are.
        for extra in ["tests/test_x.py", "fixtures/f.json", "node_modules/pkg/index.js"] {
            let path = package.join(extra);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "x").unwrap();
            assert_eq!(verify().unwrap_err(), AppInPlaceSkillError::Changed, "{extra}");
            fs::remove_file(&path).unwrap();
            verify().expect("restored");
        }
        // Any other file does.
        fs::write(package.join("helper.py"), "X = 1\n").unwrap();
        assert_eq!(verify().unwrap_err(), AppInPlaceSkillError::Changed);
        fs::remove_file(package.join("helper.py")).unwrap();
        verify().expect("restored");
        // A mode change alone is a change.
        fs::set_permissions(package.join("SKILL.md"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(verify().unwrap_err(), AppInPlaceSkillError::Changed);
    }

    #[test]
    fn the_fingerprint_is_bounded() {
        let fixture = fixture();
        let package = skill(&fixture, "big", NODE_SCRIPT);
        for index in 0..=MAX_IN_PLACE_FINGERPRINT_FILES {
            fs::write(package.join(format!("f{index}")), b"").unwrap();
        }
        let error = skill_fingerprint(&package).unwrap_err().to_string();
        assert!(error.contains("cannot be fingerprinted"), "{error}");
    }

    #[test]
    fn a_crashed_calls_credential_directory_is_swept_at_boot() {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        // A pid that cannot be running (above the kernel's maximum).
        let leftover = base.join(format!("{CREDENTIAL_ROOT_PREFIX}99999999-test"));
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("config.json"), "{}").unwrap();
        let live = AppPrivateCredentialRoot::create().unwrap();
        assert!(sweep_stale_credential_roots() >= 1);
        assert!(!leftover.exists());
        assert!(live.path().exists(), "this process's own directory is kept");
    }

    #[test]
    fn a_private_credential_root_is_0700_and_removed_on_drop() {
        let root = AppPrivateCredentialRoot::create().unwrap();
        let path = root.path().to_path_buf();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
        fs::write(path.join("config.json"), "{}").unwrap();
        drop(root);
        assert!(!path.exists());
    }

    #[test]
    fn only_plain_interpreters_are_accepted() {
        assert_eq!(
            entry_launch(b"#!/usr/bin/env python3\n").unwrap().0,
            AppInPlaceLaunch::PinnedPython3
        );
        assert_eq!(
            entry_launch(b"#!/usr/bin/env node\n").unwrap(),
            (AppInPlaceLaunch::Script, Some("node".to_owned()))
        );
        assert_eq!(
            entry_launch(b"#!/bin/bash -e\n").unwrap(),
            (AppInPlaceLaunch::Script, Some("/bin/bash".to_owned()))
        );
        assert_eq!(
            entry_launch(b"\xcf\xfa\xed\xfe\0\0\0\0").unwrap().0,
            AppInPlaceLaunch::Native
        );
        for refused in [
            &b"#!/usr/bin/env -S node --flag\n"[..],
            b"#!/usr/bin/env node --flag\n",
            b"print(1)\n",
            b"#!relative\n",
        ] {
            assert!(entry_launch(refused).is_err(), "{refused:?}");
        }
    }
}
