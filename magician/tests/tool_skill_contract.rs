//! Tier 1 hermetic contract tests for every governed tool skill in `skillshub/`.
//!
//! These are package-shape assertions, not live provider calls: nothing here
//! opens a socket, resolves a credential value, or executes an adapter. They
//! exist because the tool runtime was rewritten and a third of the tool surface
//! broke without a single test going red — the manifests still parsed, the
//! adapters still had unit tests, and the things that actually failed were the
//! joins between them: an adapter reaching for a parameter its manifest no
//! longer sends, a governed kill that fires before the work it backs, a script
//! driving an execution model production no longer has.
//!
//! Every test below loops the whole surface and collects every failure before
//! asserting. Returning on the first bad skill hides the other twenty, which is
//! precisely how a third of the surface went dark unnoticed.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

use serde::Deserialize;
use tool_runtime_core::{
    action_overrides::{
        compile_typed_action_overrides, lower_typed_action_invocation, TypedActionInputDelivery,
        TypedActionInvocationErrorCode, TypedActionParameter, TYPED_ACTION_OVERRIDES_V1,
        TYPED_ACTION_OVERRIDES_V2,
    },
    canary::SkillRuntimeCanary,
    credential_injection::{ChildEnvironmentBaseline, ChildEnvironmentVariable},
    manifest::{
        AuthKind, ProfileSelection, RuntimeLimits, RuntimeProtocol, SkillRuntimeContract,
        SKILL_RUNTIME_CONTRACT_V1,
    },
    manifest_parser::{parse_skill_runtime_package, SkillRuntimePackage},
    manifest_validation::validate_skill_runtime_contract,
};

fn workspace_root() -> &'static Path {
    // tests/ runs with CARGO_MANIFEST_DIR = magician/ ; skillshub/
    // lives at the monorepo root beside it.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician/ has a parent")
}

fn skillshub_dir() -> PathBuf {
    workspace_root().join("skillshub")
}

fn workspace_vendored_bins() -> BTreeSet<String> {
    let lock_path = skillshub_dir().join("package-lock.json");
    let lock: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&lock_path).expect("read skillshub package-lock.json"),
    )
    .expect("parse skillshub package-lock.json");
    let packages = lock
        .get("packages")
        .and_then(serde_json::Value::as_object)
        .expect("skillshub package-lock.json contains packages");
    let mut bins = BTreeSet::new();
    for (package_path, package) in packages {
        let Some(relative) = package_path.strip_prefix("node_modules/") else {
            continue;
        };
        // Nested dependency bins are not linked into the workspace-root .bin.
        if relative.contains("/node_modules/") {
            continue;
        }
        let Some(bin) = package.get("bin") else {
            continue;
        };
        if let Some(entries) = bin.as_object() {
            bins.extend(entries.keys().cloned());
        } else if bin.is_string() {
            bins.insert(relative.rsplit('/').next().unwrap_or(relative).to_owned());
        }
    }

    // Workspace-owned Rust CLIs are vendored source just as surely as npm
    // packages are. Their setup target may leave a generated symlink under a
    // skill's bin/ directory, but a clean checkout does not need the compiled
    // target to exist for this package-shape contract to recognize ownership.
    for entry in fs::read_dir(workspace_root()).expect("read workspace root") {
        let Ok(entry) = entry else { continue };
        let manifest = entry.path().join("Cargo.toml");
        let Ok(source) = fs::read_to_string(manifest) else {
            continue;
        };
        let mut in_bin = false;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed == "[[bin]]" {
                in_bin = true;
                continue;
            }
            if trimmed.starts_with('[') {
                in_bin = false;
                continue;
            }
            if in_bin {
                if let Some(name) = trimmed
                    .strip_prefix("name = \"")
                    .and_then(|value| value.strip_suffix('"'))
                {
                    bins.insert(name.to_owned());
                    in_bin = false;
                }
            }
        }
    }
    bins
}

/// One discovered governed tool skill.
///
/// `package` holds the parse result rather than a parsed package so that a
/// manifest that no longer compiles is reported by
/// [`every_tool_skill_manifest_parses`] instead of panicking inside discovery
/// and taking every other assertion down with it.
struct ToolSkill {
    name: String,
    dir: PathBuf,
    frontmatter: serde_yaml::Value,
    package: Result<SkillRuntimePackage, String>,
}

impl ToolSkill {
    fn contract(&self) -> Option<&SkillRuntimeContract> {
        self.package.as_ref().ok().map(|package| &package.contract)
    }

    /// One `metadata.magician.<key>` node, read straight off the frontmatter.
    ///
    /// The validated contract deliberately does not carry install hints or
    /// canary declarations, so those live here rather than being smuggled into
    /// the runtime vocabulary.
    fn magician_extension(&self, key: &str) -> Option<&serde_yaml::Value> {
        self.frontmatter.get("metadata")?.get("magician")?.get(key)
    }

    /// Every regular file the package ships under `bin/`.
    fn shipped_bin_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = fs::read_dir(self.dir.join("bin")) else {
            return files;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                files.push(entry.path());
            }
        }
        files.sort();
        files
    }
}

fn frontmatter_source(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(&rest[..end])
}

/// Every skill whose `SKILL.md` declares `metadata.magician.runtime_contract`.
///
/// That declaration is what makes a skill a *tool* skill: procedure skills and
/// personality modes carry prose only and have nothing here to contract over.
fn tool_skills() -> &'static [ToolSkill] {
    static SKILLS: OnceLock<Vec<ToolSkill>> = OnceLock::new();
    SKILLS.get_or_init(|| {
        let root = skillshub_dir();
        let mut names = fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        names.sort();

        let mut skills = Vec::new();
        for name in names {
            let dir = root.join(&name);
            let manifest = dir.join("SKILL.md");
            let Ok(source) = fs::read_to_string(&manifest) else {
                continue;
            };
            let Some(raw) = frontmatter_source(&source) else {
                continue;
            };
            let frontmatter: serde_yaml::Value = match serde_yaml::from_str(raw) {
                Ok(value) => value,
                // An unreadable frontmatter cannot be classified as a tool
                // skill at all; the parser assertion below reports it through
                // the same path a semantic failure takes.
                Err(_) => serde_yaml::Value::Null,
            };
            let declares_contract = frontmatter
                .get("metadata")
                .and_then(|value| value.get("magician"))
                .and_then(|value| value.get("runtime_contract"))
                .is_some();
            if !declares_contract {
                continue;
            }
            let package = parse_skill_runtime_package(&source)
                .map_err(|error| error.to_string())
                .and_then(|package| {
                    package.ok_or_else(|| {
                        "frontmatter declares runtime_contract but no package parsed".to_owned()
                    })
                });
            skills.push(ToolSkill {
                name,
                dir,
                frontmatter,
                package,
            });
        }
        assert!(
            !skills.is_empty(),
            "skillshub/ must contain governed tool skills: {}",
            root.display()
        );
        skills
    })
}

