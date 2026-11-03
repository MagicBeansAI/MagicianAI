//! Where a task works on a folder outside Magician's workspace.
//!
//! A task that VibeDev did not start carries no `VibeDev project:` line, so a
//! coding turn or a git command it aims at an outside repository has no
//! project rules to run under. The first such touch asks the owner where to
//! work (an isolated workspace, an isolated branch or the original folder);
//! the answer creates — or finds — the project that owns the folder and binds
//! the task to it, so every later call of the task, on any delegated hop,
//! runs under that project without asking again.
//!
//! The binding is server-written: the task's own entry in the project's
//! `run_links`, carrying the chosen work location — the same record a VibeDev
//! admission writes for its root run (`record_vibedev_project_run`). Nothing
//! the model sends can create or move it. A task belongs to one project: a
//! binding to a second one is refused, never rewritten.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::projects::{
    append_vibedev_run_task_id, normalize_vibedev_project_store, project_store_lock,
    read_vibedev_projects, vibedev_project_store_path, VibeDevProjectRecord, VibeDevProjectStore,
    VibeDevProjectTrust, VibeDevWorkLocation, VIBEDEV_THREAD_ID,
};
use super::trusted_workspace::project_repo_folder;

/// The owner's answers to the work-location question, as `(id, label)`.
/// The first is the default and the recommendation.
pub const WORK_LOCATION_CHOICES: [(&str, &str); 4] = [
    ("isolated_workspace", "Isolated workspace (recommended)"),
    ("isolated_branch", "Isolated branch"),
    ("original_folder", "Original folder"),
    ("cancel", "Cancel"),
];

/// The trust mode a work-location answer picks; `None` for Cancel or an
/// answer this question does not offer.
pub fn trust_for_choice(choice: &str) -> Option<VibeDevProjectTrust> {
    match choice {
        "isolated_workspace" => Some(VibeDevProjectTrust::Sandboxed),
        "isolated_branch" => Some(VibeDevProjectTrust::Trusted),
        "original_folder" => Some(VibeDevProjectTrust::Original),
        _ => None,
    }
}

/// Whether `folder` is inside Magician's own live source tree (the
/// process-wide `live_repo_source_fence`).
pub fn is_magician_source(folder: &Path) -> bool {
    crate::magician_v2::execution::coding_engine::live_repo_source_fence()
        .is_some_and(|(source, _)| folder.starts_with(source))
}

/// The owner-facing question for `folder`: a title and the body naming the
/// folder and what each answer does. `magician_source` says the folder is
/// Magician's own source code.
pub fn work_location_question(folder: &Path, magician_source: bool) -> String {
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    let source_note = if magician_source {
        " This is Magician's own source code."
    } else {
        ""
    };
    format!(
        "Where should Magician work on {name}?\n\n\
         An agent wants to change {folder}, which is outside Magician's workspace.{source_note} \
         Pick how it works there; this task keeps the answer.\n\n\
         - Isolated workspace: Magician works on a copy, and its changes come back as a diff for \
         you to approve.\n\
         - Isolated branch: Magician works in its own git clone on a `vibedev/…` branch.\n\
         - Original folder: Magician edits your real folder, and each edit waits for your \
         approval (needs an OS sandbox).",
        folder = folder.display(),
    )
}

/// The owner's home folder, canonical.
fn home_folder() -> Option<PathBuf> {
    dirs::home_dir().and_then(|home| home.canonicalize().ok())
}

/// A folder too broad to be one project: the filesystem root, the owner's
/// home folder, or anything that holds it.
fn too_broad(folder: &Path, home: Option<&Path>) -> bool {
    folder.parent().is_none() || home.is_some_and(|home| home.starts_with(folder))
}

fn nearest_git_root_in(canonical: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let root = canonical
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())?;
    (!too_broad(root, home)).then(|| root.to_path_buf())
}

/// The nearest git repository root holding `canonical` (or `canonical`
/// itself). A root that is too broad to be a project — `/`, the home folder
/// or an ancestor of it — does not count.
pub fn nearest_git_root(canonical: &Path) -> Option<PathBuf> {
    nearest_git_root_in(canonical, home_folder().as_deref())
}

fn work_location_folder_in(canonical: &Path, home: Option<&Path>) -> Option<PathBuf> {
    nearest_git_root_in(canonical, home)
        .or_else(|| (!too_broad(canonical, home)).then(|| canonical.to_path_buf()))
}

