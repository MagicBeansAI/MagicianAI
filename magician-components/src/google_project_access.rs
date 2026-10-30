//! Whether a Google account may use the Cloud project behind the Workspace
//! OAuth client, and granting it when the host can.
//!
//! Every Google Workspace account Magician signs in bills its API calls to the
//! project named in `client_secret.json`, and the Gmail bot's `gws gmail
//! +watch` creates a Pub/Sub topic and subscription there. An account that is
//! not the project's owner therefore needs two project roles, or mail sync
//! fails with "Caller does not have required permission to use project" and
//! the watcher restarts forever on `pubsub.topics.create`. Signing in again
//! cannot fix that, so setup has to.
//!
//! The check reads the project's IAM policy with the host's own `gcloud`
//! (never an account's per-profile `CLOUDSDK_CONFIG`). A grant runs only when
//! that gcloud account can set the policy; otherwise callers show
//! [`AccessReport::commands`] to someone who can. Bash setup, the terminal
//! wizard (`magician.bin google-project-access`), the Service accounts page
//! and Desktop onboarding all go through this module.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Project roles a non-owner account needs: quota/billing on the project for
/// every API call, and Pub/Sub topics and subscriptions for the Gmail watch.
pub const REQUIRED_ROLES: [&str; 2] = [
    "roles/serviceusage.serviceUsageConsumer",
    "roles/pubsub.editor",
];

/// Basic roles that already include both required roles.
const COVERING_ROLES: [&str; 2] = ["roles/owner", "roles/editor"];

/// Roles that let the host's gcloud account change the project IAM policy.
const POLICY_ADMIN_ROLES: [&str; 3] = [
    "roles/owner",
    "roles/resourcemanager.projectIamAdmin",
    "roles/iam.securityAdmin",
];

const GCLOUD_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessStatus {
    Granted,
    Missing,
    /// The policy could not be read (no gcloud, not signed in, no permission).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessReport {
    pub project_id: String,
    pub email: String,
    pub status: AccessStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_roles: Vec<String>,
    pub detail: String,
    /// The host gcloud account that can grant the missing roles, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_account: Option<String>,
    /// Commands a project owner can run instead; empty when nothing is missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    pub console_url: String,
}

impl AccessReport {
    pub fn can_grant(&self) -> bool {
        self.status != AccessStatus::Granted && self.grant_account.is_some()
    }
}

