//! `CliTemplateDispatcher` — generic YAML-driven dispatcher for inner-loop
//! pack tools.
//!
//! Construction:
//! - `base`: argv prefix (resolved at construction from
//!   `implementation.command` if set, else `[pack.name]`).
//!
//! Per-primitive call: shells out to `<base> <primitive> <args...>` for the
//! preferred browser-style `args: string[]` contract, captures stdout/stderr,
//! and surfaces JSON when the CLI emits valid JSON. Older flag-mapping schemas
//! remain supported as a compatibility path.
//!
//! Stateless — every call is one short-lived subprocess. Tools that need
//! session state (e.g. `browser`) get a hand-coded Rust dispatcher instead.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant as TokioInstant;
use tracing::{debug, warn};

use super::super::runner::{PrimitiveDispatcher, PrimitiveToolResult};
use super::args::{
    append_flag, arguments_object, build_argv, optional_string_array, split_shell_words,
    stringify_scalar,
};
use crate::magician_v2::artifact_v2::CapabilityScopePaths;
use crate::magician_v2::execution::capability::{
    coerce_bool_flag_default, coerce_bool_flag_value, CapabilityPackDefinition, CommandArgMapping,
    ImplementationType, NativeActionSchemaDef, ParameterDef,
};

/// Per-command wall-clock cap. CLI-template tools are expected to be fast;
/// longer ops should use a different impl type or get their own Rust
/// dispatcher.
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;
// CLI-template tools return textual/JSON payloads. These deliberately generous
// caps preserve existing large artifact responses while preventing a faulty
// subprocess or remote provider from growing process memory without bound.
const MAX_CLI_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLI_STDERR_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROGRESS_LINE_BYTES: usize = 16 * 1024;

struct BoundedPipeRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn read_bounded_pipe<R>(
    mut pipe: R,
    max_bytes: usize,
    progress_publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
) -> BoundedPipeRead
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    let mut exceeded = false;
    let mut progress_line = Vec::new();
    let mut progress_line_truncated = false;

    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let already_exceeded = exceeded;
        let remaining = max_bytes.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        exceeded |= retained < read;

        if let Some(publisher) = progress_publisher.as_ref().filter(|_| !already_exceeded) {
            for byte in &chunk[..retained] {
                if *byte == b'\n' {
                    publish_progress_line(publisher, &progress_line, progress_line_truncated);
                    progress_line.clear();
                    progress_line_truncated = false;
                } else if progress_line.len() < MAX_PROGRESS_LINE_BYTES {
                    progress_line.push(*byte);
                } else {
                    progress_line_truncated = true;
                }
            }
            progress_line_truncated |= retained < read;
        }
    }

    if let Some(publisher) = progress_publisher.as_ref() {
        publish_progress_line(publisher, &progress_line, progress_line_truncated);
    }
    BoundedPipeRead { bytes, exceeded }
}

fn publish_progress_line(
    publisher: &Arc<dyn Fn(String) + Send + Sync + 'static>,
    line: &[u8],
    truncated: bool,
) {
    let mut text = String::from_utf8_lossy(line).trim().to_string();
    if truncated {
        text.push_str(" [progress line truncated]");
    }
    if !text.is_empty() {
        publisher(text);
    }
}

pub struct CliTemplateDispatcher {
    /// Capability name, used in diagnostics.
    pack_name: String,
    /// Argv prefix prepended to every primitive call.
    base: Vec<String>,
    /// Explicit cwd override (legacy `with_cwd(PathBuf)` builder).
    /// When set, takes precedence over `cwd_template`.
    cwd: Option<PathBuf>,
    /// YAML-declared cwd template (e.g. `'{working_dir}'`). Interpolated
    /// against the call's tool arguments at spawn time so per-call params
    /// like `working_dir` can drive the subprocess cwd.
    cwd_template: Option<String>,
    /// Scoped path interpolation context.
    scope_paths: Option<CapabilityScopePaths>,
    /// Pack-level env applied to every primitive call.
    env: HashMap<String, String>,
    /// Pack-level argv suffix applied to every primitive call.
    suffix_args: Vec<String>,
    /// Action dispatch metadata keyed by primitive name.
    action_schemas: HashMap<String, NativeActionSchemaDef>,
    /// Pack-level parameters, used for default values and interpolation.
    parameter_defs: Vec<ParameterDef>,
    /// Default command timeout.
    timeout_secs: u64,
    /// Optional sink for subprocess stderr lines emitted while the
    /// primitive is in flight. The chat path sets this to a closure that
    /// republishes each line as an `ActionProgress` `ProgressMessage` so
    /// the chat UI's existing inline_pack card updates with the helper's
    /// real progress (e.g. `[NANOBANANA2] generate_content started`).
    /// `None` for autonomous-loop dispatch and tests.
    progress_publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
}

impl std::fmt::Debug for CliTemplateDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliTemplateDispatcher")
            .field("pack_name", &self.pack_name)
            .field("base", &self.base)
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field(
                "action_schemas",
                &self.action_schemas.keys().collect::<Vec<_>>(),
            )
            .field("timeout_secs", &self.timeout_secs)
            .field("progress_publisher", &self.progress_publisher.is_some())
            .finish()
    }
}

impl CliTemplateDispatcher {
    /// Construct a dispatcher with an explicit base argv. Non-empty.
    pub fn new(base: Vec<String>) -> Result<Self> {
        if base.is_empty() {
            bail!("CliTemplateDispatcher requires a non-empty base argv");
        }
        Ok(Self {
            pack_name: "cli_template".to_string(),
            base,
            cwd: None,
            cwd_template: None,
            scope_paths: None,
            env: HashMap::new(),
            suffix_args: Vec::new(),
            action_schemas: HashMap::new(),
            parameter_defs: Vec::new(),
            timeout_secs: DEFAULT_COMMAND_TIMEOUT_SECS,
            progress_publisher: None,
        })
    }

    /// Attach a progress sink that receives each line of subprocess stderr
    /// as the primitive runs. Returns `self` for builder chaining.
    pub fn with_progress_publisher(
        mut self,
        publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
    ) -> Self {
        self.progress_publisher = publisher;
        self
    }

    /// Resolve the dispatcher base from a pack's name + optional
    /// `implementation.command` override.
    pub fn from_pack(pack_name: &str, command_override: Option<&[String]>) -> Result<Self> {
        let base = match command_override {
            Some(argv) if !argv.is_empty() => argv.to_vec(),
            _ => vec![pack_name.to_string()],
        };
        Self::new(base)
    }

