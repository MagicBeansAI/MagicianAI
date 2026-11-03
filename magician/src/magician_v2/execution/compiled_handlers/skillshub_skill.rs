//! `validate_skill`, `test_skill` and `install_skill`: check and install a
//! skill from the repo's `skillshub/` without leaving the conversation.
//!
//! The validator only parses files, so it runs without asking. Running a
//! skill's tests executes code from the checkout, and installing writes the
//! scope's skills layer, so both ask the owner first. Install is the same
//! linked install the Skills page does (`POST /skills/install`).
//!
//! They read the live checkout: a skill a coding run wrote in a workspace copy
//! is there once its change is applied. Python runs with a curated
//! environment (no provider keys or tokens), and all three are off the plane
//! (`NEVER_ON_THE_PLANE`), where a consumer could write the code they run.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};

use super::shared::require_scope_str;
use crate::magician_v2::{
    execution::{agent_resources::AgentResources, error::ExecutionError},
    skills::{
        loader::{diagnose_skill_dir, SkillDirVerdict},
        path_rewrite::SKILLSHUB_ROOT_ENV,
        scope_layer,
    },
    user_requests::{RequestOption, UserRequest},
};

const VALIDATE_TIMEOUT: Duration = Duration::from_secs(120);
const TEST_TIMEOUT: Duration = Duration::from_secs(600);
const OUTPUT_TAIL_CHARS: usize = 6_000;
const APPROVAL_TIMEOUT_SECS: u64 = 300;

pub async fn validate_handle(
    _resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let root = repo_root()?;
    let name = skill_name(&args, "validate_skill")?;
    validate(&root, &name).await
}

pub async fn test_handle(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    test_in(&resources, &repo_root()?, &args).await
}

async fn test_in(
    resources: &AgentResources,
    root: &Path,
    args: &Value,
) -> Result<Value, ExecutionError> {
    let name = skill_name(args, "test_skill")?;
    let suites = test_suites(root, &name)?;
    if suites.is_empty() {
        return Ok(json!({ "skill": name, "status": "no_tests" }));
    }
    let listed = suites
        .iter()
        .map(|suite| suite.display.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if !owner_approves(
        resources,
        args,
        "skill_test_approval",
        format!("Run the tests for skill `{name}`? They run code from the repo checkout: {listed}."),
        json!({ "kind": "skill_test_approval", "skill": name, "suites": listed }),
    )
    .await?
    {
        return Ok(json!({ "skill": name, "status": "denied" }));
    }
    run_tests(root, &name, &suites).await
}

pub async fn install_handle(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "install_skill")?;
    let workspace = require_scope_str(&args, "__workspace", "install_skill")?;
    // In a container the scope tree may be the host's, mounted: links made in
    // here would point at the container's checkout and dangle on the host.
    if std::env::var_os(SKILLSHUB_ROOT_ENV).is_some_and(|value| !value.is_empty()) {
        return Err(ExecutionError::Step(
            "install_skill does not run in a container: the scope's skills belong to the host. \
             Install it there (Skills page, or make -C skillshub install-scope)."
                .into(),
        ));
    }
    let root = repo_root()?;
    let name = skill_name(&args, "install_skill")?;
    check_loads(&root, &name)?;
    if !owner_approves(
        &resources,
        &args,
        "skill_install_approval",
        format!(
            "Install skill `{name}` from the repo's skillshub into {principal}/{workspace}? \
             New chat sessions there will be able to use it."
        ),
        json!({
            "kind": "skill_install_approval",
            "skill": name,
            "scope": format!("{principal}/{workspace}"),
        }),
    )
    .await?
    {
        return Ok(json!({ "skill": name, "status": "denied" }));
    }
    let layout = &resources.artifact_workspace;
    let skills_root = layout.scope_skills_root(&principal, &workspace);
    let store = layout.system_root().join(scope_layer::SKILL_BIN_STORE_DIRNAME);
    let skills_root_for_install = skills_root.clone();
    let hub = root.join("skillshub");
    let install_name = name.clone();
    tokio::task::spawn_blocking(move || {
        scope_layer::install_linked_skill(
            &hub,
            &skills_root_for_install,
            &install_name,
            Some(&store),
        )
    })
    .await
    .map_err(|error| ExecutionError::Step(format!("install_skill task failed: {error}")))?
    .map_err(|error| {
        ExecutionError::Step(format!(
            "installing `{name}` into {principal}/{workspace} failed: {error}"
        ))
    })?;
    if let Some(resolver) = resources.content_acquisition_resolver() {
        resolver.invalidate_scope(&principal, &workspace);
    }
    crate::magician_v2::execution::primitive_dispatch::cli_template::dispatcher::invalidate_pack_dir_index(
        &skills_root,
    );
    tracing::info!(skill = %name, %principal, %workspace, "install_skill: installed");
    Ok(json!({
        "skill": name,
        "status": "installed",
        "scope": format!("{principal}/{workspace}"),
        "path": skills_root.join(&name).display().to_string(),
        "note": "New chat sessions in this scope discover the skill; no restart is needed.",
    }))
}

