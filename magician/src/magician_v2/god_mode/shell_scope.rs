//! What a shell command runs, read with a real Bash parser (tree-sitter-bash),
//! and the scopes an owner may grant it.
//!
//! Every simple command in the line is extracted, including those inside
//! command and process substitutions, pipelines, `&&`/`;` lists, subshells
//! and control flow. A grant covers a line only if it covers every command
//! in it.
//!
//! A line is *opaque* when what it runs cannot be named from its text; it can
//! only receive an exact invocation grant, never a program or prefix grant:
//! - it does not parse cleanly;
//! - a command name comes from an expansion (`$CMD args`);
//! - it defines a function, or uses `eval`, `source` or `.`;
//! - an interpreter runs inline code (`bash -c`, `python3 -c`, `node -e`)
//!   or reads code from stdin (`… | sh`);
//! - a program runs another command from its arguments (`sudo`, `env`,
//!   `xargs`, `find -exec`, `timeout`, `ssh`, …);
//! - a command is named by a path (`./git`, `/tmp/x/git`): a grant names a
//!   program, and any file can carry that name;
//! - it sets variables (`PATH=. git`, `export …`, `X=1; …`, `unset …`),
//!   which change what later commands run;
//! - it writes a file through a redirect (`> file`, `>> file`), other than
//!   to `/dev/null`, `/dev/stdout` or `/dev/stderr` or onto another
//!   descriptor (`2>&1`).
//!
//! A line takes the *exact invocation* path when it is opaque, blocked,
//! given standard input, or runs a file ([`ShellReading::code_files`]: a
//! command named by a path, also behind a runner, a `source ./x` target, an
//! interpreter's script). Whether such a line may get a lasting grant is
//! decided by construction, not by modelling what it does:
//! [`bindable_invocation`] accepts only ONE simple command, a path-named
//! script or `bash`/`sh` with the script first, followed by plain literal
//! arguments, in printable ASCII with no `\`, and with no list, pipeline,
//! redirect, heredoc, assignment, expansion, glob or `~` (`python3` is never
//! bindable: it imports modules from the script's folder). `god_mode` then
//! binds that one file by content, and only when it is a text script
//! outside the system folders. Every other line on the exact path is
//! allowed once only.
//!
//! The rest of the reading (what each command runs, `once_only`,
//! [`ShellReading::unbound`], the allowlist of inert programs) classifies
//! lines and explains the card; it never makes a lasting grant possible.
//! Inline shell code (`bash -c '…'`, `eval '…'`, `su -c '…'`, `watch …`,
//! `flock f -c '…'`, `trap '…'`) is read like the line itself.
//!
//! Some lines run code no grant can bind, and get no lasting grant on either
//! path ([`ShellReading::once_only`]):
//! - an interpreter runs code fed to its input from the line (`curl … | sh`,
//!   `sh <<EOF`, `sh < file`, `exec < ./x; sh`), or a script chosen when it
//!   runs (`xargs bash`, `bash ./*.sh`, `find -exec bash {}`);
//! - a command, script or inline code comes from an expansion (`$CMD`,
//!   `sudo $X`, `bash "$S"`, `python3 -c "$(curl …)"`, `source "$F"`);
//! - an interpreter or runner takes an option outside the few known to load
//!   nothing (`python3 -m`, `node -r`, `bash -l`, `bash +l`, `--rcfile`,
//!   `sudo -D`, `env -C`, `env -S`), or a runner is not read here (`ssh`,
//!   `chroot`, `script`, `parallel`, `hash`, `alias`);
//! - it sets a variable other than an inert one (`LANG`, `LC_*`, `TZ`,
//!   `TERM`, …), in the line or the step's env: `PATH`, `HOME`, `BASH_ENV`,
//!   `NODE_OPTIONS`, `PYTHONPATH`, `LD_PRELOAD`, `DYLD_*`, …; or `source`s a
//!   file looked up on `PATH`;
//! - it may write a file it runs before running it (a write redirect, a
//!   writing program or other code earlier in the line), or while running
//!   it (a writer in a pipeline, a loop, `&`, `xargs` or `find -exec`), or a
//!   file it runs is a device or process file (`/dev/stdin`, `/proc/…/fd`);
//! - a file is named relative to a directory the line may leave before it
//!   runs (`cd x && ./run.sh`, `. ./env.sh && ./run.sh`, `find -execdir`);
//! - the line does not parse, or its inline code or runners nest too deeply.
//!
//! A file it runs that is missing, unreadable, not a regular file or larger
//! than the hashing cap makes the line once-only too (checked by `god_mode`).

use tree_sitter::{Node, Parser};

/// Shells and interpreters, with the flags that make them run inline code.
const INTERPRETERS: &[(&str, &[&str])] = &[
    ("sh", &["-c"]),
    ("bash", &["-c"]),
    ("zsh", &["-c"]),
    ("dash", &["-c"]),
    ("ksh", &["-c"]),
    ("fish", &["-c", "--command"]),
    ("python", &["-c"]),
    ("python3", &["-c"]),
    ("node", &["-e", "--eval", "-p", "--print"]),
    ("deno", &["eval"]),
    ("bun", &["-e", "--eval"]),
    ("ruby", &["-e"]),
    ("perl", &["-e", "-E"]),
    ("php", &["-r"]),
    ("osascript", &["-e"]),
];

/// Shells, whose inline code (`-c`) is read like the line itself.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// Builtins that change the shell's directory.
const DIRECTORY_CHANGERS: &[&str] = &["cd", "pushd", "popd"];

/// How deep inline code (`bash -c 'eval …'`) or runners (`sudo nohup …`)
/// are read before the line counts as unbindable.
const MAX_NESTING: usize = 8;

/// Programs that run another command taken from their arguments.
const COMMAND_RUNNERS: &[&str] = &[
    "sudo",
    "doas",
    "su",
    "env",
    "exec",
    "command",
    "builtin",
    "nohup",
    "time",
    "nice",
    "ionice",
    "timeout",
    "watch",
    "xargs",
    "parallel",
    "ssh",
    "chroot",
    "script",
    "flock",
    "caffeinate",
    "eval",
    "source",
    ".",
    "trap",
    "hash",
    "alias",
];

/// Redirect targets a write may name and still be granted.
const HARMLESS_WRITE_TARGETS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr"];

/// One simple command: its program and literal arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// The command name as written. A name with a `/` makes the line
    /// opaque, so a granted name is always looked up on `PATH`.
    pub program: String,
    /// Arguments as written, quotes removed. `None` for one that expands
    /// (`$X`, `$(…)`, globs are kept as text).
    pub args: Vec<Option<String>>,
}

impl ShellCommand {
    /// The whole command, when every argument is literal.
    pub fn exact(&self) -> Option<String> {
        let mut words = vec![self.program.clone()];
        for arg in &self.args {
            words.push(arg.clone()?);
        }
        Some(words.join(" "))
    }

    /// `<program> <first argument>`, when that argument is literal and not
    /// an option.
    pub fn prefix(&self) -> Option<String> {
        match self.args.first() {
            Some(Some(first)) if !first.starts_with('-') => {
                Some(format!("{} {first}", self.program))
            },
            _ => None,
        }
    }
}

/// How the line runs a file. Each must be a readable regular file, found
/// from the working directory, for a lasting grant to bind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeFileKind {
    /// A command named by a path (`./run.sh`, `sudo ./run.sh`).
    Program,
    /// A `source`/`.` target named with a `/` (one without is looked up on
    /// the command's `PATH`, which makes the line once-only).
    Sourced,
    /// An interpreter's script (`bash setup.sh`).
    Script,
}

/// A file the line runs as code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    /// As written, quotes removed. A quoted leading `~` does not expand, so
    /// it is kept relative (`./~/x`).
    pub path: String,
    pub kind: CodeFileKind,
}

/// What a command line runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellReading {
    pub commands: Vec<ShellCommand>,
    /// Why the line needs an exact invocation scope, if it does.
    pub opaque: Option<String>,
    /// Why no lasting grant at all may cover the line, if none may: what it
    /// runs is in neither its text nor a file the reading binds.
    pub once_only: Option<String>,
    /// Why an exact invocation grant cannot bind the line, if it cannot: an
    /// expansion, a shell function, or a program outside those understood
    /// here (`INERT_PROGRAMS`, the runners and interpreters read below, a
    /// bound file). Program and prefix grants, for a line that is not opaque
    /// and runs no file, do not depend on it.
    pub unbound: Option<String>,
    /// Files the line runs as code, which an exact invocation grant binds by
    /// content. A line that runs any takes the exact invocation path.
    pub code_files: Vec<CodeFile>,
    /// Why some of what runs is not in the line's text at all (a command or
    /// code from an expansion, code fed to a shell's input, a script chosen
    /// when it runs, a line that does not parse), if so. Always once-only;
    /// god mode's auto-approve asks for it too.
    pub hidden: Option<String>,
    /// Why a command the line shows needs the owner even under auto-approve:
    /// it deletes or runs as root ([`hazard`]), if one does.
    pub hazard: Option<String>,
    /// `cd`/`pushd`/`popd` commands, and `source`/`.` commands (whose file may
    /// change directory), for `moves_before_a_relative_file`.
    directory_changers: usize,
    sources: usize,
    /// How deep in inline code (`bash -c '…'`) this reading is.
    depth: usize,
    /// Inline code whose shell's input the outer line feeds
    /// (`curl … | bash -c 'sh'`).
    input_fed: bool,
    /// Something earlier in the line may have written files: a write
    /// redirect, a program that writes, or code that ran.
    wrote: bool,
    /// Writes by redirects and writing programs.
    other_writes: usize,
    /// Commands that ran code (a bound file, an interpreter, an unknown
    /// program), any of which may write.
    runners: usize,
    /// Parts of the line run at once or repeatedly: a pipeline, `&`, a loop,
    /// `xargs`, `find -exec`, `watch`.
    concurrent: bool,
}

impl ShellReading {
    fn at_depth(depth: usize, input_fed: bool) -> Self {
        Self {
            commands: Vec::new(),
            opaque: None,
            once_only: None,
            unbound: None,
            code_files: Vec::new(),
            hidden: None,
            hazard: None,
            directory_changers: 0,
            sources: 0,
            depth,
            input_fed,
            wrote: false,
            other_writes: 0,
            runners: 0,
            concurrent: false,
        }
    }
}