    /// Build a scoped dispatcher directly from an inner-loop pack.
    pub fn from_definition(
        pack: &CapabilityPackDefinition,
        scope_paths: Option<CapabilityScopePaths>,
    ) -> Result<Self> {
        let ImplementationType::Primitive {
            command,
            cwd,
            env,
            suffix_args,
            timeout_secs,
            ..
        } = &pack.implementation
        else {
            bail!("pack `{}` is not an inner-loop pack", pack.name);
        };

        let base = match command {
            Some(argv) if !argv.is_empty() => argv.to_vec(),
            _ => vec![pack.name.clone()],
        };
        let mut dispatcher = Self::new(base)?;
        dispatcher.pack_name = pack.name.clone();
        dispatcher.scope_paths = scope_paths;
        dispatcher.env = env.clone();
        dispatcher.suffix_args = suffix_args.clone();
        dispatcher.action_schemas = pack.native_action_schemas.clone();
        dispatcher.parameter_defs = pack.parameters.clone();
        dispatcher.timeout_secs = timeout_secs.unwrap_or_else(|| {
            pack.execution
                .as_ref()
                .and_then(|meta| meta.default_timeout_secs)
                .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
        });
        if let Some(cwd) = cwd
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            // Store the template; interpolation happens at spawn time so
            // tool-arg placeholders like `{working_dir}` resolve against
            // the call's actual arguments. Pure context-var templates
            // (e.g. `{scope_capabilities_root}`) work the same way.
            dispatcher.cwd_template = Some(cwd.to_string());
        }
        Ok(dispatcher)
    }

    /// Set the working directory used when spawning subprocesses.
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Read-only accessor for the resolved base argv (mostly for tests).
    pub fn base(&self) -> &[String] {
        &self.base
    }

    async fn spawn_with_timeout(
        &self,
        argv: &[String],
        timeout_secs: u64,
        env_args: &Map<String, Value>,
        stdin: Option<&str>,
    ) -> Result<PrimitiveToolResult> {
        let start = Instant::now();
        // The skill's runtime tree, when this dispatcher is scoped and the
        // pack is installed: it decides both MAGICIAN_SKILL_DIR and the
        // child's PATH below. Resolved once, ahead of the command, so the
        // program can be looked up against the PATH the child will see and
        // the spawn stays on `posix_spawn` (see `runtime_core::process`).
        let scoped_skill = self.scope_paths.as_ref().and_then(|paths| {
            let skill_dir = resolve_skill_dir(paths, &self.pack_name)?;
            // Scripts + `bin/` must resolve for THIS environment: rewritten
            // to the real skillshub dir in a container, identity natively.
            // Per-skill secrets (`config/.env`, read below) stay on the REAL
            // scope dir — they are NOT materialized into skillshub.
            let runtime_dir =
                crate::magician_v2::skills::path_rewrite::rewrite_skill_dir(&skill_dir);
            Some((skill_dir, runtime_dir))
        });
        let child_path: Option<String> = scoped_skill
            .as_ref()
            .zip(self.scope_paths.as_ref())
            .and_then(|((_, runtime_dir), paths)| {
                // Prepend, in priority order:
                //   1. this skill's `bin/` (vendored skill-private tools
                //      populated by `make -C skillshub setup-pdftotext|
                //      setup-ocr|setup-marimo` etc.)
                //   2. skillshub `.venv/bin/` (Python interpreter with
                //      Pillow / fpdf2 / google-genai for any skill that
                //      shells `python3 …`)
                //   3. skillshub `node_modules/.bin/` (npm-vendored CLIs
                //      hoisted by the root npm workspace — gws, telegraf,
                //      tgcli, wu, …).
                //   4. skillshub `.node/bin/` (project-pinned Node 24 LTS:
                //      `node`, `npm`, `npx`, plus corepack shims for
                //      `pnpm` / `yarn`).
                // Each layer is included only when the directory actually
                // exists, so a fresh checkout that hasn't run the matching
                // `make setup-*` target still boots without empty PATH
                // segments.
                let skill_bin = runtime_dir.join("bin");
                let parent_path = std::env::var("PATH").unwrap_or_default();
                paths
                    .subprocess_bin_path(Some(skill_bin.as_path()), &parent_path)
                    .or_else(|| (!parent_path.is_empty()).then_some(parent_path))
            });
        // The later env layers — the skill's `config/.env`, then this
        // template's own `env` — win over that augmentation; when one carries
        // `PATH`, it is the PATH the child receives, so the program is
        // resolved against it. Loaded here, applied below in the same order
        // as before.
        let skill_env = match &scoped_skill {
            Some((skill_dir, _)) => Some(load_skill_command_env(skill_dir)?),
            None => None,
        };
        let template_path = match self.env.get("PATH") {
            Some(value) => Some(self.interpolate(value, env_args)?),
            None => None,
        };
        let final_path = template_path
            .or_else(|| skill_env.as_ref().and_then(|env| env.get("PATH").cloned()))
            .or_else(|| child_path.clone());
        // Wrapped in the OS-sandbox gate when enabled (live repo read-only);
        // identical to `Command::new(argv[0]).args(argv[1..])` when the gate is
        // off (default). See `coding_engine::os_sandbox_command`.
        let sandbox_program =
            runtime_core::process::resolve_program_str(argv[0].as_str(), final_path.as_deref())
                .into_os_string();
        let sandbox_args: Vec<std::ffi::OsString> = argv[1..]
            .iter()
            .map(|a| std::ffi::OsString::from(a.as_str()))
            .collect();
        let mut cmd = crate::magician_v2::execution::coding_engine::os_sandbox_command(
            sandbox_program.as_os_str(),
            &sandbox_args,
        );
        // Resolve cwd: explicit `with_cwd(PathBuf)` override wins; otherwise
        // interpolate the YAML template against this call's args so e.g.
        // `cwd: '{working_dir}'` resolves per-call.
        let resolved_cwd: Option<PathBuf> = if let Some(cwd) = &self.cwd {
            Some(cwd.clone())
        } else if let Some(tpl) = &self.cwd_template {
            let interpolated = self.interpolate(tpl, env_args)?;
            let trimmed = interpolated.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        } else {
            None
        };
        // Isolation: with no explicit cwd, run in the scope sandbox
        // (`workdirs/home`) instead of inheriting the process CWD (= the live
        // magician repo). On its own this is not enough — the sandbox is nested
        // under the repo, so git would still walk UP to the live `.git`; the
        // GIT_CEILING_DIRECTORIES fence below stops that. Together a stray
        // `git checkout -b` / `git add` with no cwd can't touch the user's repo.
        let resolved_cwd = match resolved_cwd {
            Some(cwd) => Some(cwd),
            None => self.scope_paths.as_ref().map(|paths| {
                let _ = std::fs::create_dir_all(&paths.home_root);
                paths.home_root.clone()
            }),
        };
        let resolved_cwd =
            crate::magician_v2::subprocess_owners::active_skill_working_lease().or(resolved_cwd);
        if let Some(cwd) = &resolved_cwd {
            cmd.current_dir(cwd);
        }
        if let Some(paths) = &self.scope_paths {
            cmd.env("HOME", &paths.home_root);
            // Fence git's upward `.git` search at the scope workdirs boundary so
            // a subprocess running inside the (repo-nested) sandbox cannot
            // discover or mutate the live magician repo's `.git`.
            cmd.env("GIT_CEILING_DIRECTORIES", &paths.workdirs_root);
            // Browser-cache wiring (container only). Setting HOME to the scoped
            // `workdirs/home` (above) means agent-browser's headed/headless mode
            // looks for its baked Chromiums under THIS empty scoped HOME
            // (~/.cloakbrowser for obscura, ~/.agent-browser for Chrome-for-
            // Testing) and RE-DOWNLOADS them at runtime — defeating the image
            // bake. The Dockerfile (Phase 2.3) bakes both into a stable, world-
            // readable /opt/browser-cache. Point the browser skill at that bake.
            //
            // Identity-native discipline: gated on /opt/browser-cache existing,
            // so on a native host (no bake) this is a complete no-op and skill
            // subprocesses are byte-identical. Same discipline as the
            // skillshub-root path rewrite above. Default `cdp`→host-Chrome mode
            // is unaffected either way (it never reads these caches).
            //
            // - obscura (the config DEFAULT): the `cloakbrowser` SDK's
            //   get_cache_dir() honors CLOAKBROWSER_CACHE_DIR (default
            //   ~/.cloakbrowser) — a clean env override, so point it straight at
            //   the bake.
            // - Chrome-for-Testing (the FALLBACK): agent-browser's
            //   get_browsers_dir() is hardcoded to $HOME/.agent-browser/browsers
            //   with NO env override, so we instead symlink the scoped HOME's
            //   `.agent-browser` at the bake (idempotent, link-if-absent).
            // Both baked trees are read-only to the non-root runtime user; that
            // is sufficient — find/use of an already-present browser needs no
            // write (cloak only READS its version marker; agent-browser only
            // READS its browsers dir before launch).
            let browser_cache = Path::new("/opt/browser-cache");
            if browser_cache.is_dir() {
                // Only point cloak at the baked cache when it actually exists —
                // symmetric to the `baked_ab.is_dir()` guard below — so a partial
                // bake (browser-cache dir present but `.cloakbrowser` not yet
                // populated) doesn't aim CLOAKBROWSER_CACHE_DIR at a missing dir.
                let baked_cloak = browser_cache.join(".cloakbrowser");
                if baked_cloak.is_dir() {
                    cmd.env("CLOAKBROWSER_CACHE_DIR", &baked_cloak);
                }
                // agent-browser has no cache-dir env, so link the scoped HOME's
                // `.agent-browser` at the bake. Link-if-absent: skip when the
                // scoped HOME already has a real `.agent-browser` (e.g. a prior
                // runtime download) or the link already exists, and never
                // clobber an existing entry.
                let baked_ab = browser_cache.join(".agent-browser");
                let scoped_ab = paths.home_root.join(".agent-browser");
                if baked_ab.is_dir() && !scoped_ab.exists() {
                    let _ = std::fs::create_dir_all(&paths.home_root);
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(&baked_ab, &scoped_ab);
                }
            }
            // MAGICIAN_SKILL_DIR points at this skill's runtime tree
            // location. Workspace-layer wins over `paths`-declared extras:
            //   1) <scope>/skills/<pack_name>/  (scope-layer install:
            //      skills with API keys / OAuth state, deployed by
            //      `make -C skillshub install-scope`)
            //   2) <extra_path>/skills/<pack_name>/  (each entry from
            //      `tool-runtime-config.yaml :: registry.paths`, in
            //      declared order)
            // Run.sh wrappers read $MAGICIAN_SKILL_DIR to resolve
            // sibling scripts.
            if let Some((_, runtime_dir)) = &scoped_skill {
                if let Some(p) = child_path.as_deref() {
                    cmd.env("PATH", p);
                }
                cmd.env("MAGICIAN_SKILL_DIR", runtime_dir);

                // This skill's own per-skill `.env` colocated with its
                // source: `<skill_dir>/config/.env` (loaded above). Each
                // skill subprocess sees only the keys it declared in
                // `config/.env.example`, populated from your repo-root
                // secrets source by `make -C skillshub setup-env`.
                for (key, value) in skill_env.into_iter().flatten() {
                    cmd.env(key, value);
                }
            }
        }
        for (key, value) in &self.env {
            cmd.env(key, self.interpolate(value, env_args)?);
        }
        for name in crate::magician_v2::subprocess_owners::forbidden_child_env_names() {
            cmd.env_remove(name);
        }
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        // Spawn the child explicitly so we can stream retained stderr lines
        // through `progress_publisher` while it runs. Stdout and stderr are
        // drained concurrently with explicit retention limits.
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {:?}", argv))?;

        if let Some(stdin) = stdin {
            let mut child_stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("CLI stdin pipe unavailable for {:?}", argv))?;
            child_stdin
                .write_all(stdin.as_bytes())
                .await
                .with_context(|| format!("failed to write stdin for {:?}", argv))?;
            child_stdin
                .shutdown()
                .await
                .with_context(|| format!("failed to close stdin for {:?}", argv))?;
        }

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();

        let stderr_buf_handle: tokio::task::JoinHandle<BoundedPipeRead> =
            if let Some(pipe) = stderr_pipe.take() {
                let publisher = self.progress_publisher.clone();
                tokio::spawn(read_bounded_pipe(pipe, MAX_CLI_STDERR_BYTES, publisher))
            } else {
                tokio::spawn(async {
                    BoundedPipeRead {
                        bytes: Vec::new(),
                        exceeded: false,
                    }
                })
            };

        let stdout_buf_handle: tokio::task::JoinHandle<BoundedPipeRead> =
            if let Some(pipe) = stdout_pipe.take() {
                tokio::spawn(read_bounded_pipe(pipe, MAX_CLI_STDOUT_BYTES, None))
            } else {
                tokio::spawn(async {
                    BoundedPipeRead {
                        bytes: Vec::new(),
                        exceeded: false,
                    }
                })
            };

        let deadline = TokioInstant::now() + Duration::from_secs(timeout_secs);
        let status = match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                return Err(anyhow::Error::from(error)
                    .context(format!("failed waiting on subprocess {:?}", argv)));
            },
            Err(_) => {
                let _ = child.kill().await;
                return Err(anyhow!(
                    "CLI command timed out after {}s: {:?}",
                    timeout_secs,
                    argv
                ));
            },
        };

        let stdout_read = stdout_buf_handle.await.unwrap_or(BoundedPipeRead {
            bytes: Vec::new(),
            exceeded: false,
        });
        let stderr_read = stderr_buf_handle.await.unwrap_or(BoundedPipeRead {
            bytes: Vec::new(),
            exceeded: false,
        });
        if stdout_read.exceeded || stderr_read.exceeded {
            return Err(anyhow!(
                "CLI command output exceeded bounded capture limits (stdout={} bytes, stderr={} bytes): {:?}",
                MAX_CLI_STDOUT_BYTES,
                MAX_CLI_STDERR_BYTES,
                argv
            ));
        }

        let stdout = String::from_utf8_lossy(&stdout_read.bytes).to_string();
        let stderr = String::from_utf8_lossy(&stderr_read.bytes).to_string();
        let parsed_json = serde_json::from_str(stdout.trim()).ok();
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        if !status.success() {
            warn!(
                argv = ?argv,
                stderr = %stderr.trim(),
                "CLI command failed"
            );
        }
        // Bind back to the previous variable name so the existing `output.status.success()`
        // shape below keeps working without further surgery.
        struct Output {
            status: std::process::ExitStatus,
        }
        let output = Output { status };

        debug!(
            target: "primitive_cli",
            argv = ?argv,
            elapsed_ms,
            success = output.status.success(),
            "CLI primitive dispatched"
        );

        Ok(PrimitiveToolResult {
            success: output.status.success(),
            stdout,
            stderr,
            parsed_json,
            artifacts: Vec::new(),
            elapsed_ms,
        })
    }

    fn build_action_argv(&self, tool_name: &str, arguments: &Value) -> Result<(Vec<String>, u64)> {
        let runtime_timeout_override = arguments
            .get("timeout_secs")
            .and_then(|value| match value {
                Value::Number(number) => number.as_u64(),
                Value::String(text) => text.parse::<u64>().ok(),
                _ => None,
            })
            .filter(|secs| *secs > 0);

        let Some(schema) = self.action_schemas.get(tool_name) else {
            return Ok((
                build_argv(&self.base, tool_name, arguments)?,
                runtime_timeout_override.unwrap_or(self.timeout_secs),
            ));
        };

        let args = arguments_object(arguments)?;
        let mut argv = Vec::new();
        for token in &self.base {
            argv.push(self.interpolate(token, args)?);
        }

        if !schema.argv.is_empty() {
            for token in &schema.argv {
                argv.push(self.interpolate(token, args)?);
            }
        } else if !schema.skip_tool_name {
            argv.push(tool_name.to_string());
        }

        if schema.arg_mappings.is_empty() {
            self.append_passthrough_or_default_flags(&mut argv, args)?;
        } else {
            self.append_mapped_args(&mut argv, &schema.arg_mappings, args)?;
        }

        for token in &schema.suffix_args {
            argv.push(self.interpolate(token, args)?);
        }
        for token in &self.suffix_args {
            argv.push(self.interpolate(token, args)?);
        }

        Ok((
            argv,
            runtime_timeout_override
                .or(schema.timeout_secs)
                .unwrap_or(self.timeout_secs),
        ))
    }

    fn append_passthrough_or_default_flags(
        &self,
        argv: &mut Vec<String>,
        args: &Map<String, Value>,
    ) -> Result<()> {
        if let Some(tokens) = optional_string_array(args, "args")? {
            argv.extend(tokens);
            return Ok(());
        }

        let mut keys: Vec<&String> = args.keys().collect();
        keys.sort();
        for key in keys {
            if is_reserved_control_arg(key) {
                continue;
            }
            let value = args
                .get(key)
                .ok_or_else(|| anyhow!("internal: missing key in arguments map"))?;
            append_flag(argv, key, value)?;
        }
        Ok(())
    }

    fn append_mapped_args(
        &self,
        argv: &mut Vec<String>,
        mappings: &[CommandArgMapping],
        args: &Map<String, Value>,
    ) -> Result<()> {
        for mapping in mappings {
            match mapping {
                CommandArgMapping::Positional { param } => {
                    if let Some(value) = self.param_as_string(param, args)? {
                        argv.push(value);
                    }
                },
                CommandArgMapping::Flag { flag, param } => {
                    if let Some(value) = self.param_as_string(param, args)? {
                        argv.push(self.interpolate(flag, args)?);
                        argv.push(value);
                    }
                },
                CommandArgMapping::BoolFlag { flag, param } => {
                    if param_is_truthy(param, args, &self.parameter_defs) {
                        argv.push(self.interpolate(flag, args)?);
                    }
                },
                CommandArgMapping::SplitPositional { param } => {
                    if let Some(value) = self.param_as_string(param, args)? {
                        if !value.trim().is_empty() {
                            argv.extend(split_shell_words(&value)?);
                        }
                    }
                },
                CommandArgMapping::EnvFlag { flag, env_var } => {
                    let value = std::env::var(env_var)
                        .with_context(|| format!("environment variable `{env_var}` is not set"))?;
                    argv.push(self.interpolate(flag, args)?);
                    argv.push(value);
                },
                CommandArgMapping::Passthrough { param } => {
                    if let Some(value) = args.get(param) {
                        match value {
                            Value::Null => {},
                            Value::Array(items) => {
                                for item in items {
                                    match item {
                                        Value::String(s) => argv.push(s.clone()),
                                        Value::Null => {},
                                        other => argv.push(stringify_scalar(other)?),
                                    }
                                }
                            },
                            Value::String(s) => argv.push(s.clone()),
                            other => argv.push(stringify_scalar(other)?),
                        }
                    }
                },
                CommandArgMapping::FixedArgs { args: fixed } => {
                    for token in fixed {
                        argv.push(self.interpolate(token, args)?);
                    }
                },
            }
        }
        Ok(())
    }

    fn param_as_string(&self, param: &str, args: &Map<String, Value>) -> Result<Option<String>> {
        if let Some(value) = args.get(param).or_else(|| {
            self.parameter_defs
                .iter()
                .find(|def| def.name == param || def.aliases.iter().any(|alias| alias == param))
                .and_then(|def| args.get(&def.name))
        }) {
            return match value {
                Value::Null => Ok(None),
                Value::String(value) => Ok(Some(value.clone())),
                other => stringify_scalar(other).map(Some),
            };
        }

        if let Some(def) = self
            .parameter_defs
            .iter()
            .find(|def| def.name == param || def.aliases.iter().any(|alias| alias == param))
        {
            return Ok(def.default.clone().filter(|value| !value.is_empty()));
        }

        Ok(None)
    }

    fn interpolate(&self, template: &str, args: &Map<String, Value>) -> Result<String> {
        let mut value = match &self.scope_paths {
            Some(paths) => paths.apply_vars(template),
            None => template.to_string(),
        };

        // {skill_runtime_root} resolves to the same location as
        // MAGICIAN_SKILL_DIR — workspace-layer first, then each
        // `registry.paths` extras root. The system-shared tier was
        // retired (v0.6.572); scope-installed skills (image-generation,
        // metabase, …) live under <scope>/skills/, and per-deployment
        // overlays live under each <extra>/skills/.
        if value.contains("{skill_runtime_root}") {
            if let Some(paths) = &self.scope_paths {
                if let Some(skill_runtime_root) = resolve_skill_dir(paths, &self.pack_name) {
                    // The materialized skill dir holds host-absolute symlinks;
                    // rewrite it for this environment (identity natively) so
                    // `{skill_runtime_root}/scripts/...` resolves in a container.
                    let skill_runtime_root =
                        crate::magician_v2::skills::path_rewrite::rewrite_skill_dir(
                            &skill_runtime_root,
                        );
                    value = value.replace(
                        "{skill_runtime_root}",
                        &skill_runtime_root.to_string_lossy(),
                    );
                }
            }
        }

        let mut names = HashSet::new();
        for key in args.keys() {
            names.insert(key.as_str());
        }
        for def in &self.parameter_defs {
            names.insert(def.name.as_str());
        }

        for name in names {
            let placeholder = format!("{{{name}}}");
            if !value.contains(&placeholder) {
                continue;
            }
            let replacement = self.param_as_string(name, args)?.unwrap_or_default();
            value = value.replace(&placeholder, &replacement);
        }

        Ok(value)
    }
}

