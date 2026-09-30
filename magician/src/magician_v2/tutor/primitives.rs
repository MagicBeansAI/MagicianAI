//! Data-driven Tutor drawing primitives.
//!
//! A Tutor drawing "primitive" is authored as a declarative JSON *recipe* that
//! describes which shape fields it reads (with `defaults`) plus an ordered list
//! of geometry `draw` ops. A generic interpreter on each client (iOS, web)
//! executes any recipe against the native canvas, so a new primitive is a new
//! file — never new native code and never an app rebuild.
//!
//! This backend module is intentionally thin: it **serves and validates**
//! recipes. It does NOT interpret geometry — each `draw` op is kept as an opaque
//! `serde_json::Value`, checked only for structural sanity and hostile-input
//! bounds (see [`TutorPrimitiveRecipe::validate`] and the `*_MAX_*` caps).
//!
//! Storage layout (all under the runtime root, `MAGICIAN_ROOT_DIR` /
//! `$HOME/MagicianNotes`):
//!
//! ```text
//! <root>/tutor_primitives/*.json                       ← shared/global built-ins
//! <root>/scopes/<principal>/<workspace>/tutor_primitives/*.json  ← per-scope custom
//! ```
//!
//! [`load`] reads the global folder then the scope folder, parses + validates
//! each file, **skips + warns** on any invalid file (a bad community recipe can
//! never crash the run or break the set), and merges by `type` (scope overrides
//! global).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::warn;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// The fixed, safe op vocabulary. A recipe may only reference these; anything
/// else is rejected at validation time. Adding an op is a deliberate,
/// cross-platform change to every interpreter — never done via a data file.
pub const KNOWN_OPS: [&str; 10] = [
    "line",
    "polyline",
    "polygon",
    "rect",
    "circle",
    "arc",
    "bezier",
    "arrowhead",
    "label",
    "cursive_label",
];

// Hostile-input guards (Appendix A of the design plan). A single bad/hostile
// file can never make the loader or a client expensive.
/// Maximum recipes served from one merged set.
pub const MAX_RECIPES: usize = 512;
/// Maximum `draw` ops in a single recipe.
pub const MAX_OPS_PER_RECIPE: usize = 64;
/// Maximum points referenced by a single op (e.g. a `polyline`/`polygon`).
pub const MAX_POINTS_PER_OP: usize = 256;

/// A declarative Tutor primitive recipe. Parsed from a `*.json` file; the
/// `draw` ops are kept as raw `serde_json::Value` because the backend only
/// serves + validates them — the client interpreter executes the geometry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TutorPrimitiveRecipe {
    /// The primary shape `type` this recipe renders.
    #[serde(rename = "type")]
    pub type_: String,
    /// Optional extra `type` values that resolve to the same recipe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Optional monotonically-increasing recipe version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Field defaults consulted after the shape's own fields during expression
    /// evaluation on the client.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub defaults: Map<String, Value>,
    /// Ordered list of draw ops. Each is an object with a string `op` in
    /// [`KNOWN_OPS`] plus op-specific params (kept opaque here).
    pub draw: Vec<Value>,
}