/// Parse only: never execute substitutions while validating an override.
pub fn syntax_is_valid(command: &str) -> bool {
    let mut parser = Parser::new();
    !command.trim().is_empty()
        && parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .is_ok()
        && parser
            .parse(command, None)
            .is_some_and(|tree| !tree.root_node().has_error())
}

/// Read `command` with the Bash grammar.
pub fn read(command: &str) -> ShellReading {
    read_at(command, ShellReading::at_depth(0, false))
}

/// How the one script of a bindable line runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRunner {
    /// `./deploy.sh …`: executed directly, through its `#!` line, or as a
    /// shell script when it has none.
    Direct,
    /// `bash x.sh …` or `sh x.sh …`.
    Shell,
}

/// The one file a bindable line runs, as written, and how it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRun {
    pub script: String,
    pub runner: ScriptRunner,
}

/// The only shape a lasting exact invocation grant may cover, read by
/// construction rather than by modelling what a line does: the whole line is
/// ONE simple command, either a file named by a path (`./deploy.sh`,
/// `bin/tool`, `/abs/x.sh`) or `bash`/`sh` with the script as its first
/// argument (no option before it), followed only by plain literal arguments.
/// Plain: an unquoted word with no glob, brace, tilde, quote, `$`, `=` or
/// operator character, or a single-quoted string with no `=`; a leading `-`
/// is fine (the script receives it). The line may hold only printable ASCII,
/// spaces and tabs, and no `\` at all: Bash keeps `\r`, VT, FF, NBSP and
/// U+3000 inside words and reads `\` before a blank or newline as an escape
/// or continuation, where the parser would split or join words. Anything
/// else (a list, pipeline, `&`, subshell, group, loop, `if`, function,
/// comment, any redirect, heredoc or herestring, an assignment or env
/// prefix, any expansion or substitution, `~`) is `None`. `python3` is not
/// bindable: it puts the script's folder first on `sys.path`, so a sibling
/// module the agent writes runs with it. `god_mode` then checks the file
/// itself: a text script outside the system folders.
pub fn bindable_invocation(command: &str) -> Option<ScriptRun> {
    let text = command.trim_matches(|ch: char| matches!(ch, ' ' | '\t' | '\n'));
    if text
        .chars()
        .any(|ch| ch == '\\' || !(ch == ' ' || ch == '\t' || ('!'..='~').contains(&ch)))
    {
        return None;
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(text, None)?;
    let root = tree.root_node();
    // One statement and nothing else: no `;`, `&` or newline terminator, no
    // comment.
    if root.has_error() || root.kind() != "program" || root.child_count() != 1 {
        return None;
    }
    let statement = root.child(0)?;
    if statement.kind() != "command" {
        return None;
    }
    let source = text.as_bytes();
    let mut cursor = statement.walk();
    let parts: Vec<Node<'_>> = statement.children(&mut cursor).collect();
    // Only blanks around and between the command's parts: nothing the
    // parser skips as an extra may join or split words.
    let blank = |range: std::ops::Range<usize>| {
        source
            .get(range)
            .is_some_and(|gap| gap.iter().all(|byte| matches!(byte, b' ' | b'\t')))
    };
    let mut edges = vec![0];
    for part in &parts {
        edges.push(part.start_byte());
        edges.push(part.end_byte());
    }
    edges.push(source.len());
    if !edges.chunks(2).all(|pair| blank(pair[0]..pair[1])) {
        return None;
    }
    let (name, args) = parts.split_first()?;
    // An assignment or redirect before the name, or any part that is not a
    // plain word (a redirect, herestring or `$` after it), is refused.
    if name.kind() != "command_name" {
        return None;
    }
    let program = plain_word(name.named_child(0)?, source, false)?;
    if program.starts_with('-') {
        return None;
    }
    let args: Vec<String> = args
        .iter()
        .map(|arg| plain_word(*arg, source, true))
        .collect::<Option<_>>()?;
    if program.contains('/') {
        return Some(ScriptRun {
            script: program,
            runner: ScriptRunner::Direct,
        });
    }
    let runner = match program.as_str() {
        "bash" | "sh" => ScriptRunner::Shell,
        _ => return None,
    };
    let script = args.first()?;
    // `bash -l x`, `bash +o …`: no option may come first.
    if script.starts_with('-') || script.starts_with('+') {
        return None;
    }
    Some(ScriptRun {
        script: script.clone(),
        runner,
    })
}

/// A word that means only its text, for [`bindable_invocation`]. `quoted`:
/// a single-quoted string is allowed too (an argument).
fn plain_word(node: Node<'_>, source: &[u8], quoted: bool) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let value = match node.kind() {
        "word" | "number" => {
            let special = |ch: char| {
                matches!(
                    ch,
                    '*' | '?'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '~'
                        | '\\'
                        | '$'
                        | '`'
                        | '='
                        | '!'
                        | '#'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '|'
                        | '&'
                        | ';'
                        | '"'
                        | '\''
                )
            };
            if text.chars().any(special) {
                return None;
            }
            text.to_string()
        },
        "raw_string" if quoted => text.strip_prefix('\'')?.strip_suffix('\'')?.to_string(),
        _ => return None,
    };
    if value.is_empty() || value.contains('=') {
        return None;
    }
    Some(value)
}

/// Read `command` into `reading`: fresh, or seeded by the runner of inline
/// code (its depth, fed input, earlier writes).
fn read_at(command: &str, mut reading: ShellReading) -> ShellReading {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        return opaque_reading("the shell parser is unavailable");
    }
    let Some(tree) = parser.parse(command, None) else {
        return opaque_reading("the command could not be parsed");
    };
    let root = tree.root_node();
    if root.has_error() {
        return opaque_reading("the command does not parse as shell");
    }
    let source = command.as_bytes();
    walk(root, source, &mut reading);
    if reading.opaque.is_none() && reading.commands.is_empty() {
        reading.opaque = Some("the line runs no command".to_string());
    }
    if moves_before_a_relative_file(&reading) {
        once(
            &mut reading,
            "it may change directory before it runs a file named relative to it",
        );
    }
    // Run at once or repeatedly, another writer (or a second piece of code)
    // may change a file while it runs: Bash reads a script as it goes.
    if !reading.code_files.is_empty()
        && reading.concurrent
        && (reading.other_writes > 0 || reading.runners > 1)
    {
        once(
            &mut reading,
            "it may change a file it runs while running it",
        );
    }
    reading
}

/// A line that cannot be read runs what no grant can name or bind.
fn opaque_reading(reason: &str) -> ShellReading {
    let mut reading = ShellReading::at_depth(0, false);
    reading.opaque = Some(reason.to_string());
    reading.once_only = Some(reason.to_string());
    reading.unbound = Some(reason.to_string());
    reading.hidden = Some(reason.to_string());
    reading
}

/// Whether a file the line runs is named relative to a directory the line
/// may leave first: any `cd`/`pushd`/`popd`, or a sourced file (which may
/// `cd`) beside another relative file. A grant would bind the file in the
/// working directory while the shell runs another.
fn moves_before_a_relative_file(reading: &ShellReading) -> bool {
    let relative = |file: &&CodeFile| {
        !(file.path.starts_with('/') || file.path == "~" || file.path.starts_with("~/"))
    };
    let sourced = reading
        .code_files
        .iter()
        .filter(relative)
        .filter(|file| file.kind == CodeFileKind::Sourced)
        .count();
    let others = reading.code_files.iter().filter(relative).count() - sourced;
    (reading.directory_changers > 0 && sourced + others > 0)
        || (reading.sources > 0 && others > 0)
        || (reading.sources > 1 && sourced > 0)
}

fn walk(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    match node.kind() {
        "command_substitution"
        | "process_substitution"
        | "expansion"
        | "simple_expansion"
        | "arithmetic_expansion" => {
            mark(reading, "it evaluates shell expansions");
            unbind(reading, "it evaluates shell expansions");
        },
        "function_definition" => {
            mark(reading, "it defines a shell function");
            unbind(
                reading,
                "it defines a shell function, which runs wherever it is called",
            );
        },
        // `&` is the background operator, a token of its own (`&&`, `&>`
        // and `>&` are others).
        "pipeline" | "for_statement" | "c_style_for_statement" | "while_statement" | "&" => {
            reading.concurrent = true;
        },
        "command" => read_command(node, source, reading),
        "variable_assignment" | "variable_assignments" => {
            mark(
                reading,
                "it sets shell variables, which change what later commands run",
            );
            if node.kind() == "variable_assignment" {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                    .unwrap_or_default();
                assign_variable(name, reading);
            }
        },
        "declaration_command" | "unset_command" => {
            mark(
                reading,
                "it changes shell variables, which change what later commands run",
            );
            if node.kind() == "unset_command" && names_path(node, source) {
                once(reading, SETS_PATH);
            }
        },
        "file_redirect" => read_redirect(node, source, reading),
        _ => {},
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, reading);
    }
}

const SETS_PATH: &str = "it sets PATH, which chooses the files its commands run";

/// Variables that change how output looks, never what runs.
fn inert_variable(name: &str) -> bool {
    matches!(
        name,
        "LANG" | "LANGUAGE" | "TZ" | "TERM" | "NO_COLOR" | "CI" | "COLUMNS" | "LINES"
    ) || name.starts_with("LC_")
}

/// A variable the line sets (an assignment, the step's env, which the caller
/// prefixes as assignments, or `env NAME=…`). Any but an inert one may change
/// what runs, so no lasting grant covers the line: `PATH`, `HOME` (and so
/// `~`), `BASH_ENV`, `ENV`, `NODE_OPTIONS`, `PERL5OPT`, `RUBYOPT`,
/// `PYTHONPATH`, `PYTHONSTARTUP`, `LD_PRELOAD`, `DYLD_*`, `GIT_SSH_COMMAND`,
/// `*_EDITOR`, and the rest.
fn assign_variable(name: &str, reading: &mut ShellReading) {
    if name == "PATH" || name.starts_with("PATH[") {
        once(reading, SETS_PATH);
    } else if !inert_variable(name) {
        once(
            reading,
            &format!("it sets `{name}`, which may change what runs"),
        );
    }
}

/// `unset PATH`.
fn names_path(unset: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = unset.walk();
    let named = unset
        .named_children(&mut cursor)
        .any(|child| child.utf8_text(source).is_ok_and(|text| text == "PATH"));
    named
}

fn mark(reading: &mut ShellReading, reason: &str) {
    if reading.opaque.is_none() {
        reading.opaque = Some(reason.to_string());
    }
}