fn runtime_limits(contract: &SkillRuntimeContract) -> &RuntimeLimits {
    match &contract.runtime {
        RuntimeProtocol::Cli { limits, .. } | RuntimeProtocol::Mcp { limits, .. } => limits,
    }
}

// --- Python adapter analysis --------------------------------------------------
//
// The adapters are Python and the assertions are Rust, so something has to read
// Python. A regex cannot: `grep -oE 'args\.[a-z_]+'` on the Tavily adapter
// returns nine names while the adapter reads fourteen, because its last five
// parameters are reached as `getattr(args, field)` inside `for field in (...)`
// loops. A scanner that reported 9/14 would silently pass an adapter that had
// dropped any of those five defaults — exactly the regression these assertions
// exist to catch. Python's own `ast` module is the only correct parser for
// Python, so the scan runs there and returns JSON. It is fed to `python3` on
// stdin rather than written to disk: no temporary file, no second source of
// truth in the repository, and the interpreter is the one on PATH.
//
// Handling only `getattr(args, "literal")` is not enough either — the second
// argument is the loop variable, an `ast.Name`. The scanner therefore carries an
// environment of enclosing `for <var> in (<literals>)` bindings and resolves the
// variable back to the tuple. The same resolution runs on the write side, where
// `value.setdefault(name, "")` inside such a loop is how one adapter guarantees
// four of its parameters.

const ADAPTER_SCANNER_PY: &str = r##"
import ast
import json
import sys


NAMESPACE_ANNOTATIONS = ("SimpleNamespace", "Namespace")
MISSING = object()


def annotation_name(node):
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return None


def literal(node):
    """Decode a bounded Python literal node without executing anything."""
    if isinstance(node, ast.Constant):
        if node.value is None or isinstance(node.value, (str, bool, int, float)):
            return node.value
        return MISSING
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        inner = literal(node.operand)
        if isinstance(inner, (int, float)) and not isinstance(inner, bool):
            return -inner if isinstance(node.op, ast.USub) else inner
        return MISSING
    if isinstance(node, (ast.Tuple, ast.List)):
        out = []
        for element in node.elts:
            value = literal(element)
            if value is MISSING:
                return MISSING
            out.append(value)
        return out
    if isinstance(node, ast.Dict):
        out = {}
        for key, item in zip(node.keys, node.values):
            if key is None:
                return MISSING
            name = literal(key)
            value = literal(item)
            if name is MISSING or value is MISSING or not isinstance(name, str):
                return MISSING
            out[name] = value
        return out
    return MISSING


def literal_strings(node):
    value = literal(node)
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return value
    return None