impl TutorPrimitiveRecipe {
    /// Validate a recipe's structure and enforce the hostile-input bounds.
    ///
    /// Checks: non-empty `type`; `draw` non-empty and within [`MAX_OPS_PER_RECIPE`];
    /// each op is an object carrying a string `op` in [`KNOWN_OPS`]; any op array
    /// param (`points`/`from`/`to`/…) is within [`MAX_POINTS_PER_OP`]. Geometry
    /// values themselves (numbers, field chains, expressions) are NOT interpreted
    /// here — the client does that.
    pub fn validate(&self) -> Result<(), String> {
        if self.type_.trim().is_empty() {
            return Err("recipe `type` must be a non-empty string".to_string());
        }
        if self.draw.is_empty() {
            return Err(format!("recipe `{}` has an empty `draw` list", self.type_));
        }
        if self.draw.len() > MAX_OPS_PER_RECIPE {
            return Err(format!(
                "recipe `{}` has {} draw ops (max {})",
                self.type_,
                self.draw.len(),
                MAX_OPS_PER_RECIPE
            ));
        }
        for (index, op_value) in self.draw.iter().enumerate() {
            let op_obj = op_value.as_object().ok_or_else(|| {
                format!("recipe `{}` draw op #{index} is not an object", self.type_)
            })?;
            let op_name = op_obj.get("op").and_then(Value::as_str).ok_or_else(|| {
                format!(
                    "recipe `{}` draw op #{index} is missing a string `op`",
                    self.type_
                )
            })?;
            if !KNOWN_OPS.contains(&op_name) {
                return Err(format!(
                    "recipe `{}` draw op #{index} has unknown `op` \"{op_name}\"",
                    self.type_
                ));
            }
            // Bound every array-valued param (points/from/to/…) so a hostile
            // file can't blow up an interpreter with a giant point list.
            for (key, value) in op_obj {
                if let Some(array) = value.as_array() {
                    if array.len() > MAX_POINTS_PER_OP {
                        return Err(format!(
                            "recipe `{}` draw op #{index} param `{key}` has {} points (max {})",
                            self.type_,
                            array.len(),
                            MAX_POINTS_PER_OP
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Every `type` value this recipe answers to — its primary `type` first,
    /// then its aliases, in order.
    pub fn all_types(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.type_.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// What one seeding pass changed.
///
/// `upgraded` is separate from `created` because they answer different
/// questions: `created` is a fresh install, `upgraded` means a built-in fix
/// reached a machine that already had the old copy — the case that silently
/// could not happen before versions were consulted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedOutcome {
    pub created: usize,
    pub upgraded: usize,
}

impl SeedOutcome {
    /// Whether anything was written at all.
    pub fn touched_anything(self) -> bool {
        self.created > 0 || self.upgraded > 0
    }
}

/// Resolve the global (shared) recipe folder under a runtime root:
/// `<base>/tutor_primitives`.
pub fn global_primitives_dir(base: &Path) -> PathBuf {
    base.join("tutor_primitives")
}

/// Resolve a scope's recipe folder under a runtime root:
/// `<base>/scopes/<principal>/<workspace>/tutor_primitives`. Path segments are
/// normalized through the same `scope_root` helper the rest of the runtime uses
/// (traversal-safe).
pub fn scope_primitives_dir(base: &Path, principal: &str, workspace: &str) -> PathBuf {
    ArtifactV2Workspace::new(base)
        .scope_root(principal, workspace)
        .join("tutor_primitives")
}

/// Load + merge the tutor primitive recipe set for a scope.
///
/// Reads `<base>/tutor_primitives/*.json` (global built-ins) then
/// `<base>/scopes/<principal>/<workspace>/tutor_primitives/*.json` (per-scope
/// custom / community), parsing and validating each file. Invalid or unreadable
/// files are **skipped with a `warn!`** — a bad recipe never breaks the set.
///
/// Recipes merge by `type` with **scope overriding global**. Files are visited
/// in sorted filename order for deterministic within-tier precedence (a later
/// file overriding an earlier one). The merged set is capped at [`MAX_RECIPES`]
/// (excess dropped with a `warn!`).
pub fn load(base: &Path, principal: &str, workspace: &str) -> Vec<TutorPrimitiveRecipe> {
    // BTreeMap keyed by primary `type` → deterministic ordering + merge-by-type.
    let mut merged: BTreeMap<String, TutorPrimitiveRecipe> = BTreeMap::new();

    // Global first, then scope (scope overrides on `type` collision).
    for dir in [
        global_primitives_dir(base),
        scope_primitives_dir(base, principal, workspace),
    ] {
        for recipe in load_dir(&dir) {
            merged.insert(recipe.type_.clone(), recipe);
        }
    }

    let mut recipes: Vec<TutorPrimitiveRecipe> = merged.into_values().collect();
    if recipes.len() > MAX_RECIPES {
        warn!(
            count = recipes.len(),
            max = MAX_RECIPES,
            "tutor primitive set exceeds cap; dropping excess recipes"
        );
        recipes.truncate(MAX_RECIPES);
    }
    recipes
}

/// Seed the built-in recipe set into a runtime root's global folder,
/// **create-if-missing only**.
///
/// Copies every `*.json` from `seed_dir` (the repo seed
/// `magician_data_v3/system/tutor_primitives/`) into
/// `<runtime_root>/tutor_primitives/`, and **never deletes**.
///
/// A file already present is left untouched **unless the built-in carries a
/// strictly higher `version`**, which is the one case that overwrites. Before
/// that existed, seeding was create-if-missing only, and the consequence was
/// sharp: a NEW built-in recipe reached a machine on the next boot while a FIX
/// to an existing one never did. Editing a shipped recipe looked like it had
/// landed — the seed was right, the tests were right — and the runtime kept
/// serving the old body indefinitely.
///
/// Version-gating keeps the data-safety rule that motivated create-only. A
/// hand-written recipe has no `version`, and an unversioned runtime file is
/// never overwritten; see [`builtin_supersedes`] for the full matrix.
///
/// Returns what changed. A missing seed dir is a no-op. Individual copy
/// failures are logged and skipped so one bad file can't abort boot.
pub fn seed_builtin_recipes(seed_dir: &Path, runtime_root: &Path) -> std::io::Result<SeedOutcome> {
    let entries = match std::fs::read_dir(seed_dir) {
        Ok(entries) => entries,
        // No seed templates present (e.g. a stripped deployment) — nothing to do.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(SeedOutcome::default()),
        Err(err) => return Err(err),
    };

    let target_dir = global_primitives_dir(runtime_root);

    let mut source_paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    source_paths.sort();

    // Nothing to seed → don't even create the target dir.
    if source_paths.is_empty() {
        return Ok(SeedOutcome::default());
    }
    std::fs::create_dir_all(&target_dir)?;

    let mut created = 0usize;
    let mut upgraded = 0usize;
    for source in source_paths {
        let Some(file_name) = source.file_name() else {
            continue;
        };
        let target = target_dir.join(file_name);
        if target.exists() && !builtin_supersedes(&source, &target) {
            continue;
        }
        let existed = target.exists();
        // `fs::copy` truncates the destination and writes into it, so an
        // interrupted upgrade leaves a runtime recipe that no longer parses.
        // That one is unrecoverable without an operator: `load_dir` skips it
        // with a `warn!` so the primitive silently leaves the palette, and
        // `builtin_supersedes` deliberately refuses to overwrite a runtime file
        // it cannot parse — so the next boot will not repair it either. Read
        // the built-in, then publish it atomically.
        match std::fs::read(&source).and_then(|bytes| write_bytes_durably_sync(&target, &bytes)) {
            Ok(()) => {
                if existed {
                    upgraded += 1;
                } else {
                    created += 1;
                }
            },
            Err(err) => warn!(
                source = %source.display(),
                target = %target.display(),
                error = %err,
                "failed to seed tutor primitive recipe; skipping"
            ),
        }
    }
    Ok(SeedOutcome { created, upgraded })
}

/// Whether the built-in at `source` should replace the runtime file at `target`.
///
/// **Only a strictly higher `version` earns an overwrite.** Everything else —
/// equal versions, a lower one, either side missing a version, either side
/// unreadable or unparseable — leaves the runtime file alone. The asymmetry is
/// the point: a built-in fix has to be able to reach a machine that already has
/// the old copy, but a user's own recipe (which has no reason to carry a
/// version, let alone a higher one) must survive every boot.
///
/// A runtime file we cannot parse is deliberately NOT overwritten. It is
/// already inert — `load_dir` skips it with a `warn!` — but it may be a
/// half-finished edit, and destroying that to fix something the operator can
/// fix by deleting one file is the wrong trade. The warn names the path.
fn builtin_supersedes(source: &Path, target: &Path) -> bool {
    let seed_version = match parse_recipe_file(source) {
        Ok(recipe) => recipe.version,
        Err(error) => {
            warn!(
                source = %source.display(),
                error = %error,
                "built-in tutor recipe did not parse; leaving the runtime copy alone"
            );
            return false;
        },
    };
    let runtime_version = match parse_recipe_file(target) {
        Ok(recipe) => recipe.version,
        Err(error) => {
            warn!(
                target = %target.display(),
                error = %error,
                "runtime tutor recipe did not parse; not overwriting it — delete it to restore the built-in"
            );
            return false;
        },
    };
    match (seed_version, runtime_version) {
        (Some(seed), Some(runtime)) => seed > runtime,
        // An unversioned built-in cannot claim to be newer, and an unversioned
        // runtime file is exactly the shape of a hand-written recipe.
        _ => false,
    }
}

/// Read every `*.json` recipe in one directory, parsing + validating each and
/// skipping (with a `warn!`) anything invalid. A missing directory yields an
/// empty list (not an error). Files are returned in sorted filename order so a
/// caller can rely on deterministic within-directory precedence.
fn load_dir(dir: &Path) -> Vec<TutorPrimitiveRecipe> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            // A missing folder is the common case (no custom recipes) — silent.
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    dir = %dir.display(),
                    error = %err,
                    "failed to read tutor primitives directory"
                );
            }
            return Vec::new();
        },
    };

    // Collect + sort by filename for deterministic precedence.
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();

    let mut recipes = Vec::new();
    for path in paths {
        match parse_recipe_file(&path) {
            Ok(recipe) => recipes.push(recipe),
            Err(reason) => warn!(
                path = %path.display(),
                reason = %reason,
                "skipping invalid tutor primitive recipe"
            ),
        }
    }
    recipes
}

/// A generous per-file byte cap — any real recipe is a few KB. Enforced BEFORE
/// parsing so a pathological large file can't stall a blocking-pool thread on the
/// full read + allocation before validation rejects it.
pub const MAX_RECIPE_FILE_BYTES: u64 = 256 * 1024;

/// Parse + validate a single recipe file. Returns a human-readable reason on any
/// failure so the caller can `warn!` and skip.
fn parse_recipe_file(path: &Path) -> Result<TutorPrimitiveRecipe, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|err| format!("open failed: {err}"))?;
    if let Ok(meta) = file.metadata() {
        if meta.len() > MAX_RECIPE_FILE_BYTES {
            return Err(format!(
                "file too large: {} bytes (max {})",
                meta.len(),
                MAX_RECIPE_FILE_BYTES
            ));
        }
    }
    // Bound the read regardless (defends against a size that changes after stat).
    let mut bytes = Vec::new();
    file.take(MAX_RECIPE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("read failed: {err}"))?;
    if bytes.len() as u64 > MAX_RECIPE_FILE_BYTES {
        return Err(format!(
            "file too large (max {} bytes)",
            MAX_RECIPE_FILE_BYTES
        ));
    }
    let recipe: TutorPrimitiveRecipe =
        serde_json::from_slice(&bytes).map_err(|err| format!("parse failed: {err}"))?;
    recipe.validate()?;
    Ok(recipe)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_recipe(dir: &Path, file_name: &str, body: &Value) {
        std::fs::create_dir_all(dir).expect("create recipe dir");
        std::fs::write(
            dir.join(file_name),
            serde_json::to_vec_pretty(body).expect("serialize recipe"),
        )
        .expect("write recipe file");
    }

    #[test]
    fn parses_a_valid_recipe() {
        let value = json!({
            "type": "angle_marker",
            "aliases": ["arc", "perpendicular_marker"],
            "version": 1,
            "defaults": { "size": 36, "start_angle": 0, "end_angle": 90 },
            "draw": [
                { "op": "arc", "cx": "cx", "cy": "cy", "r": "r|size",
                  "from": "start_angle", "to": "end_angle" }
            ]
        });
        let recipe: TutorPrimitiveRecipe =
            serde_json::from_value(value).expect("recipe should deserialize");
        assert_eq!(recipe.type_, "angle_marker");
        assert_eq!(recipe.aliases, vec!["arc", "perpendicular_marker"]);
        assert_eq!(recipe.version, Some(1));
        assert!(recipe.validate().is_ok());
        assert_eq!(
            recipe.all_types().collect::<Vec<_>>(),
            vec!["angle_marker", "arc", "perpendicular_marker"]
        );
    }

    #[test]
    fn rejects_unknown_op() {
        let recipe = TutorPrimitiveRecipe {
            type_: "weird".to_string(),
            aliases: Vec::new(),
            version: None,
            defaults: Map::new(),
            draw: vec![json!({ "op": "teleport", "at": [0, 0] })],
        };
        let err = recipe.validate().expect_err("unknown op must be rejected");
        assert!(err.contains("unknown"), "unexpected error: {err}");
        assert!(err.contains("teleport"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_empty_type_and_missing_op() {
        let empty_type = TutorPrimitiveRecipe {
            type_: "   ".to_string(),
            aliases: Vec::new(),
            version: None,
            defaults: Map::new(),
            draw: vec![json!({ "op": "line", "from": [0, 0], "to": [1, 1] })],
        };
        assert!(empty_type.validate().is_err());

        let missing_op = TutorPrimitiveRecipe {
            type_: "x".to_string(),
            aliases: Vec::new(),
            version: None,
            defaults: Map::new(),
            draw: vec![json!({ "from": [0, 0], "to": [1, 1] })],
        };
        assert!(missing_op.validate().is_err());
    }

    #[test]
    fn rejects_oversized_point_list() {
        let points: Vec<Value> = (0..(MAX_POINTS_PER_OP + 1))
            .map(|i| json!([i, i]))
            .collect();
        let recipe = TutorPrimitiveRecipe {
            type_: "huge".to_string(),
            aliases: Vec::new(),
            version: None,
            defaults: Map::new(),
            draw: vec![json!({ "op": "polyline", "points": points })],
        };
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn load_merges_global_and_scope_and_skips_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path();

        // Global built-ins: `rect` and `circle`.
        let global_dir = global_primitives_dir(base);
        write_recipe(
            &global_dir,
            "rect.json",
            &json!({
                "type": "rect",
                "draw": [{ "op": "rect", "x": "x", "y": "y", "w": "w", "h": "h" }]
            }),
        );
        write_recipe(
            &global_dir,
            "circle.json",
            &json!({
                "type": "circle",
                "draw": [{ "op": "circle", "cx": "cx", "cy": "cy", "r": "r" }]
            }),
        );

        // Scope: overrides `rect` (different op body) and adds `star`.
        let scope_dir = scope_primitives_dir(base, "anonymous", "default");
        write_recipe(
            &scope_dir,
            "rect.json",
            &json!({
                "type": "rect",
                "version": 9,
                "draw": [{ "op": "polygon", "points": [["x", "y"], ["x", "y"]] }]
            }),
        );
        write_recipe(
            &scope_dir,
            "star.json",
            &json!({
                "type": "star",
                "draw": [{ "op": "polyline", "points": [["x", "y"]] }]
            }),
        );
        // An invalid file that must be skipped (unknown op) — never crashes load.
        write_recipe(
            &scope_dir,
            "broken.json",
            &json!({
                "type": "broken",
                "draw": [{ "op": "explode" }]
            }),
        );
        // A non-JSON file that must also be skipped.
        std::fs::write(scope_dir.join("notes.txt"), b"not a recipe").expect("write txt");
        // Malformed JSON — skipped.
        std::fs::write(scope_dir.join("garbage.json"), b"{ not json ").expect("write garbage");

        let recipes = load(base, "anonymous", "default");
        let by_type: BTreeMap<&str, &TutorPrimitiveRecipe> =
            recipes.iter().map(|r| (r.type_.as_str(), r)).collect();

        // circle (global-only), rect (scope override), star (scope-only) — no broken.
        assert_eq!(recipes.len(), 3, "unexpected set: {by_type:?}");
        assert!(by_type.contains_key("circle"));
        assert!(by_type.contains_key("star"));
        assert!(!by_type.contains_key("broken"), "invalid recipe leaked in");

        // The scope `rect` (version 9, polygon body) won over the global one.
        let rect = by_type.get("rect").expect("rect present");
        assert_eq!(rect.version, Some(9));
        assert_eq!(
            rect.draw[0].get("op").and_then(Value::as_str),
            Some("polygon")
        );
    }

    #[test]
    fn load_on_empty_base_is_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(load(temp.path(), "anonymous", "default").is_empty());
    }

    #[test]
    fn seed_creates_missing_and_preserves_user_edits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let seed_dir = temp.path().join("seed");
        let runtime_root = temp.path().join("runtime");
        std::fs::create_dir_all(&seed_dir).expect("seed dir");

        std::fs::write(
            seed_dir.join("rect.json"),
            br#"{"type":"rect","draw":[{"op":"rect","x":"x","y":"y","w":"w","h":"h"}]}"#,
        )
        .expect("seed rect");
        std::fs::write(
            seed_dir.join("circle.json"),
            br#"{"type":"circle","draw":[{"op":"circle","cx":"cx","cy":"cy","r":"r"}]}"#,
        )
        .expect("seed circle");

        // Pre-existing user edit for `rect` that MUST be preserved.
        let target_dir = global_primitives_dir(&runtime_root);
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let user_rect =
            br#"{"type":"rect","version":42,"draw":[{"op":"polygon","points":[["x","y"]]}]}"#;
        std::fs::write(target_dir.join("rect.json"), user_rect).expect("user rect");

        // First seed: creates only the missing `circle.json`, leaves `rect.json`.
        // The user's `rect` carries version 42 and the built-in carries none, so
        // it is preserved under the version gate as well as the presence check.
        let outcome = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed");
        assert_eq!(
            outcome.created, 1,
            "only the missing file should be created"
        );
        assert_eq!(outcome.upgraded, 0);
        assert_eq!(
            std::fs::read(target_dir.join("rect.json")).expect("rect"),
            user_rect,
            "user edit must be preserved (never overwritten)"
        );
        assert!(target_dir.join("circle.json").exists());

        // Second seed is idempotent: nothing new created, user edit still intact.
        let again = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed again");
        assert_eq!(
            again,
            SeedOutcome::default(),
            "a second pass writes nothing"
        );
        assert_eq!(
            std::fs::read(target_dir.join("rect.json")).expect("rect"),
            user_rect
        );
    }

    /// Seeding a built-in fix onto a machine that already has the old copy.
    /// Before the version gate this could not happen at all: a NEW recipe
    /// reached the runtime on the next boot while a FIX to an existing one
    /// never did, so an edited built-in looked shipped and served its old body
    /// indefinitely.
    #[test]
    fn a_higher_builtin_version_replaces_the_runtime_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let seed_dir = temp.path().join("seed");
        let runtime_root = temp.path().join("runtime");
        std::fs::create_dir_all(&seed_dir).expect("seed dir");
        let target_dir = global_primitives_dir(&runtime_root);
        std::fs::create_dir_all(&target_dir).expect("target dir");

        let old = br#"{"type":"callout","version":1,"draw":[{"op":"rect","x":"x","y":"y","w":"w","h":"h"}]}"#;
        let new = br#"{"type":"callout","version":2,"draw":[{"op":"rect","x":"x","y":"y","w":"text_w","h":"text_h"}]}"#;
        std::fs::write(target_dir.join("callout.json"), old).expect("runtime callout");
        std::fs::write(seed_dir.join("callout.json"), new).expect("seed callout");

        let outcome = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed");
        assert_eq!(outcome.upgraded, 1, "the newer built-in must land");
        assert_eq!(
            outcome.created, 0,
            "it replaced a file rather than creating one"
        );
        assert_eq!(
            std::fs::read(target_dir.join("callout.json")).expect("callout"),
            new
        );

        let again = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed again");
        assert_eq!(
            again,
            SeedOutcome::default(),
            "equal versions upgrade nothing"
        );
    }

    #[test]
    fn seeding_publishes_atomically_and_leaves_no_staging_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let seed_dir = temp.path().join("seed");
        let runtime_root = temp.path().join("runtime");
        std::fs::create_dir_all(&seed_dir).expect("seed dir");
        let target_dir = global_primitives_dir(&runtime_root);
        std::fs::create_dir_all(&target_dir).expect("target dir");

        // One create and one version-gated upgrade in the same pass, so both
        // publish paths are covered.
        let old = br#"{"type":"callout","version":1,"draw":[{"op":"rect","x":"x","y":"y","w":"w","h":"h"}]}"#;
        let new = br#"{"type":"callout","version":2,"draw":[{"op":"rect","x":"x","y":"y","w":"text_w","h":"text_h"}]}"#;
        std::fs::write(target_dir.join("callout.json"), old).expect("runtime callout");
        std::fs::write(seed_dir.join("callout.json"), new).expect("seed callout");
        std::fs::write(
            seed_dir.join("circle.json"),
            br#"{"type":"circle","draw":[{"op":"circle","cx":"cx","cy":"cy","r":"r"}]}"#,
        )
        .expect("seed circle");

        let outcome = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed");
        assert_eq!(
            outcome,
            SeedOutcome {
                created: 1,
                upgraded: 1
            }
        );

        let mut names: Vec<String> = std::fs::read_dir(&target_dir)
            .expect("target listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["callout.json".to_string(), "circle.json".to_string()],
            "durable seeding must not leave staging files beside the recipes"
        );

        // Both published files parse and are loadable as recipes.
        assert_eq!(load_dir(&target_dir).len(), 2);
    }