fn once(reading: &mut ShellReading, reason: &str) {
    if reading.once_only.is_none() {
        reading.once_only = Some(reason.to_string());
    }
}

/// Once-only because what runs is not in the line's text at all.
fn hide(reading: &mut ShellReading, reason: &str) {
    once(reading, reason);
    if reading.hidden.is_none() {
        reading.hidden = Some(reason.to_string());
    }
}

fn unbind(reading: &mut ShellReading, reason: &str) {
    if reading.unbound.is_none() {
        reading.unbound = Some(reason.to_string());
    }
}

/// Something in the line may have written files: by code that ran
/// (`by_code`) or by a redirect or writing program.
fn record_write(reading: &mut ShellReading, by_code: bool) {
    reading.wrote = true;
    if by_code {
        reading.runners += 1;
    } else {
        reading.other_writes += 1;
    }
}

/// A file the line runs. One the line may have written first, or a device
/// or process file (`/dev/stdin`, `/proc/self/fd/0`, whose contents are the
/// shell's, not Magician's), cannot be bound.
fn push_code_file(reading: &mut ShellReading, file: CodeFile) {
    if file.path.starts_with("/dev/") || file.path.starts_with("/proc/") {
        once(
            reading,
            &format!("`{}` is a device or process file", file.path),
        );
    } else if reading.wrote {
        once(
            reading,
            &format!("it may write `{}` before running it", file.path),
        );
    }
    reading.code_files.push(file);
}

/// Programs an exact invocation grant may cover besides the runners and
/// interpreters read here and bound files: they run no other code, whatever
/// their arguments. A line on the exact invocation path that runs anything
/// else (`make`, `npm`, `git`, `sed`, an unknown runner like `stdbuf`) is
/// once-only.
const INERT_PROGRAMS: &[&str] = &[
    ":", "echo", "printf", "true", "false", "test", "ls", "cat", "head", "tail", "wc", "grep",
    "cut", "tr", "uniq", "date", "pwd", "sleep", "basename", "dirname", "mkdir", "chmod", "cp",
    "mv", "rm", "touch", "tee", "set", "exit", "wait",
];

/// Of [`INERT_PROGRAMS`], those that write files.
const WRITING_PROGRAMS: &[&str] = &["cp", "mv", "rm", "touch", "tee"];

/// Programs that run a command or script they are given, not read here:
/// a line that runs one takes the exact invocation path, where it is
/// once-only.
const CODE_LAUNCHERS: &[&str] = &[
    "stdbuf",
    "setsid",
    "arch",
    "npx",
    "bunx",
    "pnpx",
    "uvx",
    "uv",
    "tsx",
    "ts-node",
    "lua",
    "luajit",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "Rscript",
    "java",
    "jshell",
    "tclsh",
    "expect",
    "pwsh",
    "irb",
    "unbuffer",
    "open",
    "launchctl",
    "sandbox-exec",
    "systemd-run",
    "nsenter",
    "unshare",
    "taskset",
    "chrt",
    "pkexec",
    "runuser",
    "setpriv",
    "strace",
    "ltrace",
    "dtruss",
    "gdb",
    "lldb",
    "valgrind",
    "entr",
    "nodemon",
    "direnv",
    "mise",
    "asdf",
    "pipx",
    "poetry",
    "conda",
    "tmux",
    "screen",
];

/// Interpreter families: a name of one that is not exactly an interpreter
/// read here (`python3.12`, `node18`, `ksh93`) is not understood.
const INTERPRETER_FAMILIES: &[&str] = &[
    "python",
    "pypy",
    "node",
    "nodejs",
    "ruby",
    "perl",
    "php",
    "lua",
    "luajit",
    "awk",
    "gawk",
    "tclsh",
    "wish",
    "Rscript",
    "osascript",
    "deno",
    "bun",
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "fish",
    "tcsh",
    "csh",
    "pwsh",
    "java",
    "swift",
    "irb",
];

/// Extensions of files an unknown program may run (`lua x.lua`,
/// `uv run x.py`).
const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "ksh",
    "py",
    "pyc",
    "pyz",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "rb",
    "pl",
    "pm",
    "php",
    "lua",
    "awk",
    "tcl",
    "r",
    "scpt",
    "applescript",
    "ps1",
    "swift",
    "jar",
    "wasm",
    "command",
    "tool",
];

/// Folders whose programs are installed tools, judged by name like a bare
/// command (`/usr/bin/make` is `make`); a path-named program elsewhere is a
/// file the grant binds.
const SYSTEM_DIRS: &[&str] = &[
    "/bin/",
    "/sbin/",
    "/usr/",
    "/opt/",
    "/nix/",
    "/System/",
    "/Library/",
    "/Applications/",
];

fn looks_like_interpreter(base: &str) -> bool {
    let stem = base.trim_end_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == '-');
    INTERPRETER_FAMILIES.contains(&stem) && !INTERPRETERS.iter().any(|(name, _)| *name == base)
}

fn has_script_extension(arg: &str) -> bool {
    arg.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && !stem.ends_with('/')
            && SCRIPT_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    })
}

fn read_command(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    let Some(name) = node.child_by_field_name("name") else {
        mark(reading, "a command has no name");
        return;
    };
    let Some(program) = literal(name, source) else {
        mark(reading, "a command name comes from an expansion");
        // `$CMD` may be any file, whose contents no grant can bind.
        hide(reading, "a command name comes from an expansion");
        return;
    };
    if program.is_empty() {
        mark(reading, "a command has no name");
        return;
    }
    if program.contains('/') {
        // `./git` or `/tmp/x/git` is whatever file is there when it runs,
        // which a grant for `git` must not cover.
        mark(reading, &format!("`{program}` is named by a path"));
    }
    // `/bin/sh` runs as `sh` does.
    let base = basename(&program).to_string();
    let mut cursor = node.walk();
    let arg_nodes: Vec<Node<'_>> = node
        .children_by_field_name("argument", &mut cursor)
        .collect();
    let args: Vec<Option<String>> = arg_nodes.iter().map(|arg| literal(*arg, source)).collect();
    // `exec 0<./x.sh` may parse the descriptor as an argument.
    let only_descriptors = args.iter().all(|arg| {
        arg.as_deref()
            .is_some_and(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
    });
    if base == "exec" && only_descriptors && fed(node) {
        // `exec < ./x.sh; sh`: every later command reads that file.
        hide(
            reading,
            "`exec` points the shell's input at a file, which later commands read",
        );
    }
    if COMMAND_RUNNERS.contains(&base.as_str()) {
        if !(base == "command" && command_only_looks_up(&args)) {
            mark(reading, &format!("`{program}` runs another command"));
        }
    } else if base == "find" {
        // Its own arguments are tests and patterns (`-name '*.py'`), never
        // scripts it runs.
        if args.iter().flatten().any(|arg| {
            matches!(
                arg.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"
            )
        }) {
            mark(reading, "`find` runs a command for each match");
        }
    } else if let Some((name, inline)) = INTERPRETERS.iter().find(|(name, _)| *name == base) {
        let runs_inline = args
            .iter()
            .flatten()
            .any(|arg| inline_flag(name, inline, arg));
        let has_script = args.iter().any(|arg| match arg {
            Some(arg) => !arg.starts_with('-'),
            None => true,
        });
        if runs_inline {
            mark(reading, &format!("`{program}` runs inline code"));
        } else if !has_script {
            mark(reading, &format!("`{program}` reads code from its input"));
        }
    } else if !program.contains('/')
        && !INERT_PROGRAMS.contains(&base.as_str())
        && !DIRECTORY_CHANGERS.contains(&base.as_str())
    {
        // A program not read here that runs code it is given: the line
        // takes the exact invocation path, where it is once-only.
        if looks_like_interpreter(&base) || CODE_LAUNCHERS.contains(&base.as_str()) {
            mark(reading, &format!("`{program}` runs code it is given"));
        } else if let Some(script) = args.iter().flatten().find(|arg| has_script_extension(arg)) {
            mark(reading, &format!("`{program}` may run `{script}`"));
        }
    }
    let cx = Context {
        fed: reading.input_fed || fed(node),
        appended: false,
        depth: 0,
    };
    bind_command(&program, name, &args, &arg_nodes, cx, source, reading);
    reading.commands.push(ShellCommand { program, args });
}

/// How a command runs, for binding what it runs.
#[derive(Debug, Clone, Copy)]
struct Context {
    /// The line feeds its input (`… | sh`).
    fed: bool,
    /// More arguments are appended when it runs (`xargs bash`).
    appended: bool,
    /// How many runners wrap it (`sudo nohup …`).
    depth: usize,
}

impl Context {
    fn wrapped(self) -> Self {
        Self {
            depth: self.depth + 1,
            ..self
        }
    }
}

/// How inline code runs relative to the rest of the line.
#[derive(Debug, Clone, Copy, Default)]
struct InlineRun {
    /// The line feeds its shell's input.
    fed: bool,
    /// It runs after everything else (`trap … EXIT`).
    later: bool,
    /// It runs repeatedly (`watch`).
    repeats: bool,
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// A word the shell expands into names chosen when it runs (`*.sh`, `{}`
/// from `find -exec`, `{a,b}`).
fn is_pattern(word: &str) -> bool {
    word.chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | '{' | '}'))
}

/// Builtins that can set `PATH` from their input or arguments.
const PATH_SETTERS: &[&str] = &["read", "printf", "mapfile", "readarray", "getopts"];

/// What one command runs beyond the line's text, failing closed: a file it
/// runs is bound by content, a runner's or interpreter's own code is read
/// exactly, an inert program runs nothing else, and anything else leaves
/// the line unbound (or once-only, when what it runs cannot even be named).
fn bind_command(
    program: &str,
    node: Node<'_>,
    args: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) {
    if reading.hazard.is_none() {
        reading.hazard = hazard(program, args);
    }
    if is_pattern(program) {
        hide(reading, &format!("`{program}` is chosen when it runs"));
        return;
    }
    let path_named = program.contains('/');
    if path_named {
        push_code_file(
            reading,
            code_file(node, source, program, CodeFileKind::Program),
        );
    }
    let base = basename(program);
    if DIRECTORY_CHANGERS.contains(&base) {
        reading.directory_changers += 1;
    }
    if PATH_SETTERS.contains(&base) && args.iter().flatten().any(|arg| arg == "PATH") {
        once(reading, SETS_PATH);
    }
    let runs_code = if COMMAND_RUNNERS.contains(&base) {
        bind_runner(base, args, nodes, cx, source, reading);
        false
    } else if base == "find" {
        bind_find(args, nodes, cx, source, reading);
        false
    } else if let Some((name, inline)) = INTERPRETERS.iter().find(|(name, _)| *name == base) {
        bind_interpreter(name, inline, args, nodes, cx, source, reading)
    } else if INERT_PROGRAMS.contains(&base) || DIRECTORY_CHANGERS.contains(&base) {
        if WRITING_PROGRAMS.contains(&base) {
            record_write(reading, false);
        }
        false
    } else {
        // An installed tool not read here may run anything; a file named by
        // a path elsewhere is bound by content, unless it is an
        // interpreter not read here (`./python3.12 x.py`).
        if !path_named
            || SYSTEM_DIRS.iter().any(|dir| program.starts_with(dir))
            || looks_like_interpreter(base)
        {
            unbind(reading, &format!("`{program}` may run code no grant binds"));
        }
        true
    };
    if runs_code {
        record_write(reading, true);
    }
}