/// The magician checkout whose `skillshub/` these tools read.
fn repo_root() -> Result<PathBuf, ExecutionError> {
    crate::magician_v2::execution::coding_engine::live_repo_source_root()
        .or_else(|| std::env::current_dir().ok())
        .filter(|root| root.join("skillshub").is_dir())
        .ok_or_else(|| ExecutionError::Step("the repo's skillshub/ folder was not found".into()))
}

/// The `name` argument: one plain folder under `skillshub/` holding a SKILL.md.
fn skill_name(args: &Value, tool: &str) -> Result<String, ExecutionError> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if !plain {
        return Err(ExecutionError::Step(format!(
            "{tool} needs `name`: a skill folder under skillshub/ (got {name:?})"
        )));
    }
    Ok(name.to_string())
}

fn skill_dir(root: &Path, name: &str) -> Result<PathBuf, ExecutionError> {
    let dir = root.join("skillshub").join(name);
    if !dir.join("SKILL.md").is_file() {
        return Err(ExecutionError::Step(format!(
            "skillshub/{name} has no SKILL.md"
        )));
    }
    Ok(dir)
}

/// `skillshub/<name>` itself loads the way the runtime will load it.
fn check_loads(root: &Path, name: &str) -> Result<(), ExecutionError> {
    let dir = skill_dir(root, name)?;
    match diagnose_skill_dir(&dir) {
        SkillDirVerdict::Skill(manifest) if manifest.name == name => Ok(()),
        SkillDirVerdict::Skill(manifest) => Err(ExecutionError::Step(format!(
            "skillshub/{name} names itself `{}`: its frontmatter `name` must match the folder",
            manifest.name
        ))),
        SkillDirVerdict::App => Err(ExecutionError::Step(format!(
            "skillshub/{name} is an app package, not a skill"
        ))),
        SkillDirVerdict::Rejected(reason) => Err(ExecutionError::Step(format!(
            "the skill loader rejects skillshub/{name}: {reason}"
        ))),
    }
}

async fn validate(root: &Path, name: &str) -> Result<Value, ExecutionError> {
    skill_dir(root, name)?;
    let skill = format!("skillshub/{name}");
    let run = run_python(
        root,
        &["skillshub/scripts/validate_skill_md.py", skill.as_str()],
        VALIDATE_TIMEOUT,
    )
    .await;
    Ok(json!({
        "skill": name,
        "status": if run.ok { "valid" } else { "invalid" },
        "command": format!("python3 skillshub/scripts/validate_skill_md.py {skill}"),
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "output_tail": run.output_tail,
    }))
}

struct TestSuite {
    /// `unittest discover` arguments, relative to the repo root.
    args: Vec<String>,
    display: String,
}

