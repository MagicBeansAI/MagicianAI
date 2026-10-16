//! Durable task-recipe records and their task/shape lookup index.

use super::path_safe::ensure_safe_record_id;
use super::recipe::TaskRecipe;
use crate::magician_v2::artifact_v2::workspace::{ArtifactV2Workspace, WorkspaceFileEntry};
use crate::magician_v2::artifact_v2::ArtifactV2Error;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RecipeIndex {
    #[serde(default)]
    pub by_task_id: HashMap<String, String>,
    #[serde(default)]
    pub by_shape: HashMap<String, Vec<String>>,
}

pub struct RecipeStore {
    base: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

static INDEX_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
static REPLAY_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();
static SCOPE_MUTATION_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn index_lock(path: &PathBuf) -> Arc<Mutex<()>> {
    let locks = INDEX_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks.lock().unwrap_or_else(|error| error.into_inner());
    guard.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = guard.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    guard.insert(path.clone(), Arc::downgrade(&lock));
    lock
}

impl RecipeStore {
    pub fn new(base: PathBuf) -> Self {
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base),
            base,
        }
    }

    fn recipes_dir(&self) -> PathBuf {
        self.base.join("recipes")
    }

    fn index_path(&self) -> PathBuf {
        self.base.join("recipe_index.json")
    }

    fn entry_path(&self, entry: &WorkspaceFileEntry) -> PathBuf {
        self.base.join(&entry.relative_path)
    }

    /// Serialize the load → replay → save transaction for one recipe across
    /// task-start, HTTP, and compiled-tool surfaces in this process.
    pub fn replay_lock(&self, recipe_id: &str) -> io::Result<Arc<tokio::sync::Mutex<()>>> {
        ensure_safe_record_id(recipe_id, "recipe id")?;
        let path = self.recipes_dir().join(format!("{recipe_id}.json"));
        let locks = REPLAY_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&path).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(path, Arc::downgrade(&lock));
        Ok(lock)
    }

    /// Serialize scope-wide recipe publication and destructive lifecycle work.
    /// Replays retain their narrower per-recipe locks; purge takes both locks
    /// so it cannot race either a compiler publication or an in-flight replay.
    pub fn scope_mutation_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        let locks = SCOPE_MUTATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&self.base).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(self.base.clone(), Arc::downgrade(&lock));
        lock
    }

    /// Acquire recipe locks in stable order. Callers must keep the returned
    /// guards alive through every catalog/skill/data mutation in the purge.
    pub async fn lock_replays(
        &self,
        recipe_ids: impl IntoIterator<Item = String>,
    ) -> io::Result<Vec<tokio::sync::OwnedMutexGuard<()>>> {
        let mut recipe_ids: Vec<_> = recipe_ids.into_iter().collect();
        recipe_ids.sort_unstable();
        recipe_ids.dedup();
        let mut guards = Vec::with_capacity(recipe_ids.len());
        for recipe_id in recipe_ids {
            guards.push(self.replay_lock(&recipe_id)?.lock_owned().await);
        }
        Ok(guards)
    }

    fn is_not_found(error: &ArtifactV2Error) -> bool {
        matches!(error, ArtifactV2Error::Io(inner) if inner.kind() == io::ErrorKind::NotFound)
    }

    pub fn save(&self, recipe: &TaskRecipe) -> io::Result<()> {
        ensure_safe_record_id(&recipe.id, "recipe id")?;
        let lock = index_lock(&self.index_path());
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        self.save_and_index_unlocked(recipe, None)
    }

    /// Publish a recipe and its originating task binding under one index lock.
    /// This prevents an origin purge from interleaving between the recipe file
    /// write and task-index publication and leaving a binding to deleted data.
    pub fn save_and_bind(&self, recipe: &TaskRecipe, task_id: &str) -> io::Result<()> {
        ensure_safe_record_id(&recipe.id, "recipe id")?;
        ensure_safe_record_id(task_id, "task id")?;
        let lock = index_lock(&self.index_path());
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        self.save_and_index_unlocked(recipe, Some(task_id))
    }

    fn save_and_index_unlocked(
        &self,
        recipe: &TaskRecipe,
        task_id: Option<&str>,
    ) -> io::Result<()> {
        let dir = self.recipes_dir();
        self.workspace_layout
            .create_dir_all_path_sync(&dir)
            .map_err(io::Error::other)?;
        let recipe_path = dir.join(format!("{}.json", recipe.id));
        let previous_bytes = match self.workspace_layout.read_to_string_path_sync(&recipe_path) {
            Ok(json) => Some(json.into_bytes()),
            Err(error) if Self::is_not_found(&error) => None,
            Err(error) => return Err(io::Error::other(error)),
        };
        // Read and construct the next index before replacing the recipe file.
        // A corrupt/unreadable index must not create an unindexed orphan.
        let mut index = self.read_index()?;
        for ids in index.by_shape.values_mut() {
            ids.retain(|id| id != &recipe.id);
        }
        index.by_shape.retain(|_, ids| !ids.is_empty());
        index
            .by_shape
            .entry(recipe.shape.fingerprint.clone())
            .or_default()
            .push(recipe.id.clone());
        if let Some(task_id) = task_id {
            index
                .by_task_id
                .insert(task_id.to_owned(), recipe.id.clone());
        }
        let json = serde_json::to_string_pretty(recipe).map_err(io::Error::other)?;
        self.workspace_layout
            .write_atomic_path_sync(&recipe_path, json.as_bytes())
            .map_err(io::Error::other)?;
        if let Err(index_error) = self.write_index_unlocked(&index) {
            let rollback = match previous_bytes {
                Some(previous) => self
                    .workspace_layout
                    .write_atomic_path_sync(&recipe_path, &previous)
                    .map_err(io::Error::other),
                None => match self.workspace_layout.remove_file_path_sync(&recipe_path) {
                    Ok(()) => Ok(()),
                    Err(error) if Self::is_not_found(&error) => Ok(()),
                    Err(error) => Err(io::Error::other(error)),
                },
            };
            return match rollback {
                Ok(()) => Err(index_error),
                Err(rollback_error) => Err(io::Error::other(format!(
                    "recipe index publication failed ({index_error}); recipe rollback also failed ({rollback_error})"
                ))),
            };
        }
        Ok(())
    }

    pub fn load(&self, recipe_id: &str) -> io::Result<Option<TaskRecipe>> {
        ensure_safe_record_id(recipe_id, "recipe id")?;
        let path = self.recipes_dir().join(format!("{recipe_id}.json"));
        match self.workspace_layout.read_to_string_path_sync(&path) {
            Ok(json) => {
                let recipe: TaskRecipe = serde_json::from_str(&json)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if recipe.id != recipe_id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recipe payload identity does not match its filename",
                    ));
                }
                Ok(Some(recipe))
            },
            Err(error) if Self::is_not_found(&error) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    /// Validate durable scope before exposing recipe content or using the
    /// caller's credentials. A copied/corrupt record is not scope authority.
    pub fn load_for_scope(
        &self,
        recipe_id: &str,
        principal: &str,
        workspace: &str,
    ) -> io::Result<Option<TaskRecipe>> {
        let recipe = self.load(recipe_id)?;
        if recipe.as_ref().is_some_and(|recipe| {
            recipe.scope_principal != principal || recipe.scope_workspace != workspace
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recipe durable scope does not match the requested scope",
            ));
        }
        Ok(recipe)
    }

    pub fn list_for_scope(&self, principal: &str, workspace: &str) -> io::Result<Vec<TaskRecipe>> {
        let mut recipes = self.list()?;
        recipes.retain(|recipe| {
            recipe.scope_principal == principal && recipe.scope_workspace == workspace
        });
        Ok(recipes)
    }

    pub fn list(&self) -> io::Result<Vec<TaskRecipe>> {
        let entries = match self
            .workspace_layout
            .read_dir_path_sync(&self.recipes_dir())
        {
            Ok(entries) => entries,
            Err(error) if Self::is_not_found(&error) => return Ok(Vec::new()),
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut recipes = Vec::with_capacity(entries.len());
        for entry in entries.into_iter().filter(|entry| entry.is_file) {
            let path = self.entry_path(&entry);
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match self.workspace_layout.read_to_string_path_sync(&path) {
                Ok(json) => match serde_json::from_str::<TaskRecipe>(&json) {
                    Ok(recipe)
                        if path.file_stem().and_then(|value| value.to_str())
                            == Some(recipe.id.as_str()) =>
                    {
                        recipes.push(recipe)
                    },
                    Ok(recipe) => tracing::warn!(
                        "[API_MINING] skipping recipe whose stored id `{}` does not match {}",
                        recipe.id,
                        path.display()
                    ),
                    Err(error) => tracing::warn!(
                        "[API_MINING] skipping malformed recipe {}: {error}",
                        path.display()
                    ),
                },
                Err(error) => tracing::warn!(
                    "[API_MINING] skipping unreadable recipe {}: {error}",
                    path.display()
                ),
            }
        }
        Ok(recipes)
    }

    /// Enumerate lock identities without deserializing recipe content.
    /// Destructive paths use this so a malformed file cannot hide an
    /// in-flight replay from their lock set.
    pub fn list_ids(&self) -> io::Result<Vec<String>> {
        self.enumerate_recipe_ids(true)
    }

    /// Enumerate every identity that can be accepted by `replay_lock`, while
    /// ignoring unaddressable filenames. Full scope purge uses this because an
    /// invalid filename cannot have an in-flight replay and must not prevent
    /// deletion of the whole mining tree.
    pub fn list_lockable_ids(&self) -> io::Result<Vec<String>> {
        self.enumerate_recipe_ids(false)
    }

    fn enumerate_recipe_ids(&self, strict: bool) -> io::Result<Vec<String>> {
        let entries = match self
            .workspace_layout
            .read_dir_path_sync(&self.recipes_dir())
        {
            Ok(entries) => entries,
            Err(error) if Self::is_not_found(&error) => return Ok(Vec::new()),
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut ids = Vec::new();
        for entry in entries.into_iter().filter(|entry| entry.is_file) {
            let path = self.entry_path(&entry);
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                if strict {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recipe filename is not UTF-8",
                    ));
                }
                tracing::warn!(
                    "[API_MINING] full purge cannot lock unaddressable recipe filename {}",
                    path.display()
                );
                continue;
            };
            if let Err(error) = ensure_safe_record_id(id, "recipe id") {
                if strict {
                    return Err(error);
                }
                tracing::warn!(
                    "[API_MINING] full purge skipping invalid replay-lock identity {}: {error}",
                    path.display()
                );
                continue;
            }
            let id = id.to_owned();
            ids.push(id);
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Read every recipe file and require the filename identity to agree with
    /// its payload. Destructive origin cleanup calls this while holding every
    /// replay lock so corrupt state is reported instead of silently omitted.
    pub fn list_strict(&self) -> io::Result<Vec<TaskRecipe>> {
        let ids = self.list_ids()?;
        let mut recipes = Vec::with_capacity(ids.len());
        for id in ids {
            let recipe = self.load(&id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("recipe `{id}` disappeared during strict enumeration"),
                )
            })?;
            if recipe.id != id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "recipe filename id `{id}` does not match stored id `{}`",
                        recipe.id
                    ),
                ));
            }
            recipes.push(recipe);
        }
        Ok(recipes)
    }

    pub fn find_by_task(&self, task_id: &str) -> io::Result<Option<TaskRecipe>> {
        ensure_safe_record_id(task_id, "task id")?;
        match self.read_index()?.by_task_id.get(task_id) {
            Some(id) => self.load(id),
            None => Ok(None),
        }
    }

    pub fn find_by_shape(&self, fingerprint: &str) -> io::Result<Vec<TaskRecipe>> {
        let index = self.read_index()?;
        let mut seen = HashSet::new();
        let mut recipes = Vec::new();
        for id in index.by_shape.get(fingerprint).into_iter().flatten() {
            if seen.insert(id) {
                if let Some(recipe) = self.load(id)? {
                    recipes.push(recipe);
                }
            }
        }
        Ok(recipes)
    }

    /// Delete one recipe, its run ledger, and every task/shape binding. This
    /// is the exact rollback primitive used when publication loses its live
    /// feature-switch race; unlike `remove_origin`, it cannot remove an
    /// unrelated recipe that happens to share an origin.
    pub fn remove_recipe(&self, recipe_id: &str) -> io::Result<bool> {
        ensure_safe_record_id(recipe_id, "recipe id")?;
        let lock = index_lock(&self.index_path());
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut removed = false;
        let file = self.recipes_dir().join(format!("{recipe_id}.json"));
        match self.workspace_layout.remove_file_path_sync(&file) {
            Ok(()) => removed = true,
            Err(error) if Self::is_not_found(&error) => {},
            Err(error) => return Err(io::Error::other(error)),
        }
        let run_dir = self.recipes_dir().join(recipe_id);
        match self.workspace_layout.remove_dir_all_path_sync(&run_dir) {
            Ok(()) => removed = true,
            Err(error) if Self::is_not_found(&error) => {},
            Err(error) => return Err(io::Error::other(error)),
        }
        let mut index = self.read_index()?;
        let task_bindings_before = index.by_task_id.len();
        index
            .by_task_id
            .retain(|_, candidate| candidate != recipe_id);
        removed |= index.by_task_id.len() != task_bindings_before;
        for recipe_ids in index.by_shape.values_mut() {
            let shape_bindings_before = recipe_ids.len();
            recipe_ids.retain(|candidate| candidate != recipe_id);
            removed |= recipe_ids.len() != shape_bindings_before;
        }
        index
            .by_shape
            .retain(|_, recipe_ids| !recipe_ids.is_empty());
        self.write_index_unlocked(&index)?;
        Ok(removed)
    }

    /// Delete every recipe version that learned or can replay the origin and
    /// remove its task/shape bindings. Cross-origin recipes are removed as a
    /// unit: retaining the other steps would leave an incomplete DAG that can
    /// silently execute a partial task.
    pub fn remove_origin(&self, origin: &str) -> io::Result<Vec<String>> {
        let lock = index_lock(&self.index_path());
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let recipes = self.list_strict()?;
        let removed_ids: Vec<_> = recipes
            .into_iter()
            .filter(|recipe| {
                recipe.versions.iter().any(|version| {
                    version.origins.iter().any(|candidate| candidate == origin)
                        || version.steps.iter().any(|step| step.origin == origin)
                })
            })
            .map(|recipe| recipe.id)
            .collect();
        if removed_ids.is_empty() {
            return Ok(removed_ids);
        }
        for recipe_id in &removed_ids {
            ensure_safe_record_id(recipe_id, "recipe id")?;
        }
        for recipe_id in &removed_ids {
            let file = self.recipes_dir().join(format!("{recipe_id}.json"));
            match self.workspace_layout.remove_file_path_sync(&file) {
                Ok(()) => {},
                Err(error) if Self::is_not_found(&error) => {},
                Err(error) => return Err(io::Error::other(error)),
            }
            let run_dir = self.recipes_dir().join(recipe_id);
            match self.workspace_layout.remove_dir_all_path_sync(&run_dir) {
                Ok(()) => {},
                Err(error) if Self::is_not_found(&error) => {},
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        let removed: HashSet<_> = removed_ids.iter().cloned().collect();
        let mut index = self.read_index()?;
        index
            .by_task_id
            .retain(|_, recipe_id| !removed.contains(recipe_id));
        for recipe_ids in index.by_shape.values_mut() {
            recipe_ids.retain(|recipe_id| !removed.contains(recipe_id));
        }
        index
            .by_shape
            .retain(|_, recipe_ids| !recipe_ids.is_empty());
        self.write_index_unlocked(&index)?;
        Ok(removed_ids)
    }

    pub fn read_index(&self) -> io::Result<RecipeIndex> {
        match self
            .workspace_layout
            .read_to_string_path_sync(&self.index_path())
        {
            Ok(json) => serde_json::from_str(&json)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if Self::is_not_found(&error) => Ok(RecipeIndex::default()),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn write_index_unlocked(&self, index: &RecipeIndex) -> io::Result<()> {
        self.workspace_layout
            .create_dir_all_path_sync(&self.base)
            .map_err(io::Error::other)?;
        let json = serde_json::to_string_pretty(&index).map_err(io::Error::other)?;
        self.workspace_layout
            .write_atomic_path_sync(self.index_path(), json.as_bytes())
            .map_err(io::Error::other)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::recipe::*;
    use tempfile::TempDir;

    fn minimal(id: &str, fingerprint: &str) -> TaskRecipe {
        TaskRecipe {
            id: id.into(),
            scope_principal: "p".into(),
            scope_workspace: "w".into(),
            agent_id: "a".into(),
            shape: RecipeShape {
                description_template: None,
                template: "t".into(),
                fingerprint: fingerprint.into(),
                inputs: vec![],
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec![],
                steps: vec![],
                data_flows: vec![],
                answer_spec: vec![],
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: Default::default(),
                compiled_from: CompiledFrom {
                    task_id: "task_x".into(),
                    execution_id: "e".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: vec![],
                    trace_files: vec![],
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn save_then_load_and_index_by_task_and_shape() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        store
            .save_and_bind(&minimal("rcp_1", "fp_a"), "task_x")
            .unwrap();
        assert_eq!(store.load("rcp_1").unwrap().unwrap().id, "rcp_1");
        assert_eq!(store.find_by_task("task_x").unwrap().unwrap().id, "rcp_1");
        assert_eq!(store.find_by_shape("fp_a").unwrap().len(), 1);
        assert!(store.find_by_shape("fp_zzz").unwrap().is_empty());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn scope_aware_reads_reject_copied_records_before_exposing_content() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        store.save(&minimal("rcp_scope", "fp")).unwrap();
        assert!(store
            .load_for_scope("rcp_scope", "p", "w")
            .unwrap()
            .is_some());
        for (principal, workspace) in [("other", "w"), ("p", "other")] {
            assert_eq!(
                store
                    .load_for_scope("rcp_scope", principal, workspace)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
            );
            assert!(store
                .list_for_scope(principal, workspace)
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn save_is_idempotent_for_the_shape_index() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        let recipe = minimal("rcp_1", "fp_a");
        store.save(&recipe).unwrap();
        store.save(&recipe).unwrap();
        assert_eq!(store.find_by_shape("fp_a").unwrap().len(), 1);
    }

    #[test]
    fn an_unreadable_index_cannot_create_an_unindexed_recipe() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("recipe_index.json"), b"not-json").unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());

        assert!(store
            .save_and_bind(&minimal("rcp_orphan", "fp_orphan"), "task_orphan")
            .is_err());

        assert!(!tmp.path().join("recipes/rcp_orphan.json").exists());
    }

    #[test]
    fn list_ids_includes_a_malformed_recipe_for_conservative_locking() {
        let tmp = TempDir::new().unwrap();
        let recipes = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        std::fs::write(recipes.join("rcp_malformed.json"), b"not-json").unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());

        assert_eq!(store.list_ids().unwrap(), vec!["rcp_malformed"]);
        assert!(store.list().unwrap().is_empty());
        assert!(store.list_strict().is_err());
    }

    #[test]
    fn strict_list_rejects_a_filename_payload_identity_mismatch() {
        let tmp = TempDir::new().unwrap();
        let recipes = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        std::fs::write(
            recipes.join("rcp_filename.json"),
            serde_json::to_vec(&minimal("rcp_payload", "fp_mismatch")).unwrap(),
        )
        .unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());

        assert!(store.list().unwrap().is_empty());
        assert!(store.list_strict().is_err());
    }

    #[test]
    fn full_purge_lock_enumeration_skips_unaddressable_recipe_ids() {
        let tmp = TempDir::new().unwrap();
        let recipes = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        std::fs::write(recipes.join("invalid id.json"), b"not-json").unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());

        assert!(store.list_ids().is_err());
        assert!(store.list_lockable_ids().unwrap().is_empty());
    }

    #[test]
    fn remove_origin_deletes_cross_origin_recipe_runs_and_all_bindings() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        let mut removed_recipe = minimal("rcp_removed", "fp_removed");
        removed_recipe.versions[0].origins =
            vec!["https://api.one.test".into(), "https://api.two.test".into()];
        let mut kept_recipe = minimal("rcp_kept", "fp_kept");
        kept_recipe.versions[0].origins = vec!["https://api.three.test".into()];
        store
            .save_and_bind(&removed_recipe, "task_removed")
            .unwrap();
        store.save_and_bind(&kept_recipe, "task_kept").unwrap();
        let run_dir = tmp.path().join("recipes/rcp_removed");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("run.jsonl"), b"{}\n").unwrap();

        let removed = store.remove_origin("https://api.one.test").unwrap();

        assert_eq!(removed, vec!["rcp_removed"]);
        assert!(store.load("rcp_removed").unwrap().is_none());
        assert!(!run_dir.exists());
        assert!(store.find_by_task("task_removed").unwrap().is_none());
        assert!(store.find_by_shape("fp_removed").unwrap().is_empty());
        assert_eq!(
            store.find_by_task("task_kept").unwrap().unwrap().id,
            "rcp_kept"
        );
    }

    #[test]
    fn remove_recipe_is_exact_and_cleans_all_of_its_bindings() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        store
            .save_and_bind(&minimal("rcp_removed", "fp_removed"), "task_removed")
            .unwrap();
        store
            .save_and_bind(&minimal("rcp_kept", "fp_kept"), "task_kept")
            .unwrap();
        let run_dir = tmp.path().join("recipes/rcp_removed");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("run.jsonl"), b"{}\n").unwrap();

        assert!(store.remove_recipe("rcp_removed").unwrap());

        assert!(store.load("rcp_removed").unwrap().is_none());
        assert!(!run_dir.exists());
        assert!(store.find_by_task("task_removed").unwrap().is_none());
        assert!(store.find_by_shape("fp_removed").unwrap().is_empty());
        assert_eq!(
            store.find_by_task("task_kept").unwrap().unwrap().id,
            "rcp_kept"
        );
    }

    #[tokio::test]
    async fn purge_lock_protocol_waits_for_an_in_flight_replay() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let store = RecipeStore::new(root.clone());
        let replay_guard = store
            .replay_lock("rcp_serialized")
            .unwrap()
            .lock_owned()
            .await;
        let waiter = tokio::spawn(async move {
            RecipeStore::new(root)
                .lock_replays(vec!["rcp_serialized".to_string()])
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(replay_guard);
        let guards = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("purge lock should be released")
            .expect("lock waiter should join");
        assert_eq!(guards.len(), 1);
    }

    #[tokio::test]
    async fn compiler_and_purge_share_one_scope_mutation_lock() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let first = RecipeStore::new(root.clone())
            .scope_mutation_lock()
            .lock_owned()
            .await;
        let waiter = tokio::spawn(async move {
            RecipeStore::new(root)
                .scope_mutation_lock()
                .lock_owned()
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("scope mutation lock should be released")
            .expect("scope lock waiter should join");
    }

    #[test]
    fn unsafe_ids_are_rejected_before_touching_disk() {
        let tmp = TempDir::new().unwrap();
        let store = RecipeStore::new(tmp.path().to_path_buf());
        assert!(store.load("../etc/passwd").is_err());
        assert!(store
            .save_and_bind(&minimal("..", "unsafe_recipe"), "task")
            .is_err());
        assert!(store
            .save_and_bind(&minimal("rcp_safe", "unsafe_task"), "..")
            .is_err());
    }
}