/// The command in `words` (a runner's or `find -exec`'s), bound like one in
/// the line.
fn bind_words(
    words: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) {
    if cx.depth >= MAX_NESTING {
        hide(reading, "its commands are wrapped too deeply");
        return;
    }
    match words.first() {
        None => {},
        Some(None) => hide(reading, "a command name comes from an expansion"),
        Some(Some(program)) => bind_command(
            program,
            nodes[0],
            &words[1..],
            &nodes[1..],
            cx,
            source,
            reading,
        ),
    }
}

/// Where a runner's command starts, past the options it takes: `flags`
/// take no value, `valued` one. `None` for any other option, or a value
/// that expands: it may change what runs (`sudo -D dir`, `env -S …`).
fn skip_options(args: &[Option<String>], flags: &[&str], valued: &[&str]) -> Option<usize> {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let arg = arg.as_deref()?;
        if arg == "--" {
            return Some(at + 1);
        }
        if !arg.starts_with('-') {
            return Some(at);
        }
        if flags.contains(&arg) {
            at += 1;
        } else if valued.contains(&arg) {
            args.get(at + 1)?.as_ref()?;
            at += 2;
        } else {
            return None;
        }
    }
    Some(at)
}

/// Why `program` needs the owner even under auto-approve: it deletes
/// (`rm -r`/`-f` or by pattern, `find -delete`, `git reset --hard`, …),
/// destroys a disk or file system, changes a whole tree's owner or mode, or
/// runs as root. Read from the words as written; an argument that expands
/// counts as one that may delete.
fn hazard(program: &str, args: &[Option<String>]) -> Option<String> {
    let base = basename(program);
    let literal = || args.iter().flatten().map(String::as_str);
    let short_has =
        |arg: &str, letters: &[char]| short_flags(arg).is_some_and(|flags| flags.contains(letters));
    let first_word = || literal().find(|arg| !arg.starts_with('-'));
    let reason = match base {
        "sudo" | "doas" | "su" | "pkexec" => format!("`{base}` runs as root"),
        "rm" => {
            let forceful = args
                .iter()
                .take_while(|arg| arg.as_deref() != Some("--"))
                .flatten()
                .any(|arg| {
                    matches!(
                        arg.as_str(),
                        "--recursive" | "--force" | "--no-preserve-root"
                    ) || short_has(arg.as_str(), &['r', 'R', 'f'])
                });
            let wide = args
                .iter()
                .any(|arg| arg.as_deref().map_or(true, is_pattern));
            if !(forceful || wide) {
                return None;
            }
            "`rm` deletes recursively, forcefully or by pattern".to_string()
        },
        "shred" | "srm" | "wipefs" | "fdisk" | "sfdisk" | "parted" => {
            format!("`{base}` destroys data")
        },
        name if name == "mkfs" || name.starts_with("mkfs.") || name.starts_with("newfs") => {
            format!("`{base}` formats a disk")
        },
        "dd" if literal().any(|arg| arg.starts_with("of=")) => {
            "`dd of=…` overwrites its target".to_string()
        },
        "diskutil"
            if first_word().is_some_and(|verb| {
                [
                    "erase",
                    "zero",
                    "secureErase",
                    "partitionDisk",
                    "reformat",
                    "randomDisk",
                ]
                .iter()
                .any(|prefix| verb.starts_with(prefix))
            }) =>
        {
            "`diskutil` erases a disk".to_string()
        },
        "find" if literal().any(|arg| arg == "-delete") => {
            "`find -delete` deletes what it matches".to_string()
        },
        "rsync"
            if literal()
                .any(|arg| arg.starts_with("--delete") || arg == "--remove-source-files") =>
        {
            "`rsync --delete` deletes files".to_string()
        },
        "chmod" | "chown" | "chgrp"
            if literal().any(|arg| arg == "--recursive" || short_has(arg, &['R'])) =>
        {
            format!("`{base} -R` changes a whole tree")
        },
        "git" => return git_hazard(args),
        _ => return None,
    };
    Some(reason)
}

