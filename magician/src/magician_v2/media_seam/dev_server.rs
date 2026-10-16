use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

pub use crate::magician_v2::media_seam::dev_server_manager::{
    dev_server_manager, DevServerSessionManager, DevServerStatus, DevServerStatusView,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevCommand {
    pub program: String,
    pub args: Vec<String>,
    pub display: String,
}

static LOCAL_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"https?://(?:localhost|127(?:\.\d{1,3}){3}|0\.0\.0\.0|\[::1\])(?::\d{2,5})?(?:/[^\s'"<>`]*)?"#,
    )
    .expect("valid local preview URL regex")
});

const VITE_CONFIG_NAMES: &[&str] = &["vite.config.ts", "vite.config.js", "vite.config.mjs"];
const ASTRO_CONFIG_NAMES: &[&str] = &["astro.config.ts", "astro.config.js", "astro.config.mjs"];

pub fn parse_ready_line(line: &str) -> Option<(String, u16)> {
    let stripped = strip_ansi(line);
    let matched = LOCAL_URL_RE.find(&stripped)?.as_str();
    normalize_local_url(matched)
}

pub fn detect_pm(dir: &Path) -> String {
    if dir.join("bun.lockb").exists() {
        "bun".to_string()
    } else if dir.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if dir.join("yarn.lock").exists() {
        "yarn".to_string()
    } else {
        "npm".to_string()
    }
}

pub fn detect_dev_command(dir: &Path) -> Result<DevCommand, String> {
    let package = read_package_json(dir)?;
    let pm = detect_pm(dir);
    let dev_script = script_value(package.as_ref(), "dev");
    let start_script = script_value(package.as_ref(), "start");
    let vite_config = find_config(dir, VITE_CONFIG_NAMES).is_some();
    let astro_config = find_config(dir, ASTRO_CONFIG_NAMES).is_some();

    if let Some(script) = dev_script {
        let mut command = package_manager_command(&pm, "dev", true);
        if is_vite_script(script)
            || is_astro_script(script)
            || vite_config
            || astro_config
            || package_has_dependency(package.as_ref(), "vite")
            || package_has_dependency(package.as_ref(), "astro")
        {
            append_host_args(&mut command, true);
        }
        return Ok(command);
    }

    if let Some(script) = start_script {
        let mut command = package_manager_command(&pm, "start", false);
        if is_vite_script(script)
            || is_astro_script(script)
            || vite_config
            || astro_config
            || package_has_dependency(package.as_ref(), "vite")
            || package_has_dependency(package.as_ref(), "astro")
        {
            append_host_args(&mut command, true);
        }
        return Ok(command);
    }

    if vite_config {
        return Ok(DevCommand {
            program: "npx".to_string(),
            args: vec![
                "vite".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
            ],
            display: "npx vite --host 127.0.0.1".to_string(),
        });
    }

    Ok(package_manager_command(&pm, "dev", true))
}

pub fn with_vite_hmr_config(
    dir: &Path,
    project_id: &str,
    command: DevCommand,
) -> Result<DevCommand, String> {
    if !is_vite_project(dir)? {
        return Ok(command);
    }

    let vibedev_dir = dir.join(".vibedev");
    std::fs::create_dir_all(&vibedev_dir).map_err(|error| {
        format!(
            "create VibeDev config directory `{}`: {error}",
            vibedev_dir.display()
        )
    })?;
    let config_filename = format!("vite.config.{project_id}.mjs");
    let config_path = vibedev_dir.join(&config_filename);
    let user_config = find_config(dir, VITE_CONFIG_NAMES);
    let hmr_path = format!("/api/magician/v2/vibedev/projects/{project_id}/preview/proxy/");
    let contents = render_vite_hmr_config(user_config.as_deref(), &hmr_path)?;
    std::fs::write(&config_path, contents).map_err(|error| {
        format!(
            "write VibeDev Vite config `{}`: {error}",
            config_path.display()
        )
    })?;
    let relative_config = PathBuf::from(".vibedev").join(config_filename);
    let config_arg = relative_config.to_string_lossy().to_string();
    Ok(DevCommand {
        program: "npx".to_string(),
        args: vec![
            "vite".to_string(),
            "--config".to_string(),
            config_arg.clone(),
        ],
        display: format!("npx vite --config {config_arg}"),
    })
}

/// Coarse project shape, used to decide whether the Preview tab is the hero
/// (web) or hidden (non-web → Code/Diff/Tests lead), and which checks to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Node,
    Rust,
    Python,
    Static,
    Unknown,
}

pub fn detect_project_kind(dir: &Path) -> ProjectKind {
    if dir.join("Cargo.toml").is_file() {
        return ProjectKind::Rust;
    }
    if dir.join("package.json").is_file() {
        return ProjectKind::Node;
    }
    if dir.join("pyproject.toml").is_file()
        || dir.join("requirements.txt").is_file()
        || dir.join("setup.py").is_file()
    {
        return ProjectKind::Python;
    }
    if dir.join("index.html").is_file() {
        return ProjectKind::Static;
    }
    ProjectKind::Unknown
}

/// Whether this project has a previewable dev server (S2). Non-web projects
/// (Rust/CLI/backend) return false so the cockpit hides Preview.
pub fn supports_preview(dir: &Path) -> bool {
    match detect_project_kind(dir) {
        ProjectKind::Static => true,
        ProjectKind::Node => {
            let package = read_package_json(dir).ok().flatten();
            script_value(package.as_ref(), "dev").is_some()
                || script_value(package.as_ref(), "start").is_some()
                || find_config(dir, VITE_CONFIG_NAMES).is_some()
                || find_config(dir, ASTRO_CONFIG_NAMES).is_some()
        },
        _ => false,
    }
}

/// A build/test/lint/typecheck command to run for the self-heal loop (S1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckCommand {
    /// "build" | "test" | "lint" | "typecheck"
    pub kind: String,
    pub program: String,
    pub args: Vec<String>,
    pub display: String,
}

fn npm_check(pm: &str, kind: &str, script: &str) -> CheckCommand {
    let cmd = package_manager_command(pm, script, true);
    CheckCommand {
        kind: kind.to_string(),
        program: cmd.program,
        args: cmd.args,
        display: cmd.display,
    }
}

fn raw_check(kind: &str, program: &str, args: &[&str]) -> CheckCommand {
    let args: Vec<String> = args.iter().map(|value| value.to_string()).collect();
    CheckCommand {
        kind: kind.to_string(),
        display: format!("{program} {}", args.join(" ")),
        program: program.to_string(),
        args,
    }
}

/// The checks available for a project, by shape (S1). Node maps package.json
/// scripts (build/test/lint/typecheck — `check`/`tsc` count as typecheck);
/// Rust/Python use sensible defaults.
pub fn detect_check_commands(dir: &Path) -> Vec<CheckCommand> {
    let mut out = Vec::new();
    match detect_project_kind(dir) {
        ProjectKind::Node => {
            let pm = detect_pm(dir);
            let package = read_package_json(dir).ok().flatten();
            let groups: &[(&str, &[&str])] = &[
                ("typecheck", &["typecheck", "check", "check:types", "tsc"]),
                ("lint", &["lint"]),
                ("test", &["test"]),
                ("build", &["build"]),
            ];
            for (kind, scripts) in groups {
                for script in *scripts {
                    if script_value(package.as_ref(), script).is_some() {
                        out.push(npm_check(&pm, kind, script));
                        break;
                    }
                }
            }
        },
        ProjectKind::Rust => {
            out.push(raw_check("build", "cargo", &["check"]));
            out.push(raw_check("test", "cargo", &["test"]));
            out.push(raw_check("lint", "cargo", &["clippy"]));
        },
        ProjectKind::Python => {
            out.push(raw_check("test", "python3", &["-m", "pytest", "-q"]));
        },
        ProjectKind::Static | ProjectKind::Unknown => {},
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeployTargetInfo {
    pub framework: String,
    pub build_command: Option<CheckCommand>,
    pub output_dir: String,
}

pub fn detect_deploy_target(dir: &Path) -> Option<DeployTargetInfo> {
    let kind = detect_project_kind(dir);
    if kind == ProjectKind::Rust || kind == ProjectKind::Python || kind == ProjectKind::Unknown {
        return None;
    }

    let build_command = detect_check_commands(dir)
        .into_iter()
        .find(|c| c.kind == "build");

    let mut framework = "unknown".to_string();
    let mut output_dir = ".".to_string();

    if kind == ProjectKind::Static {
        framework = "static".to_string();
        output_dir = ".".to_string();
    } else if kind == ProjectKind::Node {
        let package = read_package_json(dir).ok().flatten();
        if package_has_dependency(package.as_ref(), "next") {
            framework = "nextjs".to_string();
            output_dir = "out".to_string();
        } else if package_has_dependency(package.as_ref(), "@sveltejs/kit") {
            framework = "sveltekit".to_string();
            output_dir = "build".to_string();
        } else if package_has_dependency(package.as_ref(), "astro") {
            framework = "astro".to_string();
            output_dir = "dist".to_string();
        } else if package_has_dependency(package.as_ref(), "vite")
            || find_config(dir, VITE_CONFIG_NAMES).is_some()
        {
            framework = "vite".to_string();
            output_dir = "dist".to_string();
        }
    }

    Some(DeployTargetInfo {
        framework,
        build_command,
        output_dir,
    })
}

fn strip_ansi(input: &str) -> String {
    let bytes = strip_ansi_escapes::strip(input.as_bytes());
    String::from_utf8_lossy(&bytes).to_string()
}

fn normalize_local_url(raw: &str) -> Option<(String, u16)> {
    let trimmed = raw.trim().trim_end_matches([',', ')', '.', ';']);
    let mut parsed = url::Url::parse(trimmed).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    // `url::Url::host_str` returns IPv6 hosts in bracketed form (`[::1]`), so
    // compare against the unbracketed address.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let is_loopbackish = bare == "0.0.0.0"
        || bare == "::1"
        || bare == "::"
        || (bare.starts_with("127.") && is_loopback_ipv4(bare));
    if is_loopbackish {
        parsed.set_host(Some("localhost")).ok()?;
    } else if host != "localhost" {
        return None;
    }
    let port = parsed.port_or_known_default()?;
    Some((parsed.to_string(), port))
}

fn is_loopback_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts[0] == "127"
        && parts
            .iter()
            .all(|part| part.parse::<u8>().map(|_| true).unwrap_or(false))
}

fn read_package_json(dir: &Path) -> Result<Option<Value>, String> {
    let path = dir.join("package.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("read package.json `{}`: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("parse package.json `{}`: {error}", path.display()))
}

fn script_value<'a>(package: Option<&'a Value>, name: &str) -> Option<&'a str> {
    package?
        .get("scripts")?
        .get(name)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn package_manager_command(pm: &str, script: &str, run: bool) -> DevCommand {
    let args = if run {
        vec!["run".to_string(), script.to_string()]
    } else {
        vec![script.to_string()]
    };
    DevCommand {
        program: pm.to_string(),
        display: format!("{pm} {}", args.join(" ")),
        args,
    }
}

fn append_host_args(command: &mut DevCommand, script_command: bool) {
    if script_command {
        command.args.push("--".to_string());
    }
    command.args.push("--host".to_string());
    command.args.push("127.0.0.1".to_string());
    command.display = format!("{} {}", command.program, command.args.join(" "));
}

fn is_vite_script(script: &str) -> bool {
    command_mentions_tool(script, "vite")
}

fn is_astro_script(script: &str) -> bool {
    command_mentions_tool(script, "astro")
}

fn command_mentions_tool(script: &str, tool: &str) -> bool {
    script
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .any(|part| part.eq_ignore_ascii_case(tool))
}

fn package_has_dependency(package: Option<&Value>, name: &str) -> bool {
    let Some(package) = package else {
        return false;
    };
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|section| {
            package
                .get(section)
                .and_then(|value| value.get(name))
                .is_some()
        })
}

fn find_config(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn is_vite_project(dir: &Path) -> Result<bool, String> {
    if find_config(dir, VITE_CONFIG_NAMES).is_some() {
        return Ok(true);
    }
    let package = read_package_json(dir)?;
    Ok(script_value(package.as_ref(), "dev")
        .map(is_vite_script)
        .unwrap_or(false)
        || script_value(package.as_ref(), "start")
            .map(is_vite_script)
            .unwrap_or(false)
        || package_has_dependency(package.as_ref(), "vite"))
}

fn render_vite_hmr_config(user_config: Option<&Path>, hmr_path: &str) -> Result<String, String> {
    let hmr_path = serde_json::to_string(hmr_path)
        .map_err(|error| format!("serialize Vite HMR path: {error}"))?;
    let user_config_block = if let Some(path) = user_config {
        let absolute = path.canonicalize().map_err(|error| {
            format!(
                "canonicalize user Vite config `{}`: {error}",
                path.display()
            )
        })?;
        let url = url::Url::from_file_path(&absolute).map_err(|_| {
            format!(
                "convert user Vite config path `{}` to file URL",
                absolute.display()
            )
        })?;
        let url = serde_json::to_string(url.as_str())
            .map_err(|error| format!("serialize user Vite config URL: {error}"))?;
        format!("import userDefault from {url};")
    } else {
        "const userDefault = {};".to_string()
    };
    Ok(format!(
        r#"import {{ mergeConfig }} from 'vite';
{user_config_block}

export default async (env) => mergeConfig(
  typeof userDefault === 'function' ? await userDefault(env) : userDefault,
  {{
    server: {{
      host: '127.0.0.1',
      allowedHosts: true,
      hmr: {{ path: {hmr_path} }}
    }}
  }}
);
"#
    ))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;

    #[test]
    fn parse_ready_line_handles_vite_banner_with_ansi() {
        let line = "\u{1b}[32m  ➜\u{1b}[39m  Local:   http://localhost:5173/";
        assert_eq!(
            parse_ready_line(line),
            Some(("http://localhost:5173/".to_string(), 5173))
        );
    }

    #[test]
    fn parse_ready_line_handles_next_banner() {
        let line = "- Local:        http://127.0.0.1:3000";
        assert_eq!(
            parse_ready_line(line),
            Some(("http://localhost:3000/".to_string(), 3000))
        );
    }

    #[test]
    fn parse_ready_line_handles_cra_banner_and_zero_host() {
        let line = "  Local:            http://0.0.0.0:3000";
        assert_eq!(
            parse_ready_line(line),
            Some(("http://localhost:3000/".to_string(), 3000))
        );
    }

    #[test]
    fn parse_ready_line_handles_ipv6_loopback() {
        let line = "ready on http://[::1]:4173/app";
        assert_eq!(
            parse_ready_line(line),
            Some(("http://localhost:4173/app".to_string(), 4173))
        );
    }

    #[test]
    fn detect_pm_prefers_known_lockfiles() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(detect_pm(temp.path()), "npm");
        std::fs::write(temp.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_pm(temp.path()), "yarn");
        std::fs::write(temp.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_pm(temp.path()), "pnpm");
        std::fs::write(temp.path().join("bun.lockb"), "").unwrap();
        assert_eq!(detect_pm(temp.path()), "bun");
    }

    #[test]
    fn detect_dev_command_uses_dev_script_and_host_for_vite() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"dev":"vite --clearScreen false"}}"#,
        )
        .unwrap();
        let command = detect_dev_command(temp.path()).unwrap();
        assert_eq!(command.program, "npm");
        assert_eq!(command.args, ["run", "dev", "--", "--host", "127.0.0.1"]);
        assert_eq!(command.display, "npm run dev -- --host 127.0.0.1");
    }

    #[test]
    fn detect_dev_command_uses_start_script_for_cra() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("yarn.lock"), "").unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"start":"react-scripts start"}}"#,
        )
        .unwrap();
        let command = detect_dev_command(temp.path()).unwrap();
        assert_eq!(command.program, "yarn");
        assert_eq!(command.args, ["start"]);
        assert_eq!(command.display, "yarn start");
    }

    #[test]
    fn detect_dev_command_uses_direct_vite_when_only_config_exists() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("vite.config.mjs"), "export default {};").unwrap();
        let command = detect_dev_command(temp.path()).unwrap();
        assert_eq!(command.program, "npx");
        assert_eq!(command.args, ["vite", "--host", "127.0.0.1"]);
        assert_eq!(command.display, "npx vite --host 127.0.0.1");
    }

    #[test]
    fn detect_dev_command_falls_back_to_pm_run_dev() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("pnpm-lock.yaml"), "").unwrap();
        let command = detect_dev_command(temp.path()).unwrap();
        assert_eq!(command.program, "pnpm");
        assert_eq!(command.args, ["run", "dev"]);
        assert_eq!(command.display, "pnpm run dev");
    }

    #[test]
    fn detect_project_kind_prefers_rust_then_node() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(detect_project_kind(temp.path()), ProjectKind::Unknown);
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_project_kind(temp.path()), ProjectKind::Node);
        std::fs::write(temp.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project_kind(temp.path()), ProjectKind::Rust);
    }

    #[test]
    fn detect_check_commands_maps_node_scripts() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"build":"vite build","test":"vitest run","check":"svelte-check"}}"#,
        )
        .unwrap();
        let kinds: Vec<String> = detect_check_commands(temp.path())
            .into_iter()
            .map(|command| command.kind)
            .collect();
        assert!(kinds.contains(&"build".to_string()));
        assert!(kinds.contains(&"test".to_string()));
        assert!(kinds.contains(&"typecheck".to_string())); // `check` → typecheck
        assert!(!kinds.contains(&"lint".to_string())); // no lint script
    }

    #[test]
    fn supports_preview_true_for_vite_node_false_for_rust() {
        let node = tempfile::tempdir().unwrap();
        std::fs::write(
            node.path().join("package.json"),
            r#"{"scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        assert!(supports_preview(node.path()));
        let rust = tempfile::tempdir().unwrap();
        std::fs::write(rust.path().join("Cargo.toml"), "[package]").unwrap();
        assert!(!supports_preview(rust.path()));
    }
}
