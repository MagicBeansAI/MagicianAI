//! Goal workspace access: who may read and write
//! `<notes space>/Workspaces/<workspace>/` (`docs/features/goal-workspaces.md`,
//! "Who can see a workspace").
//!
//! The rules, owner-approved 2026-10-20:
//!
//! - A workspace is visible only to the admin agent (`personal-assistant`,
//!   Magican; always a member) and the agents listed in `members:` in the
//!   frontmatter of `Workspaces/<ws>/README.md`. Everyone else can neither
//!   read nor write anything in it.
//! - A private goal (`visibility: private`, or no `visibility`) is readable and
//!   writable only by the admin and, when it is a member, the agent named in
//!   the goal's `owner:` (`owner: user` means the admin only). `internal` and
//!   `public` goals are open to every member.
//! - Fail closed, so the rules cannot be rewritten by the agents they bind:
//!   only the admin writes a workspace's `README.md`, creates, renames or
//!   deletes entries directly under `Workspaces/`, or changes a workspace's
//!   `Goals/` folder itself. A missing or unreadable README, frontmatter that
//!   does not parse, or a README without `members` leaves the workspace to the
//!   admin alone. A goal file whose frontmatter does not parse counts as a
//!   private goal with no owner. A goal `owner` grants nothing beyond that
//!   goal, and only to a member.
//! - The acting agent comes from runtime identity
//!   ([`super::native_executors::ACTING_AGENT`]), never from tool arguments.
//!   No identity is a non-member.
//! - Delegation carries read access (owner-approved 2026-10-26): a delegate
//!   may READ whatever the agent that delegated to it may read — its
//!   workspaces, and its private goals — for that delegated work. Writes stay
//!   the delegate's own. The delegators come from server-set identity only
//!   ([`read_delegators_for`]): the suspended owners of an in-run delegation
//!   and the verified source of a delegated child execution.
//!
//! Every agent path checks here: the file executor and the `read_file` /
//! `write_file` / `edit_file` / `apply_patch` family (staging and the
//! diff-approval apply), `grep` and `glob`, the notes tools, and the OS boxes
//! around the shell and the coding engines ([`WorkspaceDenies`]). God mode
//! does not lift it. The owner's own notes API and UI do not pass through it.
//!
//! The same module holds the always-on fence around the notes hybrid search
//! index (`<scope>/notes/hybrid-index`): it embeds every note, workspace
//! goals included, so no agent reads it directly.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

/// The agent that administers every workspace when config names none.
pub const DEFAULT_ADMIN_AGENT: &str = "personal-assistant";
/// The folder under a notes space that holds goal workspaces.
pub const WORKSPACES_DIR: &str = "Workspaces";
const README_FILE: &str = "README.md";
const GOALS_DIR: &str = "Goals";
/// Frontmatter is read from at most this many leading bytes.
const FRONTMATTER_LIMIT: u64 = 4096;
/// Entries one workspace scan may visit. Past it the workspace is the
/// admin's alone (fail closed) until it shrinks.
const MAX_SCAN_ENTRIES: usize = 20_000;
/// Workspaces read under one `Workspaces/` folder for an OS profile.
const MAX_WORKSPACES_PER_ROOT: usize = 2_000;

/// SBPL rule that keeps every agent shell out of the notes hybrid index.
pub const HYBRID_INDEX_SBPL_RULE: &str = concat!(
    "(deny file-read* file-write* (regex #\"/scopes/[^/]+/[^/]+/notes/hybrid-index(/|$)\"))\n",
    "(deny file-read* file-write* (regex #\"/goal_workspace_reads\\.json(\\.tmp)?$\"))\n",
);

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// `execution.goal_workspace_access`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSettings {
    pub enabled: bool,
    pub admin_agent: String,
}

impl Default for AccessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            admin_agent: DEFAULT_ADMIN_AGENT.to_string(),
        }
    }
}

static SETTINGS: RwLock<Option<AccessSettings>> = RwLock::new(None);

/// Apply `execution.goal_workspace_access` (boot and config reload). A blank
/// admin falls back to [`DEFAULT_ADMIN_AGENT`].
pub fn configure(enabled: bool, admin_agent: &str) {
    let admin_agent = admin_agent.trim();
    let settings = AccessSettings {
        enabled,
        admin_agent: if admin_agent.is_empty() {
            DEFAULT_ADMIN_AGENT.to_string()
        } else {
            admin_agent.to_string()
        },
    };
    if !enabled {
        tracing::warn!(
            "[GOAL-WORKSPACES] execution.goal_workspace_access.enabled is false: every agent can read and write every goal workspace its sandbox reaches"
        );
    }
    let mut guard = SETTINGS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(settings);
}

fn settings() -> AccessSettings {
    #[cfg(any(test, feature = "test-fixtures"))]
    if let Some(settings) =
        TEST_OVERRIDE.with(|cell| cell.borrow().as_ref().map(|o| o.settings.clone()))
    {
        return settings;
    }
    SETTINGS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

/// The admin agent: always a member of every workspace, and the only agent
/// that may change membership or the folder layout.
pub fn admin_agent() -> String {
    settings().admin_agent
}

fn is_admin_with(settings: &AccessSettings, agent: Option<&str>) -> bool {
    agent.is_some_and(|agent| agent == settings.admin_agent)
}

// ---------------------------------------------------------------------------
// Owner-started VibeDev sessions act as the admin (owner decision 2026-10-20)
// ---------------------------------------------------------------------------

/// VibeDev tasks the owner started from the VibeDev API, as `(principal,
/// workspace, task_id)`. In process only: an agent can neither add one (the
/// only writer is the authenticated owner request) nor forge one on disk.
/// After a restart a resumed session is fenced by its agent's own identity
/// again (fail closed).
static OWNER_VIBEDEV_TASKS: LazyLock<RwLock<std::collections::HashSet<(String, String, String)>>> =
    LazyLock::new(|| RwLock::new(std::collections::HashSet::new()));

/// Record a VibeDev task the owner started (the VibeDev API's start
/// handler, after authenticating the owner's request).
pub fn record_owner_started_vibedev_task(principal: &str, workspace: &str, task_id: &str) {
    OWNER_VIBEDEV_TASKS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert((
            principal.to_string(),
            workspace.to_string(),
            task_id.to_string(),
        ));
}

/// Whether the owner started `task_id` as a VibeDev session (its root frame
/// acts as the admin, [`acting_agent_for`]).
pub fn is_owner_started_session(principal: &str, workspace: &str, task_id: &str) -> bool {
    is_owner_started_vibedev_task(principal, workspace, task_id)
}

fn is_owner_started_vibedev_task(principal: &str, workspace: &str, task_id: &str) -> bool {
    OWNER_VIBEDEV_TASKS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&(
            principal.to_string(),
            workspace.to_string(),
            task_id.to_string(),
        ))
}

/// The agent a run acts as for goal workspace access: the run's active owner
/// (before its agent id, so a delegated child acts as itself), except that
/// an owner-started VibeDev session acts as the admin. That holds only for
/// the session's own root frame: a VibeDev run (`vibedev_run`) whose task the
/// owner started through the VibeDev API, in its root execution, with no
/// suspended owner above it. Engineers it delegates to (a child execution or
/// an in-context owner frame) and any coding run an agent starts elsewhere
/// keep their own identity.
pub fn acting_agent_for(ctx: &super::agentic::AgenticContext) -> Option<String> {
    let own = super::native_executors::acting_agent_from_context(
        ctx.active_owner_agent_id.as_deref(),
        ctx.agent_id.as_deref(),
    );
    let root_frame = ctx.owner_stack.is_empty()
        && match (
            ctx.root_execution_id.as_deref(),
            ctx.execution_id.as_deref(),
        ) {
            (Some(root), Some(execution)) => root == execution,
            (None, _) => true,
            (Some(_), None) => false,
        };
    let owner_started = ctx.vibedev_run
        && root_frame
        && match (
            ctx.principal.as_deref(),
            ctx.workspace.as_deref(),
            ctx.task_id.as_deref(),
        ) {
            (Some(principal), Some(workspace), Some(task_id)) => {
                is_owner_started_vibedev_task(principal, workspace, task_id)
            },
            _ => false,
        };
    if owner_started {
        Some(admin_agent())
    } else {
        own
    }
}

/// Most delegators a run carries read access from.
const MAX_READ_DELEGATORS: usize = 16;

/// The agents whose goal workspace READ access this run carries, nearest
/// first, transitively: the suspended owners that delegated to the active
/// owner within this run, then every agent up the execution's durable read
/// lineage (`AgenticContext::read_lineage`: its record's `read_lineage`, set
/// by the server from a verified delegation or the parent's record, then its
/// `delegation_chain`), and the delegated source the server recorded on the
/// invocation (`invocation_context_override`: delegation surface, delegated
/// source, checked against the source's durable authorization revision).
/// Never from tool arguments, the model or a checkpoint. Excludes the acting
/// agent itself.
pub fn read_delegators_for(ctx: &super::agentic::AgenticContext) -> Vec<String> {
    use crate::magician_v2::agents::{InvocationSourceKind, InvocationSurface};
    let acting = acting_agent_for(ctx);
    let mut delegators: Vec<String> = Vec::new();
    let mut add = |agent: &str| {
        let agent = agent.trim();
        if !agent.is_empty()
            && acting.as_deref() != Some(agent)
            && !delegators.iter().any(|known| known == agent)
            && delegators.len() < MAX_READ_DELEGATORS
        {
            delegators.push(agent.to_string());
        }
    };
    for owner in ctx.owner_stack.iter().rev() {
        add(owner);
    }
    // The owner's own VibeDev session acts as the admin in its root frame;
    // engineers working beneath it in that run read as the admin lets them.
    if ctx.vibedev_run
        && !ctx.owner_stack.is_empty()
        && ctx.root_execution_id.is_some()
        && ctx.root_execution_id == ctx.execution_id
        && match (
            ctx.principal.as_deref(),
            ctx.workspace.as_deref(),
            ctx.task_id.as_deref(),
        ) {
            (Some(principal), Some(workspace), Some(task_id)) => {
                is_owner_started_vibedev_task(principal, workspace, task_id)
            },
            _ => false,
        }
    {
        add(&admin_agent());
    }
    for ancestor in ctx.read_lineage.iter().rev() {
        add(ancestor);
    }
    if let Some(invocation) = ctx.invocation_context_override.as_ref() {
        // Only an invocation addressed to the acting agent names its delegator.
        // A Plane grant restamps a delegated run's surface to Plane and keeps
        // its delegated source; a run the Plane started itself is Direct.
        // Delegations only. A handover carries reads too (owner decision
        // 2026-10-26), but through the lineage the server records when a
        // verified handover starts a run, and through the owner stack in a
        // run — never through a Handover invocation: giving control BACK
        // (yield-back, a pipeline stage finishing) builds one with the
        // returning agent as its source, which would hand reads upward.
        let delegated = matches!(
            invocation.surface,
            InvocationSurface::Delegation | InvocationSurface::Plane
        ) && invocation.source_kind == InvocationSourceKind::Delegated;
        if delegated && acting.as_deref() == Some(invocation.target_agent_id.as_str()) {
            if let Some(source) = invocation.source_agent_id.as_deref() {
                add(source);
            }
        }
    }
    delegators
}

// ---------------------------------------------------------------------------
// Workspaces roots
// ---------------------------------------------------------------------------

#[cfg_attr(test, allow(dead_code))]
static ROOTS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// Register `Workspaces/` folders (absolute paths; they need not exist yet).
/// Boot registers every scope's notes spaces and any configured sandbox root
/// named `Workspaces`; saving notes settings and every notes tool call
/// register theirs again, so a space configured later is fenced from its
/// first use.
pub fn register_workspaces_roots<I: IntoIterator<Item = PathBuf>>(roots: I) {
    let fresh: Vec<PathBuf> = roots
        .into_iter()
        .filter(|root| root.is_absolute())
        .collect();
    if fresh.is_empty() {
        return;
    }
    // Unit tests share one process: a root a test registers stays on its
    // thread, so it never fences another test's shell.
    #[cfg(test)]
    TEST_REGISTERED.with(|cell| {
        let mut registered = cell.borrow_mut();
        for root in fresh {
            if !registered.contains(&root) {
                registered.push(root);
            }
        }
    });
    #[cfg(not(test))]
    {
        let mut guard = ROOTS
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for root in fresh {
            let resolved = super::native_executors::resolve_for_policy(&root);
            if !guard
                .iter()
                .any(|known| *known == root || *known == resolved)
            {
                tracing::info!(root = %root.display(), "[GOAL-WORKSPACES] fencing goal workspaces");
                guard.push(root);
            }
        }
    }
}

/// The runtime's scope layout, so roots of scopes created after boot (or a
/// provider switched in their notes settings) are found without a notes
/// tool call: re-read at most every [`LAYOUT_REFRESH`].
#[cfg_attr(test, allow(dead_code))]
static NOTES_LAYOUT: std::sync::Mutex<
    Option<(
        crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
        Option<std::time::Instant>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg_attr(test, allow(dead_code))]
const LAYOUT_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);

/// Register the runtime's scope layout (boot) and read its notes spaces now.
pub fn register_notes_layout(
    layout: crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
) {
    let spaces =
        crate::magician_v2::notes::NotesSettingsStore::with_workspace_layout(layout.clone())
            .notes_space_roots_blocking();
    register_notes_spaces(spaces);
    *NOTES_LAYOUT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some((layout, Some(std::time::Instant::now())));
}

/// Re-read the layout's notes spaces when due. One caller at a time; the
/// others go on with the roots already known.
#[cfg_attr(test, allow(dead_code))]
fn refresh_layout_roots() {
    let Ok(mut guard) = NOTES_LAYOUT.try_lock() else {
        return;
    };
    let Some((layout, refreshed_at)) = guard.as_mut() else {
        return;
    };
    if refreshed_at.is_some_and(|at| at.elapsed() < LAYOUT_REFRESH) {
        return;
    }
    *refreshed_at = Some(std::time::Instant::now());
    let layout = layout.clone();
    drop(guard);
    let spaces = crate::magician_v2::notes::NotesSettingsStore::with_workspace_layout(layout)
        .notes_space_roots_blocking();
    register_notes_spaces(spaces);
}

