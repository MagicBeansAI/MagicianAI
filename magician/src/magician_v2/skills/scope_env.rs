//! Per-scope values in a skill's runtime contract.
//!
//! A remote MCP skill whose server differs per workspace (each workspace runs
//! its own Metabase, say) names the origin as a reference instead of a
//! literal, so one `SKILL.md`, linked into every scope, serves them all:
//!
//! ```yaml
//! transport:
//!   kind: streamable_http
//!   endpoint: ${METABASE_BASE_URL}/api/metabase-mcp
//! discovery:
//!   oauth:
//!     authorization_issuer: ${METABASE_BASE_URL}
//! ```
//!
//! Rules:
//!
//! - Only `runtime.transport.endpoint` (Streamable HTTP),
//!   `runtime.discovery.endpoint_aliases.*` and
//!   `runtime.discovery.oauth.authorization_issuer` take a reference, and only
//!   as the whole origin: `${NAME}` alone or followed by a `/path`. A `${`
//!   anywhere else in the contract fails the skill.
//! - The value comes only from the scope's own
//!   `<scope>/skills/<skill>/config/.env`, read literally (no interpolation),
//!   and must be an https origin: `https://host[:port]`, no path, query,
//!   credentials or fragment. Never the server's environment, never the
//!   symlink-resolved or container source path, never a skillshub or extras
//!   root: a reference resolves only in
//!   `…/scopes/<principal>/<workspace>/skills/<skill>`, and the `.env` it
//!   reads must stay inside that skills root (the usual
//!   `config -> ../.skill-state/<skill>/config` link does).
//! - A missing or invalid value withdraws the skill from that scope with a
//!   reason naming the variable ([`ScopeEnvError`]); the placeholder never
//!   reaches a catalog, a Connect flow or a run, and neither does another
//!   scope's value.
//! - Every consumer that reads those URL fields parses through
//!   [`parse_runtime_package`]: the capability build (whose package the
//!   governed MCP run executes), Connect/status and the Skills page. Connect
//!   and the run therefore derive the same OAuth binding, and the MagicRun
//!   validator only ever sees the resolved contract. An authoring source
//!   (skillshub, the catalog's template rows) parses with
//!   [`ScopeEnvSource::Template`], which checks the reference shape and
//!   stands in [`TEMPLATE_ORIGIN`]; such a contract is never executed.
//! - Caches keyed on a scope's skill sources fold in [`scope_env_stamp`], so
//!   a changed value rebuilds the skill.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

use tool_runtime_core::{
    manifest::{McpTransport, RuntimeProtocol, SkillRuntimeContract},
    manifest_parser::{parse_skill_runtime_package, ManifestParseError, SkillRuntimePackage},
    manifest_validation::validate_skill_runtime_contract,
};

/// The scope's per-skill env file, relative to the skill folder.
pub const SCOPE_ENV_FILE: &str = "config/.env";
/// Stand-in origin for an authoring source (RFC 2606 reserved): the rest of
/// the contract is validated around it, and nothing ever connects to it.
pub const TEMPLATE_ORIGIN: &str = "https://scope-env.invalid";
/// A skill `.env` is a handful of lines; anything larger is not read.
const MAX_SCOPE_ENV_BYTES: u64 = 64 * 1024;
const MAX_VARIABLE_BYTES: usize = 128;

/// Where a contract's references resolve.
#[derive(Debug, Clone, Copy)]
pub enum ScopeEnvSource<'a> {
    /// The skill folder as listed in its scope (`<scope>/skills/<skill>`),
    /// never its symlink-resolved source.
    Skill(&'a Path),
    /// An authoring source: shape-checked, [`TEMPLATE_ORIGIN`] stands in.
    Template,
}

/// Why a contract's scope references did not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeEnvError {
    /// A `${` outside the fields and shape that take one: a skill defect.
    Misplaced { field: String },
    /// The folder is not a workspace's installed skill (skillshub, an extras
    /// root, a fixture), so it has no scope env to read.
    NotAScope { variable: String },
    Unset { variable: String },
    Invalid { variable: String },
    /// The `.env` is unreadable, oversized, or leaves the scope's skills root.
    Unavailable { variable: String },
    /// The resolved contract fails the MagicRun validator.
    Contract { message: String },
}