#[async_trait]
impl PrimitiveDispatcher for CliTemplateDispatcher {
    async fn dispatch(&self, tool_name: &str, arguments: &Value) -> Result<PrimitiveToolResult> {
        let env_args = arguments_object(arguments)?;
        let stdin = optional_stdin(env_args)?;
        let (argv, timeout_secs) = self.build_action_argv(tool_name, arguments)?;
        let result = self
            .spawn_with_timeout(&argv, timeout_secs, env_args, stdin.as_deref())
            .await;
        if let Err(err) = &result {
            warn!(
                target: "primitive_cli",
                pack = %self.pack_name,
                tool_name,
                error = %err,
                "CLI primitive dispatch failed"
            );
        }
        result
    }
}

/// True when a `BoolFlag` mapping should emit its flag.
///
/// Looks up `param` in the call args (with alias resolution); falls
/// back to the registered `ParameterDef::default` when the param is
/// absent. Truthiness rules live in `coerce_bool_flag_value` /
/// `coerce_bool_flag_default` (see `execution::capability`).
fn param_is_truthy(param: &str, args: &Map<String, Value>, defs: &[ParameterDef]) -> bool {
    if let Some(value) = args.get(param).or_else(|| {
        defs.iter()
            .find(|def| def.name == param || def.aliases.iter().any(|alias| alias == param))
            .and_then(|def| args.get(&def.name))
    }) {
        return coerce_bool_flag_value(value);
    }
    if let Some(def) = defs
        .iter()
        .find(|def| def.name == param || def.aliases.iter().any(|alias| alias == param))
    {
        if let Some(default) = def.default.as_deref() {
            return coerce_bool_flag_default(default);
        }
    }
    false
}

