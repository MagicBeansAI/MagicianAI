//! The writer touches the one file holding the operator's provider tokens, so
//! every test here is about not damaging what is already in it.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("magician-envfile-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

#[test]
fn a_value_lands_and_the_file_is_owner_only() {
    let dir = scratch("basic");
    let env = EnvFile::at(&dir);
    assert_eq!(env.set("OPENAI_API_KEY", "sk-test").unwrap(), Wrote::Added);

    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(text.contains("OPENAI_API_KEY=sk-test"), "{text}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(env.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a file of tokens must not be world-readable");
    }
}

#[test]
fn an_existing_value_is_never_replaced() {
    // Re-running the wizard after pasting a key must not offer to overwrite it
    // with a typo, and must not silently duplicate the line.
    let dir = scratch("existing");
    let env = EnvFile::at(&dir);
    env.set("KAPSO_API_KEY", "the-real-one").unwrap();
    assert_eq!(
        env.set("KAPSO_API_KEY", "a-mistake").unwrap(),
        Wrote::AlreadySet
    );

    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(text.contains("the-real-one"));
    assert!(!text.contains("a-mistake"));
    assert_eq!(
        text.matches("KAPSO_API_KEY").count(),
        1,
        "no duplicate line: {text}"
    );
}

#[test]
fn a_commented_suggestion_is_not_a_setting() {
    // The shipped .env.example comments out every optional key. Reading those
    // as set would make the wizard skip the very key it needs to ask for.
    let dir = scratch("commented");
    let env = EnvFile::at(&dir);
    std::fs::write(env.path(), "# EXA_API_KEY=your-key-here\n").unwrap();
    assert!(!env.is_set("EXA_API_KEY"));
    assert_eq!(env.set("EXA_API_KEY", "real").unwrap(), Wrote::Added);
}

#[test]
fn an_empty_assignment_is_not_a_setting() {
    let dir = scratch("empty");
    let env = EnvFile::at(&dir);
    std::fs::write(env.path(), "TELEGRAM_TOKEN=\n").unwrap();
    assert!(
        !env.is_set("TELEGRAM_TOKEN"),
        "an empty value is not configuration"
    );
}

#[test]
fn empty_seed_assignments_are_filled_in_both_runtime_env_files() {
    let dir = scratch("paired-empty");
    let env = EnvFile::at(&dir);
    for file in [".env", ".env.development"] {
        std::fs::write(dir.join(file), "# owner note\nTOKEN=\nOTHER=keep\n").unwrap();
    }
    env.set("TOKEN", "configured").unwrap();
    for file in [".env", ".env.development"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(file)).unwrap(),
            "# owner note\nTOKEN=configured\nOTHER=keep\n"
        );
    }
}

#[test]
fn existing_development_credentials_are_preserved() {
    let dir = scratch("paired-preserve");
    let development = dir.join(".env.development");
    std::fs::write(&development, "TOKEN=owners-value\n").unwrap();
    EnvFile::at(&dir).set("TOKEN", "new-value").unwrap();
    assert_eq!(
        std::fs::read_to_string(development).unwrap(),
        "TOKEN=owners-value\n"
    );
    assert!(
        !dir.join(".env").exists(),
        "an already-configured development key must not prompt/write a replacement"
    );
}

#[test]
fn special_characters_are_literal_and_multiline_values_are_rejected() {
    let dir = scratch("literal");
    let env = EnvFile::at(&dir);
    env.set("TOKEN", "value #with $dollar and \"quotes\"")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(env.path()).unwrap(),
        "TOKEN=\"value #with \\$dollar and \\\"quotes\\\"\"\n"
    );
    assert!(env.set("OTHER", "line\nINJECTED=value").is_err());
    assert!(!std::fs::read_to_string(env.path())
        .unwrap()
        .contains("INJECTED"));
}

#[test]
fn everything_already_in_the_file_survives() {
    // The file is the operator's: comments, ordering, and keys this wizard has
    // never heard of. Appending is the only safe thing to do to it.
    let dir = scratch("preserve");
    let env = EnvFile::at(&dir);
    let original = "# my notes\nSOMETHING_ELSE=keep-me\n\n# a blank line above\n";
    std::fs::write(env.path(), original).unwrap();

    env.set("TAVILY_API_KEY", "tvly-x").unwrap();
    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(
        text.starts_with(original),
        "the original must be untouched:\n{text}"
    );
    assert!(text.trim_end().ends_with("TAVILY_API_KEY=tvly-x"));
}

#[test]
fn a_file_without_a_trailing_newline_does_not_get_a_welded_line() {
    let dir = scratch("noeol");
    let env = EnvFile::at(&dir);
    std::fs::write(env.path(), "FIRST=one").unwrap();
    env.set("SECOND", "two").unwrap();

    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(text.contains("FIRST=one\n"), "{text:?}");
    assert!(text.contains("SECOND=two"), "{text:?}");
    assert!(
        !text.contains("oneSECOND"),
        "welded onto the last line: {text:?}"
    );
}