impl ScopeEnvError {
    /// The scope's configuration withdrew the skill (an unset or invalid
    /// value, or no scope env at all), as opposed to a defect in the skill.
    /// A withdrawal is a catalog change, not a load failure to hold back. An
    /// unreadable `.env` is not one: a transient read error must not drop a
    /// served skill, so it is held back like any load failure.
    pub fn withdraws_skill(&self) -> bool {
        matches!(
            self,
            Self::NotAScope { .. } | Self::Unset { .. } | Self::Invalid { .. }
        )
    }

    fn stamp_tag(&self) -> &'static str {
        match self {
            Self::Misplaced { .. } => "misplaced",
            Self::NotAScope { .. } => "not_a_scope",
            Self::Unset { .. } => "unset",
            Self::Invalid { .. } => "invalid",
            Self::Unavailable { .. } => "unavailable",
            Self::Contract { .. } => "contract",
        }
    }
}

impl fmt::Display for ScopeEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Misplaced { field } => write!(
                f,
                "{field}: a scope env reference must be the whole origin (`${{NAME}}` or \
                 `${{NAME}}/path`) of a runtime URL field that takes one"
            ),
            Self::NotAScope { variable } => write!(
                f,
                "{variable} resolves only in a workspace's installed skill \
                 (<scope>/skills/<skill>/{SCOPE_ENV_FILE}); this copy is not one"
            ),
            Self::Unset { variable } => write!(
                f,
                "set {variable} to this workspace's https origin in the skill's \
                 {SCOPE_ENV_FILE} to enable it"
            ),
            Self::Invalid { variable } => write!(
                f,
                "{variable} in this workspace's skill {SCOPE_ENV_FILE} is not an https origin \
                 (https://host[:port], no path, query, credentials or fragment)"
            ),
            Self::Unavailable { variable } => write!(
                f,
                "{variable}: this workspace's skill {SCOPE_ENV_FILE} is unreadable or leaves \
                 the workspace's skills folder"
            ),
            Self::Contract { message } => {
                write!(f, "resolved runtime contract is invalid: {message}")
            },
        }
    }
}

impl std::error::Error for ScopeEnvError {}

/// [`parse_runtime_package`]'s failure: the package itself, or its scope
/// references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedPackageError {
    Parse(ManifestParseError),
    ScopeEnv(ScopeEnvError),
}

impl ScopedPackageError {
    pub fn withdraws_skill(&self) -> bool {
        matches!(self, Self::ScopeEnv(error) if error.withdraws_skill())
    }
}

impl fmt::Display for ScopedPackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "failed to parse governed runtime package: {error}"),
            Self::ScopeEnv(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ScopedPackageError {}

/// Parse a `SKILL.md`'s governed runtime package with its scope references
/// resolved: the one parse every consumer of the contract's URL fields uses.
/// A source without `${` parses exactly as
/// [`parse_skill_runtime_package`] does.
pub fn parse_runtime_package(
    source: &str,
    scope_env: ScopeEnvSource<'_>,
) -> Result<Option<SkillRuntimePackage>, ScopedPackageError> {
    let Some(mut package) =
        parse_skill_runtime_package(source).map_err(ScopedPackageError::Parse)?
    else {
        return Ok(None);
    };
    if source.contains("${") {
        resolve_package(&mut package, scope_env).map_err(ScopedPackageError::ScopeEnv)?;
    }
    Ok(Some(package))
}

/// Resolve `package`'s scope references in place and return the variables
/// it referenced. A package that referenced any is validated again, so the
/// MagicRun endpoint and OAuth URL checks run on the resolved values.
pub fn resolve_package(
    package: &mut SkillRuntimePackage,
    scope_env: ScopeEnvSource<'_>,
) -> Result<BTreeSet<String>, ScopeEnvError> {
    let variables = match scope_env {
        ScopeEnvSource::Template => {
            substitute(&mut package.contract, |_| Ok(TEMPLATE_ORIGIN.to_owned()))?
        },
        ScopeEnvSource::Skill(skill_dir) => {
            let mut values = None;
            substitute(&mut package.contract, |variable| {
                origin_from(
                    values.get_or_insert_with(|| scope_env_values(skill_dir)),
                    variable,
                )
            })?
        },
    };
    if !variables.is_empty() {
        validate_skill_runtime_contract(&package.contract).map_err(|error| {
            ScopeEnvError::Contract {
                message: error.to_string(),
            }
        })?;
    }
    Ok(variables)
}

/// What the scope env contributes to a skill's catalog entry: `None` when
/// the manifest's frontmatter references nothing, else a digest of how each
/// referenced variable resolves in `skill_dir` (its origin, or why not).
/// Source fingerprints and catalog revisions fold this in so a changed,
/// blanked or invalid value rebuilds the skill.
pub fn scope_env_stamp(skill_md: &[u8], skill_dir: &Path) -> Option<blake3::Hash> {
    let variables = referenced_variables(frontmatter(skill_md)?);
    if variables.is_empty() {
        return None;
    }
    let values = scope_env_values(skill_dir);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"scope-env-stamp.v1\0");
    for variable in &variables {
        hasher.update(variable.as_bytes());
        hasher.update(&[0]);
        match origin_from(&values, variable) {
            Ok(origin) => {
                hasher.update(b"ok:");
                hasher.update(origin.as_bytes());
            },
            Err(error) => {
                hasher.update(b"err:");
                hasher.update(error.stamp_tag().as_bytes());
            },
        }
        hasher.update(&[0]);
    }
    Some(hasher.finalize())
}