class Scanner:
    def __init__(self, tree):
        self.tree = tree
        self.namespaces = set()
        self.reads = set()
        self.renames = {}
        self.imports = set()
        self.constants = {}
        self.required = None
        self.defaults = None
        self.guaranteed = set()
        self.envelopes = set()

    def collect_envelopes(self):
        """Variables holding the decoded envelope that becomes the namespace.

        Only writes to THAT dict guarantee an attribute. Counting a write to
        any dict would let `body[field] = value` — the outgoing provider
        request, built in a loop over the very parameters in question — pose as
        a guarantee that the incoming parameter exists.
        """
        for node in ast.walk(self.tree):
            if not isinstance(node, ast.Call):
                continue
            if not (isinstance(node.func, ast.Name) and node.func.id == "SimpleNamespace"):
                continue
            for keyword in node.keywords:
                if keyword.arg is None and isinstance(keyword.value, ast.Name):
                    self.envelopes.add(keyword.value.id)

    def is_envelope(self, node):
        return isinstance(node, ast.Name) and node.id in self.envelopes

    def collect_namespaces(self):
        for node in ast.walk(self.tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                arguments = node.args
                every = (
                    list(arguments.posonlyargs)
                    + list(arguments.args)
                    + list(arguments.kwonlyargs)
                )
                for argument in every:
                    if annotation_name(argument.annotation) in NAMESPACE_ANNOTATIONS:
                        self.namespaces.add(argument.arg)
            if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call):
                function = node.value.func
                name = None
                if isinstance(function, ast.Name):
                    name = function.id
                elif isinstance(function, ast.Attribute):
                    name = function.attr
                if name in ("parse_input", "parse_args"):
                    for target in node.targets:
                        if isinstance(target, ast.Name):
                            self.namespaces.add(target.id)

    def collect_module_level(self):
        for node in self.tree.body:
            targets = []
            value = None
            if isinstance(node, ast.Assign):
                targets = [t for t in node.targets if isinstance(t, ast.Name)]
                value = node.value
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                targets = [node.target]
                value = node.value
            if value is None:
                continue
            decoded = literal(value)
            for target in targets:
                if target.id == "REQUIRED_PARAMETERS":
                    names = literal_strings(value)
                    if names is not None:
                        self.required = names
                elif target.id == "PARAMETER_DEFAULTS":
                    if isinstance(decoded, dict):
                        self.defaults = decoded
                elif isinstance(decoded, int) and not isinstance(decoded, bool):
                    self.constants[target.id] = decoded

    def collect_imports(self):
        for node in ast.walk(self.tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    self.imports.add(alias.name.split(".")[0])
            elif isinstance(node, ast.ImportFrom):
                if node.module and node.level == 0:
                    self.imports.add(node.module.split(".")[0])

    def collect_renames(self):
        for node in ast.walk(self.tree):
            if not isinstance(node, ast.Assign) or len(node.targets) != 1:
                continue
            target = node.targets[0]
            call = node.value
            if not isinstance(target, ast.Subscript) or not isinstance(call, ast.Call):
                continue
            if not self.is_envelope(target.value):
                continue
            if not (
                isinstance(target.slice, ast.Constant)
                and isinstance(target.slice.value, str)
            ):
                continue
            function = call.func
            if not (isinstance(function, ast.Attribute) and function.attr == "pop"):
                continue
            if not call.args:
                continue
            source = call.args[0]
            if isinstance(source, ast.Constant) and isinstance(source.value, str):
                self.renames[source.value] = target.slice.value

    def names_of(self, node, environment):
        """Resolve a string-valued expression to every name it can take.

        A literal resolves to itself; a bare identifier resolves through the
        enclosing `for <var> in (...)` tuple. That second case is the one both a
        regex and a literal-only AST check miss.
        """
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            return [node.value]
        if isinstance(node, ast.Name):
            return list(environment.get(node.id, ()))
        return []

    def collect_guaranteed(self, node, environment):
        """Names the adapter itself guarantees are present on the namespace."""
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Subscript) and self.is_envelope(target.value):
                for name in self.names_of(target.slice, environment):
                    self.guaranteed.add(name)
        if not isinstance(node, ast.Call):
            return
        function = node.func
        attribute = function.attr if isinstance(function, ast.Attribute) else None
        if attribute == "setdefault" and node.args and self.is_envelope(function.value):
            for name in self.names_of(node.args[0], environment):
                self.guaranteed.add(name)
        if attribute == "set_defaults":
            for keyword in node.keywords:
                if keyword.arg:
                    self.guaranteed.add(keyword.arg)
        if attribute in ("add_argument", "add_subparsers", "add_parser"):
            explicit = None
            for keyword in node.keywords:
                if keyword.arg == "dest" and isinstance(keyword.value, ast.Constant):
                    if isinstance(keyword.value.value, str):
                        explicit = keyword.value.value
            if explicit is not None:
                self.guaranteed.add(explicit)
                return
            for argument in node.args:
                if not (
                    isinstance(argument, ast.Constant)
                    and isinstance(argument.value, str)
                ):
                    continue
                flag = argument.value
                if flag.startswith("--"):
                    self.guaranteed.add(flag[2:].replace("-", "_"))
                    return
                if flag.startswith("-"):
                    continue
                self.guaranteed.add(flag.replace("-", "_"))
                return

    def visit(self, node, environment):
        self.collect_guaranteed(node, environment)
        if isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Load):
            if isinstance(node.value, ast.Name) and node.value.id in self.namespaces:
                self.reads.add(node.attr)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            if node.func.id == "getattr" and len(node.args) >= 2:
                subject = node.args[0]
                if isinstance(subject, ast.Name) and subject.id in self.namespaces:
                    for name in self.names_of(node.args[1], environment):
                        self.reads.add(name)
                        # A three-argument getattr carries its own fallback and
                        # cannot raise, so it is its own guarantee.
                        if len(node.args) >= 3:
                            self.guaranteed.add(name)
        if isinstance(node, (ast.For, ast.AsyncFor)):
            child = environment
            names = literal_strings(node.iter)
            if names is not None and isinstance(node.target, ast.Name):
                child = dict(environment)
                child[node.target.id] = names
            self.visit(node.iter, environment)
            for statement in node.body:
                self.visit(statement, child)
            for statement in node.orelse:
                self.visit(statement, environment)
            return
        for child in ast.iter_child_nodes(node):
            self.visit(child, environment)

    def run(self):
        self.collect_envelopes()
        self.collect_namespaces()
        self.collect_module_level()
        self.collect_imports()
        self.collect_renames()
        self.visit(self.tree, {})
        return {
            "python": True,
            "imports": sorted(self.imports),
            "namespace_reads": sorted(self.reads),
            "required_parameters": self.required,
            "parameter_defaults": self.defaults,
            "renames": self.renames,
            "guaranteed_names": sorted(self.guaranteed),
            "integer_constants": self.constants,
        }


def is_python(path, source):
    if path.endswith(".py"):
        return True
    return source.startswith("#!") and "python" in source.split("\n", 1)[0]


def scan(path):
    try:
        with open(path, "rb") as handle:
            raw = handle.read()
    except OSError as error:
        return {"error": "read failed: %s" % error}
    try:
        source = raw.decode("utf-8")
    except UnicodeDecodeError:
        return {"python": False}
    if not is_python(path, source):
        return {"python": False}
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError as error:
        return {"error": "python parse failed: %s" % error}
    return Scanner(tree).run()


def main(argv):
    sys.stdout.write(json.dumps({path: scan(path) for path in argv}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
"##;

#[derive(Debug, Deserialize)]
struct AdapterScan {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    python: bool,
    #[serde(default)]
    imports: BTreeSet<String>,
    /// Attribute names read off the parsed governed-input namespace.
    #[serde(default)]
    namespace_reads: BTreeSet<String>,
    /// `REQUIRED_PARAMETERS`, when the adapter mirrors its manifest.
    #[serde(default)]
    required_parameters: Option<BTreeSet<String>>,
    /// `PARAMETER_DEFAULTS`, when the adapter mirrors its manifest.
    #[serde(default)]
    parameter_defaults: Option<BTreeMap<String, serde_json::Value>>,
    /// Manifest parameter name -> adapter attribute name, recovered from the
    /// adapter's own `value[<new>] = value.pop(<old>, ...)` rename.
    #[serde(default)]
    renames: BTreeMap<String, String>,
    /// Names the adapter itself guarantees are present on the namespace.
    #[serde(default)]
    guaranteed_names: BTreeSet<String>,
    #[serde(default)]
    integer_constants: BTreeMap<String, i64>,
}

/// Scan every shipped adapter once, keyed by absolute path.
fn adapter_scans() -> &'static BTreeMap<PathBuf, AdapterScan> {
    static SCANS: OnceLock<BTreeMap<PathBuf, AdapterScan>> = OnceLock::new();
    SCANS.get_or_init(|| {
        let paths = tool_skills()
            .iter()
            .flat_map(ToolSkill::shipped_bin_files)
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return BTreeMap::new();
        }

        let mut child = Command::new("python3")
            .arg("-")
            .args(paths.iter().map(|path| path.as_os_str()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                // A missing interpreter must be loud. Skipping silently would
                // turn every adapter assertion into a green that proves
                // nothing, which is the failure mode this file exists to end.
                panic!(
                    "python3 is required to parse the Python adapters with Python's own \
                     ast module: {error}"
                )
            });
        child
            .stdin
            .as_mut()
            .expect("scanner stdin")
            .write_all(ADAPTER_SCANNER_PY.as_bytes())
            .expect("write scanner source");
        let output = child.wait_with_output().expect("run adapter scanner");
        assert!(
            output.status.success(),
            "adapter scanner failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );

        let scans: BTreeMap<PathBuf, AdapterScan> = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("decode adapter scan JSON: {error}"));
        let failures = scans
            .iter()
            .filter_map(|(path, scan)| {
                scan.error
                    .as_ref()
                    .map(|error| format!("{}: {error}", path.display()))
            })
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "{failures:#?}");
        scans
    })
}