/// [`register_workspaces_roots`] for notes space roots: each one's
/// `Workspaces/` folder.
pub fn register_notes_spaces<I: IntoIterator<Item = PathBuf>>(spaces: I) {
    register_workspaces_roots(spaces.into_iter().map(|space| space.join(WORKSPACES_DIR)));
}

/// Every configured path whose last component is `Workspaces`, from the
/// agents' file and shell sandboxes: the folder the setup docs tell the owner
/// to add is a goal workspaces root whatever the notes settings say.
pub fn workspaces_roots_in_config<'a, I: IntoIterator<Item = &'a String>>(
    paths: I,
) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| {
            path.is_absolute()
                && path
                    .file_name()
                    .is_some_and(|name| same_name(name, WORKSPACES_DIR))
        })
        .collect()
}

/// The registered roots, resolved the way checked paths are.
fn current_roots() -> Vec<PathBuf> {
    #[cfg(any(test, feature = "test-fixtures"))]
    if let Some(roots) = TEST_OVERRIDE.with(|cell| cell.borrow().as_ref().map(|o| o.roots.clone()))
    {
        #[cfg(test)]
        let roots = {
            let mut roots = roots;
            TEST_REGISTERED.with(|cell| roots.extend(cell.borrow().iter().cloned()));
            roots
        };
        return resolve_roots(&roots);
    }
    #[cfg(test)]
    let roots = TEST_REGISTERED.with(|cell| cell.borrow().clone());
    #[cfg(not(test))]
    refresh_layout_roots();
    #[cfg(not(test))]
    let roots = ROOTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    resolve_roots(&roots)
}

fn resolve_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut resolved: Vec<PathBuf> = roots
        .iter()
        .map(|root| super::native_executors::resolve_for_policy(root))
        .collect();
    resolved.sort();
    resolved.dedup();
    resolved
}

#[cfg(any(test, feature = "test-fixtures"))]
struct TestOverride {
    roots: Vec<PathBuf>,
    settings: AccessSettings,
}

#[cfg(any(test, feature = "test-fixtures"))]
thread_local! {
    static TEST_OVERRIDE: std::cell::RefCell<Option<TestOverride>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Roots registered on this thread in unit tests (never process-wide).
    static TEST_REGISTERED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `body` on this thread with `roots` as the only registered
/// `Workspaces/` folders and default settings, without touching the
/// process-wide registry other tests share.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn with_test_roots<T>(roots: Vec<PathBuf>, body: impl FnOnce() -> T) -> T {
    with_test_config(roots, AccessSettings::default(), body)
}

/// [`with_test_roots`] with explicit settings.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn with_test_config<T>(
    roots: Vec<PathBuf>,
    settings: AccessSettings,
    body: impl FnOnce() -> T,
) -> T {
    let _guard = install_test_config(roots, settings);
    body()
}

/// Restores the previous test override when dropped.
#[cfg(any(test, feature = "test-fixtures"))]
#[must_use]
pub struct TestRootsGuard(Option<TestOverride>);

#[cfg(any(test, feature = "test-fixtures"))]
impl Drop for TestRootsGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_OVERRIDE.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// [`with_test_roots`] for async tests on a current-thread runtime: the
/// override holds on this thread until the guard drops.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn install_test_roots(roots: Vec<PathBuf>) -> TestRootsGuard {
    install_test_config(roots, AccessSettings::default())
}

#[cfg(any(test, feature = "test-fixtures"))]
fn install_test_config(roots: Vec<PathBuf>, settings: AccessSettings) -> TestRootsGuard {
    TestRootsGuard(
        TEST_OVERRIDE.with(|cell| cell.borrow_mut().replace(TestOverride { roots, settings })),
    )
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Names compare without ASCII case on macOS, whose default file system
/// ignores it (`readme.md` is `README.md` there); elsewhere exactly.
fn same_component(left: &OsStr, right: &OsStr) -> bool {
    if cfg!(target_os = "macos") {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

/// A fixed name (`README.md`, `Goals`) matches without ASCII case on every
/// platform: on a case-sensitive disk a differently cased copy is refused
/// too, which only ever narrows.
fn same_name(name: &OsStr, fixed: &str) -> bool {
    name.to_string_lossy().eq_ignore_ascii_case(fixed)
}

fn normal_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect()
}

/// `path` relative to `root` as normal components, or `None` when it is not
/// at or under `root`.
fn relative_components(path: &Path, root: &Path) -> Option<Vec<OsString>> {
    let path_parts = normal_components(path);
    let root_parts = normal_components(root);
    if path_parts.len() < root_parts.len() {
        return None;
    }
    root_parts
        .iter()
        .zip(&path_parts)
        .all(|(root, part)| same_component(root, part))
        .then(|| path_parts[root_parts.len()..].to_vec())
}

fn is_within(path: &Path, root: &Path) -> bool {
    relative_components(path, root).is_some()
}

fn same_path(left: &Path, right: &Path) -> bool {
    relative_components(left, right).is_some_and(|rest| rest.is_empty())
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Whether `path` is at or under a registered `Workspaces/` folder (resolved
/// like the checks resolve it). Cheap callers use it to skip work.
pub fn is_under_workspaces_root(path: &Path) -> bool {
    let roots = current_roots();
    if roots.is_empty() {
        return false;
    }
    let resolved = super::native_executors::resolve_for_policy(&absolute(path));
    roots.iter().any(|root| is_within(&resolved, root))
}

// ---------------------------------------------------------------------------
// Workspace facts (README members, goal frontmatter), cached by stat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    inode: u64,
}

#[derive(Debug, Clone)]
struct GoalFile {
    path: PathBuf,
    private: bool,
    owner: Option<String>,
    /// `(device, inode)`, so a hard link elsewhere is recognised.
    inode: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceInfo {
    /// `None`: no README, an unreadable one, frontmatter that does not parse
    /// or no `members` — the admin alone.
    members: Option<BTreeSet<String>>,
    goals: Vec<GoalFile>,
    /// Hard-linked files of the workspace that are no goal here, by inode
    /// (a goal of another workspace may be one of them).
    linked: Vec<(PathBuf, (u64, u64))>,
}

impl WorkspaceInfo {
    fn is_member(&self, settings: &AccessSettings, agent: Option<&str>) -> bool {
        let Some(agent) = agent else {
            return false;
        };
        agent == settings.admin_agent
            || self
                .members
                .as_ref()
                .is_some_and(|members| members.contains(agent))
    }

    /// Whether a member `agent` may read and write `goal`.
    fn may_touch(&self, goal: &GoalFile, settings: &AccessSettings, agent: Option<&str>) -> bool {
        if !goal.private || is_admin_with(settings, agent) {
            return true;
        }
        match (goal.owner.as_deref(), agent) {
            (Some(owner), Some(agent)) => owner != "user" && owner == agent,
            _ => false,
        }
    }
}

type Signature = Vec<StatStamp>;

static CACHE: LazyLock<
    RwLock<HashMap<PathBuf, (Signature, Arc<WorkspaceInfo>, std::time::Instant)>>,
> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// How long what was read about a workspace (or an agent's OS denies, or
/// the hard-link index) is reused without another stat walk. Every write
/// through Magician's own tools clears it at once ([`invalidate`]); a change
/// made by a shell inside the box is seen within this long.
fn fresh_for() -> std::time::Duration {
    #[cfg(test)]
    if !TEST_CACHING.with(std::cell::Cell::get) {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::from_secs(1)
}

#[cfg(test)]
thread_local! {
    /// Unit tests rescan every time unless a test turns caching on.
    static TEST_CACHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Forget what was read about any workspace containing `path` (and every
/// agent's computed denies): Magician just changed it.
pub fn invalidate(path: &Path) {
    let resolved = super::native_executors::resolve_for_policy(&absolute(path));
    CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|ws_dir, _| !is_within(&resolved, ws_dir) && !is_within(ws_dir, &resolved));
    DENIES_MEMO
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    *INODE_INDEX
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn stamp(path: &Path, metadata: &std::fs::Metadata) -> StatStamp {
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        metadata.ino()
    };
    #[cfg(not(unix))]
    let inode = 0;
    StatStamp {
        path: path.to_path_buf(),
        modified: metadata.modified().ok(),
        len: metadata.len(),
        inode,
    }
}

/// Whether a file has more than one name.
fn has_other_names(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn device_inode(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

struct Scan {
    signature: Signature,
    readme: Option<PathBuf>,
    goal_candidates: Vec<PathBuf>,
    /// Regular files with another name (a hard link) that are no Markdown
    /// note: one may be a goal under another name (`p.txt` → `p.md`).
    linked: Vec<(PathBuf, (u64, u64))>,
    overflow: bool,
}

/// Stat the files that decide a workspace's access: its README and every
/// Markdown note anywhere in it, at any depth (a goal is governed wherever
/// it sits, not only under `Goals/`). Symlinks are stamped but never
/// followed or counted. Past [`MAX_SCAN_ENTRIES`] the workspace is left to
/// the admin (fail closed).
fn scan_workspace(ws_dir: &Path) -> Scan {
    let mut scan = Scan {
        signature: Vec::new(),
        readme: None,
        goal_candidates: Vec::new(),
        linked: Vec::new(),
        overflow: false,
    };
    let readme = ws_dir.join(README_FILE);
    if let Ok(metadata) = std::fs::symlink_metadata(&readme) {
        scan.signature.push(stamp(&readme, &metadata));
        // A README that is a symlink could point at a file members can
        // write: only a regular file counts.
        if metadata.file_type().is_file() {
            scan.readme = Some(readme);
        }
    }
    let Ok(metadata) = std::fs::symlink_metadata(ws_dir) else {
        return scan;
    };
    if !metadata.file_type().is_dir() {
        return scan;
    }
    let mut entries = 0usize;
    let mut pending = vec![ws_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(reader) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
        for entry in reader.flatten() {
            entries += 1;
            if entries > MAX_SCAN_ENTRIES {
                scan.overflow = true;
                return scan;
            }
            let path = entry.path();
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                children.push((path, metadata));
            }
        }
        children.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, metadata) in children {
            let file_type = metadata.file_type();
            let is_readme = dir == ws_dir
                && path
                    .file_name()
                    .is_some_and(|name| same_name(name, README_FILE));
            if is_readme {
                continue;
            }
            scan.signature.push(stamp(&path, &metadata));
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| same_name(extension, "md"))
            {
                scan.goal_candidates.push(path);
            } else if file_type.is_file() && has_other_names(&metadata) {
                if let Some(inode) = device_inode(&metadata) {
                    scan.linked.push((path, inode));
                }
            }
        }
    }
    scan
}

/// The leading frontmatter block of `path`, read from its first
/// [`FRONTMATTER_LIMIT`] bytes.
enum Frontmatter {
    /// The file has no frontmatter.
    Absent,
    Parsed(serde_yaml::Mapping),
    /// Unreadable, unterminated within the limit, or not a YAML mapping.
    Broken,
}

fn read_frontmatter(path: &Path) -> Frontmatter {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(FRONTMATTER_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return Frontmatter::Broken;
    }
    parse_frontmatter(&String::from_utf8_lossy(&bytes))
}

fn parse_frontmatter(text: &str) -> Frontmatter {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    match lines.next() {
        Some(first) if first.trim_end() == "---" => {},
        _ => return Frontmatter::Absent,
    }
    let mut block = String::new();
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return match serde_yaml::from_str::<serde_yaml::Value>(&block) {
                Ok(serde_yaml::Value::Mapping(map)) => Frontmatter::Parsed(map),
                Ok(serde_yaml::Value::Null) => Frontmatter::Parsed(serde_yaml::Mapping::new()),
                _ => Frontmatter::Broken,
            };
        }
        block.push_str(line);
    }
    Frontmatter::Broken
}

fn yaml_text(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    match map.get(serde_yaml::Value::String(key.to_string()))? {
        serde_yaml::Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        },
        _ => None,
    }
}

fn parse_members(map: &serde_yaml::Mapping) -> Option<BTreeSet<String>> {
    let names: Vec<String> = match map.get(serde_yaml::Value::String("members".to_string()))? {
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_yaml::Value::String(text) => text.split(',').map(str::to_string).collect(),
        _ => return None,
    };
    Some(
        names
            .iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

fn parse_goal(path: &Path) -> Option<GoalFile> {
    let inode = std::fs::metadata(path).ok().as_ref().and_then(device_inode);
    match read_frontmatter(path) {
        // An ordinary note in Goals/ (the Goals screen skips it too).
        Frontmatter::Absent => None,
        Frontmatter::Parsed(map) => {
            let kind = yaml_text(&map, "magician_kind")?;
            if !kind.eq_ignore_ascii_case("goal") {
                return None;
            }
            let visibility = yaml_text(&map, "visibility").map(|value| value.to_ascii_lowercase());
            let private = !matches!(visibility.as_deref(), Some("internal") | Some("public"));
            Some(GoalFile {
                path: path.to_path_buf(),
                private,
                owner: yaml_text(&map, "owner"),
                inode,
            })
        },
        // Fail closed: a goal file nobody can read the rules of is the
        // admin's alone.
        Frontmatter::Broken => Some(GoalFile {
            path: path.to_path_buf(),
            private: true,
            owner: None,
            inode,
        }),
    }
}

fn build_info(ws_dir: &Path, scan: &Scan, settings: &AccessSettings) -> WorkspaceInfo {
    if scan.overflow {
        tracing::warn!(
            workspace = %ws_dir.display(),
            "[GOAL-WORKSPACES] too many entries to check; the workspace is left to the admin agent"
        );
        return WorkspaceInfo::default();
    }
    let members = scan
        .readme
        .as_deref()
        .and_then(|readme| match read_frontmatter(readme) {
            Frontmatter::Parsed(map) => parse_members(&map),
            Frontmatter::Absent | Frontmatter::Broken => None,
        });
    let mut goals: Vec<GoalFile> = scan
        .goal_candidates
        .iter()
        .filter_map(|path| parse_goal(path))
        .collect();
    // A file sharing a goal's inode is that goal under another name,
    // whatever it is called (`Goals/p.txt` → `p.md`).
    let mut linked = Vec::new();
    for (path, inode) in &scan.linked {
        match goals
            .iter()
            .find(|goal| goal.inode == Some(*inode))
            .cloned()
        {
            Some(goal) => goals.push(GoalFile {
                path: path.clone(),
                ..goal
            }),
            None => linked.push((path.clone(), *inode)),
        }
    }
    for goal in &goals {
        if let Some(owner) = goal.owner.as_deref() {
            let member = owner == settings.admin_agent
                || members
                    .as_ref()
                    .is_some_and(|members| members.contains(owner));
            if owner != "user" && !member {
                tracing::warn!(
                    workspace = %ws_dir.display(),
                    owner,
                    "[GOAL-WORKSPACES] a goal names an owner that is not in the workspace's members; that agent cannot reach it"
                );
            }
        }
    }
    WorkspaceInfo {
        members,
        goals,
        linked,
    }
}

/// What decides access in `ws_dir`, re-read only when a stat of its README,
/// goal folders or goal files changed.
fn workspace_info(ws_dir: &Path) -> Arc<WorkspaceInfo> {
    let cached = CACHE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(ws_dir)
        .map(|(signature, info, checked_at)| (signature.clone(), Arc::clone(info), *checked_at));
    if let Some((_, info, checked_at)) = &cached {
        if checked_at.elapsed() < fresh_for() {
            return Arc::clone(info);
        }
    }
    let scan = scan_workspace(ws_dir);
    if let Some((signature, info, _)) = cached {
        if signature == scan.signature && !scan.overflow {
            if let Some(entry) = CACHE
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get_mut(ws_dir)
            {
                entry.2 = std::time::Instant::now();
            }
            return info;
        }
    }
    let info = Arc::new(build_info(ws_dir, &scan, &settings()));
    if !scan.overflow {
        let mut cache = CACHE
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.insert(
            ws_dir.to_path_buf(),
            (scan.signature, Arc::clone(&info), std::time::Instant::now()),
        );
    }
    info
}

type InodeIndex = HashMap<(u64, u64), Vec<(PathBuf, PathBuf)>>;

/// Every goal file's `(device, inode)` → its workspace and path, rebuilt at
/// most every [`fresh_for`] (or when the roots change).
static INODE_INDEX: LazyLock<RwLock<Option<(std::time::Instant, Vec<PathBuf>, Arc<InodeIndex>)>>> =
    LazyLock::new(|| RwLock::new(None));

fn inode_index(roots: &[PathBuf]) -> Arc<InodeIndex> {
    if let Some((built_at, built_roots, index)) = INODE_INDEX
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        if built_at.elapsed() < fresh_for() && built_roots == roots {
            return Arc::clone(index);
        }
    }
    let mut index = InodeIndex::new();
    for root in roots {
        for ws_dir in workspace_dirs(root) {
            for goal in &workspace_info(&ws_dir).goals {
                if let Some(inode) = goal.inode {
                    index
                        .entry(inode)
                        .or_default()
                        .push((ws_dir.clone(), goal.path.clone()));
                }
            }
        }
    }
    let index = Arc::new(index);
    *INODE_INDEX
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((
        std::time::Instant::now(),
        roots.to_vec(),
        Arc::clone(&index),
    ));
    index
}

// ---------------------------------------------------------------------------
// Decisions for one path
// ---------------------------------------------------------------------------

/// What an action does at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    /// Writes, creates or deletes the one entry.
    Write,
    /// Moves or recursively deletes the entry and everything under it.
    Subtree,
}

/// Why `agent` may not do `op` at `path`, or `None`. The reason names the
/// path as given and never a goal the agent cannot see. Fails closed: a path
/// that cannot be resolved is refused when it sits under a `Workspaces/`
/// folder.
pub fn refusal(path: &Path, op: Op, agent: Option<&str>) -> Option<String> {
    let settings = settings();
    if !settings.enabled || is_admin_with(&settings, agent) {
        return None;
    }
    let mut roots = current_roots();
    let shown = path.display().to_string();
    let lexical = absolute(path);
    // A `Workspaces/` folder no one registered yet (a scope made after
    // boot, a provider switched) is found from the path itself.
    if roots.is_empty()
        && !lexical
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("workspaces")
    {
        return None;
    }
    let discovered = std::iter::once(lexical.clone())
        .chain(super::native_executors::resolve_path_checked(path).ok())
        // A path under a known root needs no probe.
        .filter(|candidate| !roots.iter().any(|root| is_within(candidate, root)))
        .find_map(|candidate| discover_root(&candidate));
    if let Some(root) = discovered {
        if !roots.iter().any(|known| same_path(known, &root)) {
            register_workspaces_roots([root]);
            roots = current_roots();
        }
    }
    if roots.is_empty() {
        return None;
    }
    let resolved = match super::native_executors::resolve_path_checked(path) {
        Ok(resolved) => resolved,
        Err(_) => {
            let touches = roots.iter().any(|root| {
                is_within(&lexical, root) || (op == Op::Subtree && is_within(root, &lexical))
            });
            return touches.then(|| {
                format!("Refused: `{shown}` cannot be checked against the goal workspace rules")
            });
        },
    };
    let mut targets = vec![resolved];
    if op != Op::Read {
        // A delete or move acts on the entry itself, not on what a link at
        // the path points to.
        if let Ok(Some(leaf)) = super::native_executors::resolve_leaf_checked(path) {
            if !targets.contains(&leaf) {
                targets.push(leaf);
            }
        }
    }
    for target in &targets {
        if let Some(reason) = decide(&shown, target, op, agent, &settings, &roots) {
            return Some(reason);
        }
        if let Some(reason) = restricted_reads_refusal(&shown, target, agent) {
            return Some(reason);
        }
    }
    // A hard link elsewhere is the same file under another name.
    if op != Op::Subtree {
        if let Some(reason) = hard_link_refusal(&shown, &targets[0], agent, &settings, &roots) {
            return Some(reason);
        }
    }
    None
}

/// The nearest ancestor of `path` named `Workspaces` that holds a goal
/// workspace (a folder with a `README.md` or `Goals/`), registered or not.
fn discover_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .filter(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| same_name(name, WORKSPACES_DIR))
        })
        .find(|ancestor| {
            // Every entry is probed (a workspace past any fixed count is
            // still found); a folder too large to probe counts as one.
            std::fs::read_dir(ancestor).is_ok_and(|entries| {
                let mut seen = 0usize;
                entries.flatten().any(|entry| {
                    seen += 1;
                    if seen > MAX_SCAN_ENTRIES {
                        return true;
                    }
                    let dir = entry.path();
                    dir.join(README_FILE).is_file() || dir.join(GOALS_DIR).is_dir()
                })
            })
        })
        .map(|root| super::native_executors::resolve_for_policy(root))
}