fn optional_stdin(args: &Map<String, Value>) -> Result<Option<String>> {
    match args.get("stdin") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.clone()))
            }
        },
        Some(other) => bail!("`stdin` must be a string when provided, got {other:?}"),
    }
}

fn is_reserved_control_arg(key: &str) -> bool {
    matches!(key, "stdin" | "timeout_secs")
}

/// Resolve a skill's runtime tree location. Workspace-layer wins,
/// `paths`-declared extras fall through:
///   1) `<scope>/skills/<pack_name>/` if SKILL.md is present there
///      (scope-installed skills with API keys / OAuth state)
///   2) `<extra_path>/skills/<pack_name>/` for each entry in
///      `tool-runtime-config.yaml :: registry.paths`, in declared order
///
/// Returns `None` only when nothing resolved AND no extras roots are
/// configured (so there's no plausible fallback to return for "fail
/// loudly later"). When extras exist, the last extras root's literal
/// path is returned even if missing, preserving the prior
/// fail-on-spawn behaviour callers expect.
fn resolve_skill_dir(scope_paths: &CapabilityScopePaths, pack_name: &str) -> Option<PathBuf> {
    let skills_root = scope_paths.capabilities_root.join("skills");

    // Fast path: the skill's directory name == its pack name. Matches
    // dugite, awk, jq, gmail, browser, etc. — any future skill that
    // follows the convention `dir == tool_schema.name` lands here in
    // O(1).
    let direct = skills_root.join(pack_name);
    if direct.join("SKILL.md").is_file() {
        return Some(direct);
    }

    // Slow path: support custom deployments where the directory differs
    // from `tool_schema.yaml::name`. Walk every subdirectory under
    // `<scope>/skills/`, read its `tool_schema.yaml`'s `name:` field,
    // and pick the dir whose declared name matches. Result is cached per
    // skills_root so the disk walk runs at most once per process per scope.
    if let Some(dir) = lookup_pack_dir(&skills_root, pack_name) {
        return Some(dir);
    }

    // Extras fallback: each `<extra_path>/skills/` is searched in
    // declared order. Same two-tier (direct-name + lookup_pack_dir)
    // resolution as scope so an extras-only skill resolves regardless
    // of dir-naming convention.
    let extras = crate::magician_v2::config_extras::extra_skills_dirs();
    let mut last_attempted: Option<PathBuf> = None;
    for extra_skills_root in &extras {
        let extra_direct = extra_skills_root.join(pack_name);
        if extra_direct.join("SKILL.md").is_file() {
            return Some(extra_direct);
        }
        if let Some(dir) = lookup_pack_dir(extra_skills_root, pack_name) {
            return Some(dir);
        }
        last_attempted = Some(extra_direct);
    }
    // Last resort — return the literal-name extras path of the last
    // configured extras root even if it doesn't exist (preserves prior
    // behaviour for callers that handle a missing directory themselves
    // rather than treating None as a hard error). When no extras are
    // configured AND nothing resolved in scope, return None so the
    // caller surfaces a clean "not installed" error instead of
    // failing on spawn with a manufactured path.
    last_attempted
}