/// Every shipped Python adapter of one skill, with its scan.
fn python_adapters(skill: &ToolSkill) -> Vec<(PathBuf, &'static AdapterScan)> {
    let scans = adapter_scans();
    skill
        .shipped_bin_files()
        .into_iter()
        .filter_map(|path| {
            let scan = scans.get(&path)?;
            scan.python.then_some((path, scan))
        })
        .collect()
}

/// Declared parameters of every action in one package, first declaration wins.
fn declared_parameters(package: &SkillRuntimePackage) -> BTreeMap<String, serde_json::Value> {
    let mut declared = BTreeMap::new();
    let Some(actions) = package.actions.as_ref() else {
        return declared;
    };
    for action in actions.actions.values() {
        for (name, parameter) in &action.parameters {
            // Serializing reaches `required` and `default` uniformly across the
            // typed parameter vocabulary without this test having to re-match
            // every variant of it.
            let projected =
                serde_json::to_value(parameter).expect("typed parameter projects to JSON");
            declared.entry(name.clone()).or_insert(projected);
        }
    }
    declared
}

fn parameter_is_required(parameter: &serde_json::Value) -> bool {
    parameter
        .get("required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn parameter_default(parameter: &serde_json::Value) -> Option<&serde_json::Value> {
    match parameter.get("default") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value),
    }
}

/// Does this string satisfy a typed parameter's own declared length bounds?
///
/// Mirrors the length half of the runtime's string check so that a candidate
/// picked below is one the runtime would actually accept. An absent ceiling
/// falls back to unbounded here rather than to the runtime's argument ceiling:
/// the compiler fills every string ceiling in before lowering ever sees it, so
/// the fallback only ever governs candidate *selection*, never admission.
fn string_fits(value: &str, min_length: Option<u64>, max_length: Option<u64>) -> bool {
    let characters = value.chars().count() as u64;
    let ceiling = max_length.unwrap_or(u64::MAX);
    characters >= min_length.unwrap_or(0)
        && characters <= ceiling
        && value.len() as u64 <= ceiling
        && !value.chars().any(char::is_control)
}

/// One value that satisfies a typed parameter's own declared bounds, or `None`
/// when its declaration admits no value at all.
///
/// The match is exhaustive on the parameter vocabulary on purpose, rather than
/// reading the serialized projection the way [`declared_parameters`] does. The
/// assertion this feeds is the thing that keeps an explicit `null` out of the
/// adapters' envelopes, so a new parameter variant must not be able to join the
/// vocabulary without someone deciding what a valid value of it looks like. A
/// projection-driven helper would silently skip an unrecognized variant and
/// leave the assertion green over exactly the gap it exists to close.
fn satisfying_value(parameter: &TypedActionParameter) -> Option<serde_json::Value> {
    match parameter {
        TypedActionParameter::String {
            default,
            enum_values,
            min_length,
            max_length,
            ..
        } => {
            let (min_length, max_length) = (*min_length, *max_length);
            // A constrained parameter admits only its own vocabulary, so a
            // synthetic string would be rejected for the wrong reason.
            if !enum_values.is_empty() {
                return enum_values
                    .iter()
                    .chain(default.iter())
                    .find(|value| string_fits(value.as_str(), min_length, max_length))
                    .map(|value| serde_json::Value::String(value.clone()));
            }
            let length = usize::try_from(min_length.unwrap_or(1).max(1)).ok()?;
            let candidate = "a".repeat(length);
            string_fits(&candidate, min_length, max_length)
                .then_some(serde_json::Value::String(candidate))
        },
        TypedActionParameter::WorkspacePath { max_length, .. } => {
            // The core validates only the bounded string shape here; binding it
            // beneath an admitted root is the dispatcher's job and is not
            // reached by lowering.
            let candidate = "a".to_owned();
            string_fits(&candidate, None, *max_length)
                .then_some(serde_json::Value::String(candidate))
        },
        TypedActionParameter::Integer {
            enum_values,
            minimum,
            maximum,
            ..
        } => {
            let (minimum, maximum) = (*minimum, *maximum);
            let in_bounds = |value: i64| {
                minimum.is_none_or(|minimum| value >= minimum)
                    && maximum.is_none_or(|maximum| value <= maximum)
            };
            if !enum_values.is_empty() {
                return enum_values
                    .iter()
                    .copied()
                    .find(|value| in_bounds(*value))
                    .map(serde_json::Value::from);
            }
            let candidate = match (minimum, maximum) {
                (Some(minimum), _) => minimum,
                (None, Some(maximum)) => maximum.min(0),
                (None, None) => 0,
            };
            in_bounds(candidate).then(|| serde_json::Value::from(candidate))
        },
        TypedActionParameter::Number {
            minimum, maximum, ..
        } => {
            let minimum = minimum.as_ref().and_then(serde_json::Number::as_f64);
            let maximum = maximum.as_ref().and_then(serde_json::Number::as_f64);
            let candidate = match (minimum, maximum) {
                (Some(minimum), _) => minimum,
                (None, Some(maximum)) => maximum.min(0.0),
                (None, None) => 0.0,
            };
            let in_bounds = candidate.is_finite()
                && minimum.is_none_or(|minimum| candidate >= minimum)
                && maximum.is_none_or(|maximum| candidate <= maximum);
            in_bounds
                .then(|| serde_json::Number::from_f64(candidate))
                .flatten()
                .map(serde_json::Value::Number)
        },
        TypedActionParameter::Boolean { default, .. } => {
            Some(serde_json::Value::Bool(default.unwrap_or(false)))
        },
        TypedActionParameter::StringArray {
            min_items,
            max_items,
            max_item_bytes,
            ..
        } => {
            let count = min_items.unwrap_or(0);
            if max_items.is_some_and(|max_items| count > max_items)
                || (count > 0 && max_item_bytes.is_some_and(|bytes| bytes < 1))
            {
                return None;
            }
            let count = usize::try_from(count).ok()?;
            Some(serde_json::Value::Array(vec![
                serde_json::Value::String(
                    "a".to_owned()
                );
                count
            ]))
        },
        // An empty container is one node at depth one and two encoded bytes.
        TypedActionParameter::JsonObject {
            max_json_bytes,
            max_depth,
            max_nodes,
            ..
        } => (max_json_bytes.is_none_or(|bytes| bytes >= 2)
            && max_depth.is_none_or(|depth| depth >= 1)
            && max_nodes.is_none_or(|nodes| nodes >= 1))
        .then(|| serde_json::Value::Object(serde_json::Map::new())),
        TypedActionParameter::JsonArray {
            max_json_bytes,
            max_depth,
            max_nodes,
            ..
        } => (max_json_bytes.is_none_or(|bytes| bytes >= 2)
            && max_depth.is_none_or(|depth| depth >= 1)
            && max_nodes.is_none_or(|nodes| nodes >= 1))
        .then(|| serde_json::Value::Array(Vec::new())),
    }
}