/// A skill's tests: its own `tests/` folder and its
/// `skillshub/scripts/tests/test_<name>.py` (dashes as underscores).
fn test_suites(root: &Path, name: &str) -> Result<Vec<TestSuite>, ExecutionError> {
    skill_dir(root, name)?;
    let mut suites = Vec::new();
    if root.join("skillshub").join(name).join("tests").is_dir() {
        let start = format!("skillshub/{name}/tests");
        suites.push(TestSuite {
            display: start.clone(),
            args: vec!["-s".into(), start],
        });
    }
    let file = format!("test_{}.py", name.replace('-', "_"));
    if root.join("skillshub/scripts/tests").join(&file).is_file() {
        suites.push(TestSuite {
            display: format!("skillshub/scripts/tests/{file}"),
            args: vec![
                "-s".into(),
                "skillshub/scripts/tests".into(),
                "-p".into(),
                file,
            ],
        });
    }
    Ok(suites)
}

async fn run_tests(root: &Path, name: &str, suites: &[TestSuite]) -> Result<Value, ExecutionError> {
    let mut results = Vec::new();
    let mut all_ok = true;
    for suite in suites {
        let mut args = vec!["-m", "unittest", "discover"];
        args.extend(suite.args.iter().map(String::as_str));
        let run = run_python(root, &args, TEST_TIMEOUT).await;
        all_ok &= run.ok;
        results.push(json!({
            "suite": suite.display,
            "ok": run.ok,
            "exit_code": run.exit_code,
            "timed_out": run.timed_out,
            "output_tail": run.output_tail,
        }));
    }
    Ok(json!({
        "skill": name,
        "status": if all_ok { "passed" } else { "failed" },
        "results": results,
    }))
}

async fn owner_approves(
    resources: &AgentResources,
    args: &Value,
    request_type: &str,
    question: String,
    context: Value,
) -> Result<bool, ExecutionError> {
    let principal = require_scope_str(args, "__principal", request_type)?;
    let workspace = require_scope_str(args, "__workspace", request_type)?;
    let service = resources
        .user_request_service
        .clone()
        .ok_or_else(|| ExecutionError::Step("owner approval service is unavailable".into()))?;
    let options = vec![
        RequestOption {
            id: "approve_once".into(),
            label: "Approve Once".into(),
            requires_input: false,
        },
        RequestOption {
            id: "deny".into(),
            label: "Deny".into(),
            requires_input: false,
        },
    ];
    let response = service
        .ask(UserRequest {
            id: String::new(),
            request_type: request_type.into(),
            question,
            options: options.clone(),
            principal,
            workspace,
            context,
            source: "skillshub".into(),
            execution_id: args
                .get("__execution_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            task_id: args
                .get("__task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            timeout_secs: APPROVAL_TIMEOUT_SECS,
            default_on_timeout: "deny".into(),
            created_at: 0,
            sensitive: None,
        })
        .await;
    Ok(response.chosen_option(&options) == Some("approve_once"))
}

struct PythonRun {
    ok: bool,
    exit_code: Option<i32>,
    timed_out: bool,
    output_tail: String,
}

/// Variables a skill's Python keeps; everything else (provider keys, bot
/// tokens, nested-session markers) is cleared.
const PYTHON_ENV_KEEP: &[&str] = &["HOME", "USER", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR"];

/// Run the skillshub Python (its `.venv` when present, as the skillshub
/// Makefile does, else `python3`) from the repo root with a curated
/// environment. Sandboxed (repo read-only) inside a coding context; bytecode
/// is not written either way.
async fn run_python(root: &Path, args: &[&str], timeout: Duration) -> PythonRun {
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let venv = root.join("skillshub/.venv/bin/python");
    // Resolved against the PATH the child gets: a bare name with a cleared
    // environment would make Rust fork instead of posix_spawn.
    let program = if venv.is_file() {
        venv.into_os_string()
    } else {
        runtime_core::process::resolve_program_str("python3", Some(&path_env)).into_os_string()
    };
    let sandbox_args: Vec<std::ffi::OsString> =
        args.iter().map(std::ffi::OsString::from).collect();
    let mut cmd = crate::magician_v2::execution::coding_engine::os_sandbox_command(
        program.as_os_str(),
        &sandbox_args,
    );
    cmd.env_clear();
    for key in PYTHON_ENV_KEEP {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("PATH", &path_env)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("NO_COLOR", "1");
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return PythonRun {
                ok: false,
                exit_code: None,
                timed_out: false,
                output_tail: format!("failed to start python: {error}"),
            }
        },
    };
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            PythonRun {
                ok: output.status.success(),
                exit_code: output.status.code(),
                timed_out: false,
                output_tail: tail_chars(&combined, OUTPUT_TAIL_CHARS),
            }
        },
        Ok(Err(error)) => PythonRun {
            ok: false,
            exit_code: None,
            timed_out: false,
            output_tail: format!("error running python: {error}"),
        },
        Err(_) => PythonRun {
            ok: false,
            exit_code: None,
            timed_out: true,
            output_tail: format!("timed out after {}s", timeout.as_secs()),
        },
    }
}