/// Look up a pack's source directory by walking `skills_root` and
/// reading each subdirectory's `tool_schema.yaml::name`. First call per
/// `skills_root` builds and caches a `pack_name → dir` index; subsequent
/// calls are HashMap lookups.
///
/// Index lifetime: process lifetime. Skills are loaded at startup +
/// scope materialization; runtime additions are rare and currently
/// require a `make install-scope` cycle anyway. If a skill is added
/// after the cache populates and before a process restart, that skill
/// won't resolve until the index is invalidated — restart picks it up.
/// Hot-reload is out of scope.
fn lookup_pack_dir(skills_root: &Path, pack_name: &str) -> Option<PathBuf> {
    let cache = pack_dir_index();
    // Canonicalize the key so the same logical scope reached through
    // two different path spellings (`/data/scope/skills` vs
    // `/data/./scope/skills`, or one resolved through a symlink and
    // another not) hits the same cache entry. Falls back to the
    // literal path if canonicalize fails (e.g. transient missing
    // dir during scope materialise) — better to serve a possibly
    // duplicated index than to panic.
    let key = skills_root
        .canonicalize()
        .unwrap_or_else(|_| skills_root.to_path_buf());

    // Fast path: the cache already has an index for this scope. Read-
    // only lookup means we drop the shard guard before computing the
    // result, so concurrent lookups never serialise.
    if let Some(entry) = cache.get(&key) {
        return entry.value().get(pack_name).cloned();
    }

    // Slow path: cache miss. Build the index OUTSIDE the DashMap shard
    // lock — `build_pack_dir_index` does N synchronous file reads
    // (`read_dir` + `read_to_string` per skill); holding the shard
    // write lock during that time would stall every other dispatch
    // hashed to the same shard for the full disk-walk duration. After
    // the build, insert under `entry()` and let one writer's value
    // win the race; subsequent calls hit the fast path.
    let built = build_pack_dir_index(skills_root);
    let resolved = built.get(pack_name).cloned();
    cache.entry(key).or_insert(built);
    resolved
}