fn decide(
    shown: &str,
    target: &Path,
    op: Op,
    agent: Option<&str>,
    settings: &AccessSettings,
    roots: &[PathBuf],
) -> Option<String> {
    let write = op != Op::Read;
    for root in roots {
        if op == Op::Subtree && is_within(root, target) && !same_path(root, target) {
            return Some(format!(
                "Refused: moving or deleting `{shown}` would take goal workspaces with it; only {} may do that",
                settings.admin_agent
            ));
        }
        let Some(rel) = relative_components(target, root) else {
            continue;
        };
        let Some(ws_name) = rel.first() else {
            return write.then(|| {
                format!(
                    "Refused: only {} changes the goal workspaces folder `{shown}` itself",
                    settings.admin_agent
                )
            });
        };
        if rel.len() == 1 && write {
            return Some(format!(
                "Refused: only {} creates, renames or deletes a goal workspace (`{shown}`)",
                settings.admin_agent
            ));
        }
        let ws_dir = root.join(ws_name);
        let info = workspace_info(&ws_dir);
        if !info.is_member(settings, agent) {
            return Some(format!(
                "Refused: `{shown}` is in a goal workspace this agent is not a member of. The owner can add it to `members:` in the workspace's README.md, through {}.",
                settings.admin_agent
            ));
        }
        if rel.len() == 2 && write {
            if same_name(&rel[1], README_FILE) {
                return Some(format!(
                    "Refused: only {} changes a goal workspace's README.md (`{shown}`), which holds its members",
                    settings.admin_agent
                ));
            }
            if same_name(&rel[1], GOALS_DIR) {
                return Some(format!(
                    "Refused: only {} creates, renames or deletes a goal workspace's Goals folder (`{shown}`)",
                    settings.admin_agent
                ));
            }
        }
        for goal in info
            .goals
            .iter()
            .filter(|goal| !info.may_touch(goal, settings, agent))
        {
            if same_path(target, &goal.path) {
                return Some(format!(
                    "Refused: `{shown}` is a private goal; only its owner and {} may read or change it",
                    settings.admin_agent
                ));
            }
            if op == Op::Subtree && is_within(&goal.path, target) {
                return Some(format!(
                    "Refused: `{shown}` holds a private goal this agent may not move or delete"
                ));
            }
        }
        return None;
    }
    None
}

/// A file with more than one name, one of which is a goal file `agent` may
/// not touch (or any goal file of a workspace it is not a member of).
fn hard_link_refusal(
    shown: &str,
    target: &Path,
    agent: Option<&str>,
    settings: &AccessSettings,
    roots: &[PathBuf],
) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(target).ok()?;
        if !metadata.is_file() || metadata.nlink() < 2 {
            return None;
        }
        let wanted = (metadata.dev(), metadata.ino());
        let index = inode_index(roots);
        let owners = index.get(&wanted)?;
        for (ws_dir, goal_path) in owners {
            let info = workspace_info(ws_dir);
            let member = info.is_member(settings, agent);
            let linked = info.goals.iter().any(|goal| {
                goal.path == *goal_path && (!member || !info.may_touch(goal, settings, agent))
            });
            if linked {
                return Some(format!(
                    "Refused: `{shown}` is another name for a goal this agent may not read or change"
                ));
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = (shown, target, agent, settings, roots);
        None
    }
}

fn workspace_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(reader) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = reader
        .flatten()
        .take(MAX_WORKSPACES_PER_ROOT)
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// [`refusal`] for the agent this execution acts as. A read it may not make
/// itself is allowed when an agent that delegated to it may make it
/// ([`read_delegators_for`]); writes are judged for the acting agent alone.
pub fn refusal_for_current(path: &Path, op: Op) -> Option<String> {
    let agent = super::native_executors::current_acting_agent();
    let delegators = super::native_executors::current_read_delegators();
    refusal_with_delegators(path, op, agent.as_deref(), &delegators)
}

/// [`refusal_for_current`] with the acting agent and its delegators captured
/// by the caller, for a check that runs outside the dispatch's task (a
/// search filter on a blocking thread).
pub fn refusal_with_delegators(
    path: &Path,
    op: Op,
    agent: Option<&str>,
    delegators: &[String],
) -> Option<String> {
    let refused = refusal(path, op, agent)?;
    if op != Op::Read {
        // A delegate that reads this path only through a delegator is told
        // why it may not write it, not that it is no member.
        let reader = (refusal(path, Op::Read, agent).is_some())
            .then(|| {
                delegators
                    .iter()
                    .find(|delegator| refusal(path, Op::Read, Some(delegator.as_str())).is_none())
            })
            .flatten();
        return Some(match reader {
            Some(delegator) => format!(
                "Refused: `{}` is read-only for this agent. It reads this goal workspace only through {delegator}'s delegation, and delegated access never writes. Write into this run's work folder instead, or ask {delegator} to make the change.",
                path.display()
            ),
            None => refused,
        });
    }
    if let Some(delegator) = delegators
        .iter()
        .find(|delegator| refusal(path, Op::Read, Some(delegator.as_str())).is_none())
    {
        tracing::info!(
            acting = agent.unwrap_or("none"),
            delegator = %delegator,
            path = %path.display(),
            "[GOAL-WORKSPACES] read allowed through delegation"
        );
        return None;
    }
    if delegators.is_empty() {
        return Some(refused);
    }
    Some(format!(
        "{refused} Delegation does not open it either: none of the agents that delegated this work ({}) may read it.",
        delegators.join(", ")
    ))
}

/// The path an argument names, if it names one: an absolute path, `~/…`, a
/// `file:` URL, or a relative path with a `/` resolved against `base`. A
/// `--flag=value` argument is judged by its value.
pub fn argument_path(arg: &str, base: Option<&Path>) -> Option<PathBuf> {
    let value = match arg.split_once('=') {
        Some((flag, value)) if flag.starts_with('-') => value,
        _ => arg,
    };
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("~/") {
        dirs::home_dir().map(|home| home.join(rest))
    } else if value == "~" {
        dirs::home_dir()
    } else if value.to_ascii_lowercase().starts_with("file:") {
        url::Url::parse(value)
            .ok()
            .and_then(|url| url.to_file_path().ok())
    } else if value.starts_with('/') {
        Some(PathBuf::from(value))
    } else if (value.contains('/') || value == "." || value == "..") && !value.contains("://") {
        base.map(|base| base.join(value))
    } else {
        None
    }
}

/// Why the acting agent may not run a process that no OS box can hold (a
/// governed CLI, whose spawn Magician does not own): its working directory
/// holds goal workspace content the agent may not read, or an argument
/// names such content or a folder holding it. `None` lets it run.
///
/// Judged for the acting agent alone: delegated reads do not apply. Nothing
/// holds such a process read-only, so a workspace it may only read through a
/// delegator would be one it could write.
pub fn unboxed_process_refusal<'a>(
    cwd: Option<&Path>,
    args: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let agent = super::native_executors::current_acting_agent();
    let denies = denies_for(agent.as_deref());
    let holds_hidden = |path: &Path| -> Option<String> {
        if let Some(refusal) =
            hybrid_index_refusal(path).or_else(|| refusal(path, Op::Read, agent.as_deref()))
        {
            return Some(refusal);
        }
        let resolved = super::native_executors::resolve_for_policy(&absolute(path));
        denies
            .hidden_dirs
            .iter()
            .chain(&denies.hidden_files)
            .any(|hidden| is_within(hidden, &resolved))
            .then(|| {
                format!(
                    "Refused: `{}` holds goal workspace content this agent may not read",
                    path.display()
                )
            })
    };
    // Reads through a delegator do not reach a process no box holds: say so.
    let holds_hidden = |path: &Path| -> Option<String> {
        holds_hidden(path).map(|refused| {
            if super::native_executors::current_read_delegators().is_empty() {
                refused
            } else {
                format!(
                    "{}. A process outside the sandbox gets only this agent's own access, \
                     not the access delegated to it: copy the file into this run's work folder \
                     with the file tools first.",
                    refused.trim_end_matches('.')
                )
            }
        })
    };
    if let Some(cwd) = cwd {
        if let Some(refusal) = holds_hidden(cwd) {
            return Some(refusal);
        }
    }
    args.into_iter()
        .filter_map(|arg| argument_path(arg, cwd))
        .find_map(|path| holds_hidden(&path))
}