/// `value` as a normalized https origin (`https://host[:port]`, lower-case
/// host), or `None`. The same rule setup's origin inputs used: no path,
/// query, credentials or fragment; a trailing `/` is dropped.
pub fn https_origin(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    let (scheme, authority) = value.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        && !host.starts_with(['.', '-'])
        && !host.contains("..");
    if !host_ok {
        return None;
    }
    let port = match port {
        None => String::new(),
        Some(port)
            if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            match port.parse::<u16>() {
                Ok(0) | Err(_) => return None,
                // `url` (and so the runtime) drops the default port.
                Ok(443) => String::new(),
                Ok(port) => format!(":{port}"),
            }
        },
        Some(_) => return None,
    };
    let origin = format!("https://{}{port}", host.to_ascii_lowercase());
    // The runtime validates with `url`; an origin it would rewrite (a
    // numeric host read as IPv4, say) is not the one the operator wrote.
    let reparsed = url::Url::parse(&origin).ok()?;
    (reparsed.as_str().trim_end_matches('/') == origin).then_some(origin)
}

/// The URL fields that may hold a reference, labelled for diagnostics.
fn reference_fields(contract: &mut SkillRuntimeContract) -> Vec<(String, &mut String)> {
    let mut fields = Vec::new();
    if let RuntimeProtocol::Mcp {
        transport,
        discovery,
        ..
    } = &mut contract.runtime
    {
        if let McpTransport::StreamableHttp { endpoint } = transport {
            fields.push(("runtime.transport.endpoint".to_owned(), endpoint));
        }
        for (alias, endpoint) in discovery.endpoint_aliases.iter_mut() {
            fields.push((
                format!("runtime.discovery.endpoint_aliases.{alias}"),
                endpoint,
            ));
        }
        if let Some(oauth) = discovery.oauth.as_mut() {
            fields.push((
                "runtime.discovery.oauth.authorization_issuer".to_owned(),
                &mut oauth.authorization_issuer,
            ));
        }
    }
    fields
}

/// `(variable, rest)` for a reference, `None` for a value without `${`.
fn reference(field: &str, value: &str) -> Result<Option<(String, String)>, ScopeEnvError> {
    if !value.contains("${") {
        return Ok(None);
    }
    let misplaced = || ScopeEnvError::Misplaced {
        field: field.to_owned(),
    };
    let (name, rest) = value
        .strip_prefix("${")
        .and_then(|body| body.split_once('}'))
        .ok_or_else(misplaced)?;
    if !is_variable_name(name) || rest.contains("${") || !(rest.is_empty() || rest.starts_with('/'))
    {
        return Err(misplaced());
    }
    Ok(Some((name.to_owned(), rest.to_owned())))
}

fn substitute(
    contract: &mut SkillRuntimeContract,
    mut origin_of: impl FnMut(&str) -> Result<String, ScopeEnvError>,
) -> Result<BTreeSet<String>, ScopeEnvError> {
    let mut variables = BTreeSet::new();
    for (field, value) in reference_fields(contract) {
        let Some((variable, rest)) = reference(&field, value.as_str())? else {
            continue;
        };
        *value = format!("{}{rest}", origin_of(&variable)?);
        variables.insert(variable);
    }
    // In a contract that uses references, one anywhere else would never be
    // expanded, so it must not be there at all; a contract that cannot be
    // checked fails the same way. A contract without references keeps any
    // literal `${` it had before references existed.
    let leftover = !variables.is_empty()
        && serde_json::to_string(&*contract).map_or(true, |text| text.contains("${"));
    if leftover {
        return Err(ScopeEnvError::Misplaced {
            field: "runtime_contract".to_owned(),
        });
    }
    Ok(variables)
}