/// The folder a work-location question is about: the nearest git repository
/// root holding `canonical`, else `canonical` itself. `None` when that is too
/// broad to be one project.
pub fn work_location_folder(canonical: &Path) -> Option<PathBuf> {
    work_location_folder_in(canonical, home_folder().as_deref())
}

/// A project's folder, canonical, or `None` when it cannot be resolved.
fn canonical_project_folder(project: &VibeDevProjectRecord, workspace_root: &Path) -> Option<PathBuf> {
    project_repo_folder(project.repo_path.as_deref(), workspace_root)
        .canonicalize()
        .ok()
}

/// Whether `task_id` is one of `project`'s runs, bound or admitted.
fn task_belongs_to(project: &VibeDevProjectRecord, task_id: &str) -> bool {
    project.run_links.contains_key(task_id) || project.run_task_ids.iter().any(|id| id == task_id)
}

/// The live project bound to `task_id` by a work-location binding: the task
/// has its own `run_links` entry, and that entry names a work location.
pub fn project_bound_to_task(scope_root: &Path, task_id: &str) -> Option<VibeDevProjectRecord> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return None;
    }
    read_vibedev_projects(scope_root).into_iter().find(|project| {
        !project.archived
            && project
                .run_links
                .get(task_id)
                .is_some_and(|link| link.work_location.is_some())
    })
}

/// The live project `task_id` belongs to: its work-location binding, else a
/// project that admitted it as a run (a VibeDev run whose link names no
/// location).
pub fn project_of_task(scope_root: &Path, task_id: &str) -> Option<VibeDevProjectRecord> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return None;
    }
    project_bound_to_task(scope_root, task_id).or_else(|| {
        read_vibedev_projects(scope_root)
            .into_iter()
            .find(|project| !project.archived && task_belongs_to(project, task_id))
    })
}

/// The index of the live project whose folder holds `canonical` (the folder
/// itself or one inside it); the deepest such folder wins.
fn owning_index(
    projects: &[VibeDevProjectRecord],
    workspace_root: &Path,
    canonical: &Path,
) -> Option<usize> {
    projects
        .iter()
        .enumerate()
        .filter(|(_, project)| !project.archived)
        .filter_map(|(index, project)| {
            let folder = canonical_project_folder(project, workspace_root)?;
            canonical
                .starts_with(&folder)
                .then(|| (folder.components().count(), index))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, index)| index)
}

/// The live project whose folder holds `canonical`; the deepest one wins.
pub fn live_project_owning(
    scope_root: &Path,
    workspace_root: &Path,
    canonical: &Path,
) -> Option<VibeDevProjectRecord> {
    let projects = read_vibedev_projects(scope_root);
    let index = owning_index(&projects, workspace_root, canonical)?;
    projects.into_iter().nth(index)
}

/// Whether `project` owns `canonical` (its folder is `canonical` or holds it).
pub fn project_owns(project: &VibeDevProjectRecord, workspace_root: &Path, canonical: &Path) -> bool {
    canonical_project_folder(project, workspace_root)
        .is_some_and(|folder| canonical.starts_with(folder))
}

/// The refusal a task gets for `wanted` when it already belongs to `project`.
pub fn belongs_elsewhere_refusal(
    project: &VibeDevProjectRecord,
    workspace_root: &Path,
    wanted: &Path,
) -> String {
    format!(
        "This task already works in VibeDev project “{}” ({}), so it does not work on {} as well. \
         Do not retry or work around it; tell the owner, who can start a separate task for that \
         folder.",
        project.name,
        project_repo_folder(project.repo_path.as_deref(), workspace_root).display(),
        wanted.display()
    )
}

/// Why a binding was not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// The task already belongs to another project; the message says which.
    BelongsElsewhere(String),
    /// The project store could not be read or written.
    Store(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BelongsElsewhere(message) | Self::Store(message) => f.write_str(message),
        }
    }
}

/// Bind `task_id` to `project` (in place): remember the run and give its link
/// the project's work location, keeping whatever else the link already says
/// (lineage, crew, lane, or a location an admission pinned). Whether
/// anything changed.
fn merge_binding(project: &mut VibeDevProjectRecord, task_id: &str) -> bool {
    let location = VibeDevWorkLocation {
        mode: project.trust,
        repo_path: project.repo_path.clone(),
    };
    let mut changed = append_vibedev_run_task_id(project, task_id);
    let link = project.run_links.entry(task_id.to_string()).or_default();
    if link.work_location.is_none() {
        link.work_location = Some(location);
        changed = true;
    }
    changed
}