/// The project id in a Google OAuth client file (`installed` or `web`).
pub fn project_id_from_client_secret(path: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Client {
        project_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct ClientFile {
        installed: Option<Client>,
        web: Option<Client>,
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    // JSON is a YAML subset; this crate keeps serde_yaml as its only parser.
    let file: ClientFile = serde_yaml::from_str(&text)
        .map_err(|_| format!("{} is not a Google OAuth client file", path.display()))?;
    file.installed
        .or(file.web)
        .and_then(|client| client.project_id)
        .map(|id| id.trim().to_string())
        .filter(|id| valid_project_id(id))
        .ok_or_else(|| format!("{} names no Cloud project", path.display()))
}

/// Google accounts named in `operator-config.yaml` (`gws_accounts` and
/// `gws_fixed_profiles`) as `(profile, expected_email)`.
pub fn operator_accounts(operator_config: &Path) -> Result<Vec<(String, String)>, String> {
    #[derive(Deserialize)]
    struct Account {
        name: String,
        expected_email: Option<String>,
    }
    #[derive(Deserialize)]
    struct OperatorConfig {
        #[serde(default)]
        gws_accounts: Vec<Account>,
        #[serde(default)]
        gws_fixed_profiles: Vec<Account>,
    }
    let text = std::fs::read_to_string(operator_config)
        .map_err(|error| format!("cannot read {}: {error}", operator_config.display()))?;
    let config: OperatorConfig = serde_yaml::from_str(&text)
        .map_err(|error| format!("cannot parse {}: {error}", operator_config.display()))?;
    Ok(config
        .gws_accounts
        .into_iter()
        .chain(config.gws_fixed_profiles)
        .filter_map(|account| {
            let email = account.expected_email?.trim().to_ascii_lowercase();
            valid_email(&email).then_some((account.name, email))
        })
        .collect())
}

/// Read the project policy and report what `email` is missing.
pub fn check(project_id: &str, email: &str) -> AccessReport {
    let email = email.trim().to_ascii_lowercase();
    let mut report = AccessReport {
        project_id: project_id.to_string(),
        email: email.clone(),
        status: AccessStatus::Unknown,
        missing_roles: Vec::new(),
        detail: String::new(),
        grant_account: None,
        commands: Vec::new(),
        console_url: format!("https://console.cloud.google.com/iam-admin/iam?project={project_id}"),
    };
    if !valid_project_id(project_id) || !valid_email(&email) {
        report.detail = "The project id or Google email is not valid.".to_string();
        return report;
    }
    report.commands = grant_commands(project_id, &email, &REQUIRED_ROLES);
    let policy = match read_policy(project_id) {
        Ok(policy) => policy,
        Err(error) => {
            report.detail = format!(
                "Could not read the IAM policy of {project_id} with this computer's gcloud \
                 ({error}). A project owner must grant {}.",
                REQUIRED_ROLES.join(" and ")
            );
            return report;
        },
    };
    report.missing_roles = missing_roles(&policy, &email);
    if report.missing_roles.is_empty() {
        report.status = AccessStatus::Granted;
        report.detail = format!("{email} can use project {project_id}.");
        report.commands.clear();
        return report;
    }
    report.status = AccessStatus::Missing;
    let missing: Vec<&str> = report.missing_roles.iter().map(String::as_str).collect();
    report.commands = grant_commands(project_id, &email, &missing);
    report.grant_account = active_account()
        .ok()
        .filter(|account| can_set_policy(&policy, account));
    report.detail = match &report.grant_account {
        Some(account) => format!(
            "{email} is missing {} on project {project_id}. This computer's gcloud account \
             {account} can grant it.",
            report.missing_roles.join(" and ")
        ),
        None => format!(
            "{email} is missing {} on project {project_id}. Ask a project owner to grant it.",
            report.missing_roles.join(" and ")
        ),
    };
    report
}

/// Grant the roles `check` found missing, using the host's gcloud account,
/// then check again. Refuses when that account cannot set the policy.
pub fn grant(project_id: &str, email: &str) -> Result<AccessReport, String> {
    let before = check(project_id, email);
    if before.status == AccessStatus::Granted {
        return Ok(before);
    }
    if before.grant_account.is_none() {
        return Err(before.detail);
    }
    for role in &before.missing_roles {
        run_gcloud(&[
            "projects",
            "add-iam-policy-binding",
            project_id,
            &format!("--member=user:{}", before.email),
            &format!("--role={role}"),
            "--condition=None",
            "--format=none",
            "--quiet",
        ])
        .map_err(|error| format!("granting {role} failed: {error}"))?;
    }
    Ok(check(project_id, email))
}

/// Required roles `email` holds neither directly, through its domain, nor
/// through a basic role that covers them. Conditional bindings do not count.
fn missing_roles(policy: &Policy, email: &str) -> Vec<String> {
    let member = format!("user:{email}");
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| format!("domain:{domain}"));
    let held = |role: &str| {
        policy.bindings.iter().any(|binding| {
            binding.role == role
                && binding.condition.is_none()
                && binding.members.iter().any(|candidate| {
                    let candidate = candidate.to_ascii_lowercase();
                    candidate == member || Some(&candidate) == domain.as_ref()
                })
        })
    };
    if COVERING_ROLES.iter().any(|role| held(role)) {
        return Vec::new();
    }
    REQUIRED_ROLES
        .iter()
        .filter(|role| !held(role))
        .map(|role| role.to_string())
        .collect()
}

fn can_set_policy(policy: &Policy, account: &str) -> bool {
    let admin = format!("user:{account}");
    policy.bindings.iter().any(|binding| {
        POLICY_ADMIN_ROLES.contains(&binding.role.as_str())
            && binding.condition.is_none()
            && binding
                .members
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&admin))
    })
}

fn grant_commands(project_id: &str, email: &str, roles: &[&str]) -> Vec<String> {
    roles
        .iter()
        .map(|role| {
            format!(
                "gcloud projects add-iam-policy-binding {project_id} \
                 --member=user:{email} --role={role} --condition=None"
            )
        })
        .collect()
}

#[derive(Deserialize)]
struct Policy {
    #[serde(default)]
    bindings: Vec<Binding>,
}

#[derive(Deserialize)]
struct Binding {
    role: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    condition: Option<serde_yaml::Value>,
}

fn read_policy(project_id: &str) -> Result<Policy, String> {
    let text = run_gcloud(&[
        "projects",
        "get-iam-policy",
        project_id,
        "--format=json",
        "--quiet",
    ])?;
    serde_yaml::from_str(&text).map_err(|_| "gcloud returned an unreadable policy".to_string())
}

fn active_account() -> Result<String, String> {
    let account = run_gcloud(&["config", "get-value", "account", "--quiet"])?;
    let account = account.trim().to_ascii_lowercase();
    valid_email(&account)
        .then_some(account)
        .ok_or_else(|| "gcloud has no active account".to_string())
}

