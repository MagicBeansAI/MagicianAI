//! What a shell command runs, read with a real Bash parser (tree-sitter-bash),
//! and the scopes an owner may grant it.
//!
//! Every simple command in the line is extracted, including those inside
//! command and process substitutions, pipelines, `&&`/`;` lists, subshells
//! and control flow. A grant covers a line only if it covers every command
//! in it.
//!
//! A line is *opaque* when what it runs cannot be named from its text; it can
//! only be allowed once:
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

/// Programs that run another command taken from their arguments.
const COMMAND_RUNNERS: &[&str] = &[
    "sudo", "doas", "su", "env", "exec", "command", "builtin", "nohup", "time", "nice",
    "ionice", "timeout", "watch", "xargs", "parallel", "ssh", "chroot", "script", "flock",
    "caffeinate", "eval", "source", ".", "trap", "hash", "alias",
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
            Some(Some(first)) if !first.starts_with('-') => Some(format!("{} {first}", self.program)),
            _ => None,
        }
    }
}

/// What a command line runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellReading {
    pub commands: Vec<ShellCommand>,
    /// Why the line is opaque (only "allow once"), if it is.
    pub opaque: Option<String>,
}

/// Read `command` with the Bash grammar.
pub fn read(command: &str) -> ShellReading {
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
    let mut reading = ShellReading {
        commands: Vec::new(),
        opaque: None,
    };
    walk(root, source, &mut reading);
    if reading.opaque.is_none() && reading.commands.is_empty() {
        reading.opaque = Some("the line runs no command".to_string());
    }
    reading
}

fn opaque_reading(reason: &str) -> ShellReading {
    ShellReading {
        commands: Vec::new(),
        opaque: Some(reason.to_string()),
    }
}

fn walk(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    match node.kind() {
        "function_definition" => {
            mark(reading, "it defines a shell function");
        },
        "command" => read_command(node, source, reading),
        "variable_assignment" | "variable_assignments" => {
            mark(reading, "it sets shell variables, which change what later commands run");
        },
        "declaration_command" | "unset_command" => {
            mark(reading, "it changes shell variables, which change what later commands run");
        },
        "file_redirect" => read_redirect(node, source, reading),
        _ => {},
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, reading);
    }
}

fn mark(reading: &mut ShellReading, reason: &str) {
    if reading.opaque.is_none() {
        reading.opaque = Some(reason.to_string());
    }
}

fn read_command(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    let Some(name) = node.child_by_field_name("name") else {
        mark(reading, "a command has no name");
        return;
    };
    let Some(program) = literal(name, source) else {
        mark(reading, "a command name comes from an expansion");
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
    let mut args = Vec::new();
    let mut cursor = node.walk();
    for arg in node.children_by_field_name("argument", &mut cursor) {
        args.push(literal(arg, source));
    }
    if COMMAND_RUNNERS.contains(&program.as_str()) {
        mark(reading, &format!("`{program}` runs another command"));
    } else if program == "find"
        && args.iter().flatten().any(|arg| {
            matches!(arg.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete")
        })
    {
        mark(reading, "`find` runs a command for each match");
    } else if let Some((_, inline)) = INTERPRETERS.iter().find(|(name, _)| *name == program) {
        let runs_inline = args.iter().flatten().any(|arg| inline.contains(&arg.as_str()));
        let has_script = args.iter().any(|arg| match arg {
            Some(arg) => !arg.starts_with('-'),
            None => true,
        });
        if runs_inline {
            mark(reading, &format!("`{program}` runs inline code"));
        } else if !has_script {
            mark(reading, &format!("`{program}` reads code from its input"));
        }
    }
    reading.commands.push(ShellCommand { program, args });
}

/// An output redirect (`>`, `>>`, `>|`, `&>`, `&>>`, `>& file`) writes a
/// file the grant never named: the line is opaque unless the target is a
/// harmless device or another descriptor (`2>&1`, `>&-`). Input redirects
/// and heredocs only read.
fn read_redirect(node: Node<'_>, source: &[u8], reading: &mut ShellReading) {
    let mut cursor = node.walk();
    let Some(operator) = node
        .children(&mut cursor)
        .find(|child| !child.is_named())
        .map(|child| child.kind())
    else {
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
            Some(text.trim_start_matches('\'').trim_end_matches('\'').to_string())
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
        read(command).commands.into_iter().map(|c| c.program).collect()
    }

    #[test]
    fn every_command_in_the_line_is_read() {
        assert_eq!(programs("cd repo && npm install --save foo | tee log; ls -la"), ["cd", "npm", "tee", "ls"]);
        assert_eq!(programs("echo $(git rev-parse HEAD)"), ["echo", "git"]);
        assert_eq!(programs("(cd x; make) || echo failed"), ["cd", "make", "echo"]);
        assert_eq!(programs("for f in *.md; do wc -l \"$f\"; done"), ["wc"]);
        assert_eq!(programs("/usr/bin/git status"), ["/usr/bin/git"], "the name as written");
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
        assert_eq!(read("echo \"hi there\"").commands[0].exact().as_deref(), Some("echo hi there"));
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
            "echo 'unterminated",
            "hash -p /tmp/x/git git",
            "alias git=/tmp/x/git",
        ] {
            assert!(read(line).opaque.is_some(), "{line}");
        }
        for line in ["git clone x", "python3 build.py", "bash setup.sh", "echo $(date)", "npm run build"] {
            assert!(read(line).opaque.is_none(), "{line}: {:?}", read(line).opaque);
        }
    }

    /// A grant names a program; `./git` or `/tmp/x/git` is whatever file
    /// sits at that path, so it can only be allowed once.
    #[test]
    fn a_command_named_by_a_path_is_opaque() {
        for line in ["./git status", "/tmp/x/git status", "../bin/git status", "~/bin/git status", "echo hi && ./git status"] {
            let reading = read(line);
            assert!(reading.opaque.is_some(), "{line}");
            assert!(reading.commands.iter().all(|command| command.program != "git"), "{line}: kept as written");
        }
        assert_eq!(read("./git status").commands[0].program, "./git");
        assert_eq!(read("./git status").commands[0].exact().as_deref(), Some("./git status"));
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
        assert!(read("for f in a b; do echo \"$f\"; done").opaque.is_none());
        assert!(read("make CC=gcc --jobs=4").opaque.is_none(), "an argument with `=` is not an assignment");
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
            assert!(read(line).opaque.is_none(), "{line}: {:?}", read(line).opaque);
        }
    }
}