/// `git` subcommands that discard work or delete history, branches or files.
fn git_hazard(args: &[Option<String>]) -> Option<String> {
    let mut at = 0;
    while let Some(Some(arg)) = args.get(at) {
        if matches!(
            arg.as_str(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            at += 2;
        } else if arg.starts_with('-') {
            at += 1;
        } else {
            break;
        }
    }
    let sub = args.get(at)?.as_deref()?;
    let rest: Vec<&str> = args[at + 1..]
        .iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let has = |names: &[&str]| rest.iter().any(|arg| names.contains(arg));
    let destroys = match sub {
        "reset" => has(&["--hard"]),
        "clean" => !has(&["-n", "--dry-run"]),
        "push" => {
            has(&["-f", "--mirror", "--delete", "-d", "--prune"])
                || rest.iter().any(|arg| {
                    arg.starts_with("--force") || arg.starts_with('+') || arg.starts_with(':')
                })
        },
        "branch" => has(&["-d", "-D", "--delete"]),
        "checkout" => has(&["--", ".", "-f", "--force"]),
        "restore" => !has(&["--staged"]) || has(&["--worktree", "-W"]),
        "stash" => rest
            .first()
            .is_some_and(|verb| matches!(*verb, "drop" | "clear")),
        "reflog" => rest
            .first()
            .is_some_and(|verb| matches!(*verb, "expire" | "delete")),
        "update-ref" => has(&["-d"]),
        "rm" | "filter-branch" | "filter-repo" => true,
        _ => false,
    };
    destroys.then(|| format!("`git {sub}` discards or deletes work"))
}

/// Under a runner the reading does not bind, any word may start a command:
/// each is checked, and so is any word that parses as a line of its own
/// (`ssh host 'rm -rf x'`).
fn scan_hazards(args: &[Option<String>], reading: &mut ShellReading) {
    for (at, arg) in args.iter().enumerate() {
        if reading.hazard.is_some() {
            return;
        }
        let Some(word) = arg.as_deref() else {
            continue;
        };
        reading.hazard = hazard(word, &args[at + 1..]);
        if reading.hazard.is_none()
            && word.contains(char::is_whitespace)
            && reading.depth + 1 < MAX_NESTING
            && syntax_is_valid(word)
        {
            reading.hazard = read_at(word, ShellReading::at_depth(reading.depth + 1, false)).hazard;
        }
    }
}

/// `command -v`/`-V` (with or without `-p`) only prints how each name
/// resolves: it runs nothing.
fn command_only_looks_up(args: &[Option<String>]) -> bool {
    let mut looks_up = false;
    for arg in args {
        let Some(flags) = arg.as_deref().and_then(|arg| arg.strip_prefix('-')) else {
            break;
        };
        if flags.is_empty() || !flags.chars().all(|ch| matches!(ch, 'p' | 'v' | 'V')) {
            return false;
        }
        looks_up |= flags.contains(|ch| ch == 'v' || ch == 'V');
    }
    looks_up
}

/// What a runner runs. Only runners whose command (or command string) is
/// read exactly are bound; any other, or an option outside the small set
/// each one is known to take, makes the line once-only.
fn bind_runner(
    runner: &str,
    args: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) {
    let inner = cx.wrapped();
    let words: Option<Vec<&str>> = args.iter().map(|arg| arg.as_deref()).collect();
    let options = |reading: &mut ShellReading| {
        once(
            reading,
            &format!("`{runner}` takes an option that may change what runs"),
        );
        scan_hazards(args, reading);
    };
    let fed = InlineRun {
        fed: cx.fed,
        ..InlineRun::default()
    };
    let start = match runner {
        "source" | "." => {
            reading.sources += 1;
            match args.first() {
                None => {},
                Some(None) => hide(
                    reading,
                    &format!("`{runner}` reads a file named by an expansion"),
                ),
                // Looked up on the command's own PATH, which the line
                // cannot show.
                Some(Some(path)) if !path.contains('/') => once(
                    reading,
                    &format!("`{runner} {path}` looks the file up on PATH (write `./{path}`)"),
                ),
                Some(Some(path)) if is_pattern(path) || path.starts_with('-') => {
                    hide(
                        reading,
                        &format!("`{runner} {path}` is chosen when it runs"),
                    );
                },
                Some(Some(path)) => {
                    push_code_file(
                        reading,
                        code_file(nodes[0], source, path, CodeFileKind::Sourced),
                    );
                    record_write(reading, true);
                },
            }
            return;
        },
        "eval" => {
            match words {
                Some(words) => read_inline(&words.join(" "), runner, fed, reading),
                None => hide(reading, "`eval` runs code from an expansion"),
            }
            return;
        },
        "command" if command_only_looks_up(args) => return,
        "exec" | "command" | "builtin" | "nohup" | "time" | "ionice" => {
            skip_options(args, &[], &[])
        },
        "nice" => skip_options(args, &[], &["-n"]),
        // `timeout DURATION command …`
        "timeout" => skip_options(args, &[], &[]).map(|at| at + 1),
        "caffeinate" => skip_options(args, &["-d", "-i", "-m", "-s", "-u"], &[]),
        "sudo" => skip_options(
            args,
            &["-E", "-H", "-n", "-k", "-S", "-A", "-b"],
            &["-u", "-g"],
        ),
        "doas" => skip_options(args, &["-n"], &["-u"]),
        "xargs" => {
            // Its command runs repeatedly, with arguments from its input: an
            // interpreter there runs a script chosen when it runs.
            reading.concurrent = true;
            match skip_options(args, &["-0", "-r", "-t", "-x"], &["-n", "-P", "-L"]) {
                Some(at) => bind_words(
                    &args[at..],
                    &nodes[at..],
                    Context {
                        fed: false,
                        appended: true,
                        ..inner
                    },
                    source,
                    reading,
                ),
                None => options(reading),
            }
            return;
        },
        "env" => {
            let mut at = 0;
            while let Some(Some(arg)) = args.get(at) {
                if arg.starts_with('-') {
                    options(reading);
                    return;
                }
                match arg.split_once('=') {
                    Some((name, _)) => {
                        assign_variable(name, reading);
                        at += 1;
                    },
                    None => break,
                }
            }
            Some(at)
        },
        // `su [user] -c 'code'`; anything else (a login shell, options) runs
        // what the line does not show.
        "su" => {
            let code = match words.as_deref() {
                Some(["-c", code]) => Some(*code),
                Some([user, "-c", code] | ["-c", code, user]) if !user.starts_with('-') => {
                    Some(*code)
                },
                _ => None,
            };
            match code {
                Some(code) => read_inline(code, runner, fed, reading),
                None => options(reading),
            }
            return;
        },
        // `watch command …` runs the words through `sh -c`, repeatedly.
        "watch" => {
            match words {
                Some(words) if !words.is_empty() && words.iter().all(|w| !w.starts_with('-')) => {
                    read_inline(
                        &words.join(" "),
                        runner,
                        InlineRun {
                            repeats: true,
                            ..InlineRun::default()
                        },
                        reading,
                    );
                },
                _ => options(reading),
            }
            return;
        },
        // `flock FILE -c 'code'` or `flock FILE command …`.
        "flock" => {
            match words.as_deref() {
                Some([file, "-c", code]) if !file.starts_with('-') => {
                    read_inline(code, runner, fed, reading);
                },
                Some([file, command, ..])
                    if !file.starts_with('-') && !command.starts_with('-') =>
                {
                    bind_words(&args[1..], &nodes[1..], inner, source, reading);
                },
                _ => options(reading),
            }
            return;
        },
        // `trap 'code' SIGNAL …`, run after everything else may have
        // written; bare `trap` lists traps.
        "trap" => {
            match args.first() {
                None => {},
                Some(Some(code)) if !code.starts_with('-') => {
                    read_inline(
                        code,
                        runner,
                        InlineRun {
                            later: true,
                            ..InlineRun::default()
                        },
                        reading,
                    );
                },
                _ => options(reading),
            }
            return;
        },
        // `ssh` runs remotely, `chroot` under another root, `script` and
        // `parallel` through their own parsing, `hash`/`alias` rename
        // commands: none is bound.
        _ => {
            once(
                reading,
                &format!("`{runner}` runs a command that cannot be bound"),
            );
            scan_hazards(args, reading);
            return;
        },
    };
    match start {
        Some(at) if at <= args.len() => {
            bind_words(&args[at..], &nodes[at..], inner, source, reading);
        },
        _ => options(reading),
    }
}

/// `find`'s `-exec`/`-ok` commands, each bound like one in the line; `{}`
/// there is a match chosen when it runs. `-execdir`/`-okdir` run in each
/// match's folder, which no grant can bind.
fn bind_find(
    args: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) {
    if args.iter().flatten().any(|arg| {
        matches!(
            arg.as_str(),
            "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
        )
    }) {
        record_write(reading, false);
    }
    let mut at = 0;
    while at < args.len() {
        match args[at].as_deref() {
            Some("-exec" | "-ok") => {
                // Once per match.
                reading.concurrent = true;
                let end = args[at + 1..]
                    .iter()
                    .position(|arg| matches!(arg.as_deref(), Some(";" | "+")))
                    .map_or(args.len(), |end| at + 1 + end);
                bind_words(
                    &args[at + 1..end],
                    &nodes[at + 1..end],
                    Context {
                        fed: false,
                        ..cx.wrapped()
                    },
                    source,
                    reading,
                );
                at = end + 1;
            },
            Some("-execdir" | "-okdir") => {
                once(
                    reading,
                    "`find -execdir` runs its command in each match's folder",
                );
                scan_hazards(&args[at + 1..], reading);
                return;
            },
            _ => at += 1,
        }
    }
}

/// A file the line runs, named by `node` (whose literal text is `path`).
fn code_file(node: Node<'_>, source: &[u8], path: &str, kind: CodeFileKind) -> CodeFile {
    // Only an unquoted leading `~` expands to a home folder.
    let expands = node.utf8_text(source).is_ok_and(|raw| raw.starts_with('~'));
    let path = if path.starts_with('~') && !expands {
        format!("./{path}")
    } else {
        path.to_string()
    };
    CodeFile { path, kind }
}

/// Whether `arg` makes interpreter `name` run inline code: one of its inline
/// flags, or for a shell a cluster of short flags with `c` (`-lc`, `-ec`).
fn inline_flag(name: &str, inline: &[&str], arg: &str) -> bool {
    inline.contains(&arg)
        || (SHELLS.contains(&name) && short_flags(arg).is_some_and(|flags| flags.contains('c')))
}

/// The letters of a short-flag cluster (`-xs` → `xs`).
fn short_flags(arg: &str) -> Option<&str> {
    arg.strip_prefix('-')
        .filter(|flags| !flags.is_empty() && !flags.starts_with('-'))
}

/// How many values a non-shell interpreter option takes, for the few known
/// to run no other code; `None` for any other (`python3 -m`, `-W`,
/// `node -r`, `perl -I`), which makes the line once-only.
fn option_arity(name: &str, arg: &str) -> Option<usize> {
    match name {
        "python" | "python3" => matches!(arg, "-u" | "-B" | "-I" | "-E").then_some(0),
        _ => None,
    }
}

/// What interpreter `name` runs beyond the line's text, failing closed, and
/// whether it runs code itself (and so may write). Every option before its
/// script must be one known to run no other code. Its literal inline code is
/// the line's text; a script is a file to bind. Code fed to it from the
/// line, or a script chosen when it runs, cannot be bound.
fn bind_interpreter(
    name: &str,
    inline: &[&str],
    args: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) -> bool {
    if SHELLS.contains(&name) {
        return bind_shell(name, inline, args, nodes, cx, source, reading);
    }
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let Some(arg) = arg.as_deref() else {
            hide(
                reading,
                &format!("`{name}` takes an argument from an expansion"),
            );
            return false;
        };
        if inline.contains(&arg) {
            if matches!(args.get(at + 1), Some(None)) {
                hide(
                    reading,
                    &format!("`{name}` runs inline code from an expansion"),
                );
                return false;
            }
            // Literal code: the line's text.
            return true;
        }
        if arg == "-" {
            return code_from_input(name, cx, reading);
        }
        if !arg.starts_with('-') {
            break;
        }
        match option_arity(name, arg) {
            Some(0) => at += 1,
            Some(_) => match args.get(at + 1) {
                Some(Some(_)) => at += 2,
                _ => {
                    hide(
                        reading,
                        &format!("`{name} {arg}` takes a value from an expansion"),
                    );
                    return false;
                },
            },
            None => {
                once(
                    reading,
                    &format!("`{name}` takes `{arg}`, which may run other code"),
                );
                return false;
            },
        }
    }
    match args.get(at) {
        None => code_from_input(name, cx, reading),
        // `deno run x.ts`, `bun x`: a subcommand, not a script.
        Some(Some(script)) if matches!(name, "deno" | "bun") => {
            once(reading, &format!("`{name} {script}` may run a subcommand"));
            false
        },
        Some(Some(script)) if is_pattern(script) => {
            hide(
                reading,
                &format!("`{name} {script}` runs a script chosen when it runs"),
            );
            false
        },
        Some(Some(script)) => {
            push_code_file(
                reading,
                code_file(nodes[at], source, script, CodeFileKind::Script),
            );
            true
        },
        Some(None) => {
            hide(
                reading,
                &format!("`{name}` runs a script named by an expansion"),
            );
            false
        },
    }
}

/// Code a non-shell interpreter reads from its input: the invocation's own
/// standard input is bound with it; code the line feeds it, or a script
/// appended when it runs, is not.
fn code_from_input(name: &str, cx: Context, reading: &mut ShellReading) -> bool {
    if cx.appended {
        hide(
            reading,
            &format!("`{name}` runs a script chosen when it runs"),
        );
    } else if cx.fed {
        hide(reading, &format!("`{name}` runs code fed to its input"));
    }
    true
}

/// A shell's options, then its inline code or script. Options may follow
/// `-c` (`bash -c -e 'code'`); `--` or `-` ends them; `+x` turns one off.
/// Only `-c`, `-s`, `-e`, `-u`, `-x`, `-v`, `-o NAME`, `--norc` and
/// `--noprofile` are known to load nothing; a login or interactive shell
/// (`-l`, `-i`), `--rcfile`, `-O` and the rest are once-only. A shell reading
/// code from its input may run files no grant binds, so its line is
/// unbound, or once-only when the line feeds that input.
fn bind_shell(
    name: &str,
    inline: &[&str],
    args: &[Option<String>],
    nodes: &[Node<'_>],
    cx: Context,
    source: &[u8],
    reading: &mut ShellReading,
) -> bool {
    let mut inline_code = false;
    let mut from_input = false;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let Some(arg) = arg.as_deref() else {
            hide(
                reading,
                &format!("`{name}` takes an argument from an expansion"),
            );
            return false;
        };
        if arg == "--" || arg == "-" {
            at += 1;
            break;
        }
        if inline.contains(&arg) {
            inline_code = true;
            at += 1;
            continue;
        }
        if matches!(arg, "--norc" | "--noprofile") {
            at += 1;
            continue;
        }
        let (on, flags) = match (arg.strip_prefix('-'), arg.strip_prefix('+')) {
            (Some(flags), _) => (true, flags),
            (None, Some(flags)) => (false, flags),
            (None, None) => break,
        };
        if flags == "o" {
            match args.get(at + 1) {
                Some(Some(_)) => {
                    at += 2;
                    continue;
                },
                _ => {
                    hide(
                        reading,
                        &format!("`{name} {arg}` takes a value from an expansion"),
                    );
                    return false;
                },
            }
        }
        let known = if on { "csuexv" } else { "uexv" };
        if flags.is_empty() || !flags.chars().all(|flag| known.contains(flag)) {
            once(
                reading,
                &format!("`{name} {arg}` may run startup files or other code"),
            );
            return false;
        }
        if on {
            inline_code |= flags.contains('c');
            from_input |= flags.contains('s');
        }
        at += 1;
    }
    if inline_code {
        match args.get(at) {
            Some(Some(code)) => read_inline(
                code,
                name,
                InlineRun {
                    fed: cx.fed,
                    ..InlineRun::default()
                },
                reading,
            ),
            Some(None) => hide(
                reading,
                &format!("`{name}` runs inline code from an expansion"),
            ),
            None => {},
        }
        return false;
    }
    if from_input {
        return shell_reads_input(name, cx, reading);
    }
    match args.get(at) {
        None => shell_reads_input(name, cx, reading),
        Some(Some(script)) if is_pattern(script) => {
            hide(
                reading,
                &format!("`{name} {script}` runs a script chosen when it runs"),
            );
            false
        },
        Some(Some(script)) => {
            push_code_file(
                reading,
                code_file(nodes[at], source, script, CodeFileKind::Script),
            );
            true
        },
        Some(None) => {
            hide(
                reading,
                &format!("`{name}` runs a script named by an expansion"),
            );
            false
        },
    }
}