/// `MAGICIAN_GCLOUD_BIN`, then `PATH`, then the usual install locations: a
/// service started by launchd often lacks Homebrew's bin directory.
fn gcloud_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MAGICIAN_GCLOUD_BIN").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("gcloud"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let known = [
        Some(PathBuf::from("/opt/homebrew/bin/gcloud")),
        Some(PathBuf::from("/usr/local/bin/gcloud")),
        Some(PathBuf::from("/usr/bin/gcloud")),
        home.map(|home| home.join("google-cloud-sdk/bin/gcloud")),
    ];
    from_path
        .chain(known.into_iter().flatten())
        .find(|path| path.is_file())
}

fn run_gcloud(args: &[&str]) -> Result<String, String> {
    let binary = gcloud_binary().ok_or_else(|| "gcloud is not installed".to_string())?;
    let mut command = Command::new(binary);
    command
        .args(args)
        // The host's own gcloud login, never a Workspace profile's.
        .env_remove("CLOUDSDK_CONFIG")
        .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run gcloud: {error}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < GCLOUD_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(100));
            },
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("gcloud timed out".to_string());
            },
            Err(error) => return Err(format!("gcloud failed: {error}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    let reason = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ERROR:"))
        .or_else(|| stderr.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("gcloud failed")
        .chars()
        .take(300)
        .collect();
    Err(reason)
}

fn valid_project_id(id: &str) -> bool {
    (6..=30).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && email.len() <= 254
        && email
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+-'".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_comes_from_either_client_shape() {
        let dir = std::env::temp_dir().join(format!("gpa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let installed = dir.join("installed.json");
        std::fs::write(
            &installed,
            r#"{"installed":{"client_id":"x","project_id":"magician-gws"}}"#,
        )
        .unwrap();
        assert_eq!(
            project_id_from_client_secret(&installed).unwrap(),
            "magician-gws"
        );
        let web = dir.join("web.json");
        std::fs::write(&web, r#"{"web":{"project_id":"other-proj-1"}}"#).unwrap();
        assert_eq!(project_id_from_client_secret(&web).unwrap(), "other-proj-1");
        let none = dir.join("none.json");
        std::fs::write(&none, r#"{"installed":{"client_id":"x"}}"#).unwrap();
        assert!(project_id_from_client_secret(&none).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn operator_accounts_include_fixed_profiles_and_skip_missing_emails() {
        let dir = std::env::temp_dir().join(format!("gpa-op-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("operator-config.yaml");
        std::fs::write(
            &path,
            "gws_accounts:\n  - name: work\n    expected_email: Me@Example.com\n  - name: bare\n\
             gws_fixed_profiles:\n  - name: presto\n    expected_email: bot@gmail.com\n\
             other: 1\n",
        )
        .unwrap();
        assert_eq!(
            operator_accounts(&path).unwrap(),
            vec![
                ("work".to_string(), "me@example.com".to_string()),
                ("presto".to_string(), "bot@gmail.com".to_string()),
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_inputs_never_reach_gcloud() {
        let report = check("magician-gws", "not-an-email");
        assert_eq!(report.status, AccessStatus::Unknown);
        assert!(report.commands.is_empty());
        let report = check("Bad Project; rm", "a@b.com");
        assert_eq!(report.status, AccessStatus::Unknown);
    }

    fn policy(json: &str) -> Policy {
        serde_yaml::from_str(json).unwrap()
    }

    #[test]
    fn missing_roles_reads_users_domains_and_basic_roles() {
        let live = policy(
            r#"{"bindings":[
              {"role":"roles/owner","members":["user:owner@gmail.com"]},
              {"role":"roles/pubsub.editor","members":["user:Bot@Gmail.com"]},
              {"role":"roles/serviceusage.serviceUsageConsumer","members":["domain:corp.com"]},
              {"role":"roles/pubsub.editor","members":["user:cond@gmail.com"],
               "condition":{"title":"t","expression":"true"}}
            ],"etag":"x"}"#,
        );
        assert!(missing_roles(&live, "owner@gmail.com").is_empty());
        assert_eq!(
            missing_roles(&live, "bot@gmail.com"),
            vec!["roles/serviceusage.serviceUsageConsumer"]
        );
        assert_eq!(
            missing_roles(&live, "x@corp.com"),
            vec!["roles/pubsub.editor"]
        );
        assert_eq!(missing_roles(&live, "cond@gmail.com").len(), 2);
        assert!(can_set_policy(&live, "owner@gmail.com"));
        assert!(!can_set_policy(&live, "bot@gmail.com"));
    }

    #[test]
    fn grant_commands_name_each_role() {
        let commands = grant_commands("magician-gws", "a@b.com", &REQUIRED_ROLES);
        assert_eq!(commands.len(), 2);
        assert!(commands[0].contains("--member=user:a@b.com"));
        assert!(commands[1].contains("--role=roles/pubsub.editor"));
    }
}
