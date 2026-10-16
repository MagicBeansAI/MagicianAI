//! Argv builder for the generic YAML-driven CLI dispatcher.
//!
//! Pure function — no subprocess work, no I/O. Easy to test exhaustively.

use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};

/// Build the argv for one inner-loop primitive call.
///
/// `base` — argv prefix (typically `[pack_name]` or `implementation.command`).
/// `primitive` — primitive name (e.g. `read`, `search`).
/// `arguments` — JSON object of parameter values from the inner-LLM tool call.
///
/// Produces: `[base..., primitive, --flag, value, --flag, value, ...]`.
pub fn build_argv(base: &[String], primitive: &str, arguments: &Value) -> Result<Vec<String>> {
    if base.is_empty() {
        bail!("CLI base prefix is empty (pack must declare `implementation.command` or have a non-empty name)");
    }
    let obj = arguments_object(arguments)?;

    let mut argv: Vec<String> = base.to_vec();
    argv.push(primitive.to_string());

    // Iterate parameters in stable, alphabetical order so argv is
    // deterministic across runs (helpful for tests and trace diffs).
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();

    for key in keys {
        let value = obj
            .get(key)
            .ok_or_else(|| anyhow!("internal: missing key in arguments map"))?;
        append_flag(&mut argv, key, value)?;
    }

    Ok(argv)
}

/// Append a single parameter as `--flag [value]` to argv.
pub fn arguments_object(arguments: &Value) -> Result<&Map<String, Value>> {
    match arguments {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(empty_map()),
        other => bail!(
            "tool arguments must be a JSON object; got {}",
            type_name(other)
        ),
    }
}