    /// The data-safety half. Each of these must leave the runtime file alone,
    /// and the unversioned case is the important one — a hand-written recipe
    /// has no reason to carry a version, so "no version" must never read as
    /// "old".
    #[test]
    fn a_builtin_never_overwrites_a_file_it_cannot_prove_is_older() {
        for (name, runtime_body) in [
            (
                "older_builtin",
                &br#"{"type":"r","version":9,"draw":[{"op":"rect","x":"x"}]}"#[..],
            ),
            (
                "unversioned_runtime",
                &br#"{"type":"r","draw":[{"op":"rect","x":"x"}]}"#[..],
            ),
            ("unparseable_runtime", &b"{ not json at all"[..]),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let seed_dir = temp.path().join("seed");
            let runtime_root = temp.path().join("runtime");
            std::fs::create_dir_all(&seed_dir).expect("seed dir");
            let target_dir = global_primitives_dir(&runtime_root);
            std::fs::create_dir_all(&target_dir).expect("target dir");

            std::fs::write(
                seed_dir.join("r.json"),
                br#"{"type":"r","version":2,"draw":[{"op":"circle","cx":"cx","cy":"cy","r":"r"}]}"#,
            )
            .expect("seed");
            std::fs::write(target_dir.join("r.json"), runtime_body).expect("runtime");

            let outcome = seed_builtin_recipes(&seed_dir, &runtime_root).expect("seed");
            assert_eq!(
                outcome,
                SeedOutcome::default(),
                "{name} must not be written"
            );
            assert_eq!(
                std::fs::read(target_dir.join("r.json")).expect("runtime file"),
                runtime_body,
                "{name} must survive byte for byte"
            );
        }
    }

    #[test]
    fn seed_missing_source_is_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outcome = seed_builtin_recipes(&temp.path().join("nope"), &temp.path().join("runtime"))
            .expect("noop seed");
        assert_eq!(outcome, SeedOutcome::default());
    }
}