/// A project name for `folder` that no live project of the scope uses: the
/// folder's name, then `name 2`, `name 3`, …
fn unique_project_name(folder: &Path, projects: &[VibeDevProjectRecord]) -> String {
    let base: String = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Project".to_string())
        .chars()
        .take(96)
        .collect();
    let taken = |name: &str| {
        projects
            .iter()
            .any(|project| !project.archived && project.name.eq_ignore_ascii_case(name))
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .expect("an unused numbered name exists")
}

/// A new project record for `folder` (canonical) under `trust`. It has no
/// cockpit conversation yet; the cockpit shows that as a warning and starts
/// one with the project's next run.
fn new_folder_project(
    folder: &Path,
    trust: VibeDevProjectTrust,
    projects: &[VibeDevProjectRecord],
) -> VibeDevProjectRecord {
    let now = chrono::Utc::now().timestamp_millis();
    VibeDevProjectRecord {
        project_id: uuid::Uuid::new_v4().to_string(),
        name: unique_project_name(folder, projects),
        chat_thread_id: VIBEDEV_THREAD_ID.to_string(),
        chat_session_id: String::new(),
        repo_path: Some(folder.display().to_string()),
        active_root_task_id: None,
        run_task_ids: Vec::new(),
        preview_url: None,
        deploy_url: None,
        created_at_ms: now,
        updated_at_ms: now,
        archived: false,
        source_meeting_thread_id: None,
        source_chat_session_id: None,
        published_url: None,
        deployments: Vec::new(),
        trust,
        base_branch: None,
        checks: None,
        build_caches: Vec::new(),
        sandbox_write_paths: Vec::new(),
        applied_sandbox_approvals: Vec::new(),
        run_links: Default::default(),
        crews: Vec::new(),
    }
}

/// What a binding binds the task to.
enum BindTarget<'a> {
    /// This project.
    Project(&'a str),
    /// The live project that owns this folder (canonical), else a new one
    /// with this trust.
    Folder {
        folder: &'a Path,
        trust: VibeDevProjectTrust,
    },
}

/// Bind under the store lock, deciding from the store as read under it: a
/// project another caller created or bound meanwhile is seen, and a task that
/// already belongs to a different project is refused.
async fn bind_in_store(
    scope_root: &Path,
    workspace_root: &Path,
    task_id: &str,
    target: BindTarget<'_>,
) -> Result<VibeDevProjectRecord, BindError> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return Err(BindError::Store(
            "a work-location binding needs a task id".to_string(),
        ));
    }
    let path = vibedev_project_store_path(scope_root);
    let lock = project_store_lock(&path);
    let _guard = lock.lock().await;
    let mut store = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice::<VibeDevProjectStore>(&bytes)
            .map_err(|error| BindError::Store(format!("{}: {error}", path.display())))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            VibeDevProjectStore::default()
        },
        Err(error) => return Err(BindError::Store(format!("{}: {error}", path.display()))),
    };
    normalize_vibedev_project_store(&mut store);
    let (existing, wanted) = match &target {
        BindTarget::Project(project_id) => {
            let index = store
                .projects
                .iter()
                .position(|project| project.project_id == *project_id && !project.archived)
                .ok_or_else(|| {
                    BindError::Store(format!("no live VibeDev project {project_id} in this scope"))
                })?;
            let wanted =
                project_repo_folder(store.projects[index].repo_path.as_deref(), workspace_root);
            (Some(index), wanted)
        },
        BindTarget::Folder { folder, .. } => (
            owning_index(&store.projects, workspace_root, folder),
            folder.to_path_buf(),
        ),
    };
    let chosen_id = existing.map(|index| store.projects[index].project_id.clone());
    if let Some(other) = store.projects.iter().find(|project| {
        !project.archived
            && Some(project.project_id.as_str()) != chosen_id.as_deref()
            && task_belongs_to(project, task_id)
    }) {
        return Err(BindError::BelongsElsewhere(belongs_elsewhere_refusal(
            other,
            workspace_root,
            &wanted,
        )));
    }
    let (index, created) = match (existing, &target) {
        (Some(index), _) => (index, false),
        (None, BindTarget::Folder { folder, trust }) => {
            let project = new_folder_project(folder, *trust, &store.projects);
            store.projects.push(project);
            (store.projects.len() - 1, true)
        },
        (None, BindTarget::Project(_)) => unreachable!("a project target always has an index"),
    };
    let changed = merge_binding(&mut store.projects[index], task_id) || created;
    if changed {
        store.projects[index].updated_at_ms = chrono::Utc::now().timestamp_millis();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| BindError::Store(format!("{}: {error}", parent.display())))?;
        }
        let payload = serde_json::to_vec_pretty(&store)
            .map_err(|error| BindError::Store(error.to_string()))?;
        crate::magician_v2::artifact_v2::io::write_bytes_durably(&path, &payload)
            .await
            .map_err(|error| BindError::Store(format!("{}: {error}", path.display())))?;
    }
    Ok(store.projects[index].clone())
}