fn shell_reads_input(name: &str, cx: Context, reading: &mut ShellReading) -> bool {
    if cx.appended || cx.fed {
        hide(reading, &format!("`{name}` runs code fed to its input"));
    } else {
        unbind(
            reading,
            &format!("`{name}` runs shell code from its standard input, which may run files no grant binds"),
        );
    }
    true
}

/// Code a shell runs from a literal string (`bash -c '…'`, `eval '…'`), read
/// like the line itself, after what the line may already have written.
fn read_inline(code: &str, runner: &str, run: InlineRun, reading: &mut ShellReading) {
    if code.trim().is_empty() {
        return;
    }
    if reading.depth + 1 >= MAX_NESTING {
        hide(reading, "its inline code nests too deeply");
        return;
    }
    if !syntax_is_valid(code) {
        hide(
            reading,
            &format!("`{runner}` runs inline code that does not parse"),
        );
        return;
    }
    let mut seed = ShellReading::at_depth(reading.depth + 1, run.fed);
    seed.wrote = reading.wrote || run.later;
    seed.concurrent = run.repeats;
    let inner = read_at(code, seed);
    if let Some(reason) = &inner.once_only {
        once(reading, reason);
    }
    if let Some(reason) = &inner.unbound {
        unbind(reading, reason);
    }
    if let Some(reason) = &inner.hidden {
        hide(reading, reason);
    }
    if reading.hazard.is_none() {
        reading.hazard.clone_from(&inner.hazard);
    }
    reading.code_files.extend(inner.code_files);
    reading.directory_changers += inner.directory_changers;
    reading.sources += inner.sources;
    reading.wrote |= inner.wrote;
    reading.other_writes += inner.other_writes;
    reading.runners += inner.runners;
    reading.concurrent |= inner.concurrent;
}

/// Whether the line feeds `command`'s input: a pipe into it, or a heredoc,
/// herestring or file redirect on it or on a statement around it.
fn fed(command: Node<'_>) -> bool {
    let mut cursor = command.walk();
    if command
        .children_by_field_name("redirect", &mut cursor)
        .any(feeds_input)
    {
        return true;
    }
    let mut node = command;
    while let Some(parent) = node.parent() {
        // `a | b`: `b` follows a `|` (in a heredoc's `<<EOF | sh` too).
        if parent.kind() == "pipeline" && node.prev_sibling().is_some() {
            return true;
        }
        if parent.kind() == "redirected_statement" {
            let mut cursor = parent.walk();
            if parent.children(&mut cursor).any(feeds_input) {
                return true;
            }
        }
        node = parent;
    }
    false
}

fn feeds_input(node: Node<'_>) -> bool {
    match node.kind() {
        "heredoc_redirect" | "herestring_redirect" => true,
        "file_redirect" => matches!(redirect_operator(node), Some("<" | "<&")),
        _ => false,
    }
}