fn is_variable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= MAX_VARIABLE_BYTES
        && bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Why a skill folder has no scope env to read at all.
#[derive(Debug, Clone, Copy)]
enum ScopeEnvLookup {
    NotAScope,
    Unavailable,
}

type ScopeEnvValues = Result<HashMap<String, String>, ScopeEnvLookup>;

fn origin_from(values: &ScopeEnvValues, variable: &str) -> Result<String, ScopeEnvError> {
    let variable = variable.to_owned();
    let values = match values {
        Ok(values) => values,
        Err(ScopeEnvLookup::NotAScope) => return Err(ScopeEnvError::NotAScope { variable }),
        Err(ScopeEnvLookup::Unavailable) => return Err(ScopeEnvError::Unavailable { variable }),
    };
    match values.get(&variable).map(|value| value.trim()) {
        None | Some("") => Err(ScopeEnvError::Unset { variable }),
        Some(value) => https_origin(value).ok_or(ScopeEnvError::Invalid { variable }),
    }
}

/// `…/scopes/<principal>/<workspace>/skills/<skill>`: the only folder shape
/// whose `config/.env` belongs to exactly one workspace.
fn is_scope_skill_dir(skill_dir: &Path) -> bool {
    let Some(skills) = skill_dir.parent() else {
        return false;
    };
    let scopes = skills
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent);
    skill_dir.file_name().is_some()
        && skills.file_name().is_some_and(|name| name == "skills")
        && scopes
            .and_then(Path::file_name)
            .is_some_and(|name| name == "scopes")
}

/// The scope env of one installed skill, read literally. A missing file (or
/// a dangling `config` link) is an empty env; a file that resolves outside
/// the scope's skills root is refused rather than read.
fn scope_env_values(skill_dir: &Path) -> ScopeEnvValues {
    if !is_scope_skill_dir(skill_dir) {
        return Err(ScopeEnvLookup::NotAScope);
    }
    let path = skill_dir.join(SCOPE_ENV_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(_) => return Err(ScopeEnvLookup::Unavailable),
    }
    let contained = match (
        std::fs::canonicalize(&path),
        skill_dir.parent().map(std::fs::canonicalize),
    ) {
        (Ok(file), Some(Ok(skills_root))) => file.starts_with(&skills_root),
        // A dangling `config` link: nothing configured.
        (Err(error), _) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HashMap::new())
        },
        _ => false,
    };
    if !contained {
        return Err(ScopeEnvLookup::Unavailable);
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_SCOPE_ENV_BYTES => {},
        _ => return Err(ScopeEnvLookup::Unavailable),
    }
    std::fs::read_to_string(&path)
        .map(|text| parse_env(&text))
        .map_err(|_| ScopeEnvLookup::Unavailable)
}

/// `KEY=value` lines; `#` comments, an optional `export `, and one pair of
/// matching outer quotes. No `$` interpolation: a value is taken as written.
fn parse_env(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric()) {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        values.insert(key.to_owned(), value.to_owned());
    }
    values
}

/// The YAML between a leading `---` line and the next `---` line.
fn frontmatter(skill_md: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(skill_md).ok()?;
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---").unwrap_or(rest.len());
    Some(&rest[..end])
}

fn referenced_variables(text: &str) -> BTreeSet<String> {
    let mut variables = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        rest = &rest[start + 2..];
        if let Some((name, _)) = rest.split_once('}') {
            if is_variable_name(name) {
                variables.insert(name.to_owned());
            }
        }
    }
    variables
}