/// Read an optional exact argv-token array.
pub fn optional_string_array(obj: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(items) = value.as_array() else {
        bail!("`{key}` must be an array of strings");
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(s) = item.as_str() else {
            bail!("`{key}` must contain only strings");
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
}

fn empty_map() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

pub fn append_flag(argv: &mut Vec<String>, name: &str, value: &Value) -> Result<()> {
    let flag = format!("--{}", to_kebab_case(name));
    match value {
        Value::Null => Ok(()), // skip nulls
        Value::Bool(true) => {
            argv.push(flag);
            Ok(())
        },
        Value::Bool(false) => Ok(()), // explicit false → omit
        Value::String(s) => {
            argv.push(flag);
            argv.push(s.clone());
            Ok(())
        },
        Value::Number(n) => {
            argv.push(flag);
            argv.push(n.to_string());
            Ok(())
        },
        Value::Array(items) => {
            for item in items {
                if matches!(item, Value::Null) {
                    continue;
                }
                argv.push(flag.clone());
                argv.push(stringify_scalar(item)?);
            }
            Ok(())
        },
        Value::Object(_) => {
            argv.push(flag);
            argv.push(value.to_string()); // compact JSON
            Ok(())
        },
    }
}

/// Stringify a JSON scalar (or compact-JSON for non-scalar) for use as a
/// CLI argument value.
pub fn stringify_scalar(value: &Value) -> Result<String> {
    Ok(match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => bail!("null is not a valid CLI argument value"),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    })
}

/// Convert `snake_case` (or `camelCase`) to `kebab-case`.
pub fn to_kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch == '_' {
            out.push('-');
        } else if ch.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Split a string into shell-like words without invoking a shell.
///
/// This is intentionally lexical only: no variables, command substitution,
/// globbing, pipes, redirects, or other shell semantics are interpreted.
pub fn split_shell_words(s: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    let mut in_word = false;

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
                chars.next();
            },
            '\'' => {
                in_word = true;
                chars.next();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => bail!("unterminated single quote"),
                    }
                }
            },
            '"' => {
                in_word = true;
                chars.next();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('"') => current.push('"'),
                            Some('\\') => current.push('\\'),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            },
                            None => current.push('\\'),
                        },
                        Some(c) => current.push(c),
                        None => bail!("unterminated double quote"),
                    }
                }
            },
            _ => {
                in_word = true;
                current.push(c);
                chars.next();
            },
        }
    }

    if in_word {
        words.push(current);
    }

    Ok(words)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn b(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn args(argv: Vec<String>) -> Vec<String> {
        argv
    }

    #[test]
    fn primitive_with_no_args_just_prepends_base() {
        let got = build_argv(&b("gmail"), "list", &json!({})).unwrap();
        assert_eq!(got, vec!["gmail", "list"]);
    }

    #[test]
    fn null_arguments_treated_as_empty_object() {
        let got = build_argv(&b("gmail"), "list", &Value::Null).unwrap();
        assert_eq!(got, vec!["gmail", "list"]);
    }

    #[test]
    fn empty_base_errors() {
        let err = build_argv(&[], "list", &json!({})).unwrap_err();
        assert!(format!("{err}").contains("CLI base prefix is empty"));
    }

    #[test]
    fn non_object_arguments_errors() {
        let err = build_argv(&b("gmail"), "list", &json!("oops")).unwrap_err();
        assert!(format!("{err}").contains("must be a JSON object"));
    }

    #[test]
    fn multi_token_base_used_verbatim() {
        let got = build_argv(&b("gws gmail"), "read", &json!({"message_id": "abc"})).unwrap();
        assert_eq!(got, vec!["gws", "gmail", "read", "--message-id", "abc"]);
    }

    #[test]
    fn snake_case_converted_to_kebab() {
        let got = build_argv(
            &b("gmail"),
            "search",
            &json!({"max_results": 10, "query": "foo"}),
        )
        .unwrap();
        // alphabetical: max_results then query
        assert_eq!(
            got,
            args(vec![
                "gmail".into(),
                "search".into(),
                "--max-results".into(),
                "10".into(),
                "--query".into(),
                "foo".into(),
            ])
        );
    }

    #[test]
    fn boolean_true_emits_flag_only() {
        let got = build_argv(&b("tool"), "run", &json!({"verbose": true, "name": "x"})).unwrap();
        assert_eq!(
            got,
            vec!["tool", "run", "--name", "x", "--verbose"],
            "boolean true → bare flag, no value"
        );
    }

    #[test]
    fn boolean_false_omits_flag_entirely() {
        let got = build_argv(&b("tool"), "run", &json!({"verbose": false, "name": "x"})).unwrap();
        assert_eq!(got, vec!["tool", "run", "--name", "x"]);
    }

    #[test]
    fn null_value_omitted() {
        let got = build_argv(&b("tool"), "run", &json!({"name": "x", "extra": null})).unwrap();
        assert_eq!(got, vec!["tool", "run", "--name", "x"]);
    }

    #[test]
    fn integer_and_float_serialize() {
        let got = build_argv(&b("tool"), "run", &json!({"count": 5, "ratio": 0.25})).unwrap();
        assert_eq!(got, vec!["tool", "run", "--count", "5", "--ratio", "0.25"]);
    }

    #[test]
    fn array_emits_repeated_flag() {
        let got = build_argv(&b("tool"), "select", &json!({"values": ["A", "B", "C"]})).unwrap();
        assert_eq!(
            got,
            vec!["tool", "select", "--values", "A", "--values", "B", "--values", "C"]
        );
    }

    #[test]
    fn array_with_nulls_skips_nulls() {
        let got = build_argv(&b("tool"), "select", &json!({"values": ["A", null, "B"]})).unwrap();
        assert_eq!(
            got,
            vec!["tool", "select", "--values", "A", "--values", "B"]
        );
    }

    #[test]
    fn array_of_mixed_scalars() {
        let got = build_argv(&b("tool"), "run", &json!({"ids": [1, 2, 3]})).unwrap();
        assert_eq!(
            got,
            vec!["tool", "run", "--ids", "1", "--ids", "2", "--ids", "3"]
        );
    }

    #[test]
    fn object_value_compact_json_encoded() {
        let got = build_argv(
            &b("tool"),
            "set",
            &json!({"config": {"width": 1024, "height": 768}}),
        )
        .unwrap();
        // serde_json::Value preserves the input order via the `preserve_order` feature
        // setting on the workspace; the test asserts only the keys are present and values are right
        let config_arg = &got[3];
        let parsed: serde_json::Value =
            serde_json::from_str(config_arg).expect("config arg parses");
        assert_eq!(parsed, json!({"width": 1024, "height": 768}));
        assert_eq!(&got[..3], &["tool", "set", "--config"]);
    }

    #[test]
    fn argv_is_deterministic_across_param_orderings() {
        let a = build_argv(&b("g"), "search", &json!({"a": 1, "b": 2, "c": 3})).unwrap();
        let b_argv = build_argv(&b("g"), "search", &json!({"c": 3, "a": 1, "b": 2})).unwrap();
        assert_eq!(a, b_argv);
    }

    #[test]
    fn kebab_handles_camelcase_and_underscore_mix() {
        // camelCase is rare here but let's not break on it
        assert_eq!(to_kebab_case("messageId"), "message-id");
        assert_eq!(to_kebab_case("message_id"), "message-id");
        assert_eq!(to_kebab_case("HTTPResponse"), "h-t-t-p-response");
        assert_eq!(to_kebab_case("simple"), "simple");
    }
}