#[test]
fn every_tool_skill_manifest_parses() {
    // `tool-runtime.typed-action-overrides.v1` is still a supported authoring
    // version and 19 packages remain on it; v2 is what new and migrated
    // packages are authored against. Both are current in the sense that
    // matters here — the compiler accepts them — so this pins the supported set
    // rather than a single value it would take a separate migration to reach.
    let supported_action_versions = [TYPED_ACTION_OVERRIDES_V1, TYPED_ACTION_OVERRIDES_V2];
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let package = match &skill.package {
            Ok(package) => package,
            Err(error) => {
                failures.push(format!("{}: manifest does not parse: {error}", skill.name));
                continue;
            },
        };
        let version = package.contract.schema_version.as_str();
        if version != SKILL_RUNTIME_CONTRACT_V1 {
            failures.push(format!(
                "{}: runtime_contract schema_version is {version}, expected \
                 {SKILL_RUNTIME_CONTRACT_V1}",
                skill.name
            ));
        }
        if let Some(actions) = package.actions.as_ref() {
            if !supported_action_versions.contains(&actions.schema_version.as_str()) {
                failures.push(format!(
                    "{}: runtime_actions schema_version is {}, expected one of {:?}",
                    skill.name, actions.schema_version, supported_action_versions
                ));
            }
        }
        match validate_skill_runtime_contract(&package.contract) {
            Ok(validated) => {
                if let Some(actions) = package.actions.as_ref() {
                    if let Err(error) =
                        compile_typed_action_overrides(&skill.name, validated, actions)
                    {
                        failures.push(format!(
                            "{}: typed actions do not compile: {error}",
                            skill.name
                        ));
                    }
                }
            },
            Err(error) => {
                failures.push(format!("{}: contract is invalid: {error}", skill.name));
            },
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_tool_skill_declares_bins_that_exist() {
    // A declared executable is accounted for when the package ships it, when
    // the repository vendors it through the skillshub npm workspace, or — for a
    // genuinely external CLI the operator installs — when the manifest says how
    // to obtain it. Falling back to a bare PATH probe instead would make this
    // assertion report the developer's machine rather than the package, and a
    // package that names a file the rewrite deleted would still slip through on
    // a host that happened to have a same-named binary installed.
    // Read ownership from the committed lock rather than this machine's
    // generated node_modules tree. That keeps the test hermetic on a clean
    // clone while still requiring setup-deps to have a governed source for
    // every npm-backed executable.
    let vendored = workspace_vendored_bins();
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Some(contract) = skill.contract() else {
            continue;
        };
        let documented = skill.magician_extension("install_hint").is_some();
        for bin in contract.requires.bins.iter() {
            if skill.dir.join("bin").join(bin).is_file() {
                continue;
            }
            if vendored.contains(bin) {
                continue;
            }
            if !documented {
                failures.push(format!(
                    "{}: declares bin {bin}, which the package does not ship, the \
                     workspace does not vendor, and no install_hint explains",
                    skill.name
                ));
            }
        }

        // The entrypoint is the exact process the runtime execs. A package with
        // its own bin/ boundary must provide that process locally or through
        // the committed workspace lock. A generic install hint is not enough:
        // retaining this stricter rule catches an adapter deleted out from
        // under its own manifest.
        let entrypoint = contract.requires.entrypoint.clone().or_else(|| {
            (contract.requires.bins.len() == 1)
                .then(|| contract.requires.bins.iter().next().cloned())
                .flatten()
        });
        let ships_bin_dir = skill.dir.join("bin").is_dir();
        match (entrypoint, ships_bin_dir) {
            (Some(entrypoint), true)
                if !skill.dir.join("bin").join(&entrypoint).is_file()
                    && !vendored.contains(&entrypoint) =>
            {
                failures.push(format!(
                    "{}: ships bin/ but its entrypoint {entrypoint} is neither local nor workspace-vendored",
                    skill.name
                ));
            },
            (None, true) => failures.push(format!(
                "{}: ships bin/ but declares no entrypoint among {:?}",
                skill.name, contract.requires.bins
            )),
            _ => {},
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every secret NAME the deployment declares. Values are never read.
///
/// `.env.example` is the repository's own record of the secret surface, so the
/// check stays hermetic — it does not depend on which keys this particular
/// machine happens to hold. A real `.env`, where one exists, is admitted as an
/// additional source of names for installations that carry keys the example
/// has not caught up with.
fn declared_secret_names() -> &'static BTreeSet<String> {
    static NAMES: OnceLock<BTreeSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let root = workspace_root();
        let mut names = BTreeSet::new();
        for relative in [
            "magician_data_v3/.env.example",
            "magician_data_v3/.env",
            ".env",
        ] {
            let Ok(source) = fs::read_to_string(root.join(relative)) else {
                continue;
            };
            for line in source.lines() {
                // Commented-out entries still declare the name; a commented
                // example is exactly how an unset optional key is recorded.
                let line = line.trim_start().trim_start_matches('#').trim();
                let Some((name, _value)) = line.split_once('=') else {
                    continue;
                };
                // `_value` is bound only to be dropped here. Nothing in this
                // file reads, compares, prints, or stores a secret value.
                let name = name.trim().trim_start_matches("export ").trim();
                if !name.is_empty()
                    && name.chars().all(|character| {
                        character.is_ascii_uppercase()
                            || character.is_ascii_digit()
                            || character == '_'
                    })
                {
                    names.insert(name.to_owned());
                }
            }
        }
        assert!(
            !names.is_empty(),
            "no secret names could be read from the declared environment files"
        );
        names
    })
}

#[test]
fn every_secret_binding_names_a_resolvable_secret() {
    let declared = declared_secret_names();
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Some(contract) = skill.contract() else {
            continue;
        };
        for binding in &contract.auth.secret_bindings {
            // Presence of the NAME only. No value is read, compared, or logged.
            if !declared.contains(&binding.secret_ref) {
                failures.push(format!(
                    "{}: secret binding {} names {}, which no declared environment \
                     file offers",
                    skill.name, binding.name, binding.secret_ref
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_parameter_the_adapter_reads_is_declared() {
    // An attribute read off the parsed governed envelope raises AttributeError
    // when the name is absent, and the runtime surfaces that as an opaque
    // one-word error. The envelope carries what the caller supplied plus every
    // parameter whose manifest declares a `default:`; a parameter that is
    // neither required nor defaulted simply does not arrive. So every attribute
    // an adapter reads must be guaranteed by the manifest or by the adapter's
    // own fallback — `PARAMETER_DEFAULTS`, a `setdefault`, an argparse
    // `add_argument`, or a three-argument `getattr`.
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Ok(package) = &skill.package else {
            continue;
        };
        let declared = declared_parameters(package);
        for (path, scan) in python_adapters(skill) {
            if scan.namespace_reads.is_empty() {
                continue;
            }
            let mut guaranteed = scan.guaranteed_names.clone();
            guaranteed.extend(scan.required_parameters.iter().flatten().cloned());
            guaranteed.extend(
                scan.parameter_defaults
                    .iter()
                    .flat_map(BTreeMap::keys)
                    .cloned(),
            );
            for (name, parameter) in &declared {
                if parameter_is_required(parameter) || parameter_default(parameter).is_some() {
                    guaranteed.insert(scan.renames.get(name).unwrap_or(name).clone());
                }
            }
            for read in &scan.namespace_reads {
                if !guaranteed.contains(read) {
                    failures.push(format!(
                        "{}: {} reads `{read}` off the governed envelope, which neither \
                         the manifest nor the adapter guarantees is present",
                        skill.name,
                        path.display()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn adapter_defaults_match_their_manifest() {
    // The adapters mirror their manifest defaults as module constants because
    // they must not read their own YAML at execution time. Nothing but a
    // comment binds the two, so changing a `default:` in a SKILL.md leaves both
    // adapter suites green while the adapter keeps sending the stale value.
    // This is the join that actually holds them together — the same shape as
    // `manifest_governed_kill_secs()` in the adapters' own test suites, run
    // from the other side.
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Ok(package) = &skill.package else {
            continue;
        };
        let declared = declared_parameters(package);
        for (path, scan) in python_adapters(skill) {
            let (Some(required), Some(defaults)) = (
                scan.required_parameters.as_ref(),
                scan.parameter_defaults.as_ref(),
            ) else {
                continue;
            };
            let adapter_name = |name: &String| scan.renames.get(name).unwrap_or(name).clone();
            let expected_required = declared
                .iter()
                .filter(|(_, parameter)| parameter_is_required(parameter))
                .map(|(name, _)| adapter_name(name))
                .collect::<BTreeSet<_>>();
            if *required != expected_required {
                failures.push(format!(
                    "{}: {} REQUIRED_PARAMETERS is {required:?}, manifest requires \
                     {expected_required:?}",
                    skill.name,
                    path.display()
                ));
            }
            let expected_optional = declared
                .iter()
                .filter(|(_, parameter)| !parameter_is_required(parameter))
                .map(|(name, parameter)| (adapter_name(name), parameter))
                .collect::<BTreeMap<_, _>>();
            let mirrored = defaults.keys().cloned().collect::<BTreeSet<_>>();
            let expected_keys = expected_optional.keys().cloned().collect::<BTreeSet<_>>();
            if mirrored != expected_keys {
                failures.push(format!(
                    "{}: {} PARAMETER_DEFAULTS covers {mirrored:?}, manifest declares \
                     optional {expected_keys:?}",
                    skill.name,
                    path.display()
                ));
            }
            for (name, parameter) in expected_optional {
                let Some(mirrored) = defaults.get(&name) else {
                    continue;
                };
                // A parameter the manifest leaves without a `default:` is
                // absent from the envelope, so the adapter's own fallback is
                // the only value it can carry — it must be Python `None`.
                let expected = parameter_default(parameter);
                let matches = match expected {
                    Some(value) => value == mirrored,
                    None => mirrored.is_null(),
                };
                if !matches {
                    failures.push(format!(
                        "{}: {} defaults `{name}` to {mirrored}, manifest declares {}",
                        skill.name,
                        path.display(),
                        expected
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "no default".to_owned())
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn no_declared_parameter_accepts_an_explicit_null() {
    // Adapters fill absent parameters with `setdefault`, which fills only keys
    // that are ABSENT. An explicit `null` is present, so it would survive:
    // `{"max_results": null}` reaches `results[:None]`, which is the whole list
    // — a declared result cap silently disabled, failing open with no error.
    // Two shipped adapters slice on a caller-facing cap this way, and the same
    // shape is available to any numeric bound reached through `setdefault`.
    //
    // What actually stops that today is upstream of every adapter, and is a
    // property of the parameter vocabulary rather than of any one package: the
    // typed vocabulary has no nullable variant, so `validate_runtime_parameter`
    // rejects `null` against every one of its arms, and canonical-JSON lowering
    // validates each value on its way into the envelope. The defect is
    // unreachable because of that, and for no other reason.
    //
    // So this pins the property, not the packages. Rejection is also strictly
    // better than stripping the key would be: stripping would hand the adapter
    // its own fallback and turn a caller's mistake into a silent success,
    // whereas the caller now gets an error. Relaxing the runtime — adding a
    // nullable type, or coercing `null` to a default — reopens the hole in
    // every canonical-JSON adapter at once, which is precisely what this
    // catches. It deliberately covers every declared parameter rather than just
    // the numeric caps: which parameter a bound is reached through is an
    // adapter's private business, and enumerating today's caps here would rot.
    //
    // Argv delivery is out of scope by construction, not by omission. Canonical
    // JSON is the only lane that materializes EVERY declared parameter into a
    // payload the executable parses; argv emits only what an explicit mapping
    // names. A null on a mapped parameter is validated and rejected on the same
    // path as below, and a null on an unmapped one is never read, so no argv
    // parameter carries a null across the executable boundary either way.
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Ok(package) = &skill.package else {
            continue;
        };
        let Some(overrides) = package.actions.as_ref() else {
            continue;
        };
        if overrides.input_delivery != TypedActionInputDelivery::CanonicalJsonStdin {
            continue;
        }
        let Ok(validated) = validate_skill_runtime_contract(&package.contract) else {
            // Reported, with its cause, by `every_tool_skill_manifest_parses`.
            continue;
        };
        let catalog = match compile_typed_action_overrides(&skill.name, validated, overrides) {
            Ok(catalog) => catalog,
            Err(error) => {
                failures.push(format!(
                    "{}: typed actions do not compile, so no bound of theirs can be \
                     proven here: {error}",
                    skill.name
                ));
                continue;
            },
        };

        for (action_id, action) in &catalog.actions {
            if action.invocation.input_delivery != TypedActionInputDelivery::CanonicalJsonStdin {
                continue;
            }
            // Lowering rejects a missing required parameter before it looks at
            // any value, so the probe needs a baseline that is otherwise valid
            // or every assertion below would pass for the wrong reason.
            let required = action
                .definition
                .input_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let mut baseline = serde_json::Map::new();
            let mut unconstructible = BTreeSet::new();
            for name in &required {
                // Names outside the declared parameters are runtime-owned
                // controls; the baseline check below catches any that turn out
                // to be required without a default.
                let Some(parameter) = action.parameters.get(name) else {
                    continue;
                };
                match satisfying_value(parameter) {
                    Some(value) => {
                        baseline.insert(name.clone(), value);
                    },
                    None => {
                        unconstructible.insert(name.clone());
                    },
                }
            }
            if !unconstructible.is_empty() {
                failures.push(format!(
                    "{}: {action_id} requires {unconstructible:?}, which this assertion \
                     cannot construct a valid value for, so it cannot prove the action \
                     rejects a null",
                    skill.name
                ));
                continue;
            }
            let baseline = serde_json::Value::Object(baseline);
            if let Err(error) = lower_typed_action_invocation(action, &baseline) {
                failures.push(format!(
                    "{}: {action_id} rejects this assertion's own null-free baseline \
                     invocation ({error}), so a green below would prove nothing",
                    skill.name
                ));
                continue;
            }

            for name in action.parameters.keys() {
                let mut probe = baseline
                    .as_object()
                    .cloned()
                    .expect("the baseline invocation is an object");
                probe.insert(name.clone(), serde_json::Value::Null);
                match lower_typed_action_invocation(action, &serde_json::Value::Object(probe)) {
                    Err(error)
                        if error.code == TypedActionInvocationErrorCode::InvalidParameter => {},
                    Err(error) => failures.push(format!(
                        "{}: {action_id} rejects `{name}: null` as {:?} rather than as an \
                         invalid parameter; the null must fail on its own declared type, \
                         not incidentally on something else",
                        skill.name, error.code
                    )),
                    Ok(lowered) => failures.push(format!(
                        "{}: {action_id} accepts `{name}: null` and puts it on the \
                         adapter's stdin as {}; any bound the adapter reaches through \
                         `{name}` can now be defeated by an explicit null",
                        skill.name,
                        lowered.stdin.unwrap_or_default()
                    )),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_timeout_respects_the_kill_margin() {
    // The governed kill is a backstop for a runtime that has stopped
    // responding. A backstop that fires before the work it backs stops being a
    // backstop and becomes the primary timeout under the wrong name, and every
    // provider deadline and retry the adapter wrote turns into dead code
    // because the blunt kill always gets there first.
    //
    // `phase7_migration.rs` pins this for six named adapters against a reviewed
    // table of exact seconds. This is the same invariant with the table
    // removed: any adapter that mirrors the ceiling is held to it, so a seventh
    // adapter is covered the day it is written rather than the day someone
    // remembers to extend a constant.
    const MIRROR_CONSTANTS: [&str; 3] = [
        "INNER_WORST_CASE_SECS",
        "GOVERNED_KILL_MARGIN_SECS",
        "GOVERNED_KILL_CEILING_SECS",
    ];
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Ok(package) = &skill.package else {
            continue;
        };
        let limits = runtime_limits(&package.contract);

        for (path, scan) in python_adapters(skill) {
            let present = MIRROR_CONSTANTS
                .iter()
                .filter(|name| scan.integer_constants.contains_key(**name))
                .count();
            if present == 0 {
                continue;
            }
            if present != MIRROR_CONSTANTS.len() {
                failures.push(format!(
                    "{}: {} mirrors part of the governed timeout budget; all of {:?} \
                     must be declared together",
                    skill.name,
                    path.display(),
                    MIRROR_CONSTANTS
                ));
                continue;
            }
            let inner = scan.integer_constants["INNER_WORST_CASE_SECS"];
            let margin = scan.integer_constants["GOVERNED_KILL_MARGIN_SECS"];
            let mirrored = scan.integer_constants["GOVERNED_KILL_CEILING_SECS"];
            let Some(declared) = limits.timeout_secs else {
                failures.push(format!(
                    "{}: {} mirrors a governed kill of {mirrored}s but the contract \
                     declares no runtime.limits.timeout_secs",
                    skill.name,
                    path.display()
                ));
                continue;
            };
            let declared = i64::from(declared);
            if mirrored != declared {
                failures.push(format!(
                    "{}: {} mirrors {mirrored}s, runtime.limits.timeout_secs is \
                     {declared}s",
                    skill.name,
                    path.display()
                ));
            }
            if margin <= 0 {
                failures.push(format!(
                    "{}: {} states a {margin}s margin; the kill must outlast the work, \
                     not tie with it",
                    skill.name,
                    path.display()
                ));
            }
            if declared < inner + margin {
                failures.push(format!(
                    "{}: {} governed kill {declared}s does not outlast its {inner}s \
                     inner provider budget plus a {margin}s margin",
                    skill.name,
                    path.display()
                ));
            }
        }

        // A typed action may lower the ceiling; it may never raise it above the
        // kill the runtime will actually enforce.
        let Some(actions) = package.actions.as_ref() else {
            continue;
        };
        for (name, action) in &actions.actions {
            let (Some(action_secs), Some(declared)) = (action.timeout_secs, limits.timeout_secs)
            else {
                continue;
            };
            if action_secs > declared {
                failures.push(format!(
                    "{}: action {name} asks for {action_secs}s above the {declared}s \
                     governed kill",
                    skill.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The baseline the governed runtime rebuilds a child environment from, given a
/// contract. Mirrors the selection in
/// `magician/src/magician_v2/execution/primitive_dispatch/governed_runtime.rs`,
/// where a CLI that owns its own session gets `HOME` on top of the portable
/// baseline and everything else runs portable.
fn governed_baseline(contract: &SkillRuntimeContract) -> ChildEnvironmentBaseline {
    match (contract.auth.kind, &contract.auth.profile_selection) {
        (AuthKind::CliProfile, ProfileSelection::Implicit) => {
            ChildEnvironmentBaseline::cli_owned_session()
        },
        _ => ChildEnvironmentBaseline::portable_cli(),
    }
}

#[test]
fn every_networked_skill_receives_a_ca_bundle() {
    // The governed runtime `env_clear`s and rebuilds the child environment from
    // a finite allowlist. When that allowlist lacked `SSL_CERT_FILE`, every
    // Python adapter whose interpreter ships no trust store lost TLS
    // verification and every HTTPS call failed — twenty-one of sixty-four
    // skills went dark, and nothing went red. The variable is name-only policy
    // here; its value stays runtime-owned and is never inherited from the
    // caller.
    const NETWORK_MODULES: [&str; 4] = ["urllib", "requests", "httpx", "http"];
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Ok(package) = &skill.package else {
            continue;
        };
        for (path, scan) in python_adapters(skill) {
            let networked = NETWORK_MODULES
                .iter()
                .filter(|module| scan.imports.contains(**module))
                .collect::<Vec<_>>();
            if networked.is_empty() {
                continue;
            }
            let baseline = governed_baseline(&package.contract);
            if !baseline
                .variables()
                .contains(&ChildEnvironmentVariable::SslCertFile)
            {
                failures.push(format!(
                    "{}: {} imports {networked:?} but its governed baseline omits \
                     {}; every HTTPS call it makes will fail certificate verification",
                    skill.name,
                    path.display(),
                    ChildEnvironmentVariable::SslCertFile.as_str()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_tool_skill_declares_a_canary() {
    // The Tier 2 lane can only cover what the packages declare, so an absent
    // declaration is a failure here rather than a quietly smaller live run.
    //
    // An explicit exemption counts as declared. Some skills genuinely have no
    // safe live call — they send messages, place orders, or drive the
    // operator's desktop — and the honest record of that belongs in the package
    // beside the contract. What must never be possible is for "this cannot be
    // canaried" and "nobody got round to it" to look identical.
    let mut failures = Vec::new();
    for skill in tool_skills() {
        let Some(node) = skill.magician_extension("runtime_canary") else {
            failures.push(format!(
                "{}: declares no metadata.magician.runtime_canary",
                skill.name
            ));
            continue;
        };
        let canary = match SkillRuntimeCanary::parse_node(node) {
            Ok(canary) => canary,
            Err(error) => {
                failures.push(format!(
                    "{}: runtime_canary is invalid: {error}",
                    skill.name
                ));
                continue;
            },
        };
        let Some(run) = canary.run() else {
            continue;
        };
        // A canary that names an action its own package does not declare can
        // never have run, so it would sit green forever as a lie about
        // coverage.
        if let Ok(package) = &skill.package {
            if let Some(actions) = package.actions.as_ref() {
                if !actions.actions.contains_key(&run.action) {
                    failures.push(format!(
                        "{}: canary names action {}, which the package does not declare",
                        skill.name, run.action
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every literal `skillshub/…` path appearing in one text file.
fn referenced_skill_paths(source: &str) -> BTreeSet<String> {
    const PREFIX: &str = "skillshub/";
    let mut found = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(PREFIX) {
        let start = cursor + offset;
        let mut end = start + PREFIX.len();
        while end < bytes.len() {
            let character = bytes[end] as char;
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '+')
            {
                end += 1;
            } else {
                break;
            }
        }
        let candidate = source[start..end].trim_end_matches(['.', '/']);
        if candidate.len() > PREFIX.len() {
            found.insert(candidate.to_owned());
        }
        cursor = end.max(start + PREFIX.len());
    }
    found
}

fn text_files_under(root: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "node_modules" || name == "__pycache__" || name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => text_files_under(&path, into),
            Ok(kind) if kind.is_file() => into.push(path),
            _ => {},
        }
    }
}

#[test]
fn no_script_references_a_missing_skill_path() {
    // The rewrite moved every adapter from `<skill>/scripts/*.py` to
    // `<skill>/bin/<name>` and changed how they are invoked. A harness still
    // pointing at the old layout does not merely fail — it tests an execution
    // model production no longer has, and reports on it as though it were live.
    //
    // Operator-supplied material is deliberately absent from the tree, so a
    // reference git is told to ignore is a reference to something the operator
    // provides, not a broken path.
    let root = workspace_root();
    let mut files = Vec::new();
    text_files_under(&root.join("scripts"), &mut files);
    text_files_under(&root.join("magician").join("examples"), &mut files);
    files.sort();

    let mut unresolved: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &files {
        let Ok(source) = fs::read_to_string(file) else {
            continue;
        };
        for reference in referenced_skill_paths(&source) {
            if root.join(&reference).exists() {
                continue;
            }
            unresolved.entry(reference).or_default().insert(
                file.strip_prefix(root)
                    .unwrap_or(file)
                    .display()
                    .to_string(),
            );
        }
    }

    let ignored = git_ignored(root, unresolved.keys());
    let failures = unresolved
        .into_iter()
        .filter(|(reference, _)| !ignored.contains(reference))
        .map(|(reference, referents)| {
            format!("{reference} does not exist; referenced by {referents:?}")
        })
        .collect::<Vec<_>>();
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Which of the given repository-relative paths git deliberately ignores.
fn git_ignored<'a>(root: &Path, paths: impl Iterator<Item = &'a String>) -> BTreeSet<String> {
    let paths = paths.cloned().collect::<Vec<_>>();
    if paths.is_empty() {
        return BTreeSet::new();
    }
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        // Without git every candidate stays a failure, which is the safe
        // direction: an unresolvable reference is reported rather than excused.
        return BTreeSet::new();
    };
    let _ = child
        .stdin
        .as_mut()
        .expect("git stdin")
        .write_all(paths.join("\n").as_bytes());
    let Ok(output) = child.wait_with_output() else {
        return BTreeSet::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}