/// A redirect's operator (`>`, `<`, `>&`, …): its first unnamed child.
fn redirect_operator(node: Node<'_>) -> Option<&'static str> {
    let mut cursor = node.walk();
    let operator = node
        .children(&mut cursor)
        .find(|child| !child.is_named())
        .map(|child| child.kind());
    operator
}

/// An output redirect (`>`, `>>`, `>|`, `&>`, `&>>`, `>& file`) writes a
/// file the grant never named: the line is opaque unless the target is a
/// harmless device or another descriptor (`2>&1`, `>&-`). Input redirects
/// and heredocs only read.
fn read_redirect(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    let Some(operator) = redirect_operator(node) else {
        return;
    };
    if !matches!(operator, ">" | ">>" | ">|" | "&>" | "&>>" | ">&") {
        return;
    }
    let mut cursor = node.walk();
    let target: Option<String> = node
        .children_by_field_name("destination", &mut cursor)
        .map(|part| literal(part, source))
        .collect();
    let harmless = target.as_deref().is_some_and(|target| {
        HARMLESS_WRITE_TARGETS.contains(&target)
            || (operator == ">&" && (target == "-" || target.chars().all(|ch| ch.is_ascii_digit())))
    });
    if !harmless {
        mark(reading, "it writes a file through a redirect");
        record_write(reading, false);
    }
}

/// The literal text of a word node, quotes removed; `None` when any part of
/// it expands at run time.
fn literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "command_name" => {
            let child = node.named_child(0)?;
            literal(child, source)
        },
        "word" | "number" => node.utf8_text(source).ok().map(unescape),
        "raw_string" => {
            let text = node.utf8_text(source).ok()?;
            Some(
                text.trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .to_string(),
            )
        },
        "string" => {
            // Double quotes: literal only when no part expands.
            let mut cursor = node.walk();
            let mut out = String::new();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "\"" => {},
                    "string_content" => out.push_str(child.utf8_text(source).ok()?),
                    _ => return None,
                }
            }
            Some(out)
        },
        "concatenation" => {
            let mut cursor = node.walk();
            let mut out = String::new();
            for child in node.named_children(&mut cursor) {
                out.push_str(&literal(child, source)?);
            }
            Some(out)
        },
        _ => None,
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(command: &str) -> Vec<String> {
        read(command)
            .commands
            .into_iter()
            .map(|c| c.program)
            .collect()
    }

    #[test]
    fn complex_valid_shell_is_askable_without_evaluating_it() {
        for command in [
            r#"printf '%s' "${VALUE}""#,
            "echo $(date)",
            "cat <(printf hi)",
        ] {
            assert!(syntax_is_valid(command), "{command}");
            assert!(read(command).opaque.is_some(), "{command}");
        }
        assert!(!syntax_is_valid("echo 'unterminated"));
        assert!(!syntax_is_valid(""));
        assert!(read("echo '$(literal)'").opaque.is_none());
    }

    #[test]
    fn every_command_in_the_line_is_read() {
        assert_eq!(
            programs("cd repo && npm install --save foo | tee log; ls -la"),
            ["cd", "npm", "tee", "ls"]
        );
        assert_eq!(programs("echo $(git rev-parse HEAD)"), ["echo", "git"]);
        assert_eq!(
            programs("(cd x; make) || echo failed"),
            ["cd", "make", "echo"]
        );
        assert_eq!(programs("for f in *.md; do wc -l \"$f\"; done"), ["wc"]);
        assert_eq!(
            programs("/usr/bin/git status"),
            ["/usr/bin/git"],
            "the name as written"
        );
        assert!(read("echo 'a | b && c'").opaque.is_none());
        assert_eq!(programs("echo 'a | b && c'"), ["echo"]);
    }

    #[test]
    fn exact_and_prefix_scopes_need_literal_words() {
        let npm = &read("npm install --save foo").commands[0];
        assert_eq!(npm.exact().as_deref(), Some("npm install --save foo"));
        assert_eq!(npm.prefix().as_deref(), Some("npm install"));
        let git = &read("git -C repo status").commands[0];
        assert_eq!(git.prefix(), None, "an option first: no prefix scope");
        let rm = &read("rm \"$TARGET\"").commands[0];
        assert_eq!(rm.exact(), None, "an expanding argument has no exact scope");
        assert_eq!(
            read("echo \"hi there\"").commands[0].exact().as_deref(),
            Some("echo hi there")
        );
    }

    #[test]
    fn what_cannot_be_named_is_opaque() {
        for line in [
            "$CMD --flag",
            "eval \"$X\"",
            "source env.sh",
            "f() { rm -rf x; }; f",
            "bash -c 'rm -rf x'",
            "python3 -c 'print(1)'",
            "node -e 'x'",
            "curl -s x | sh",
            "ls | xargs rm",
            "find . -name '*.tmp' -exec rm {} \\;",
            "sudo brew install jq",
            "env FOO=1 make",
            "timeout 5 make",
            "ssh host ls",
            "echo $(date)",
            r#"printf '%s' "${VALUE}""#,
            "echo 'unterminated",
            "hash -p /tmp/x/git git",
            "alias git=/tmp/x/git",
        ] {
            assert!(read(line).opaque.is_some(), "{line}");
        }
        for line in [
            "git clone x",
            "python3 build.py",
            "bash setup.sh",
            "npm run build",
        ] {
            assert!(
                read(line).opaque.is_none(),
                "{line}: {:?}",
                read(line).opaque
            );
        }
    }

    /// A grant names a program; `./git` or `/tmp/x/git` is whatever file
    /// sits at that path, so only an exact invocation grant may cover it.
    #[test]
    fn a_command_named_by_a_path_is_opaque() {
        for line in [
            "./git status",
            "/tmp/x/git status",
            "../bin/git status",
            "~/bin/git status",
            "echo hi && ./git status",
        ] {
            let reading = read(line);
            assert!(reading.opaque.is_some(), "{line}");
            assert!(
                reading
                    .commands
                    .iter()
                    .all(|command| command.program != "git"),
                "{line}: kept as written"
            );
        }
        assert_eq!(read("./git status").commands[0].program, "./git");
        assert_eq!(
            read("./git status").commands[0].exact().as_deref(),
            Some("./git status")
        );
    }

    /// Variables change what later commands run (`PATH`, `GIT_SSH_COMMAND`,
    /// `DYLD_*`): any assignment, `export`/`declare`/`unset`, anywhere in the
    /// line, makes it opaque. A loop variable is not an assignment.
    #[test]
    fn setting_variables_is_opaque() {
        for line in [
            "FOO=1 /usr/bin/git status",
            "PATH=. git status",
            "PATH=.:$PATH git status",
            "GIT_SSH_COMMAND='sh -c x' git fetch",
            "export PATH=/tmp/x:$PATH; git status",
            "export PATH",
            "X=1; git status",
            "X=1",
            "declare -x PATH=/tmp/x",
            "readonly X=1 && git status",
            "unset PATH; git status",
            "git status && (PATH=. git log)",
            "echo $(PATH=. git status)",
        ] {
            assert!(read(line).opaque.is_some(), "{line}");
        }
        assert!(read("for f in a b; do echo literal; done").opaque.is_none());
        assert!(read("for f in a b; do echo \"$f\"; done").opaque.is_some());
        assert!(
            read("make CC=gcc --jobs=4").opaque.is_none(),
            "an argument with `=` is not an assignment"
        );
    }

    /// A write through a redirect touches a file no grant named; reads,
    /// heredoc input and descriptor duplication do not.
    #[test]
    fn a_redirected_write_is_opaque() {
        for line in [
            "echo hi > ~/.zshrc",
            "echo hi >> ~/.zshrc",
            "echo hi >| out.txt",
            "make &> build.log",
            "make &>> build.log",
            "make 2> err.log",
            "echo hi >& out.txt",
            "echo hi > \"$OUT\"",
            "cat > ~/Library/LaunchAgents/x.plist <<EOF\nx\nEOF",
            "cat <<EOF > out.txt\nx\nEOF",
            "(echo hi) > out.txt",
            "{ echo hi; } >> out.txt",
            "echo $(echo hi > out.txt)",
        ] {
            assert!(read(line).opaque.is_some(), "{line}");
        }
        for line in [
            "make 2>&1",
            "make 2>&1 | tee",
            "make > /dev/null",
            "make >/dev/null 2>&1",
            "make &> /dev/null",
            "echo hi >&2",
            "echo hi > /dev/stderr",
            "make 2>&-",
            "wc -l < input.txt",
            "cat <<EOF\nx\nEOF",
            "grep x <<< \"text\"",
        ] {
            assert!(
                read(line).opaque.is_none(),
                "{line}: {:?}",
                read(line).opaque
            );
        }
    }

    fn file(path: &str, kind: CodeFileKind) -> CodeFile {
        CodeFile {
            path: path.to_string(),
            kind,
        }
    }

    /// The files a line runs, which an exact invocation grant binds by
    /// content: the shell runs each by this name.
    #[test]
    fn files_the_line_runs_are_named_for_binding() {
        use CodeFileKind::{Program, Script, Sourced};
        for (line, files) in [
            ("./run.sh --fast", vec![file("./run.sh", Program)]),
            ("bin/x", vec![file("bin/x", Program)]),
            ("~/bin/x", vec![file("~/bin/x", Program)]),
            ("'~/x/run.sh'", vec![file("./~/x/run.sh", Program)]),
            ("source ./env.sh", vec![file("./env.sh", Sourced)]),
            (". ./env.sh", vec![file("./env.sh", Sourced)]),
            (
                "LANG=C bash -e setup.sh arg",
                vec![file("setup.sh", Script)],
            ),
            ("bash -c -e './evil.sh'", vec![file("./evil.sh", Program)]),
            ("bash -c -- './evil.sh'", vec![file("./evil.sh", Program)]),
            ("bash - ./evil.sh", vec![file("./evil.sh", Script)]),
            ("bash +x ./evil.sh", vec![file("./evil.sh", Script)]),
            ("bash +o posix ./evil.sh", vec![file("./evil.sh", Script)]),
            (
                "chmod +x ./run.sh && ./run.sh",
                vec![file("./run.sh", Program)],
            ),
            ("./run.sh > out.log", vec![file("./run.sh", Program)]),
            ("./run.sh 2>&1 | grep x", vec![file("./run.sh", Program)]),
            (
                "bash -e -o pipefail -- ./x.sh",
                vec![file("./x.sh", Script)],
            ),
            (
                "sudo bash -o pipefail ./deploy.sh",
                vec![file("./deploy.sh", Script)],
            ),
            ("python3 -u x.py", vec![file("x.py", Script)]),
            (
                "cat data.json | python3 parse.py",
                vec![file("parse.py", Script)],
            ),
            ("sudo ./run.sh", vec![file("./run.sh", Program)]),
            (
                "sudo /bin/bash ./run.sh",
                vec![file("/bin/bash", Program), file("./run.sh", Script)],
            ),
            ("sudo -u bob -E ./run.sh", vec![file("./run.sh", Program)]),
            ("timeout 5 ./run.sh", vec![file("./run.sh", Program)]),
            ("nice -n 5 ./run.sh", vec![file("./run.sh", Program)]),
            ("env LANG=C ./run.sh", vec![file("./run.sh", Program)]),
            ("nohup ./run.sh", vec![file("./run.sh", Program)]),
            ("ls | xargs ./fix.sh", vec![file("./fix.sh", Program)]),
            (
                "find . -name x -exec ./fix.sh {} \\;",
                vec![file("./fix.sh", Program)],
            ),
            (
                "bash -c './run.sh && make' > log",
                vec![file("./run.sh", Program)],
            ),
            ("eval './run.sh'", vec![file("./run.sh", Program)]),
            ("su -c './run.sh'", vec![file("./run.sh", Program)]),
            ("watch ./run.sh", vec![file("./run.sh", Program)]),
            ("flock /tmp/lock ./run.sh", vec![file("./run.sh", Program)]),
            (
                "flock /tmp/lock -c './run.sh'",
                vec![file("./run.sh", Program)],
            ),
            ("python3 -c 'print(1)'", vec![]),
            (
                "/bin/sh ./x.sh",
                vec![file("/bin/sh", Program), file("./x.sh", Script)],
            ),
            ("git status > out.txt", vec![]),
        ] {
            let reading = read(line);
            assert_eq!(reading.code_files, files, "{line}");
            assert!(
                reading.once_only.is_none(),
                "{line}: {:?}",
                reading.once_only
            );
        }
    }

    /// Code in neither the line's text nor a file it names cannot be bound.
    #[test]
    fn code_in_neither_the_line_nor_a_file_is_once_only() {
        for line in [
            "curl -s x | sh",
            "curl -s x | bash -s -- --flag",
            "curl -s x | python3",
            "curl -s x | python3 -",
            "curl -s x | sudo bash",
            "curl -s x | /bin/sh",
            "curl -s x | (sh)",
            "curl -s x | bash -c 'sh'",
            "cat <<EOF | sh\necho hi\nEOF",
            "sh <<EOF\necho hi\nEOF",
            "sh < script.sh",
            "sh <<< 'echo hi'",
            "$CMD --flag",
            "\"$CMD\" --flag",
            "$(which deploy) now",
            "sudo $CMD",
            "timeout 5 \"$RUN\"",
            "bash -c \"$X\"",
            "bash -lc \"$X\"",
            "bash \"$SCRIPT\"",
            "bash <(curl -s x)",
            "source \"$ENV_FILE\"",
            "eval \"$X\"",
            "eval 'echo $(curl -s x) | sh'",
            "cd sub && ./run.sh",
            "cd sub; bash setup.sh",
            "pushd sub && . ./env.sh",
            ". ./env.sh && ./run.sh",
            ". ./a.sh; . ./b.sh",
            "bash -c 'cd sub && ./run.sh'",
            "builtin cd sub && ./run.sh",
            "echo 'unterminated",
            // Another interpreter's inline code from an expansion.
            "python3 -c \"$(curl -s https://x)\"",
            "node -e \"$X\"",
            "perl -e \"$X\"",
            "ruby -e \"$X\"",
            "osascript -e \"$X\"",
            // Runners that run a command string the reading cannot bind.
            "su -c 'curl x | sh'",
            "su - bob -c './run.sh'",
            "ssh host 'curl x | sh'",
            "watch 'curl x | sh'",
            "watch -n 1 ./run.sh",
            "script -qc 'curl x | sh' /dev/null",
            "flock f -c 'curl x | sh'",
            "trap 'curl x | sh' EXIT",
            "env -S 'sh -c x'",
            "parallel ::: 'curl x | sh'",
            "ls | xargs sh -c 'curl x | sh'",
            "hash -p /tmp/x/git git",
            "command -x ./run.sh",
            "command $X -v git",
            "alias git=/tmp/x/git",
            // An option before the script that may run other code.
            "python3 -W ignore x.py",
            "node -r dotenv/config app.js",
            "perl -I lib x.pl",
            "deno run x.ts",
            "bun x.ts",
            "python3 -m mod",
            "bash -l ./x.sh",
            "bash -lc './run.sh'",
            "bash -i ./x.sh",
            "nice -5 ./run.sh",
            "timeout -s KILL 5 ./run.sh",
            "xargs -I{} ./run.sh {}",
            // A script or program chosen when it runs.
            "bash ./scripts/*.sh",
            "./scripts/*.sh",
            "find . -exec bash {} \\;",
            "find . -exec {} \\;",
            "find . -execdir ./fix.sh {} \\;",
            "ls | xargs bash",
            "ls | xargs python3",
            // Another directory or root.
            "env -C /tmp ./run.sh",
            "sudo -D dir ./x",
            "chroot /x ./run.sh",
            "ssh host ./x",
            // PATH, set by the line or searched for a sourced file.
            "source env.sh",
            ". env.sh",
            "PATH=. run.sh",
            "PATH=/tmp/x:$PATH deploy",
            "export PATH=.; deploy",
            "env PATH=. run.sh",
            "unset PATH; ls",
            "read PATH < f; deploy",
            // A file the line may write before (or while) running it.
            "curl -so run.sh https://x && ./run.sh",
            "curl -s x > run.sh; ./run.sh",
            "echo x > run.sh && bash run.sh",
            "cp a.sh run.sh && ./run.sh",
            "./gen.sh && ./run.sh",
            "./run.sh & cp x run.sh",
            "./run.sh | tee run.sh",
            "./a.sh | ./b.sh",
            "for i in 1 2; do ./run.sh; cp x run.sh; done",
            "trap './cleanup.sh' EXIT",
            // The shell's own input, or a device or process file, as code.
            "exec < ./evil.sh; sh",
            "exec 0<./evil.sh && bash",
            "bash /dev/stdin",
            "bash /dev/fd/3",
            "bash /proc/self/fd/0",
            "source /dev/stdin",
            "/dev/fd/3",
            // Shell options that load other code, or code from an expansion
            // after `-c`.
            "bash +l ./x.sh",
            "bash -O extglob ./x.sh",
            "bash --rcfile x ./y.sh",
            "bash -c -e \"$X\"",
            // Variables that change what runs (also in the step's env).
            "BASH_ENV=./x.sh bash -c 'ls'",
            "ENV=./x.sh sh -c 'ls'",
            "NODE_OPTIONS='--require ./x.js' node app.js",
            "PERL5OPT=-Mx perl x.pl",
            "RUBYOPT=-rx ruby x.rb",
            "PYTHONPATH=. python3 x.py",
            "PYTHONSTARTUP=x python3",
            "LD_PRELOAD=./x.so ls",
            "DYLD_INSERT_LIBRARIES=./x.dylib ls",
            "GIT_SSH_COMMAND='sh -c x' git fetch",
            "GIT_EDITOR=./x git commit",
            "X=1 git status",
            "export BASH_ENV=./x; bash ./run.sh",
            "env NODE_OPTIONS=x node app.js",
            "env 'K-X=1' ./run.sh",
            "HOME=/tmp/x ~/run.sh",
            "env HOME=/tmp/x ~/run.sh",
        ] {
            assert!(read(line).once_only.is_some(), "{line}");
        }
        for line in [
            "./run.sh",
            ". ./env.sh",
            "source ./env.sh",
            "bash setup.sh",
            "python3 -c 'print(1)'",
            "node -e 'console.log(1)'",
            "sudo bash -o pipefail ./deploy.sh",
            "trap '' INT",
            "python3",
            "cd /abs && /abs/run.sh",
            "cd sub && make",
            "LANG=C git status",
            "LANG=C ./run.sh",
            "LC_ALL=C bash run.sh",
            "TZ=UTC ./run.sh",
            "chmod +x ./run.sh && ./run.sh",
            "set -e; ./run.sh",
            "./run.sh > out.log",
            "./run.sh 2>&1 | grep x",
            "mkdir -p out && ./run.sh",
            "for f in a b; do ./run.sh; done",
            "./run.sh && ls",
            "sudo brew install jq",
            "ls | xargs rm",
            "find . -name '*.tmp' -exec rm {} \\;",
            "cat <<EOF\nx\nEOF",
            // A lookup runs nothing.
            "command -v ocr || true",
            "command -pV git",
            "command -v \"$TOOL\"",
        ] {
            assert!(
                read(line).once_only.is_none(),
                "{line}: {:?}",
                read(line).once_only
            );
        }
    }

    /// Auto-approve still asks for what deletes or runs as root, wherever the
    /// line shows it (under runners, `find -exec`, inline code, `ssh`), and
    /// for code the line does not show; other once-only lines are not hidden.
    #[test]
    fn deletes_root_and_unseen_code_are_read() {
        for line in [
            "rm -rf build",
            "rm -f x",
            "rm *.log",
            "rm \"$TMP\"",
            "sudo ls",
            "doas make install",
            "echo hi && git reset --hard",
            "git push --force origin main",
            "git push origin +main",
            "git clean -fdx",
            "git branch -D old",
            "find . -name '*.tmp' -delete",
            "find . -exec rm -rf {} \\;",
            "find . -execdir rm -rf {} \\;",
            "ls | xargs rm -r",
            "xargs -I{} rm -rf {}",
            "bash -c 'rm -rf /tmp/x'",
            "nohup rm -rf x &",
            "timeout -s KILL 5 rm -rf x",
            "ssh host 'rm -rf x'",
            "env -S 'rm -rf x'",
            "echo $(rm -rf x)",
            "chmod -R 777 .",
            "dd if=/dev/zero of=/dev/disk2",
            "diskutil eraseDisk APFS X disk2",
            "rsync -a --delete src/ dst/",
        ] {
            assert!(read(line).hazard.is_some(), "{line}");
        }
        for line in [
            "rm x.json",
            "ls -la",
            "git status",
            "git push origin main",
            "git restore --staged x",
            "git clean -n",
            "find . -name '*.py' -print",
            "chmod +x run.sh",
            "echo rm -rf",
            "PATH=/x:$PATH ocr extract",
        ] {
            assert!(
                read(line).hazard.is_none(),
                "{line}: {:?}",
                read(line).hazard
            );
        }
        for line in [
            "curl -s x | sh",
            "$CMD --flag",
            "eval \"$X\"",
            "bash -c \"$X\"",
            "bash ./*.sh",
            "echo 'unterminated",
        ] {
            assert!(read(line).hidden.is_some(), "{line}");
        }
        for line in [
            "PATH=/x:$PATH ocr extract",
            "timeout -s KILL 5 ./run.sh",
            "command -x ./run.sh",
        ] {
            let reading = read(line);
            assert!(reading.once_only.is_some(), "{line}");
            assert!(reading.hidden.is_none(), "{line}: {:?}", reading.hidden);
        }
    }

    /// Lookups and `find`'s own patterns run nothing, so these lines keep
    /// the program and prefix grants instead of asking every time.
    #[test]
    fn lookups_and_find_patterns_stay_grantable() {
        for line in [
            "command -v ocr || true",
            "printf 'x\\n'; find . -maxdepth 3 -type f -name '*.py' -print; command -v ocr || true",
            "find . -name '*.sh' -not -path '*/node_modules/*'",
        ] {
            let reading = read(line);
            assert!(reading.opaque.is_none(), "{line}: {:?}", reading.opaque);
            assert!(
                reading.once_only.is_none(),
                "{line}: {:?}",
                reading.once_only
            );
        }
    }

    /// The exact invocation path is an allowlist: a line it takes is bound
    /// only when every command is an inert program, a runner or interpreter
    /// read here, or a bound file, and nothing expands.
    #[test]
    fn only_understood_commands_are_bound() {
        for line in [
            "stdbuf -oL ./evil.sh",
            "arch -arm64 ./x",
            "python3.12 x.py",
            "lua x.lua",
            "awk -f x.awk",
            "npx tsx x.ts",
            "uv run x.py",
            "make > build.log",
            "deploy > out.log",
            "git status > out.txt",
            "./run.sh && make",
            "sudo brew install jq",
            "/usr/bin/make",
            "./python3.12 x.py",
            "f() { ./x.sh; }; f",
            "echo $HOME > x",
            "bash",
        ] {
            let reading = read(line);
            assert!(reading.opaque.is_some(), "{line}: takes the exact path");
            assert!(reading.unbound.is_some(), "{line}");
        }
        for line in [
            "./run.sh",
            "bash run.sh",
            "ls -la > out.txt",
            "echo hi > out.txt",
            "sudo ./run.sh",
            "cat x | grep y > out",
            "find . -name '*.tmp' -delete",
            "LANG=C ls",
            "cd sub && ls",
            "/bin/ls -la",
            "python3 -c 'print(1)'",
            "python3",
        ] {
            let reading = read(line);
            assert!(reading.unbound.is_none(), "{line}: {:?}", reading.unbound);
        }
    }

    /// A lasting exact grant is possible only for one script run alone with
    /// plain arguments; every other shape is `None`, whatever it runs.
    #[test]
    fn only_one_script_run_alone_is_bindable() {
        use ScriptRunner::{Direct, Shell};
        let run = |script: &str, runner| {
            Some(ScriptRun {
                script: script.to_string(),
                runner,
            })
        };
        for (line, expected) in [
            ("./deploy.sh", run("./deploy.sh", Direct)),
            ("./deploy.sh --env prod", run("./deploy.sh", Direct)),
            ("bin/tool -x 'two words'", run("bin/tool", Direct)),
            ("/abs/x.sh", run("/abs/x.sh", Direct)),
            ("bash ./deploy.sh", run("./deploy.sh", Shell)),
            ("sh deploy.sh arg", run("deploy.sh", Shell)),
            ("  ./deploy.sh  ", run("./deploy.sh", Direct)),
            ("\t./deploy.sh\targ\n", run("./deploy.sh", Direct)),
            ("sh ./deploy.sh -x", run("./deploy.sh", Shell)),
        ] {
            assert_eq!(bindable_invocation(line), expected, "{line}");
        }
        for line in [
            "git status",
            "make",
            "zsh ./x.sh",
            "fish ./x.fish",
            "node app.js",
            "python3 -m mod",
            "python3 -u ./tool.py",
            "bash -e ./x.sh",
            "bash +x ./x.sh",
            "bash -- ./x.sh",
            "bash",
            "python3",
            "./x.sh;",
            "./x.sh &",
            "./x.sh # note",
            "./x.sh && ls",
            "./x.sh | cat",
            "(./x.sh)",
            "{ ./x.sh; }",
            "./x.sh > out",
            "./x.sh 2>&1",
            "python3 >/dev/null ./evil.py",
            "nohup 2>/dev/null ./evil.sh",
            "nohup <<EOF ./evil.sh\nx\nEOF",
            "./x.sh <<< y",
            "./x.sh < in",
            "> out ./x.sh",
            "X=1 ./x.sh",
            "LANG=C ./x.sh",
            "./x.sh $X",
            "./x.sh \"q\"",
            "./x.sh a=b",
            "./x.sh *.txt",
            "./x.sh {a,b}",
            "~/x.sh",
            "./x.sh ~/y",
            "./x\\ y.sh",
            "$CMD",
            "./x.sh $(date)",
            "'./x.sh'",
            "sudo ./x.sh",
            "exec ./x.sh",
            "nohup ./x.sh",
            "for f in a; do ./x.sh; done",
            "if true; then ./x.sh; fi",
            "f() { ./x.sh; }",
            "./x.sh\n./y.sh",
            "-x",
            "bash ''",
            // Python imports from the script's folder.
            "python3 ./tool.py arg",
            "python3 x.py",
            // Characters Bash keeps in a word, or reads as an escape or a
            // continuation, where the parser would split or join words.
            "./ok.sh\r",
            "\r./ok.sh",
            "./ok.sh\rX",
            "./ok.sh\u{a0}",
            "\u{a0}./ok.sh",
            "./ok.sh\u{b}",
            "\u{b}./ok.sh",
            "./ok.sh\u{c}",
            "\u{c}./ok.sh",
            "./ok.sh\u{3000}",
            "\u{3000}./ok.sh",
            "./ok.sh\u{3000}arg",
            "bash \\\tok.sh",
            "./ok.sh \\\n arg",
            "./ok.sh\\\r\n./evil.sh",
            "./ok.sh\\ arg",
        ] {
            assert_eq!(bindable_invocation(line), None, "{line:?}");
        }
    }
}