fn tail_chars(value: &str, max: usize) -> String {
    let count = value.chars().count();
    if count <= max {
        return value.to_string();
    }
    let tail: String = value.chars().skip(count - max).collect();
    format!("…(truncated {} chars)\n{tail}", count - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("skillshub/demo-skill");
        std::fs::create_dir_all(skill.join("tests")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: A demo procedure.\n---\n\n# Demo\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("skillshub/scripts/tests")).unwrap();
        std::fs::write(
            root.path().join("skillshub/scripts/tests/test_demo_skill.py"),
            "",
        )
        .unwrap();
        root
    }

    #[test]
    fn a_skill_name_is_one_plain_folder() {
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "../etc", "name with space"] {
            assert!(skill_name(&json!({ "name": bad }), "t").is_err(), "{bad:?}");
        }
        assert!(skill_name(&json!({}), "t").is_err());
        assert_eq!(
            skill_name(&json!({ "name": " impeccable " }), "t").unwrap(),
            "impeccable"
        );
    }

    #[test]
    fn a_skill_runs_its_own_tests_and_its_scripts_test_file() {
        let root = checkout();
        let suites = test_suites(root.path(), "demo-skill").unwrap();
        let shown: Vec<&str> = suites.iter().map(|suite| suite.display.as_str()).collect();
        assert_eq!(
            shown,
            [
                "skillshub/demo-skill/tests",
                "skillshub/scripts/tests/test_demo_skill.py"
            ]
        );
        assert!(test_suites(root.path(), "missing").is_err());
    }

    #[test]
    fn a_skill_the_loader_cannot_find_is_not_installed() {
        let root = checkout();
        assert!(check_loads(root.path(), "demo-skill").is_ok());
        let other = root.path().join("skillshub/renamed");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("SKILL.md"),
            "---\nname: not-renamed\ndescription: Mismatched.\n---\n",
        )
        .unwrap();
        assert!(check_loads(root.path(), "renamed").is_err());
    }

    #[tokio::test]
    async fn running_tests_waits_for_the_owner_and_a_denial_runs_nothing() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        use crate::magician_v2::user_requests::service::{UserRequestService, UserResponse};
        let root = checkout();
        let mut resources = access::test_resources(root.path());
        let service = Arc::new(UserRequestService::new(Arc::new(
            crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64),
        )));
        Arc::make_mut(&mut resources).user_request_service = Some(service.clone());
        let args = json!({
            "name": "demo-skill",
            "__principal": "owner",
            "__workspace": "default",
        });
        let answer = async {
            let request = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(request) = service.list_pending().await.into_iter().next() {
                        break request;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the owner is asked before any test runs");
            assert_eq!(request.request_type, "skill_test_approval");
            assert!(request.question.contains("test_demo_skill.py"));
            assert!(
                service
                    .respond(UserResponse {
                        request_id: request.id,
                        decision: "provide_input".into(),
                        input: Some("Deny".into()),
                        channel: "ios".into(),
                        sensitive: Vec::new(),
                    })
                    .await
            );
        };
        let (result, ()) = tokio::join!(test_in(&resources, root.path(), &args), answer);
        assert_eq!(result.unwrap()["status"], "denied");
    }
}