#[cfg(all(unix, any(test, feature = "test-fixtures")))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// The shipped skill: one manifest, linked into every scope.
    const SKILL: &str = include_str!("../../../../skillshub/metabase-mcp/SKILL.md");

    /// `<root>/scopes/<p>/<w>/skills/metabase-mcp` in the installed layout:
    /// `config` links to `../.skill-state/metabase-mcp/config`.
    fn scope_skill(root: &Path, scope: &str, env: Option<&str>) -> PathBuf {
        let skills = root.join("scopes").join(scope).join("skills");
        let skill = skills.join("metabase-mcp");
        let state = skills.join(".skill-state/metabase-mcp/config");
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(skill.join("SKILL.md"), SKILL).unwrap();
        std::os::unix::fs::symlink("../.skill-state/metabase-mcp/config", skill.join("config"))
            .unwrap();
        if let Some(env) = env {
            fs::write(state.join(".env"), env).unwrap();
        }
        skill
    }

    fn endpoints(package: &SkillRuntimePackage) -> (String, String) {
        let RuntimeProtocol::Mcp {
            transport: McpTransport::StreamableHttp { endpoint },
            discovery,
            ..
        } = &package.contract.runtime
        else {
            panic!("remote MCP package");
        };
        (
            endpoint.clone(),
            discovery.oauth.as_ref().unwrap().authorization_issuer.clone(),
        )
    }

    fn resolve(skill: &Path) -> Result<SkillRuntimePackage, ScopedPackageError> {
        parse_runtime_package(SKILL, ScopeEnvSource::Skill(skill)).map(Option::unwrap)
    }

    #[test]
    fn a_reference_resolves_from_the_scopes_own_env() {
        let root = tempfile::tempdir().unwrap();
        let skill = scope_skill(
            root.path(),
            "alice/work",
            Some("# Metabase\nMETABASE_BASE_URL=\"https://Metabase.Alice.example/\"\n"),
        );
        let package = resolve(&skill).expect("resolves");
        assert_eq!(
            endpoints(&package),
            (
                "https://metabase.alice.example/api/metabase-mcp".to_owned(),
                "https://metabase.alice.example".to_owned()
            )
        );
        validate_skill_runtime_contract(&package.contract).expect("validated resolved");
    }

    #[test]
    fn two_scopes_resolve_independently() {
        let root = tempfile::tempdir().unwrap();
        let alice = scope_skill(
            root.path(),
            "alice/work",
            Some("METABASE_BASE_URL=https://alice.example\n"),
        );
        let bob = scope_skill(
            root.path(),
            "bob/work",
            Some("METABASE_BASE_URL=https://bob.example:8443\n"),
        );
        let carol = scope_skill(root.path(), "carol/work", None);
        assert_eq!(
            endpoints(&resolve(&alice).unwrap()).0,
            "https://alice.example/api/metabase-mcp"
        );
        assert_eq!(
            endpoints(&resolve(&bob).unwrap()).0,
            "https://bob.example:8443/api/metabase-mcp"
        );
        // Neither neighbour's value leaks into a scope that set none.
        let error = resolve(&carol).unwrap_err();
        assert_eq!(
            error,
            ScopedPackageError::ScopeEnv(ScopeEnvError::Unset {
                variable: "METABASE_BASE_URL".to_owned()
            })
        );
        assert!(error.withdraws_skill());
        assert!(error.to_string().contains("METABASE_BASE_URL"), "{error}");
    }

    #[test]
    fn invalid_values_are_refused_and_named() {
        for value in [
            "http://metabase.example",
            "https://metabase.example/api",
            "https://metabase.example?x=1",
            "https://user:pw@metabase.example",
            "https://metabase.example#frag",
            "https://${OTHER}",
            "https://1234",
            "https://metabase.example:0",
            "https://metabase.example:99999",
            "metabase.example",
            "https://",
        ] {
            let root = tempfile::tempdir().unwrap();
            let skill = scope_skill(
                root.path(),
                "alice/work",
                Some(&format!("METABASE_BASE_URL={value}\n")),
            );
            let error = resolve(&skill).unwrap_err();
            assert_eq!(
                error,
                ScopedPackageError::ScopeEnv(ScopeEnvError::Invalid {
                    variable: "METABASE_BASE_URL".to_owned()
                }),
                "{value}"
            );
            assert!(error.withdraws_skill());
        }
    }

    /// The value never comes from the server's environment, a skillshub or
    /// extras copy, or a `.env` that resolves into another workspace.
    #[test]
    fn non_scope_sources_are_refused() {
        let root = tempfile::tempdir().unwrap();
        // The process environment is not a source.
        std::env::set_var("METABASE_BASE_URL", "https://server-env.example");
        let bare = scope_skill(root.path(), "alice/work", None);
        assert!(matches!(
            resolve(&bare).unwrap_err(),
            ScopedPackageError::ScopeEnv(ScopeEnvError::Unset { .. })
        ));
        std::env::remove_var("METABASE_BASE_URL");

        // A skillshub-shaped folder with its own .env is not a scope.
        let hub = root.path().join("skillshub/metabase-mcp");
        fs::create_dir_all(hub.join("config")).unwrap();
        fs::write(
            hub.join("config/.env"),
            "METABASE_BASE_URL=https://hub.example\n",
        )
        .unwrap();
        assert!(matches!(
            resolve(&hub).unwrap_err(),
            ScopedPackageError::ScopeEnv(ScopeEnvError::NotAScope { .. })
        ));

        // A config link into another workspace is refused, not read.
        let other = scope_skill(
            root.path(),
            "bob/work",
            Some("METABASE_BASE_URL=https://bob.example\n"),
        );
        let alice = root.path().join("scopes/alice/work/skills/metabase-mcp");
        fs::remove_file(alice.join("config")).unwrap();
        std::os::unix::fs::symlink(other.join("config"), alice.join("config")).unwrap();
        let error = resolve(&alice).unwrap_err();
        assert!(matches!(
            error,
            ScopedPackageError::ScopeEnv(ScopeEnvError::Unavailable { .. })
        ));
        // Held back like a load failure, never a withdrawal.
        assert!(!error.withdraws_skill());
    }

    #[test]
    fn a_reference_outside_the_url_fields_or_origin_position_fails_the_skill() {
        let root = tempfile::tempdir().unwrap();
        let skill = scope_skill(
            root.path(),
            "alice/work",
            Some("METABASE_BASE_URL=https://alice.example\n"),
        );
        for source in [
            SKILL.replace(
                "${METABASE_BASE_URL}/api/metabase-mcp",
                "https://alice.example/${METABASE_BASE_URL}",
            ),
            SKILL.replace("${METABASE_BASE_URL}/api", "${METABASE_BASE_URL}.evil/api"),
            SKILL.replace("${METABASE_BASE_URL}/api", "${metabase_base_url}/api"),
            SKILL.replace("namespace: metabase", "namespace: ${METABASE_BASE_URL}"),
        ] {
            let error = parse_runtime_package(&source, ScopeEnvSource::Skill(&skill)).unwrap_err();
            assert!(
                matches!(error, ScopedPackageError::ScopeEnv(ScopeEnvError::Misplaced { .. })),
                "{error}"
            );
            assert!(!error.withdraws_skill(), "a malformed skill is a defect");
        }
    }

    #[test]
    fn a_template_checks_the_shape_and_validates_around_a_stand_in() {
        let package = parse_runtime_package(SKILL, ScopeEnvSource::Template)
            .unwrap()
            .unwrap();
        assert_eq!(
            endpoints(&package),
            (
                format!("{TEMPLATE_ORIGIN}/api/metabase-mcp"),
                TEMPLATE_ORIGIN.to_owned()
            )
        );
        let malformed = SKILL.replace("${METABASE_BASE_URL}/api", "${METABASE_BASE_URL}api");
        assert!(parse_runtime_package(&malformed, ScopeEnvSource::Template).is_err());
    }

    #[test]
    fn a_source_without_references_parses_as_before() {
        let literal = SKILL.replace("${METABASE_BASE_URL}", "https://fixed.example");
        let package = parse_runtime_package(&literal, ScopeEnvSource::Skill(Path::new("/nowhere")))
            .unwrap()
            .unwrap();
        assert_eq!(
            package,
            parse_skill_runtime_package(&literal).unwrap().unwrap()
        );
    }

    #[test]
    fn the_stamp_follows_the_resolved_value() {
        let root = tempfile::tempdir().unwrap();
        let skill = scope_skill(
            root.path(),
            "alice/work",
            Some("METABASE_BASE_URL=https://one.example\n"),
        );
        let env = root
            .path()
            .join("scopes/alice/work/skills/.skill-state/metabase-mcp/config/.env");
        let stamp = || scope_env_stamp(SKILL.as_bytes(), &skill).expect("referenced");
        let first = stamp();
        assert_eq!(first, stamp(), "stable while unchanged");
        // An unrelated key does not move it; the referenced value does.
        fs::write(&env, "METABASE_BASE_URL=https://one.example\nOTHER=x\n").unwrap();
        assert_eq!(first, stamp());
        fs::write(&env, "METABASE_BASE_URL=https://two.example\n").unwrap();
        let second = stamp();
        assert_ne!(first, second);
        fs::write(&env, "METABASE_BASE_URL=\n").unwrap();
        assert_ne!(second, stamp(), "blanking it moves the stamp too");
        assert_eq!(
            scope_env_stamp(b"---\nname: plain\n---\n${NOT_FRONTMATTER}\n", &skill),
            None
        );
    }
}