/// Bind `task_id` to an existing project, under the project's trust mode.
pub async fn bind_task_to_project(
    scope_root: &Path,
    workspace_root: &Path,
    project: &VibeDevProjectRecord,
    task_id: &str,
) -> Result<VibeDevProjectRecord, BindError> {
    bind_in_store(
        scope_root,
        workspace_root,
        task_id,
        BindTarget::Project(&project.project_id),
    )
    .await
}

/// Bind `task_id` to the project for `folder`: the live project that owns
/// it — one another hop created meanwhile included, keeping its own trust —
/// else a new one with `trust`, named after the folder (de-duplicated) and
/// pointing at it by absolute path.
pub async fn bind_task_to_folder(
    scope_root: &Path,
    workspace_root: &Path,
    folder: &Path,
    trust: VibeDevProjectTrust,
    task_id: &str,
) -> Result<VibeDevProjectRecord, BindError> {
    let folder = folder
        .canonicalize()
        .map_err(|error| BindError::Store(format!("{}: {error}", folder.display())))?;
    if !folder.is_dir() {
        return Err(BindError::Store(format!("{} is not a folder", folder.display())));
    }
    bind_in_store(
        scope_root,
        workspace_root,
        task_id,
        BindTarget::Folder {
            folder: &folder,
            trust,
        },
    )
    .await
}

type AskLocks = Mutex<HashMap<(String, PathBuf), Arc<tokio::sync::Mutex<()>>>>;

/// The lock one task's question about one folder is asked under, so parallel
/// calls of the task ask once: the second waits, then finds the binding. One
/// small entry per (task, folder) asked about, for the life of the process.
pub fn ask_lock(task_id: &str, folder: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<AskLocks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        locks
            .entry((task_id.to_string(), folder.to_path_buf()))
            .or_default(),
    )
}

/// Executions remembered at most; past it the memory starts over, which can
/// only cost a run one repeated question.
const DECLINED_EXECUTIONS_MAX: usize = 4096;

type Declined = Mutex<HashMap<String, HashSet<(String, PathBuf)>>>;

fn declined() -> &'static Declined {
    static DECLINED: OnceLock<Declined> = OnceLock::new();
    DECLINED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember that the owner declined (Cancel or no answer) to let this
/// execution's task work on `folder`, so later calls of the run are refused
/// without asking again. In memory, per execution.
pub fn remember_declined(execution_id: &str, task_id: &str, folder: &Path) {
    let mut declined = declined()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if declined.len() >= DECLINED_EXECUTIONS_MAX && !declined.contains_key(execution_id) {
        declined.clear();
    }
    declined
        .entry(execution_id.to_string())
        .or_default()
        .insert((task_id.to_string(), folder.to_path_buf()));
}