/// Static cache for the per-skills-root pack-dir index. `DashMap` so
/// concurrent dispatch threads can hit the cache without serialising;
/// the inner `HashMap` is built once and never mutated, so reads are
/// lock-free after the initial entry insert.
fn pack_dir_index() -> &'static dashmap::DashMap<PathBuf, HashMap<String, PathBuf>> {
    use std::sync::OnceLock;
    static INDEX: OnceLock<dashmap::DashMap<PathBuf, HashMap<String, PathBuf>>> = OnceLock::new();
    INDEX.get_or_init(dashmap::DashMap::new)
}

/// Read every `<skills_root>/<dir>/tool_schema.yaml` and build a
/// `pack_name → dir` index. Skips dirs without a `tool_schema.yaml`
/// (personality skills, in-progress work). Skips files that fail to
/// parse rather than failing the whole index — one broken skill
/// shouldn't hide every other skill from dispatch.
fn build_pack_dir_index(skills_root: &Path) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    let entries = match std::fs::read_dir(skills_root) {
        Ok(it) => it,
        Err(_) => return index,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Some(name) = crate::magician_v2::skills::scope_loader::read_tool_schema_name(&dir) {
            index.insert(name, dir);
        }
    }
    index
}

/// Load a skill's per-skill `.env` from `<skill_dir>/config/.env`.
/// Returns an empty map if the file is absent — a skill that doesn't
/// declare any secrets needs no env file at all. Logs a warn-level
/// message when `.env.example` is present but `.env` is missing,
/// since that's almost always an operator who forgot `make setup-env`.
fn load_skill_command_env(skill_dir: &Path) -> Result<HashMap<String, String>> {
    let env_path = skill_dir.join("config").join(".env");
    let mut env = HashMap::new();
    if !env_path.exists() {
        let example_path = skill_dir.join("config").join(".env.example");
        if example_path.exists() {
            tracing::warn!(
                "skill {} declares config/.env.example but config/.env is \
                 missing — secrets won't be injected. \
                 Run `make -C skillshub setup-env`.",
                skill_dir.display()
            );
        }
        return Ok(env);
    }
    let iter = dotenvy::from_path_iter(&env_path)
        .with_context(|| format!("failed to load skill env {}", env_path.display()))?;
    for entry in iter {
        let (key, value) =
            entry.with_context(|| format!("failed to parse skill env {}", env_path.display()))?;
        env.insert(key, value);
    }
    Ok(env)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[tokio::test]
    async fn bounded_pipe_capture_retains_prefix_and_bounds_progress_lines() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let published = Arc::new(Mutex::new(Vec::<String>::new()));
        let published_sink = published.clone();
        let capture = tokio::spawn(read_bounded_pipe(
            reader,
            5,
            Some(Arc::new(move |line| {
                published_sink.lock().unwrap().push(line);
            })),
        ));
        writer.write_all(b"abc\ndefgh\n").await.unwrap();
        drop(writer);

        let captured = capture.await.unwrap();
        assert_eq!(captured.bytes, b"abc\nd");
        assert!(captured.exceeded);
        assert_eq!(
            published.lock().unwrap().as_slice(),
            ["abc", "d [progress line truncated]"]
        );
    }

    #[test]
    fn from_pack_uses_command_override_when_set() {
        let d = CliTemplateDispatcher::from_pack("gmail", Some(&["gws".into(), "gmail".into()]))
            .unwrap();
        assert_eq!(d.base(), &["gws", "gmail"]);
    }

    #[test]
    fn from_pack_falls_back_to_pack_name() {
        let d = CliTemplateDispatcher::from_pack("gmail", None).unwrap();
        assert_eq!(d.base(), &["gmail"]);
    }

    #[test]
    fn from_pack_treats_empty_override_as_absent() {
        let d = CliTemplateDispatcher::from_pack("gmail", Some(&[])).unwrap();
        assert_eq!(d.base(), &["gmail"]);
    }

    #[test]
    fn new_rejects_empty_base() {
        let err = CliTemplateDispatcher::new(Vec::new()).unwrap_err();
        assert!(format!("{err}").contains("non-empty base argv"));
    }

    #[test]
    fn action_schema_uses_declared_argv_mappings_and_timeout() {
        let mut d = CliTemplateDispatcher::new(vec!["/usr/bin/env".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                argv: vec!["gws".into(), "gmail".into()],
                arg_mappings: vec![CommandArgMapping::SplitPositional {
                    param: "command".into(),
                }],
                suffix_args: vec!["--format".into(), "json".into()],
                timeout_secs: Some(30),
                ..Default::default()
            },
        );

        let (argv, timeout_secs) = d
            .build_action_argv("run", &json!({"command": "+triage --max 5"}))
            .unwrap();

        assert_eq!(
            argv,
            vec![
                "/usr/bin/env",
                "gws",
                "gmail",
                "+triage",
                "--max",
                "5",
                "--format",
                "json"
            ]
        );
        assert_eq!(timeout_secs, 30);
    }

    #[test]
    fn action_schema_runtime_timeout_override_wins_over_schema_default() {
        let mut d = CliTemplateDispatcher::new(vec!["/usr/bin/env".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                arg_mappings: vec![CommandArgMapping::SplitPositional {
                    param: "command".into(),
                }],
                timeout_secs: Some(30),
                ..Default::default()
            },
        );

        let (_argv, timeout_secs) = d
            .build_action_argv(
                "run",
                &json!({"command": "messages send 12345@s.whatsapp.net hi", "timeout_secs": 120}),
            )
            .unwrap();

        assert_eq!(timeout_secs, 120);
    }

    #[test]
    fn action_schema_runtime_timeout_override_accepts_string_for_yaml_default_compat() {
        let mut d = CliTemplateDispatcher::new(vec!["/usr/bin/env".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                arg_mappings: vec![CommandArgMapping::SplitPositional {
                    param: "command".into(),
                }],
                timeout_secs: Some(30),
                ..Default::default()
            },
        );

        let (_argv, timeout_secs) = d
            .build_action_argv(
                "run",
                &json!({"command": "messages send 12345@s.whatsapp.net hi", "timeout_secs": "90"}),
            )
            .unwrap();

        assert_eq!(timeout_secs, 90);
    }

    #[test]
    fn action_schema_runtime_timeout_override_ignores_zero_and_invalid() {
        let mut d = CliTemplateDispatcher::new(vec!["/usr/bin/env".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                arg_mappings: vec![CommandArgMapping::SplitPositional {
                    param: "command".into(),
                }],
                timeout_secs: Some(45),
                ..Default::default()
            },
        );

        for invalid in [json!(0), json!("not-a-number"), json!(true)] {
            let (_argv, timeout_secs) = d
                .build_action_argv(
                    "run",
                    &json!({"command": "messages list 12345@s.whatsapp.net", "timeout_secs": invalid}),
                )
                .unwrap();
            assert_eq!(timeout_secs, 45);
        }
    }

    #[test]
    fn bool_flag_emits_flag_alone_when_truthy_and_skips_when_not() {
        let mut d = CliTemplateDispatcher::new(vec!["whatsgoingon".into()]).unwrap();
        d.parameter_defs = vec![
            ParameterDef {
                name: "topic".into(),
                ..Default::default()
            },
            ParameterDef {
                name: "quick".into(),
                default: Some("false".into()),
                ..Default::default()
            },
            ParameterDef {
                name: "deep".into(),
                default: Some("false".into()),
                ..Default::default()
            },
            ParameterDef {
                name: "no_auto_window".into(),
                default: Some("false".into()),
                ..Default::default()
            },
        ];
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                arg_mappings: vec![
                    CommandArgMapping::Positional {
                        param: "topic".into(),
                    },
                    CommandArgMapping::BoolFlag {
                        flag: "--quick".into(),
                        param: "quick".into(),
                    },
                    CommandArgMapping::BoolFlag {
                        flag: "--deep".into(),
                        param: "deep".into(),
                    },
                    CommandArgMapping::BoolFlag {
                        flag: "--no-auto-window".into(),
                        param: "no_auto_window".into(),
                    },
                ],
                timeout_secs: Some(30),
                ..Default::default()
            },
        );

        // JSON true triggers emit; absent / false / null skip.
        let (argv, _) = d
            .build_action_argv("run", &json!({"topic": "ai video", "quick": true}))
            .unwrap();
        assert_eq!(argv, vec!["whatsgoingon", "run", "ai video", "--quick"]);

        // Multiple truthy bools all emit, in mapping order.
        let (argv, _) = d
            .build_action_argv(
                "run",
                &json!({"topic": "ai video", "deep": true, "no_auto_window": true}),
            )
            .unwrap();
        assert_eq!(
            argv,
            vec![
                "whatsgoingon",
                "run",
                "ai video",
                "--deep",
                "--no-auto-window"
            ]
        );

        // String forms accepted (YAML defaults round-trip as strings).
        let (argv, _) = d
            .build_action_argv("run", &json!({"topic": "ai video", "quick": "true"}))
            .unwrap();
        assert_eq!(argv, vec!["whatsgoingon", "run", "ai video", "--quick"]);
        let (argv, _) = d
            .build_action_argv("run", &json!({"topic": "ai video", "quick": "yes"}))
            .unwrap();
        assert_eq!(argv, vec!["whatsgoingon", "run", "ai video", "--quick"]);
        let (argv, _) = d
            .build_action_argv("run", &json!({"topic": "ai video", "quick": "1"}))
            .unwrap();
        assert_eq!(argv, vec!["whatsgoingon", "run", "ai video", "--quick"]);

        // Falsy / unset / null / 0 / "false" / "no" all skip.
        // Numeric NaN and ±Infinity also skip (pathological JSON input).
        let nan = serde_json::Number::from_f64(f64::NAN);
        let pos_inf = serde_json::Number::from_f64(f64::INFINITY);
        let neg_inf = serde_json::Number::from_f64(f64::NEG_INFINITY);
        let mut falsy_inputs = vec![
            json!(false),
            json!(null),
            json!(0),
            json!("false"),
            json!("no"),
            json!("0"),
            json!(""),
            json!([1, 2]),
            json!({"k": "v"}),
        ];
        if let Some(n) = nan {
            falsy_inputs.push(Value::Number(n));
        }
        if let Some(n) = pos_inf {
            falsy_inputs.push(Value::Number(n));
        }
        if let Some(n) = neg_inf {
            falsy_inputs.push(Value::Number(n));
        }
        for falsy in falsy_inputs {
            let (argv, _) = d
                .build_action_argv("run", &json!({"topic": "ai video", "quick": falsy.clone()}))
                .unwrap();
            assert_eq!(
                argv,
                vec!["whatsgoingon", "run", "ai video"],
                "falsy value {falsy:?} should not emit --quick"
            );
        }

        // Param entirely absent → default ("false") → skip.
        let (argv, _) = d
            .build_action_argv("run", &json!({"topic": "ai video"}))
            .unwrap();
        assert_eq!(argv, vec!["whatsgoingon", "run", "ai video"]);
    }

    #[test]
    fn passthrough_appends_string_array_elements_after_typed_params() {
        // Hybrid schema for metabase-style hot paths: typed Flag /
        // Positional parameters PLUS a Passthrough escape hatch so the
        // LLM can still pass through CLI flags the schema does not
        // surface yet. The typed params land first (in mapping order),
        // then each Passthrough element appears verbatim, then any
        // suffix_args.
        let mut d = CliTemplateDispatcher::new(vec!["metabase-pp-cli".into()]).unwrap();
        d.action_schemas.insert(
            "table_list".to_string(),
            NativeActionSchemaDef {
                argv: vec!["table".into(), "list".into()],
                arg_mappings: vec![
                    CommandArgMapping::Flag {
                        flag: "--term".into(),
                        param: "term".into(),
                    },
                    CommandArgMapping::Flag {
                        flag: "--can-query".into(),
                        param: "can_query".into(),
                    },
                    CommandArgMapping::Passthrough {
                        param: "extra_args".into(),
                    },
                ],
                timeout_secs: Some(60),
                ..Default::default()
            },
        );

        // Boolean param round-trips to string "true" via stringify_scalar.
        let (argv, _) = d
            .build_action_argv("table_list", &json!({"term": "orders", "can_query": true}))
            .unwrap();
        assert_eq!(
            argv,
            vec![
                "metabase-pp-cli",
                "table",
                "list",
                "--term",
                "orders",
                "--can-query",
                "true",
            ]
        );

        // Passthrough only — typed params absent, escape hatch carries
        // the call.
        let (argv, _) = d
            .build_action_argv(
                "table_list",
                &json!({"extra_args": ["--limit", "50", "--include-archived"]}),
            )
            .unwrap();
        assert_eq!(
            argv,
            vec![
                "metabase-pp-cli",
                "table",
                "list",
                "--limit",
                "50",
                "--include-archived",
            ]
        );

        // Combined — typed params land first in mapping order; passthrough
        // appended after them.
        let (argv, _) = d
            .build_action_argv(
                "table_list",
                &json!({
                    "term": "orders",
                    "can_query": false,
                    "extra_args": ["--include-archived"],
                }),
            )
            .unwrap();
        assert_eq!(
            argv,
            vec![
                "metabase-pp-cli",
                "table",
                "list",
                "--term",
                "orders",
                "--can-query",
                "false",
                "--include-archived",
            ]
        );

        // Null / missing passthrough → no extra tokens.
        let (argv, _) = d
            .build_action_argv("table_list", &json!({"term": "orders"}))
            .unwrap();
        assert_eq!(
            argv,
            vec!["metabase-pp-cli", "table", "list", "--term", "orders"]
        );

        // Defensive: a stray scalar instead of an array is coerced to a
        // single token (the JSON Schema validator should reject this at
        // tool-call time; this branch keeps the dispatcher safe if it
        // ever sneaks through).
        let (argv, _) = d
            .build_action_argv("table_list", &json!({"extra_args": "--single-token"}))
            .unwrap();
        assert_eq!(
            argv,
            vec!["metabase-pp-cli", "table", "list", "--single-token"]
        );
    }

    #[test]
    fn action_schema_forwards_exact_args_without_parsing() {
        let mut d = CliTemplateDispatcher::new(vec!["gws".into()]).unwrap();
        d.action_schemas.insert(
            "gmail".to_string(),
            NativeActionSchemaDef {
                timeout_secs: Some(30),
                ..Default::default()
            },
        );

        let (argv, timeout_secs) = d
            .build_action_argv(
                "gmail",
                &json!({"args": ["+send", "--subject", "hello world"]}),
            )
            .unwrap();

        assert_eq!(
            argv,
            vec!["gws", "gmail", "+send", "--subject", "hello world"]
        );
        assert_eq!(timeout_secs, 30);
    }

    #[test]
    fn action_schema_does_not_forward_reserved_control_args_as_flags() {
        let mut d = CliTemplateDispatcher::new(vec!["metabase-pp-cli".into()]).unwrap();
        d.action_schemas.insert(
            "dataset_query".to_string(),
            NativeActionSchemaDef {
                argv: vec!["dataset".into(), "query".into()],
                suffix_args: vec!["--stdin".into()],
                ..Default::default()
            },
        );

        let (argv, _) = d
            .build_action_argv(
                "dataset_query",
                &json!({"stdin": "{\"database\":236}", "timeout_secs": 120}),
            )
            .unwrap();

        assert_eq!(argv, vec!["metabase-pp-cli", "dataset", "query", "--stdin"]);
    }

    #[tokio::test]
    async fn dispatch_writes_stdin_to_cli_subprocess() {
        let mut d = CliTemplateDispatcher::new(vec!["/bin/cat".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                skip_tool_name: true,
                ..Default::default()
            },
        );

        let result = d
            .dispatch("run", &json!({"stdin": "{\"database\":236}"}))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.stdout, "{\"database\":236}");
    }

    #[test]
    fn action_schema_can_skip_tool_name_for_direct_wrappers() {
        let mut d = CliTemplateDispatcher::new(vec!["mb-query.sh".into()]).unwrap();
        d.action_schemas.insert(
            "run".to_string(),
            NativeActionSchemaDef {
                skip_tool_name: true,
                ..Default::default()
            },
        );

        let (argv, _) = d
            .build_action_argv(
                "run",
                &json!({"args": ["api-datasets", "create-dataset", "2"]}),
            )
            .unwrap();

        assert_eq!(
            argv,
            vec!["mb-query.sh", "api-datasets", "create-dataset", "2"]
        );
    }

    #[test]
    fn action_argv_interpolation_can_use_parameter_defaults() {
        let mut d = CliTemplateDispatcher::new(vec!["/usr/bin/env".into()]).unwrap();
        d.parameter_defs.push(ParameterDef {
            name: "account".into(),
            required: false,
            default: Some("work".into()),
            description: None,
            param_type: None,
            aliases: Vec::new(),
            enum_values: None,
            schema: serde_json::Value::Null,
        });
        d.action_schemas.insert(
            "auth_status".to_string(),
            NativeActionSchemaDef {
                argv: vec![
                    "gws".into(),
                    "auth".into(),
                    "status".into(),
                    "{account}".into(),
                ],
                ..Default::default()
            },
        );

        let (argv, _) = d.build_action_argv("auth_status", &json!({})).unwrap();

        assert_eq!(argv, vec!["/usr/bin/env", "gws", "auth", "status", "work"]);
    }
}