// ---------------------------------------------------------------------------
// What an execution read from a goal workspace stays with that workspace
// ---------------------------------------------------------------------------

/// The runtime-root file that remembers, per evidence folder (an
/// execution's own folder, or the task folder when no execution is known),
/// which goal workspace paths an execution in it read. Tool results are kept
/// there, so another agent may not read that folder unless it may read every
/// one of those paths. Outside every agent's sandbox.
const RESTRICTED_READS_FILE: &str = "goal_workspace_reads.json";

type RestrictedReads = HashMap<PathBuf, BTreeSet<PathBuf>>;

/// The recorded reads as loaded from their file.
#[derive(Debug, Default)]
struct ReadsState {
    /// Where they persist (`None`: a unit test that set no file).
    file: Option<PathBuf>,
    /// Loaded once; `None` until the first use.
    loaded: Option<Loaded>,
}

#[derive(Debug)]
enum Loaded {
    Ok(RestrictedReads),
    /// The file exists but could not be read or parsed. Nothing is written
    /// over it (that would forget every task it guards), new reads are kept
    /// in memory, and every non-admin is refused the whole `scopes/` tree
    /// but its own task folder until the file reads again: it is re-read
    /// whenever its size or modification time changes, so repairing (or,
    /// knowingly, deleting) it recovers without a restart.
    Poisoned {
        reads: RestrictedReads,
        stamp: Option<(u64, Option<SystemTime>)>,
    },
}

impl Loaded {
    fn reads(&mut self) -> &mut RestrictedReads {
        match self {
            Loaded::Ok(reads) | Loaded::Poisoned { reads, .. } => reads,
        }
    }
}

fn file_stamp(file: &Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(file)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

/// Read the reads file: absent is empty; present but unreadable or
/// unparsable poisons. Entries whose folder no longer exists are pruned
/// (nothing is left there to guard).
fn load_reads(file: Option<&Path>) -> Loaded {
    let Some(file) = file else {
        return Loaded::Ok(RestrictedReads::new());
    };
    let parsed = match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice::<RestrictedReads>(&bytes).map_err(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RestrictedReads::new()),
        Err(_) => Err(()),
    };
    match parsed {
        Ok(mut reads) => {
            reads.retain(|folder, _| folder.exists());
            Loaded::Ok(reads)
        },
        Err(()) => {
            tracing::error!(
                file = %file.display(),
                "goal workspace read record is unreadable; agents are refused the scopes tree until it is repaired"
            );
            Loaded::Poisoned {
                reads: RestrictedReads::new(),
                stamp: file_stamp(file),
            }
        },
    }
}

/// Persist `reads` through the shared durable writer (unique staging file,
/// file sync and parent-directory sync). Called under the reads lock.
fn persist_reads(file: &Path, reads: &RestrictedReads) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(reads).map_err(std::io::Error::other)?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(file, &bytes)
}

#[cfg_attr(test, allow(dead_code))]
static RESTRICTED_READS: LazyLock<std::sync::Mutex<ReadsState>> =
    LazyLock::new(|| std::sync::Mutex::new(ReadsState::default()));

/// Where the reads are kept; `None` in a unit test that set none (nothing
/// is persisted then).
#[cfg_attr(test, allow(dead_code))]
fn restricted_reads_file() -> Option<PathBuf> {
    Some(crate::magician_v2::process_storage::runtime_root().join(RESTRICTED_READS_FILE))
}

#[cfg(test)]
thread_local! {
    /// Unit tests keep reads on their own thread, in their own file.
    static TEST_READS: std::cell::RefCell<Option<ReadsState>> =
        const { std::cell::RefCell::new(None) };
}

/// Keep this thread's recorded reads in `file` until the guard drops. The
/// file is loaded on first use, as in production.
#[cfg(test)]
pub(crate) fn install_test_reads_file(file: PathBuf) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_READS.with(|cell| *cell.borrow_mut() = None);
        }
    }
    TEST_READS.with(|cell| {
        *cell.borrow_mut() = Some(ReadsState {
            file: Some(file),
            loaded: None,
        })
    });
    Reset
}

/// Run `body` on the loaded state under the reads lock (loading it first,
/// and re-reading a poisoned file that changed since).
fn with_reads_state<T>(body: impl FnOnce(&mut Loaded, Option<&Path>) -> T) -> T {
    fn run<T>(state: &mut ReadsState, body: impl FnOnce(&mut Loaded, Option<&Path>) -> T) -> T {
        let reload = match &state.loaded {
            None => true,
            Some(Loaded::Poisoned { stamp, .. }) => state
                .file
                .as_deref()
                .is_some_and(|file| file_stamp(file) != *stamp),
            Some(Loaded::Ok(_)) => false,
        };
        if reload {
            let mut fresh = load_reads(state.file.as_deref());
            // Reads recorded while poisoned are kept across the reload.
            if let Some(Loaded::Poisoned { reads: pending, .. }) = state.loaded.take() {
                for (folder, paths) in pending {
                    fresh.reads().entry(folder).or_default().extend(paths);
                }
            }
            state.loaded = Some(fresh);
        }
        let file = state.file.clone();
        body(
            state.loaded.as_mut().expect("loaded above"),
            file.as_deref(),
        )
    }
    #[cfg(test)]
    {
        TEST_READS.with(|cell| match cell.borrow_mut().as_mut() {
            Some(state) => run(state, body),
            None => run(&mut ReadsState::default(), body),
        })
    }
    #[cfg(not(test))]
    {
        let mut guard = RESTRICTED_READS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.file.is_none() {
            guard.file = restricted_reads_file();
        }
        run(&mut guard, body)
    }
}

/// Remember that the current execution read `path`, when it is inside a goal
/// workspace: its evidence folder (the execution's own folder, else the task
/// folder) is then fenced like the workspace content itself. Reads outside
/// any execution are not tracked (`docs/features/goal-workspaces.md`, Limits).
pub fn record_read(path: &Path) {
    record_read_in(
        super::native_executors::current_evidence_root().as_deref(),
        path,
    );
}

/// [`record_read`] with the evidence folder given (a closure run outside
/// the dispatch scope).
pub fn record_read_in(task_root: Option<&Path>, path: &Path) {
    record_reads_in(task_root, std::iter::once(path.to_path_buf()));
}

/// [`record_read_in`] for many paths, written once.
pub fn record_reads_in(task_root: Option<&Path>, paths: impl IntoIterator<Item = PathBuf>) {
    let Some(task_root) = task_root else {
        return;
    };
    let reads: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| is_under_workspaces_root(path))
        .map(|path| super::native_executors::resolve_for_policy(&absolute(&path)))
        .collect();
    if reads.is_empty() {
        return;
    }
    let task_root = super::native_executors::resolve_for_policy(task_root);
    let changed = with_reads_state(|loaded, file| {
        let poisoned = matches!(loaded, Loaded::Poisoned { .. });
        let entry = loaded.reads().entry(task_root).or_default();
        let mut changed = false;
        for read in reads {
            changed |= entry.insert(read);
        }
        if changed && !poisoned {
            if let (Some(file), Loaded::Ok(reads)) = (file, &*loaded) {
                if let Err(error) = persist_reads(file, reads) {
                    tracing::warn!(%error, "could not persist the goal workspace read record");
                }
            }
        }
        changed
    });
    if changed {
        // OS boxes built from now on hide that folder.
        DENIES_MEMO
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

/// Record what an unfiltered process of the current execution (a shell, a
/// CLI, DuckDB, the browser) may have read: every goal workspace path its
/// box leaves it, which another agent may not reach. For the admin that is
/// every workspace and every private goal; for a member, its workspaces and
/// the private goals it owns. Nothing for an agent that reaches no
/// workspace.
pub fn record_possible_reads() {
    let settings = settings();
    if !settings.enabled {
        return;
    }
    let Some(root) = super::native_executors::current_evidence_root() else {
        return;
    };
    let agent = super::native_executors::current_acting_agent();
    // Through delegation the process may read what its delegators may.
    let readers: Vec<Option<String>> = std::iter::once(agent)
        .chain(
            super::native_executors::current_read_delegators()
                .into_iter()
                .map(Some),
        )
        .collect();
    let mut paths = Vec::new();
    for workspaces in current_roots().iter().filter(|root| root.is_dir()) {
        for ws_dir in workspace_dirs(workspaces) {
            let info = workspace_info(&ws_dir);
            let members: Vec<&Option<String>> = readers
                .iter()
                .filter(|reader| info.is_member(&settings, reader.as_deref()))
                .collect();
            if !ws_dir.is_dir() || members.is_empty() {
                continue;
            }
            paths.push(ws_dir.clone());
            paths.extend(
                info.goals
                    .iter()
                    .filter(|goal| {
                        goal.private
                            && members
                                .iter()
                                .any(|reader| info.may_touch(goal, &settings, reader.as_deref()))
                    })
                    .map(|goal| goal.path.clone()),
            );
        }
    }
    record_reads_in(Some(root.as_path()), paths);
}

/// Why `agent` may not reach `resolved`, a path inside a task folder whose
/// executions read goal workspace content it may not read.
fn restricted_reads_refusal(shown: &str, resolved: &Path, agent: Option<&str>) -> Option<String> {
    let settings = settings();
    if is_admin_with(&settings, agent) {
        return None;
    }
    // Indexed by folder: only the path's own ancestors are looked up.
    let (touched, poisoned): (Vec<PathBuf>, bool) = with_reads_state(|loaded, _| {
        let poisoned = matches!(loaded, Loaded::Poisoned { .. });
        let reads = loaded.reads();
        let touched = resolved
            .ancestors()
            .filter_map(|folder| reads.get(folder))
            .flat_map(|paths| paths.iter().cloned())
            .collect();
        (touched, poisoned)
    });
    if poisoned && poisoned_scope_refusal(resolved) {
        return Some(format!(
            "Refused: `{shown}` may hold goal workspace evidence and Magician's record of it is unreadable"
        ));
    }
    // Content the agent reads through a delegator is content it may read:
    // without this, its own execution folder closed to it after its first
    // delegated read.
    let readers = readers_with_delegators(agent);
    touched
        .iter()
        .any(|read| {
            readers
                .iter()
                .all(|reader| refusal(read, Op::Read, reader.as_deref()).is_some())
        })
        .then(|| {
            format!(
                "Refused: `{shown}` is in a task folder holding goal workspace content this agent may not read"
            )
        })
}

/// `agent`, and the agents that delegated to it when it is the agent this
/// dispatch acts as ([`read_delegators_for`]); never another agent's.
fn readers_with_delegators(agent: Option<&str>) -> Vec<Option<String>> {
    let mut readers = vec![agent.map(str::to_string)];
    if agent.is_some() && super::native_executors::current_acting_agent().as_deref() == agent {
        readers.extend(
            super::native_executors::current_read_delegators()
                .into_iter()
                .map(Some),
        );
    }
    readers
}

/// While the reads record is unreadable: whether `resolved` is in the
/// runtime's `scopes/` tree (where every task folder lives) outside the
/// current execution's own task folder.
fn poisoned_scope_refusal(resolved: &Path) -> bool {
    let scopes = super::native_executors::resolve_for_policy(&poisoned_scopes_root());
    if !is_within(resolved, &scopes) {
        return false;
    }
    !super::native_executors::current_task_root()
        .map(|root| super::native_executors::resolve_for_policy(&root))
        .is_some_and(|root| is_within(resolved, &root))
}

fn poisoned_scopes_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_SCOPES_ROOT.with(|cell| cell.borrow().clone()) {
        return root;
    }
    crate::magician_v2::process_storage::runtime_root().join("scopes")
}

#[cfg(test)]
thread_local! {
    static TEST_SCOPES_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Folders whose recorded reads `agent` may not see, for OS boxes. While
/// the record is unreadable, the whole `scopes/` tree for a non-admin.
fn restricted_task_dirs(agent: Option<&str>) -> Vec<PathBuf> {
    let settings = settings();
    if is_admin_with(&settings, agent) {
        return Vec::new();
    }
    let (reads, poisoned) = with_reads_state(|loaded, _| {
        (
            loaded.reads().clone(),
            matches!(loaded, Loaded::Poisoned { .. }),
        )
    });
    let mut dirs: Vec<PathBuf> = reads
        .into_iter()
        .filter(|(_, paths)| {
            paths
                .iter()
                .any(|read| refusal(read, Op::Read, agent).is_some())
        })
        .map(|(task_root, _)| task_root)
        .collect();
    if poisoned {
        dirs.push(poisoned_scopes_root());
    }
    dirs
}

// ---------------------------------------------------------------------------
// The hybrid notes index
// ---------------------------------------------------------------------------

/// Whether `resolved` is in a scope's notes hybrid index,
/// `…/scopes/<principal>/<workspace>/notes/hybrid-index[/…]`.
pub fn is_hybrid_index_path(resolved: &Path) -> bool {
    let parts = normal_components(resolved);
    parts
        .windows(5)
        .any(|window| window[0] == "scopes" && window[3] == "notes" && window[4] == "hybrid-index")
}

/// The always-on refusal for an agent touching the notes hybrid index.
pub fn hybrid_index_refusal(path: &Path) -> Option<String> {
    let resolved = super::native_executors::resolve_for_policy(&absolute(path));
    let reads_file = resolved
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(RESTRICTED_READS_FILE));
    if reads_file {
        return Some(format!(
            "Refused: `{}` is Magician's record of goal workspace reads",
            path.display()
        ));
    }
    // Plane harness sessions' private storage: mcp configs carrying grants,
    // harness homes with transcripts and auth copies.
    let plane_private = [
        crate::magician_v2::execution::plane::grant_channel::private_root(),
        crate::magician_v2::execution::plane::chat_turn::native_homes_root_path(),
    ];
    if plane_private.iter().any(|root| {
        is_within(
            &resolved,
            &super::native_executors::resolve_for_policy(root),
        )
    }) {
        return Some(format!(
            "Refused: `{}` is a plane harness's private storage",
            path.display()
        ));
    }
    is_hybrid_index_path(&resolved).then(|| {
        format!(
            "Refused: `{}` is the notes search index, which no agent reads directly; use search_notes",
            path.display()
        )
    })
}