/// Whether the owner already declined `folder` for this execution's task.
pub fn was_declined(execution_id: &str, task_id: &str, folder: &Path) -> bool {
    declined()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(execution_id)
        .is_some_and(|folders| folders.contains(&(task_id.to_string(), folder.to_path_buf())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::vibedev::projects::{VibeDevProjectPointer, VibeDevRunLink};

    fn scope() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let scope_root = temp.path().join("scope");
        let home = scope_root.join("workdirs/home");
        std::fs::create_dir_all(&home).expect("home");
        (temp, scope_root, home)
    }

    #[test]
    fn the_question_names_the_folder_and_its_answers_map_to_trust() {
        let question = work_location_question(Path::new("/src/magician"), false);
        assert!(question.starts_with("Where should Magician work on magician?"));
        assert!(question.contains("/src/magician"));
        assert!(!question.contains("Magician's own source code"));
        assert!(work_location_question(Path::new("/src/magician"), true)
            .contains("This is Magician's own source code."));
        assert_eq!(WORK_LOCATION_CHOICES[0].0, "isolated_workspace");
        assert_eq!(
            trust_for_choice("isolated_workspace"),
            Some(VibeDevProjectTrust::Sandboxed)
        );
        assert_eq!(
            trust_for_choice("isolated_branch"),
            Some(VibeDevProjectTrust::Trusted)
        );
        assert_eq!(
            trust_for_choice("original_folder"),
            Some(VibeDevProjectTrust::Original)
        );
        assert_eq!(trust_for_choice("cancel"), None);
        assert_eq!(trust_for_choice("deny"), None);
    }

    #[test]
    fn the_folder_asked_about_is_the_nearest_repository_root_never_a_broad_one() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical");
        let home = base.join("home");
        // The home folder is itself a repository (dotfiles), and holds two.
        std::fs::create_dir_all(home.join(".git")).expect("home git");
        let outer = home.join("outer");
        let inner = outer.join("vendor/inner");
        std::fs::create_dir_all(outer.join(".git")).expect("outer git");
        std::fs::create_dir_all(inner.join(".git")).expect("inner git");
        std::fs::create_dir_all(inner.join("src")).expect("src");
        let plain = home.join("notes");
        std::fs::create_dir_all(&plain).expect("plain");
        let home = Some(home.as_path());

        // The nearest root, not an outer one.
        assert_eq!(
            nearest_git_root_in(&inner.join("src"), home),
            Some(inner.clone())
        );
        assert_eq!(nearest_git_root_in(&outer, home), Some(outer.clone()));
        // A folder whose nearest root is the home folder has no root …
        assert_eq!(nearest_git_root_in(&plain, home), None);
        // … so the question is about the folder itself.
        assert_eq!(work_location_folder_in(&plain, home), Some(plain.clone()));
        // The home folder, `/` and anything holding the home folder are never
        // one project.
        assert_eq!(work_location_folder_in(home.unwrap(), home), None);
        assert_eq!(work_location_folder_in(&base, home), None);
        assert_eq!(work_location_folder_in(Path::new("/"), home), None);
    }

    #[tokio::test]
    async fn a_new_folder_becomes_a_project_and_a_second_bind_reuses_it() {
        let (temp, scope_root, home) = scope();
        let folder = temp.path().join("magician");
        std::fs::create_dir_all(folder.join("skills")).expect("folder");
        let canonical = folder.canonicalize().expect("canonical");

        assert!(project_bound_to_task(&scope_root, "task-a").is_none());
        let created = bind_task_to_folder(
            &scope_root,
            &home,
            &folder,
            VibeDevProjectTrust::Trusted,
            "task-a",
        )
        .await
        .expect("bind");
        assert_eq!(created.name, "magician");
        assert_eq!(created.trust, VibeDevProjectTrust::Trusted);
        assert_eq!(
            created.repo_path.as_deref(),
            Some(canonical.display().to_string().as_str())
        );
        let bound = project_bound_to_task(&scope_root, "task-a").expect("bound");
        assert_eq!(bound.project_id, created.project_id);
        assert_eq!(
            bound.run_links["task-a"].work_location,
            Some(VibeDevWorkLocation {
                mode: VibeDevProjectTrust::Trusted,
                repo_path: created.repo_path.clone(),
            })
        );

        // Another task touching a folder inside it binds to the same project
        // under that project's trust, whatever trust it would have picked.
        let again = bind_task_to_folder(
            &scope_root,
            &home,
            &folder.join("skills"),
            VibeDevProjectTrust::Original,
            "task-b",
        )
        .await
        .expect("bind again");
        assert_eq!(again.project_id, created.project_id);
        assert_eq!(read_vibedev_projects(&scope_root).len(), 1);
        assert_eq!(
            project_bound_to_task(&scope_root, "task-b")
                .and_then(|project| project.run_links["task-b"].work_location.clone())
                .map(|location| location.mode),
            Some(VibeDevProjectTrust::Trusted)
        );
        assert!(project_owns(&created, &home, &canonical.join("skills")));
        assert!(!project_owns(&created, &home, temp.path()));
    }

    #[tokio::test]
    async fn a_task_bound_to_one_project_is_refused_another() {
        let (temp, scope_root, home) = scope();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).expect("first");
        std::fs::create_dir_all(&second).expect("second");
        let one = bind_task_to_folder(
            &scope_root,
            &home,
            &first,
            VibeDevProjectTrust::Sandboxed,
            "task-a",
        )
        .await
        .expect("bind");
        let other = bind_task_to_folder(
            &scope_root,
            &home,
            &second,
            VibeDevProjectTrust::Sandboxed,
            "task-b",
        )
        .await
        .expect("other task");
        // Neither a new folder nor another project's folder rebinds it.
        for refused in [
            bind_task_to_folder(
                &scope_root,
                &home,
                &second,
                VibeDevProjectTrust::Sandboxed,
                "task-a",
            )
            .await,
            bind_task_to_project(&scope_root, &home, &other, "task-a").await,
        ] {
            match refused {
                Err(BindError::BelongsElsewhere(message)) => {
                    assert!(message.contains("“first”"), "{message}")
                },
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
        assert_eq!(
            project_bound_to_task(&scope_root, "task-a").map(|project| project.project_id),
            Some(one.project_id.clone())
        );
        // Binding to its own project again changes nothing.
        let same = bind_task_to_project(&scope_root, &home, &one, "task-a")
            .await
            .expect("same project");
        assert_eq!(same.project_id, one.project_id);
    }

    #[tokio::test]
    async fn binding_keeps_what_the_run_link_already_says() {
        let (temp, scope_root, home) = scope();
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).expect("folder");
        let project = bind_task_to_folder(
            &scope_root,
            &home,
            &folder,
            VibeDevProjectTrust::Sandboxed,
            "task-root",
        )
        .await
        .expect("bind");
        // A run the cockpit admitted with lineage and a crew lane, but no
        // location.
        let link = VibeDevRunLink {
            work_location: None,
            parent_task_id: Some("task-root".to_string()),
            crew_id: Some("crew-1".to_string()),
            lane: Some("API".to_string()),
        };
        crate::magician_v2::vibedev::projects::record_vibedev_project_run(
            &scope_root,
            &project.project_id,
            "task-lane",
            &link,
            false,
        )
        .await
        .expect("admit");
        crate::magician_v2::vibedev::projects::set_vibedev_project_active_root_task_id(
            &scope_root,
            &project.project_id,
            VibeDevProjectPointer::PinTo("task-root"),
        )
        .await
        .expect("pin");
        bind_task_to_project(&scope_root, &home, &project, "task-lane")
            .await
            .expect("bind lane");
        let stored = read_vibedev_projects(&scope_root)
            .into_iter()
            .next()
            .expect("project")
            .run_links["task-lane"]
            .clone();
        assert_eq!(stored.parent_task_id.as_deref(), Some("task-root"));
        assert_eq!(stored.crew_id.as_deref(), Some("crew-1"));
        assert_eq!(stored.lane.as_deref(), Some("API"));
        assert_eq!(
            stored.work_location.map(|location| location.mode),
            Some(VibeDevProjectTrust::Sandboxed)
        );
    }

    #[tokio::test]
    async fn a_name_another_project_uses_is_numbered() {
        let (temp, scope_root, home) = scope();
        for parent in ["a", "b"] {
            let folder = temp.path().join(parent).join("app");
            std::fs::create_dir_all(&folder).expect("folder");
            bind_task_to_folder(
                &scope_root,
                &home,
                &folder,
                VibeDevProjectTrust::Sandboxed,
                &format!("task-{parent}"),
            )
            .await
            .expect("bind");
        }
        let mut names: Vec<String> = read_vibedev_projects(&scope_root)
            .into_iter()
            .map(|project| project.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["app".to_string(), "app 2".to_string()]);
    }

    #[tokio::test]
    async fn parallel_asks_for_one_task_and_folder_share_one_lock() {
        let folder = Path::new("/tmp/ask-lock-folder");
        let first = ask_lock("task-lock", folder);
        let held = first.lock().await;
        assert!(ask_lock("task-lock", folder).try_lock().is_err());
        assert!(ask_lock("task-other", folder).try_lock().is_ok());
        drop(held);
        assert!(ask_lock("task-lock", folder).try_lock().is_ok());
    }

    #[test]
    fn a_declined_folder_is_remembered_for_its_execution_only() {
        let folder = Path::new("/tmp/declined-folder");
        assert!(!was_declined("exec-declined", "task-1", folder));
        remember_declined("exec-declined", "task-1", folder);
        assert!(was_declined("exec-declined", "task-1", folder));
        assert!(!was_declined("exec-other", "task-1", folder));
        assert!(!was_declined("exec-declined", "task-1", Path::new("/tmp/elsewhere")));
    }
}