/// Every existing `<runtime root>/scopes/*/*/notes/hybrid-index`, for OS
/// profiles that cannot match by name (bwrap).
pub fn hybrid_index_dirs(runtime_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(principals) = std::fs::read_dir(runtime_root.join("scopes")) else {
        return dirs;
    };
    for principal in principals.flatten() {
        let Ok(workspaces) = std::fs::read_dir(principal.path()) else {
            continue;
        };
        for workspace in workspaces.flatten() {
            let index = workspace.path().join("notes").join("hybrid-index");
            if index.is_dir() {
                dirs.push(index.canonicalize().unwrap_or(index));
            }
        }
    }
    dirs
}

// ---------------------------------------------------------------------------
// OS profiles
// ---------------------------------------------------------------------------

/// What an OS box must deny one agent, resolved for this host. Empty for the
/// admin, with the feature off, or with no goal workspaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceDenies {
    /// Each existing `Workspaces/` folder: closed by default (read, write,
    /// link), so a workspace created after the box was built is closed too;
    /// only the folder's own listing stays readable.
    pub fenced_roots: Vec<PathBuf>,
    /// The workspaces the agent is a member of, left open inside a fenced
    /// root (for writes only where the box already allows them).
    pub reopened: Vec<PathBuf>,
    /// Workspaces the agent is not a member of: no read, write or link.
    pub hidden_dirs: Vec<PathBuf>,
    /// Workspaces the agent reads only through an agent that delegated to it
    /// ([`read_delegators_for`]): also in `reopened`, but no write or link.
    pub read_only_dirs: Vec<PathBuf>,
    /// Private goals the agent may not touch: no read, write or link.
    pub hidden_files: Vec<PathBuf>,
    /// Entries that may not be written, renamed or deleted (READMEs, Goals
    /// folders, the folders holding a hidden goal, the `Workspaces/` folders
    /// and their ancestors), so no rename can move a hidden file to a path
    /// no rule names.
    pub write_denied: Vec<PathBuf>,
    /// Folders whose direct children may not be created, renamed or deleted
    /// (each `Workspaces/` folder).
    pub child_write_denied: Vec<PathBuf>,
    /// At least one goal workspace exists that these denies guard. Without
    /// an OS box that can apply them, the shell is refused to this agent.
    pub guards_workspaces: bool,
}

impl WorkspaceDenies {
    pub fn is_empty(&self) -> bool {
        self.fenced_roots.is_empty()
            && self.hidden_dirs.is_empty()
            && self.hidden_files.is_empty()
            && self.write_denied.is_empty()
            && self.child_write_denied.is_empty()
    }

    fn finish(mut self) -> Self {
        for list in [
            &mut self.fenced_roots,
            &mut self.reopened,
            &mut self.hidden_dirs,
            &mut self.read_only_dirs,
            &mut self.hidden_files,
            &mut self.write_denied,
            &mut self.child_write_denied,
        ] {
            list.sort();
            list.dedup();
        }
        self
    }
}

type DeniesMemo = HashMap<Option<String>, (std::time::Instant, Vec<PathBuf>, Arc<WorkspaceDenies>)>;

static DENIES_MEMO: LazyLock<RwLock<DeniesMemo>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// The denies for `agent` across every registered `Workspaces/` folder.
pub fn denies_for(agent: Option<&str>) -> WorkspaceDenies {
    let settings = settings();
    if !settings.enabled || is_admin_with(&settings, agent) {
        return WorkspaceDenies::default();
    }
    let roots = current_roots();
    let key = agent.map(str::to_string);
    if let Some((at, memo_roots, denies)) = DENIES_MEMO
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        if at.elapsed() < fresh_for() && *memo_roots == roots {
            return (**denies).clone();
        }
    }
    let denies = compute_denies(agent, &settings, &roots);
    DENIES_MEMO
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            key,
            (std::time::Instant::now(), roots, Arc::new(denies.clone())),
        );
    denies
}

fn compute_denies(
    agent: Option<&str>,
    settings: &AccessSettings,
    roots: &[PathBuf],
) -> WorkspaceDenies {
    let mut denies = WorkspaceDenies::default();
    // Task folders holding evidence of workspace reads this agent may not see.
    denies.hidden_dirs.extend(restricted_task_dirs(agent));
    // Only folders that exist: a `Workspaces/` folder not created yet holds
    // nothing to guard (the file tools still refuse creating it).
    for root in roots.iter().filter(|root| root.is_dir()) {
        denies.fenced_roots.push(root.clone());
        // The folder itself and every ancestor: renaming one would move the
        // whole tree out from under path rules.
        let mut ancestor = Some(root.as_path());
        while let Some(path) = ancestor {
            if path.parent().is_none() {
                break;
            }
            denies.write_denied.push(path.to_path_buf());
            ancestor = path.parent();
        }
        denies.child_write_denied.push(root.clone());
        for ws_dir in workspace_dirs(root) {
            if ws_dir.is_dir() {
                denies.guards_workspaces = true;
            }
            let info = workspace_info(&ws_dir);
            if !info.is_member(settings, agent) || !ws_dir.is_dir() {
                denies.hidden_dirs.push(ws_dir);
                continue;
            }
            denies.reopened.push(ws_dir.clone());
            denies.write_denied.push(ws_dir.join(README_FILE));
            denies.write_denied.push(ws_dir.join(GOALS_DIR));
            for goal in info
                .goals
                .iter()
                .filter(|goal| !info.may_touch(goal, settings, agent))
            {
                denies.hidden_files.push(goal.path.clone());
                let mut folder = goal.path.parent();
                while let Some(path) = folder {
                    if !is_within(path, &ws_dir) || same_path(path, &ws_dir) {
                        break;
                    }
                    denies.write_denied.push(path.to_path_buf());
                    folder = path.parent();
                }
            }
        }
    }
    // A hidden goal's other names in any workspace (a hard link that is no
    // goal where it sits) are hidden too.
    let hidden_inodes: Vec<(u64, u64)> = roots
        .iter()
        .filter(|root| root.is_dir())
        .flat_map(|root| workspace_dirs(root))
        .flat_map(|ws_dir| workspace_info(&ws_dir).goals.clone())
        .filter(|goal| denies.hidden_files.contains(&goal.path))
        .filter_map(|goal| goal.inode)
        .collect();
    if !hidden_inodes.is_empty() {
        for root in roots.iter().filter(|root| root.is_dir()) {
            for ws_dir in workspace_dirs(root) {
                for (path, inode) in &workspace_info(&ws_dir).linked {
                    if hidden_inodes.contains(inode) {
                        denies.hidden_files.push(path.clone());
                    }
                }
            }
        }
    }
    denies.finish()
}

/// [`denies_for`] the agent this execution acts as.
pub fn denies_for_current() -> WorkspaceDenies {
    let agent = super::native_executors::current_acting_agent();
    denies_for_acting(agent.as_deref())
}

/// [`denies_for`] `agent`, with the workspaces the current execution's
/// delegators may read opened read-only ([`read_delegators_for`]). Private
/// goals no delegator may read stay hidden inside them.
pub fn denies_for_acting(agent: Option<&str>) -> WorkspaceDenies {
    let denies = denies_for(agent);
    // The delegators in scope delegated to the agent this dispatch acts as,
    // not to another agent a caller names.
    if agent.is_none() || super::native_executors::current_acting_agent().as_deref() != agent {
        return denies;
    }
    let delegators = super::native_executors::current_read_delegators();
    if delegators.is_empty() || denies.is_empty() {
        return denies;
    }
    with_delegated_reads(denies, agent, &delegators)
}

fn with_delegated_reads(
    mut denies: WorkspaceDenies,
    agent: Option<&str>,
    delegators: &[String],
) -> WorkspaceDenies {
    let settings = settings();
    let readers: Vec<Option<&str>> = std::iter::once(agent)
        .chain(delegators.iter().map(|delegator| Some(delegator.as_str())))
        .collect();
    let reads = with_reads_state(|loaded, _| loaded.reads().clone());
    let mut still_hidden = Vec::new();
    for dir in std::mem::take(&mut denies.hidden_dirs) {
        let is_workspace = denies
            .fenced_roots
            .iter()
            .any(|root| dir.parent().is_some_and(|parent| same_path(parent, root)));
        if !is_workspace {
            // A task folder holding read evidence opens when every read in
            // it is one the agent or a delegator may make (its own execution
            // folder, after a delegated read); otherwise it stays hidden.
            let readable = reads.get(&dir).is_some_and(|paths| {
                paths.iter().all(|read| {
                    readers
                        .iter()
                        .any(|reader| refusal(read, Op::Read, *reader).is_none())
                })
            });
            if !readable {
                still_hidden.push(dir);
            }
            continue;
        }
        let info = workspace_info(&dir);
        let readers: Vec<&String> = delegators
            .iter()
            .filter(|delegator| info.is_member(&settings, Some(delegator.as_str())))
            .collect();
        if readers.is_empty() || !dir.is_dir() {
            still_hidden.push(dir);
            continue;
        }
        for goal in info.goals.iter().filter(|goal| {
            !readers
                .iter()
                .any(|reader| info.may_touch(goal, &settings, Some(reader.as_str())))
        }) {
            denies.hidden_files.push(goal.path.clone());
        }
        denies.reopened.push(dir.clone());
        denies.read_only_dirs.push(dir);
    }
    denies.hidden_dirs = still_hidden;
    // In a workspace the agent is a member of, a private goal only a
    // delegator may read is shown read-only rather than hidden, as the file
    // tools allow it: the box pins it against writes (`write_denied` is a
    // literal write deny on macOS, a read-only bind on Linux).
    let (shown, hidden): (Vec<PathBuf>, Vec<PathBuf>) = std::mem::take(&mut denies.hidden_files)
        .into_iter()
        .partition(|file| {
            delegators
                .iter()
                .any(|delegator| refusal(file, Op::Read, Some(delegator.as_str())).is_none())
        });
    denies.hidden_files = hidden;
    denies.write_denied.extend(shown);
    // A goal still hidden has other names too: a hard link that is no goal
    // where it sits, in a workspace now opened, is hidden with it.
    let hidden_inodes: Vec<(u64, u64)> = denies
        .fenced_roots
        .iter()
        .filter(|root| root.is_dir())
        .flat_map(|root| workspace_dirs(root))
        .flat_map(|ws_dir| workspace_info(&ws_dir).goals.clone())
        .filter(|goal| denies.hidden_files.contains(&goal.path))
        .filter_map(|goal| goal.inode)
        .collect();
    if !hidden_inodes.is_empty() {
        for dir in denies.read_only_dirs.clone() {
            for (path, inode) in &workspace_info(&dir).linked {
                if hidden_inodes.contains(inode) {
                    denies.hidden_files.push(path.clone());
                }
            }
        }
    }
    denies.finish()
}

fn sbpl_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn sbpl_escape_regex(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        if "\\.+*?()[]{}^$|\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// [`sbpl_rules_with`] for a profile that allows writes everywhere else.
pub fn sbpl_rules(denies: &WorkspaceDenies) -> String {
    sbpl_rules_with(denies, &|_| true)
}

/// The SBPL rules for `denies`, to emit after every allow (last match wins).
/// Deny only, never a re-open: each `Workspaces/` folder is closed except
/// its own listing and the member workspaces, which a `require-not` leaves
/// out of the deny rather than an `allow` putting back, so every rule the
/// profile made before (read-only roots, a harness's write denials, the
/// secrets) still holds inside a member workspace. `writable` is no longer
/// needed (kept for callers): a member workspace is writable exactly where
/// the profile already allowed writes.
pub fn sbpl_rules_with(denies: &WorkspaceDenies, _writable: &dyn Fn(&Path) -> bool) -> String {
    let mut rules = String::new();
    for root in &denies.fenced_roots {
        let escaped = sbpl_escape(root);
        let mut filter =
            format!("(require-all (subpath \"{escaped}\") (require-not (literal \"{escaped}\"))");
        for dir in denies.reopened.iter().filter(|dir| is_within(dir, root)) {
            filter.push_str(&format!(
                " (require-not (subpath \"{}\"))",
                sbpl_escape(dir)
            ));
        }
        filter.push(')');
        rules.push_str(&format!(
            "(deny file-read* file-write* file-link {filter})\n"
        ));
    }
    for dir in &denies.hidden_dirs {
        rules.push_str(&format!(
            "(deny file-read* file-write* file-link (subpath \"{}\"))\n",
            sbpl_escape(dir)
        ));
    }
    for dir in &denies.read_only_dirs {
        rules.push_str(&format!(
            "(deny file-write* file-link (subpath \"{}\"))\n",
            sbpl_escape(dir)
        ));
    }
    for file in &denies.hidden_files {
        rules.push_str(&format!(
            "(deny file-read* file-write* file-link (literal \"{}\"))\n",
            sbpl_escape(file)
        ));
    }
    // No write, and no hard link either: a link made elsewhere would name
    // the same file where no rule does (a README's `members:`, a goal shown
    // read-only through a delegator).
    for path in &denies.write_denied {
        rules.push_str(&format!(
            "(deny file-write* file-link (literal \"{}\"))\n",
            sbpl_escape(path)
        ));
    }
    for dir in &denies.child_write_denied {
        rules.push_str(&format!(
            "(deny file-write* (regex #\"^{}/[^/]+$\"))\n",
            sbpl_escape_regex(dir)
        ));
    }
    rules
}

/// [`bwrap_args_with`] for a box that allows writes everywhere else.
pub fn bwrap_args(denies: &WorkspaceDenies) -> Vec<OsString> {
    bwrap_args_with(denies, &|_| true)
}

/// The bwrap mounts for `denies`, to append after every other mount (a
/// later mount wins). Each `Workspaces/` folder becomes an empty tmpfs with
/// the member workspaces bound back from the host (writable only where the
/// box allows writes), then remounted read-only, so nothing can be created,
/// renamed or deleted directly under it and a workspace created later stays
/// hidden. Inside a member workspace, a folder that may not be renamed is
/// bound onto itself (a mount point cannot be renamed or removed; its files
/// stay writable), a README is a read-only bind of itself and a hidden goal
/// is `/dev/null`.
pub fn bwrap_args_with(
    denies: &WorkspaceDenies,
    writable: &dyn Fn(&Path) -> bool,
) -> Vec<OsString> {
    let mut args = Vec::new();
    let push = |args: &mut Vec<OsString>, flag: &str, from: &Path, to: &Path| {
        args.extend([
            OsString::from(flag),
            from.as_os_str().to_os_string(),
            to.as_os_str().to_os_string(),
        ]);
    };
    let roots: Vec<&PathBuf> = denies
        .fenced_roots
        .iter()
        .filter(|root| root.is_dir())
        .collect();
    for root in &roots {
        args.extend([OsString::from("--tmpfs"), root.as_os_str().to_os_string()]);
    }
    for dir in denies.reopened.iter().filter(|dir| dir.is_dir()) {
        let read_only = denies.read_only_dirs.iter().any(|ro| same_path(ro, dir));
        let flag = if writable(dir) && !read_only {
            "--bind-try"
        } else {
            "--ro-bind-try"
        };
        push(&mut args, flag, dir, dir);
    }
    for root in &roots {
        args.extend([
            OsString::from("--remount-ro"),
            root.as_os_str().to_os_string(),
        ]);
    }
    for path in &denies.write_denied {
        let inside_member = denies.reopened.iter().any(|dir| is_within(path, dir));
        if !inside_member {
            continue;
        }
        // Inside a workspace opened only to read, the read-only bind already
        // pins every entry; a writable bind here (a nested `Workspaces/`
        // folder's ancestors) would reopen the whole folder for writing.
        if denies.read_only_dirs.iter().any(|dir| is_within(path, dir)) {
            continue;
        }
        if path.is_file() {
            push(&mut args, "--ro-bind", path, path);
        } else if path.is_dir() && writable(path) {
            push(&mut args, "--bind", path, path);
        }
    }
    for file in denies.hidden_files.iter().filter(|file| file.is_file()) {
        push(&mut args, "--ro-bind", Path::new("/dev/null"), file);
    }
    // A hidden folder outside every `Workspaces/` folder (a task folder
    // holding evidence of reads this agent may not see): an empty,
    // read-only tmpfs. Inside a fenced root the tmpfs above hides it already.
    for dir in denies
        .hidden_dirs
        .iter()
        .filter(|dir| dir.is_dir() && !roots.iter().any(|root| is_within(dir, root)))
    {
        args.extend([OsString::from("--tmpfs"), dir.as_os_str().to_os_string()]);
        args.extend([
            OsString::from("--remount-ro"),
            dir.as_os_str().to_os_string(),
        ]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) struct Fixture {
        pub _dir: tempfile::TempDir,
        pub root: PathBuf,
    }

    /// `Workspaces/Magican` (members: cmo) with an internal goal owned by
    /// cmo, a private goal owned by the user and one owned by ceo.
    pub(crate) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().unwrap().join("Workspaces");
        write_fixture(&root);
        Fixture { _dir: dir, root }
    }

    /// Write the fixture workspace under `root` (a `Workspaces/` folder).
    pub(crate) fn write_fixture(root: &Path) {
        let ws = root.join("Magican");
        std::fs::create_dir_all(ws.join("Goals/X")).unwrap();
        std::fs::write(
            ws.join("README.md"),
            "---\nmembers: [cmo]\n---\n# Magican\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("Goals/X/x.md"),
            "---\nmagician_kind: goal\nowner: cmo\nvisibility: internal\n---\n# x\n\nneedle\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("Goals/X/secret.md"),
            "---\nmagician_kind: goal\nowner: user\nvisibility: private\n---\n# secret\n\nneedle\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("Goals/X/ceo plan.md"),
            "---\nmagician_kind: goal\nowner: ceo\n---\n# ceo plan\n",
        )
        .unwrap();
    }

    /// Compiled-tool resources over `root`, with an unrestricted file
    /// sandbox so only the goal workspace fence decides.
    pub(crate) fn resources(
        root: &Path,
    ) -> Arc<crate::magician_v2::execution::agent_resources::AgentResources> {
        use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
        use crate::magician_v2::agents::memory::AgentMemoryResolver;
        use crate::magician_v2::agents::storage::AgentStorage;
        Arc::new(
            crate::magician_v2::execution::agent_resources::AgentResources {
                magician_config: Arc::new(RwLock::new(crate::config::MagicianConfig::default())),
                memory_resolver: Arc::new(AgentMemoryResolver::new(root)),
                agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(
                    root,
                ))),
                artifact_workspace:
                    crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(root),
                artifact_v2_service: None,
                event_broadcaster: None,
                operation_llm_router: None,
                secret_store_resolver: None,
                content_acquisition_resolver: Arc::new(RwLock::new(None)),
                file_sandbox: runtime_core::FileSandboxConfig {
                    mode: runtime_core::FileSandboxMode::Unrestricted,
                    ..runtime_core::FileSandboxConfig::default()
                },
                relentless_research: None,
                tool_index: Arc::new(std::sync::OnceLock::new()),
                user_request_service: None,
                agent_runtime: None,
            },
        )
    }

    #[test]
    fn frontmatter_parses_members_and_fails_closed() {
        match parse_frontmatter("---\nmembers: [cmo, cro]\n---\nbody") {
            Frontmatter::Parsed(map) => {
                let members = parse_members(&map).unwrap();
                assert!(members.contains("cmo") && members.contains("cro"));
            },
            _ => panic!("parses"),
        }
        assert!(matches!(
            parse_frontmatter("# no frontmatter"),
            Frontmatter::Absent
        ));
        assert!(matches!(
            parse_frontmatter("---\nmembers: [cmo\n---\n"),
            Frontmatter::Broken
        ));
        assert!(matches!(
            parse_frontmatter("---\nmembers: [cmo]\n"),
            Frontmatter::Broken
        ));
    }

    #[test]
    fn non_members_are_refused_everything_in_the_workspace() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let goal = fx.root.join("Magican/Goals/X/x.md");
            assert!(refusal(&goal, Op::Read, Some("ambassador")).is_some());
            assert!(refusal(&goal, Op::Write, Some("ambassador")).is_some());
            assert!(refusal(
                &fx.root.join("Magican/Facts.md"),
                Op::Write,
                Some("ambassador")
            )
            .is_some());
            // No identity is a non-member.
            assert!(refusal(&goal, Op::Read, None).is_some());
            // The admin reaches everything.
            assert!(refusal(
                &fx.root.join("Magican/Goals/X/secret.md"),
                Op::Write,
                Some(DEFAULT_ADMIN_AGENT)
            )
            .is_none());
            // Outside the workspaces folder nothing changes.
            assert!(refusal(
                &fx.root.parent().unwrap().join("other.md"),
                Op::Write,
                Some("ambassador")
            )
            .is_none());
        });
    }

    #[test]
    fn a_member_reaches_internal_goals_but_not_private_ones() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            assert!(refusal(&ws.join("Goals/X/x.md"), Op::Read, Some("cmo")).is_none());
            assert!(refusal(&ws.join("Goals/X/x.md"), Op::Write, Some("cmo")).is_none());
            assert!(refusal(&ws.join("Goals/X/new.md"), Op::Write, Some("cmo")).is_none());
            for private in ["Goals/X/secret.md", "Goals/X/ceo plan.md"] {
                assert!(refusal(&ws.join(private), Op::Read, Some("cmo")).is_some());
                assert!(refusal(&ws.join(private), Op::Write, Some("cmo")).is_some());
            }
            // A folder holding a private goal cannot be moved or deleted,
            // and the refusal names no goal.
            let reason = refusal(&ws.join("Goals/X"), Op::Subtree, Some("cmo")).expect("refused");
            assert!(
                !reason.contains("secret") && !reason.contains("ceo plan"),
                "{reason}"
            );
            // Nor can the README (members) or the Goals folder itself change.
            assert!(refusal(&ws.join("README.md"), Op::Read, Some("cmo")).is_none());
            assert!(refusal(&ws.join("README.md"), Op::Write, Some("cmo")).is_some());
            assert!(refusal(&ws.join("readme.md"), Op::Write, Some("cmo")).is_some());
            assert!(refusal(&ws.join("Goals"), Op::Subtree, Some("cmo")).is_some());
            // Nor any entry directly under Workspaces/, nor the folder itself.
            assert!(refusal(&fx.root.join("Other"), Op::Write, Some("cmo")).is_some());
            assert!(refusal(&ws, Op::Subtree, Some("cmo")).is_some());
            assert!(refusal(&fx.root, Op::Subtree, Some("cmo")).is_some());
            assert!(refusal(fx.root.parent().unwrap(), Op::Subtree, Some("cmo")).is_some());
        });
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hard_links_to_a_private_goal_are_refused() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let secret = fx.root.join("Magican/Goals/X/secret.md");
            let outside = fx.root.parent().unwrap();
            let link = outside.join("link.md");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(refusal(&link, Op::Read, Some("cmo")).is_some());
            let hard = outside.join("hard.md");
            std::fs::hard_link(&secret, &hard).unwrap();
            assert!(refusal(&hard, Op::Read, Some("cmo")).is_some());
            assert!(refusal(&hard, Op::Read, Some("ambassador")).is_some());
            assert!(refusal(&hard, Op::Read, Some(DEFAULT_ADMIN_AGENT)).is_none());
        });
    }

    #[test]
    fn an_unparseable_or_missing_readme_leaves_the_workspace_to_the_admin() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            std::fs::write(ws.join("README.md"), "---\nmembers: [cmo\n---\n").unwrap();
            assert!(refusal(&ws.join("Goals/X/x.md"), Op::Read, Some("cmo")).is_some());
            assert!(refusal(
                &ws.join("Goals/X/x.md"),
                Op::Read,
                Some(DEFAULT_ADMIN_AGENT)
            )
            .is_none());
            std::fs::remove_file(ws.join("README.md")).unwrap();
            assert!(refusal(&ws.join("Goals/X/x.md"), Op::Read, Some("cmo")).is_some());
            // Restoring members restores access: the cache follows the file.
            std::fs::write(ws.join("README.md"), "---\nmembers:\n  - cmo\n---\n").unwrap();
            assert!(refusal(&ws.join("Goals/X/x.md"), Op::Read, Some("cmo")).is_none());
        });
    }

    #[test]
    fn a_goal_with_broken_frontmatter_is_private() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let goal = fx.root.join("Magican/Goals/X/x.md");
            std::fs::write(&goal, "---\nmagician_kind: goal\nowner: [cmo\n---\n").unwrap();
            assert!(refusal(&goal, Op::Read, Some("cmo")).is_some());
        });
    }

    #[test]
    fn disabled_access_refuses_nothing() {
        let fx = fixture();
        with_test_config(
            vec![fx.root.clone()],
            AccessSettings {
                enabled: false,
                admin_agent: DEFAULT_ADMIN_AGENT.into(),
            },
            || {
                assert!(refusal(
                    &fx.root.join("Magican/Goals/X/secret.md"),
                    Op::Read,
                    Some("ambassador")
                )
                .is_none());
                assert!(denies_for(Some("ambassador")).is_empty());
            },
        );
    }

    #[test]
    fn denies_hide_non_member_workspaces_and_private_goals() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let outsider = denies_for(Some("ambassador"));
            assert!(outsider.guards_workspaces);
            assert_eq!(outsider.hidden_dirs, vec![ws.clone()]);
            assert!(outsider.child_write_denied.contains(&fx.root));
            let member = denies_for(Some("cmo"));
            assert!(member.hidden_dirs.is_empty());
            assert!(member.hidden_files.contains(&ws.join("Goals/X/secret.md")));
            assert!(member
                .hidden_files
                .contains(&ws.join("Goals/X/ceo plan.md")));
            assert!(!member.hidden_files.contains(&ws.join("Goals/X/x.md")));
            for pinned in [
                ws.join("README.md"),
                ws.join("Goals"),
                ws.join("Goals/X"),
                fx.root.clone(),
            ] {
                assert!(
                    member.write_denied.contains(&pinned),
                    "{}",
                    pinned.display()
                );
            }
            assert!(denies_for(Some(DEFAULT_ADMIN_AGENT)).is_empty());
        });
    }

    #[test]
    fn sbpl_rules_deny_reads_writes_and_links() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let outsider = sbpl_rules(&denies_for(Some("ambassador")));
            assert!(outsider.contains(&format!(
                "(deny file-read* file-write* file-link (subpath \"{}\"))",
                ws.display()
            )));
            let member = sbpl_rules(&denies_for(Some("cmo")));
            assert!(member.contains(&format!(
                "(deny file-read* file-write* file-link (literal \"{}\"))",
                ws.join("Goals/X/secret.md").display()
            )));
            assert!(member.contains(&format!(
                "(deny file-write* file-link (literal \"{}\"))",
                ws.join("README.md").display()
            )));
            assert!(member.contains("/[^/]+$\"))"));
        });
    }

    /// A task folder holding evidence this agent may not read lies outside
    /// every `Workspaces/` folder: bwrap hides it with its own read-only
    /// tmpfs (inside a fenced root, the root's tmpfs hides it already).
    #[test]
    fn bwrap_hides_restricted_task_folders_outside_the_workspaces_folder() {
        let fx = fixture();
        let task = fx.root.parent().unwrap().join("task-1");
        std::fs::create_dir_all(&task).unwrap();
        let denies = WorkspaceDenies {
            fenced_roots: vec![fx.root.clone()],
            hidden_dirs: vec![task.clone(), fx.root.join("Magican")],
            guards_workspaces: true,
            ..WorkspaceDenies::default()
        };
        let args: Vec<String> = bwrap_args(&denies)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let task_text = task.display().to_string();
        let at = args
            .iter()
            .position(|arg| *arg == task_text)
            .expect("hidden");
        assert_eq!(args[at - 1], "--tmpfs");
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--remount-ro" && w[1] == task_text));
        assert!(!args.contains(&fx.root.join("Magican").display().to_string()));
    }

    #[test]
    fn bwrap_args_hide_with_tmpfs_and_dev_null() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let outsider = bwrap_args(&denies_for(Some("ambassador")));
            let position = outsider
                .iter()
                .position(|arg| arg == "--tmpfs")
                .expect("tmpfs");
            // The whole Workspaces/ folder is masked and remounted read-only
            // (no child can be created, renamed or deleted); the workspace
            // is never bound back for a non-member.
            assert_eq!(outsider[position + 1], fx.root.clone().into_os_string());
            assert!(outsider
                .windows(2)
                .any(|w| w[0] == "--remount-ro" && w[1] == fx.root.clone().into_os_string()));
            assert!(!outsider.contains(&ws.clone().into_os_string()));
            let member = bwrap_args(&denies_for(Some("cmo")));
            // Bound back for a member, Goals/ and the folder holding a hidden
            // goal pinned by a bind onto themselves.
            let ws_os = ws.clone().into_os_string();
            assert!(member
                .windows(3)
                .any(|w| w[0] == "--bind-try" && w[1] == ws_os && w[2] == ws_os));
            for pinned in [ws.join("Goals"), ws.join("Goals/X")] {
                let pinned = pinned.into_os_string();
                assert!(member
                    .windows(3)
                    .any(|w| w[0] == "--bind" && w[1] == pinned && w[2] == pinned));
            }
            let remount = member.iter().position(|arg| arg == "--remount-ro").unwrap();
            let bind_back = member.iter().position(|arg| *arg == ws_os).unwrap();
            assert!(bind_back < remount, "bound back before the remount");
            let secret = ws.join("Goals/X/secret.md").into_os_string();
            let at = member
                .iter()
                .position(|arg| *arg == secret)
                .expect("secret masked");
            assert_eq!(member[at - 1], "/dev/null");
            assert_eq!(member[at - 2], "--ro-bind");
            let readme = ws.join("README.md").into_os_string();
            assert!(member
                .windows(3)
                .any(|w| w[0] == "--ro-bind" && w[1] == readme && w[2] == readme));
        });
    }

    #[test]
    fn the_hybrid_index_is_recognised_by_its_layout() {
        assert!(is_hybrid_index_path(Path::new(
            "/r/scopes/anonymous/default/notes/hybrid-index/data.lance"
        )));
        assert!(is_hybrid_index_path(Path::new(
            "/r/scopes/anonymous/default/notes/hybrid-index"
        )));
        assert!(!is_hybrid_index_path(Path::new(
            "/r/scopes/anonymous/default/notes/settings.json"
        )));
        assert!(HYBRID_INDEX_SBPL_RULE.contains("hybrid-index"));
    }

    /// Owner decision 2026-10-26: delegation carries read access. A
    /// non-member delegate reads what its delegator may read, writes nothing
    /// there, and sees no private goal its delegator may not see.
    #[test]
    fn a_delegate_reads_what_its_delegator_may_read_and_writes_nothing() {
        use super::super::native_executors::{ACTING_AGENT, READ_DELEGATORS};
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let as_delegate = |delegators: Vec<String>, check: &dyn Fn()| {
                READ_DELEGATORS.sync_scope(delegators, || {
                    ACTING_AGENT.sync_scope(Some("ambassador".into()), check)
                })
            };
            // On its own, the non-member is refused.
            as_delegate(Vec::new(), &|| {
                assert!(refusal_for_current(&ws.join("Goals/X/x.md"), Op::Read).is_some());
            });
            // Delegated by a member: reads open, writes stay refused.
            as_delegate(vec!["cmo".into()], &|| {
                assert!(refusal_for_current(&ws.join("Goals/X/x.md"), Op::Read).is_none());
                assert!(refusal_for_current(&ws.join("Facts.md"), Op::Read).is_none());
                assert!(refusal_for_current(&ws.join("Goals/X/x.md"), Op::Write).is_some());
                let write = refusal_for_current(&ws.join("Facts.md"), Op::Write).unwrap();
                assert!(write.contains("read-only for this agent"), "{write}");
                assert!(write.contains("through cmo's delegation"), "{write}");
                assert!(!write.contains("not a member"), "{write}");
                // A private goal the member may not read stays closed.
                assert!(refusal_for_current(&ws.join("Goals/X/secret.md"), Op::Read).is_some());
                // The box opens the workspace read-only and keeps it hidden goals.
                let denies = denies_for_current();
                assert!(denies.hidden_dirs.is_empty());
                assert_eq!(denies.read_only_dirs, vec![ws.clone()]);
                assert!(denies.reopened.contains(&ws));
                assert!(denies.hidden_files.contains(&ws.join("Goals/X/secret.md")));
                assert!(!denies.hidden_files.contains(&ws.join("Goals/X/x.md")));
                let rules = sbpl_rules(&denies);
                assert!(rules.contains(&format!(
                    "(deny file-write* file-link (subpath \"{}\"))",
                    sbpl_escape(&ws)
                )));
                let ws_os = ws.clone().into_os_string();
                let args = bwrap_args(&denies);
                assert!(args
                    .windows(3)
                    .any(|w| w[0] == "--ro-bind-try" && w[1] == ws_os));
                assert!(!args
                    .windows(2)
                    .any(|w| w[0] == "--bind-try" && w[1] == ws_os));
                // A folder pinned inside it is never bound writable.
                let mut nested = denies.clone();
                nested.write_denied.push(ws.join("Goals"));
                let nested_args = bwrap_args_with(&nested, &|_| true);
                let goals_os = ws.join("Goals").into_os_string();
                assert!(!nested_args
                    .windows(2)
                    .any(|w| w[0] == "--bind" && w[1] == goals_os));
                // Nothing holds an unboxed process read-only: it is judged
                // for the acting agent alone.
                assert!(unboxed_process_refusal(Some(&ws), std::iter::empty()).is_some());
                let pdf = ws.join("Share/pack.pdf").display().to_string();
                assert!(unboxed_process_refusal(None, [pdf.as_str()]).is_some());
            });
            // Delegated by the admin: private goals read too, still no write.
            as_delegate(vec![DEFAULT_ADMIN_AGENT.into()], &|| {
                assert!(refusal_for_current(&ws.join("Goals/X/secret.md"), Op::Read).is_none());
                assert!(refusal_for_current(&ws.join("Goals/X/secret.md"), Op::Write).is_some());
            });
            // A member delegated by the admin: the private goal it could not
            // see is shown in its box, pinned read-only.
            READ_DELEGATORS.sync_scope(vec![DEFAULT_ADMIN_AGENT.into()], || {
                ACTING_AGENT.sync_scope(Some("cmo".into()), || {
                    let secret = ws.join("Goals/X/secret.md");
                    let denies = denies_for_current();
                    assert!(!denies.hidden_files.contains(&secret));
                    assert!(denies.write_denied.contains(&secret));
                })
            });
            // A delegator that is no member grants nothing.
            as_delegate(vec!["ceo".into()], &|| {
                assert!(refusal_for_current(&ws.join("Goals/X/x.md"), Op::Read).is_some());
                let write = refusal_for_current(&ws.join("Facts.md"), Op::Write).unwrap();
                assert!(write.contains("not a member"), "{write}");
                assert_eq!(denies_for_current().hidden_dirs, vec![ws.clone()]);
            });
        });
    }

    /// Only server-set identity names a delegator: the suspended owners of an
    /// in-run delegation and a delegated child's verified source.
    #[test]
    fn read_delegators_come_from_run_identity_only() {
        use crate::magician_v2::agents::{
            AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
        };
        let mut ctx = crate::magician_v2::execution::agentic::AgenticContext::new("send", "sent");
        ctx.agent_id = Some("executive-assistant".into());
        assert!(read_delegators_for(&ctx).is_empty());
        let delegated = |surface, source_kind| AgentInvocationContext {
            principal: "anonymous".into(),
            workspace: "default".into(),
            source_agent_id: Some(DEFAULT_ADMIN_AGENT.into()),
            target_agent_id: "executive-assistant".into(),
            surface,
            feature_mode: FeatureMode::None,
            source_kind,
            chat_session_id: None,
            chat_turn_id: None,
        };
        ctx.invocation_context_override = Some(delegated(
            InvocationSurface::Delegation,
            InvocationSourceKind::Delegated,
        ));
        assert_eq!(
            read_delegators_for(&ctx),
            vec![DEFAULT_ADMIN_AGENT.to_string()]
        );
        // Not a delegation: a task or chat invocation carries nothing.
        ctx.invocation_context_override = Some(delegated(
            InvocationSurface::Task,
            InvocationSourceKind::Direct,
        ));
        assert!(read_delegators_for(&ctx).is_empty());
        // A delegated run a Plane grant restamped keeps its delegator; one the
        // Plane started itself has none.
        ctx.invocation_context_override = Some(delegated(
            InvocationSurface::Plane,
            InvocationSourceKind::Delegated,
        ));
        assert_eq!(
            read_delegators_for(&ctx),
            vec![DEFAULT_ADMIN_AGENT.to_string()]
        );
        ctx.invocation_context_override = Some(delegated(
            InvocationSurface::Plane,
            InvocationSourceKind::Direct,
        ));
        assert!(read_delegators_for(&ctx).is_empty());
        // A Handover invocation names no delegator: giving control back
        // builds one, and reads must not flow upward. A verified handover's
        // reads travel in the lineage the server records instead.
        ctx.invocation_context_override = Some(delegated(
            InvocationSurface::Handover,
            InvocationSourceKind::Handover,
        ));
        assert!(read_delegators_for(&ctx).is_empty());
        // An invocation addressed to another agent names no delegator here.
        let mut other = delegated(
            InvocationSurface::Delegation,
            InvocationSourceKind::Delegated,
        );
        other.target_agent_id = "muse".into();
        ctx.invocation_context_override = Some(other);
        assert!(read_delegators_for(&ctx).is_empty());
        // In-run delegation: the suspended owners, nearest first, never itself.
        ctx.invocation_context_override = None;
        ctx.active_owner_agent_id = Some("executive-assistant".into());
        ctx.owner_stack = vec![
            DEFAULT_ADMIN_AGENT.into(),
            "cmo".into(),
            "executive-assistant".into(),
        ];
        assert_eq!(
            read_delegators_for(&ctx),
            vec!["cmo".to_string(), DEFAULT_ADMIN_AGENT.to_string()]
        );
        // Transitive across child executions: Magican → Vera → Muse. Muse's
        // record carries the whole chain, nearest delegator first.
        let mut muse = crate::magician_v2::execution::agentic::AgenticContext::new("make", "made");
        muse.agent_id = Some("muse".into());
        muse.active_owner_agent_id = Some("muse".into());
        muse.read_lineage = vec![DEFAULT_ADMIN_AGENT.into(), "executive-assistant".into()];
        assert_eq!(
            read_delegators_for(&muse),
            vec![
                "executive-assistant".to_string(),
                DEFAULT_ADMIN_AGENT.to_string()
            ]
        );
    }

    fn vibedev_ctx(task_id: &str) -> crate::magician_v2::execution::agentic::AgenticContext {
        let mut ctx = crate::magician_v2::execution::agentic::AgenticContext::new("build", "built");
        ctx.vibedev_run = true;
        ctx.principal = Some("owner".into());
        ctx.workspace = Some("default".into());
        ctx.task_id = Some(task_id.into());
        ctx.execution_id = Some("exec-root".into());
        ctx.root_execution_id = Some("exec-root".into());
        ctx.agent_id = Some("vibedev-coordinator".into());
        ctx.active_owner_agent_id = Some("vibedev-coordinator".into());
        ctx
    }

    /// Engineers working beneath the owner's own VibeDev session read as the
    /// admin it acts as; in an agent-started session they get the
    /// coordinator's reads only.
    #[test]
    fn engineers_in_the_owners_vibedev_session_read_as_the_admin() {
        record_owner_started_vibedev_task("owner", "default", "task-owner-engineers");
        let mut engineer = vibedev_ctx("task-owner-engineers");
        engineer.active_owner_agent_id = Some("engineer".into());
        engineer.owner_stack = vec!["vibedev-coordinator".into()];
        assert_eq!(
            read_delegators_for(&engineer),
            vec![
                "vibedev-coordinator".to_string(),
                DEFAULT_ADMIN_AGENT.to_string()
            ]
        );
        let mut agent_made = vibedev_ctx("task-agent-engineers");
        agent_made.active_owner_agent_id = Some("engineer".into());
        agent_made.owner_stack = vec!["vibedev-coordinator".into()];
        assert_eq!(
            read_delegators_for(&agent_made),
            vec!["vibedev-coordinator".to_string()]
        );
    }

    /// Owner decision 2026-10-20: a VibeDev session the owner started from
    /// the VibeDev API acts as the admin, in its own root frame only.
    #[test]
    fn an_owner_started_vibedev_session_acts_as_the_admin() {
        record_owner_started_vibedev_task("owner", "default", "task-owner-vibedev");
        let ctx = vibedev_ctx("task-owner-vibedev");
        assert_eq!(acting_agent_for(&ctx).as_deref(), Some(DEFAULT_ADMIN_AGENT));
    }

    /// Everything else keeps its own identity: a VibeDev task an agent made,
    /// an engineer delegated in context or as a child execution, and any
    /// coding run an agent starts outside VibeDev.
    #[test]
    fn an_agent_started_coding_run_is_still_fenced() {
        // Not recorded by the owner's request (an agent created the task).
        let agent_made = vibedev_ctx("task-agent-vibedev");
        assert_eq!(
            acting_agent_for(&agent_made).as_deref(),
            Some("vibedev-coordinator")
        );
        record_owner_started_vibedev_task("owner", "default", "task-owner-vibedev-2");
        let mut delegated = vibedev_ctx("task-owner-vibedev-2");
        delegated.owner_stack = vec!["vibedev-coordinator".into()];
        delegated.active_owner_agent_id = Some("engineer".into());
        assert_eq!(acting_agent_for(&delegated).as_deref(), Some("engineer"));
        let mut child = vibedev_ctx("task-owner-vibedev-2");
        child.execution_id = Some("exec-child".into());
        child.active_owner_agent_id = Some("engineer".into());
        assert_eq!(acting_agent_for(&child).as_deref(), Some("engineer"));
        let mut not_vibedev = vibedev_ctx("task-owner-vibedev-2");
        not_vibedev.vibedev_run = false;
        assert_eq!(
            acting_agent_for(&not_vibedev).as_deref(),
            Some("vibedev-coordinator")
        );
        // And the agent-started run is fenced from the workspace.
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let acting = acting_agent_for(&agent_made);
            assert!(refusal(
                &fx.root.join("Magican/Goals/X/x.md"),
                Op::Read,
                acting.as_deref()
            )
            .is_some());
        });
    }

    /// A private goal is governed wherever it sits: five levels deep, or
    /// outside `Goals/` altogether.
    #[test]
    fn a_private_goal_anywhere_in_the_workspace_is_hidden_from_members() {
        let fx = fixture();
        let ws = fx.root.join("Magican");
        let deep = ws.join("Goals/X/a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        let private = "---\nmagician_kind: goal\nowner: ceo\nvisibility: private\n---\n# p\n";
        std::fs::write(deep.join("deep.md"), private).unwrap();
        std::fs::create_dir_all(ws.join("Notes")).unwrap();
        std::fs::write(ws.join("Notes/elsewhere.md"), private).unwrap();
        with_test_roots(vec![fx.root.clone()], || {
            assert!(refusal(&deep.join("deep.md"), Op::Read, Some("cmo")).is_some());
            assert!(refusal(&ws.join("Notes/elsewhere.md"), Op::Read, Some("cmo")).is_some());
            assert!(refusal(&ws.join("Notes"), Op::Read, Some("cmo")).is_none());
            let member = denies_for(Some("cmo"));
            assert!(member.hidden_files.contains(&deep.join("deep.md")));
        });
    }

    /// Within the freshness window a workspace is not walked again; a write
    /// through Magician's tools forgets it at once.
    #[test]
    fn cache_skips_the_walk_within_ttl_and_invalidate_forces_it() {
        let fx = fixture();
        let ws = fx.root.join("Magican");
        TEST_CACHING.with(|cell| cell.set(true));
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                TEST_CACHING.with(|cell| cell.set(false));
            }
        }
        let _off = Off;
        with_test_roots(vec![fx.root.clone()], || {
            let goal = ws.join("Goals/X/x.md");
            assert!(refusal(&goal, Op::Read, Some("cmo")).is_none());
            // cmo removed from members behind Magician's back: still cached.
            std::fs::write(ws.join("README.md"), "---\nmembers: [cro]\n---\n").unwrap();
            assert!(refusal(&goal, Op::Read, Some("cmo")).is_none());
            invalidate(&ws.join("README.md"));
            assert!(refusal(&goal, Op::Read, Some("cmo")).is_some());
        });
    }

    /// Non-members get the whole folder closed by default, with only the
    /// member workspaces left out: a workspace made later is closed too.
    #[test]
    fn non_member_profile_denies_the_whole_workspaces_folder_and_leaves_member_workspaces_out() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let root = sbpl_escape(&fx.root);
            let ws = sbpl_escape(&fx.root.join("Magican"));
            let outsider = sbpl_rules(&denies_for(Some("ambassador")));
            let closed = format!(
                "(deny file-read* file-write* file-link (require-all (subpath \"{root}\") (require-not (literal \"{root}\"))"
            );
            assert!(outsider.contains(&format!("{closed})")), "{outsider}");
            assert!(!outsider.contains("(allow"), "{outsider}");
            let member = sbpl_rules_with(&denies_for(Some("cmo")), &|_| false);
            // The member workspace is left out of the deny, never re-opened
            // by an allow that would undo earlier denies inside it.
            assert!(
                member.contains(&format!("{closed} (require-not (subpath \"{ws}\")))")),
                "{member}"
            );
            assert!(!member.contains("(allow"), "{member}");
        });
    }

    /// A rule the profile made before the workspace rules (a read-only root
    /// or a harness write denial inside a member workspace) is not undone:
    /// the workspace rules hold no allow at all.
    #[test]
    fn member_workspace_rules_never_reopen_earlier_denies() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            for agent in ["cmo", "ambassador", "ceo"] {
                let rules = sbpl_rules_with(&denies_for(Some(agent)), &|_| true);
                assert!(!rules.contains("(allow"), "{agent}: {rules}");
            }
        });
    }

    #[test]
    fn config_roots_named_workspaces_are_found() {
        let configured = vec![
            ".".to_string(),
            "/tmp".to_string(),
            "/n/spaces/a/d/Workspaces".to_string(),
        ];
        assert_eq!(
            workspaces_roots_in_config(&configured),
            vec![PathBuf::from("/n/spaces/a/d/Workspaces")]
        );
    }

    /// A file sharing a private goal's inode is that goal, whatever its
    /// name and wherever in the workspaces it sits: refused to the file
    /// tools and hidden in OS boxes.
    #[cfg(unix)]
    #[test]
    fn a_hard_link_to_a_private_goal_under_another_name_is_that_goal() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let secret = ws.join("Goals/X/secret.md");
            std::fs::hard_link(&secret, ws.join("Goals/X/p.txt")).unwrap();
            std::fs::create_dir_all(fx.root.join("Other")).unwrap();
            std::fs::write(
                fx.root.join("Other/README.md"),
                "---\nmembers: [cmo]\n---\n",
            )
            .unwrap();
            std::fs::hard_link(&secret, fx.root.join("Other/q.bin")).unwrap();
            for path in [ws.join("Goals/X/p.txt"), fx.root.join("Other/q.bin")] {
                assert!(
                    refusal(&path, Op::Read, Some("cmo")).is_some(),
                    "{}",
                    path.display()
                );
                assert!(refusal(&path, Op::Read, Some(DEFAULT_ADMIN_AGENT)).is_none());
                assert!(
                    denies_for(Some("cmo")).hidden_files.contains(&path),
                    "{}",
                    path.display()
                );
            }
        });
    }

    /// A `Workspaces/` folder nobody registered is found from a path even
    /// when its workspace is not among its first entries.
    #[test]
    fn an_unregistered_workspaces_folder_is_found_past_many_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("Workspaces");
        for index in 0..300 {
            std::fs::create_dir_all(root.join(format!("a{index:03}"))).unwrap();
        }
        write_fixture(&root);
        // `zz` sorts last on every file system's listing order we know of;
        // the probe no longer stops early either way.
        // This moves a fixture directory, not a durable file publication;
        // the byte-oriented atomic writer cannot perform this operation.
        std::fs::rename(root.join("Magican"), root.join("zzMagican")).unwrap();
        assert_eq!(
            discover_root(&root.join("zzMagican/Goals/X/x.md")),
            Some(super::super::native_executors::resolve_for_policy(&root))
        );
    }

    fn as_agent<T>(agent: &str, evidence: Option<&Path>, body: impl FnOnce() -> T) -> T {
        use crate::magician_v2::execution::native_executors::{ACTING_AGENT, EVIDENCE_ROOT};
        ACTING_AGENT.sync_scope(Some(agent.to_string()), || match evidence {
            Some(root) => EVIDENCE_ROOT.sync_scope(root.to_path_buf(), body),
            None => body(),
        })
    }

    /// A process no OS box holds (a governed CLI) is refused a working
    /// directory or an argument that reaches goal workspace content the
    /// agent may not read; the notes root holds such content for every
    /// non-admin.
    #[test]
    fn an_unboxed_process_is_refused_a_cwd_or_argument_holding_hidden_content() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let notes = fx.root.parent().unwrap().to_path_buf();
            let elsewhere = tempfile::tempdir().unwrap();
            let elsewhere = elsewhere.path().canonicalize().unwrap();
            let run = |agent: &str, cwd: &PathBuf, args: &[&str]| {
                as_agent(agent, None, || {
                    unboxed_process_refusal(Some(cwd.as_path()), args.iter().copied())
                })
            };
            assert!(run("ambassador", &notes, &[]).is_some());
            // A member is still refused: the notes root holds others' private goals.
            assert!(run("cmo", &notes, &[]).is_some());
            assert!(run(DEFAULT_ADMIN_AGENT, &notes, &[]).is_none());
            assert!(run("ambassador", &elsewhere, &["--help", "report.md"]).is_none());
            let goal = format!("--input={}", fx.root.join("Magican/Goals/X/x.md").display());
            assert!(run("ambassador", &elsewhere, &[&goal]).is_some());
            assert!(run("cmo", &elsewhere, &[&goal]).is_none());
            let parent = notes.display().to_string();
            assert!(run("ambassador", &elsewhere, &[&parent]).is_some());
            let relative = format!("{}", fx.root.join("Magican").display());
            assert!(
                run("cmo", &elsewhere, &[&relative]).is_some(),
                "private goals inside"
            );
            assert!(run("ambassador", &notes, &["./Workspaces/Magican/README.md"]).is_some());
        });
    }

    /// The record is written whole (unique temp, flushed, renamed): no
    /// temporary file is left and a folder that no longer exists is pruned
    /// when the record loads.
    #[test]
    fn recorded_reads_persist_atomically_and_prune_vanished_folders() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let base = fx.root.parent().unwrap().to_path_buf();
            let file = base.join("reads.json");
            let task = base.join("task-a");
            std::fs::create_dir_all(&task).unwrap();
            let gone = base.join("task-gone");
            let goal = fx.root.join("Magican/Goals/X/x.md");
            let earlier: RestrictedReads =
                HashMap::from([(gone.clone(), BTreeSet::from([goal.clone()]))]);
            std::fs::write(&file, serde_json::to_vec(&earlier).unwrap()).unwrap();
            let _reads = install_test_reads_file(file.clone());
            record_reads_in(
                Some(task.as_path()),
                [goal, fx.root.join("Magican/Goals/X/secret.md")],
            );
            let saved: RestrictedReads =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(saved.len(), 1, "{saved:?}");
            assert_eq!(saved.get(&task).map(BTreeSet::len), Some(2), "{saved:?}");
            assert!(std::fs::read_dir(&base)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
        });
    }

    /// An unreadable record fails closed: every non-admin is refused the
    /// `scopes/` tree (in the file tools and as a hidden folder in OS
    /// boxes), nothing is written over the broken file, and repairing it
    /// recovers without a restart, keeping the reads recorded meanwhile.
    #[test]
    fn an_unreadable_reads_record_fails_closed_and_recovers_when_repaired() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let base = fx.root.parent().unwrap().to_path_buf();
            let file = base.join("reads.json");
            std::fs::write(&file, "{not json").unwrap();
            let scopes = base.join("scopes");
            let task = scopes.join("p/w/tasks/t1");
            std::fs::create_dir_all(&task).unwrap();
            TEST_SCOPES_ROOT.with(|cell| *cell.borrow_mut() = Some(scopes.clone()));
            let _reads = install_test_reads_file(file.clone());
            let evidence = task.join("evidence.json");
            assert!(refusal(&evidence, Op::Read, Some("cmo")).is_some());
            assert!(refusal(&evidence, Op::Read, Some(DEFAULT_ADMIN_AGENT)).is_none());
            assert!(denies_for(Some("cmo")).hidden_dirs.contains(&scopes));
            record_read_in(Some(task.as_path()), &fx.root.join("Magican/Goals/X/x.md"));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "{not json");
            std::fs::write(&file, "{}").unwrap();
            assert!(refusal(&evidence, Op::Read, Some("cmo")).is_none());
            assert!(refusal(&evidence, Op::Read, Some("ambassador")).is_some());
            assert!(!denies_for(Some("cmo")).hidden_dirs.contains(&scopes));
            TEST_SCOPES_ROOT.with(|cell| *cell.borrow_mut() = None);
        });
    }

    /// A shell, CLI, DuckDB or browser run reads whatever its box leaves
    /// it: the admin's evidence is closed to every other agent, a member's
    /// to non-members, and neither touches the shared task folder.
    #[test]
    fn unfiltered_runs_mark_their_evidence_with_what_their_box_could_read() {
        let fx = fixture();
        with_test_roots(vec![fx.root.clone()], || {
            let base = fx.root.parent().unwrap().to_path_buf();
            let _reads = install_test_reads_file(base.join("reads.json"));
            let admin_run = base.join("task/executions/e1");
            let member_run = base.join("task/executions/e2");
            std::fs::create_dir_all(&admin_run).unwrap();
            std::fs::create_dir_all(&member_run).unwrap();
            as_agent(
                DEFAULT_ADMIN_AGENT,
                Some(admin_run.as_path()),
                record_possible_reads,
            );
            as_agent("cmo", Some(member_run.as_path()), record_possible_reads);
            let out = |dir: &Path| dir.join("out.txt");
            assert!(refusal(&out(&admin_run), Op::Read, Some("cmo")).is_some());
            assert!(refusal(&out(&admin_run), Op::Read, Some("ambassador")).is_some());
            assert!(refusal(&out(&member_run), Op::Read, Some("ambassador")).is_some());
            assert!(refusal(&out(&member_run), Op::Read, Some("cmo")).is_none());
            assert!(refusal(
                &base.join("task/workdir/notes.md"),
                Op::Read,
                Some("ambassador")
            )
            .is_none());
            // An agent that reaches no workspace records nothing.
            let outsider_run = base.join("task/executions/e3");
            std::fs::create_dir_all(&outsider_run).unwrap();
            as_agent(
                "ambassador",
                Some(outsider_run.as_path()),
                record_possible_reads,
            );
            assert!(refusal(&out(&outsider_run), Op::Read, Some("cmo")).is_none());
        });
    }
}

#[cfg(test)]
pub(crate) use tests::{fixture as test_fixture, resources as test_resources, write_fixture};
