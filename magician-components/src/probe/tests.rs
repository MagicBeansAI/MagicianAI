//! Probe behaviour that can be pinned without a running stack.
//!
//! The network case uses a port nothing listens on, which is deterministic
//! everywhere and needs no fixture server: "connection refused" is exactly the
//! condition a probe meets when a component is not installed.

use std::io::Write;

use super::*;
use crate::ProbeSpec;

/// A port in the ephemeral range that nothing in this repo binds.
const DEAD: &str = "http://127.0.0.1:59517";

#[cfg(feature = "probe")]
#[tokio::test]
async fn configured_health_uses_saved_credentials_and_refuses_login_redirects() {
    use std::io::Read;
    for status in [200, 302, 403, 503] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut input = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !input.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                input.extend_from_slice(&buffer[..read]);
            }
            write!(socket, "HTTP/1.1 {status} Fixture\r\nLocation: http://127.0.0.1:1/should-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8(input).unwrap()
        });
        let path = std::env::temp_dir().join(format!(
            "configured-health-{}-{port}.env",
            std::process::id()
        ));
        std::fs::write(
            &path,
            format!(
                "TEST_ORIGIN=http://127.0.0.1:{port}\nTEST_CREDENTIAL=synthetic-private-header\n"
            ),
        )
        .unwrap();
        let result = observe(&ProbeSpec::HttpConfigured {
            origin_key: "TEST_ORIGIN".into(),
            files: vec![path.display().to_string()],
            path: "/health".into(),
            headers: [("X-Test-Token".into(), "TEST_CREDENTIAL".into())].into(),
            timeout_ms: 3000,
        })
        .await;
        std::fs::remove_file(path).unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /health "));
        assert!(request.contains("x-test-token: synthetic-private-header"));
        assert_eq!(result.is_present(), status == 200);
        assert!(!result.detail().contains("synthetic-private-header"));
        if status == 302 {
            assert!(result.detail().contains("302"));
        }
    }
}

#[cfg(feature = "probe")]
#[tokio::test]
async fn configured_health_refuses_credentials_in_urls_and_remote_plaintext() {
    let path = std::env::temp_dir().join(format!(
        "configured-health-invalid-{}.env",
        std::process::id()
    ));
    for origin in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?query=x",
    ] {
        std::fs::write(&path, format!("TEST_ORIGIN={origin}\n")).unwrap();
        let result = observe(&ProbeSpec::HttpConfigured {
            origin_key: "TEST_ORIGIN".into(),
            files: vec![path.display().to_string()],
            path: "/health".into(),
            headers: Default::default(),
            timeout_ms: 100,
        })
        .await;
        assert!(!result.is_present());
        assert!(!result.detail().contains("password"));
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn only_explicit_native_permission_grants_are_present() {
    let observed = decode_desktop_permissions(
        r#"[{"key":"microphone","state":"granted"},{"key":"screen_recording","state":"missing"},{"key":"full_disk_access","state":"unknown"}]"#,
    );
    assert!(observed["microphone"].is_present());
    assert!(matches!(observed["screen_recording"], Observed::Absent(_)));
    assert!(matches!(observed["full_disk_access"], Observed::Unknown(_)));
    assert!(decode_desktop_permissions("invalid response").is_empty());
    assert!(decode_desktop_permissions(
        r#"[{"key":"microphone","state":"granted"},{"key":"microphone","state":"missing"}]"#
    )
    .is_empty());
}

#[tokio::test]
async fn desktop_permission_is_not_attested_by_the_engine_process() {
    let observed = observe(&ProbeSpec::DesktopPermission {
        key: "microphone".into(),
    })
    .await;
    assert!(matches!(observed, Observed::Unknown(_)));
    assert!(!observed.is_present());
}

#[tokio::test]
async fn a_runtime_inventory_rechecks_missing_and_non_executable_companions() {
    let dir =
        std::env::temp_dir().join(format!("magician-runtime-inventory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let inventory = dir.join("files.yaml");
    let companion = dir.join("ffmpeg");
    let spec = ProbeSpec::FileInventory {
        path: inventory.display().to_string(),
    };
    assert!(!observe(&spec).await.is_present());
    std::fs::write(&inventory, "schema_version: 1\nfiles: []\n").unwrap();
    assert!(
        !observe(&spec).await.is_present(),
        "empty inventory must not pass"
    );
    std::fs::write(
        &inventory,
        "schema_version: 1\nfiles:\n- path: ffmpeg\n  executable: true\n",
    )
    .unwrap();
    assert!(!observe(&spec).await.is_present());
    std::fs::write(&companion, "#!/bin/sh\nexit 0\n").unwrap();
    assert!(
        !observe(&spec).await.is_present(),
        "legacy inventories need fresh setup verification"
    );
    let metadata = std::fs::metadata(&companion).unwrap();
    let modified_ns = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::fs::write(
        &inventory,
        format!(
            "schema_version: 1\nverification_revision: 2\nfiles:\n- path: ffmpeg\n  executable: true\n  size_bytes: {}\n  modified_ns: {modified_ns}\n",
            metadata.len()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&companion, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!observe(&spec).await.is_present());
        std::fs::set_permissions(&companion, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(observe(&spec).await.is_present());
    std::fs::write(&companion, "#!/bin/sh\nexit 99\n").unwrap();
    assert!(
        !observe(&spec).await.is_present(),
        "replaced companions invalidate old setup verification"
    );
    std::fs::remove_file(&companion).unwrap();
    assert!(
        !observe(&spec).await.is_present(),
        "old setup success cannot hide a missing companion"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn runtime_inventory_rejects_an_old_verification_revision() {
    let dir = std::env::temp_dir().join(format!("magician-old-inventory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let inventory = dir.join("files.yaml");
    std::fs::write(
        &inventory,
        "schema_version: 1\nfiles:\n- path: SKILL.md\n  size_bytes: 1\n  modified_ns: 1\n",
    )
    .unwrap();
    let result = observe(&ProbeSpec::FileInventory {
        path: inventory.display().to_string(),
    })
    .await;
    assert!(result.detail().contains("fresh setup verification"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn an_unreachable_service_is_absent_not_an_error() {
    let got = observe(&ProbeSpec::HttpOk {
        url: format!("{DEAD}/health"),
        timeout_ms: 300,
    })
    .await;
    match got {
        Observed::Absent(detail) => assert!(detail.contains("unreachable"), "{detail}"),
        other => panic!("expected absent, got {other:?}"),
    }
}

#[tokio::test]
async fn a_manual_probe_asks_rather_than_guessing() {
    let got = observe(&ProbeSpec::Manual {
        hint: "check System Settings".into(),
    })
    .await;
    assert_eq!(got, Observed::Unknown("check System Settings".into()));
    // Unknown must never read as usable — guessing "probably fine" on a
    // permission is how a feature fails at the moment it is used.
    assert!(!got.is_present());
}

#[tokio::test]
async fn a_missing_file_is_absent_and_a_present_one_is_not() {
    let dir = std::env::temp_dir().join("magician-components-probe-file");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("funnel-url");
    let _ = std::fs::remove_file(&path);

    let spec = ProbeSpec::FileExists {
        path: path.display().to_string(),
    };
    assert!(matches!(observe(&spec).await, Observed::Absent(_)));

    std::fs::write(&path, "https://example.test/webhook").unwrap();
    assert!(observe(&spec).await.is_present());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_file_that_exists_but_cannot_be_read_is_absent_not_present() {
    // The whole reason this probe exists. `~/Library/Messages/chat.db` is on
    // every Mac that has ever sent a text, granted or not, so an existence
    // check on it answers "have you used Messages" while looking like it
    // answers "is Full Disk Access on". A permission probe that says yes when
    // the permission is missing ends the step, and the failure turns up later
    // as an empty inbox.
    let dir = std::env::temp_dir().join("magician-components-probe-unreadable");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("chat.db");
    std::fs::write(&path, "pretend messages").unwrap();

    let spec = ProbeSpec::FileReadable {
        path: path.display().to_string(),
        because: "the permission is not granted".to_string(),
    };
    assert!(
        observe(&spec).await.is_present(),
        "readable while it is readable"
    );

    // The unreadable case. Running as root defeats file permissions entirely,
    // so this half of the test only means something as an ordinary user.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
    std::fs::set_permissions(&path, perms).unwrap();
    if std::fs::File::open(&path).is_ok() {
        let _ = std::fs::remove_file(&path);
        return; // root, or a filesystem that ignores the mode
    }
    match observe(&spec).await {
        Observed::Absent(detail) => {
            assert_eq!(
                detail, "the permission is not granted",
                "it says why, in the graph's words"
            )
        },
        other => panic!("an unreadable file must be absent, got {other:?}"),
    }

    // And gone entirely is a different answer again: whether the permission
    // would be granted is unanswerable from here, so it asks rather than guesses.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o600);
    std::fs::set_permissions(&path, perms).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(observe(&spec).await, Observed::Unknown(_)));
}

#[tokio::test]
async fn a_key_counts_as_configured_from_a_file_but_only_a_real_value() {
    let dir = std::env::temp_dir().join("magician-components-probe-key");
    let _ = std::fs::create_dir_all(&dir);
    let env_file = dir.join(".env");
    let mut f = std::fs::File::create(&env_file).unwrap();
    writeln!(f, "# SOME_KEY=commented-out").unwrap();
    writeln!(f, "EMPTY_KEY=").unwrap();
    writeln!(f, "QUOTED_KEY=\"sk-value\"").unwrap();
    drop(f);
    let files = vec![env_file.display().to_string()];

    let quoted = ProbeSpec::EnvKeyPresent {
        keys: vec!["QUOTED_KEY".into()],
        files: files.clone(),
    };
    assert!(
        observe(&quoted).await.is_present(),
        "a quoted value is a value"
    );

    let empty = ProbeSpec::EnvKeyPresent {
        keys: vec!["EMPTY_KEY".into()],
        files: files.clone(),
    };
    assert!(
        !observe(&empty).await.is_present(),
        "an empty assignment is not configuration"
    );

    let commented = ProbeSpec::EnvKeyPresent {
        keys: vec!["SOME_KEY".into()],
        files: files.clone(),
    };

    // Any-of: one configured key is enough, however many are listed.
    let any = ProbeSpec::EnvKeyPresent {
        keys: vec!["NEVER_SET_KEY".into(), "QUOTED_KEY".into()],
        files: files.clone(),
    };
    assert!(
        observe(&any).await.is_present(),
        "any one of the keys is enough"
    );

    let none = ProbeSpec::EnvKeyPresent {
        keys: vec!["EMPTY_KEY".into(), "SOME_KEY".into()],
        files: files.clone(),
    };
    assert!(
        !observe(&none).await.is_present(),
        "an empty and a commented key are still none"
    );
    assert!(
        !observe(&commented).await.is_present(),
        "a commented line is not configuration"
    );

    let absent = ProbeSpec::EnvKeyPresent {
        keys: vec!["NEVER_SET_KEY".into()],
        files,
    };
    match observe(&absent).await {
        Observed::Absent(detail) => assert!(detail.contains("NEVER_SET_KEY"), "{detail}"),
        other => panic!("expected absent, got {other:?}"),
    }
    let _ = std::fs::remove_file(&env_file);
}

#[tokio::test]
async fn a_missing_env_file_is_skipped_rather_than_failing_the_probe() {
    // The data root may not exist yet on a first run. That is "no key", not an
    // error, and must not stop the probe reading the next file in the list.
    let spec = ProbeSpec::EnvKeyPresent {
        keys: vec!["ANY_KEY".into()],
        files: vec!["/nonexistent/path/.env".into(), "/also/missing/.env".into()],
    };
    assert!(matches!(observe(&spec).await, Observed::Absent(_)));
}

#[tokio::test]
async fn an_all_keys_probe_stays_absent_until_the_whole_credential_set_exists() {
    let dir = std::env::temp_dir().join("magician-components-probe-all-keys");
    let _ = std::fs::create_dir_all(&dir);
    let env_file = dir.join(".env");
    std::fs::write(&env_file, "MAGICIAN_TEST_PAIR_A=one\n").unwrap();
    let spec = ProbeSpec::EnvKeysAllPresent {
        keys: vec!["MAGICIAN_TEST_PAIR_A".into(), "MAGICIAN_TEST_PAIR_B".into()],
        files: vec![env_file.display().to_string()],
    };
    match observe(&spec).await {
        Observed::Absent(detail) => assert!(detail.contains("MAGICIAN_TEST_PAIR_B"), "{detail}"),
        other => panic!("a partial credential set must be absent, got {other:?}"),
    }
    std::fs::write(
        &env_file,
        "MAGICIAN_TEST_PAIR_A=one\nMAGICIAN_TEST_PAIR_B=two\n",
    )
    .unwrap();
    assert!(observe(&spec).await.is_present());
    let _ = std::fs::remove_file(env_file);
}

#[test]
fn this_machine_reports_itself_in_the_graph_s_vocabulary() {
    // The spellings have to match what a `host` block can say, or every gate
    // silently refuses. `uname -m` disagrees with itself across macOS and Linux
    // for the same silicon, so detection normalises rather than the graph
    // listing both.
    let host = detect_host();
    assert!(!host.os.is_empty());
    assert!(
        ["arm64", "x86_64"].contains(&host.arch.as_str()) || !host.arch.is_empty(),
        "unexpected arch spelling: {}",
        host.arch
    );
    assert_ne!(host.arch, "aarch64", "aarch64 must be normalised to arm64");
    // A machine running this test has memory; 0 would mean detection failed,
    // which is the value every gate treats as "does not qualify".
    assert!(
        host.memory_gb > 0,
        "physical memory should be readable here"
    );
}

#[test]
fn configured_model_helpers_read_the_selected_model_and_ollama_catalog() {
    let dir = std::env::temp_dir().join("magician-components-model-probe");
    let _ = std::fs::create_dir_all(&dir);
    let config = dir.join("magician-config.yaml");
    std::fs::write(
        &config,
        "runtime:\n  ollama:\n    local_generation:\n      selected: gemma4:12b\n",
    )
    .unwrap();
    let path = vec![
        "runtime".to_string(),
        "ollama".to_string(),
        "local_generation".to_string(),
        "selected".to_string(),
    ];
    assert_eq!(
        selected_yaml_string(&config.display().to_string(), &path).unwrap(),
        "gemma4:12b"
    );

    let models =
        ollama_models(r#"{"models":[{"name":"gemma4:12b"},{"model":"woof-4b:latest"}]}"#).unwrap();
    assert_eq!(models, vec!["gemma4:12b", "woof-4b:latest"]);
    assert!(model_matches(&models[1], "woof-4b"));
    assert!(!model_matches(&models[0], "woof-4b"));
    let _ = std::fs::remove_file(config);
}

#[tokio::test]
async fn any_route_that_works_satisfies_an_any_of_probe() {
    let dir = std::env::temp_dir().join("magician-components-probe-any-of");
    let _ = std::fs::create_dir_all(&dir);
    let here = dir.join("readable");
    std::fs::write(&here, "x").unwrap();
    let missing = dir.join("not-there");
    let _ = std::fs::remove_file(&missing);
    let dead = ProbeSpec::HttpBodyContains {
        url: format!("{DEAD}/host/imessage/status"),
        needle: "\"readable\":true".into(),
        timeout_ms: 300,
    };

    // The second route working is enough; the first failing is not a veto.
    let got = observe(&ProbeSpec::AnyOf {
        probes: vec![
            dead.clone(),
            ProbeSpec::FileExists {
                path: here.display().to_string(),
            },
        ],
    })
    .await;
    assert!(got.is_present(), "{got:?}");

    // An unanswerable route must not hide one that answered no.
    let got = observe(&ProbeSpec::AnyOf {
        probes: vec![ProbeSpec::Manual { hint: "ask".into() }, dead],
    })
    .await;
    match got {
        Observed::Absent(detail) => {
            assert!(
                detail.contains("unreachable") && detail.contains("ask"),
                "{detail}"
            )
        },
        other => panic!("expected absent, got {other:?}"),
    }

    // Only unanswerable routes: ask, don't guess.
    let got = observe(&ProbeSpec::AnyOf {
        probes: vec![ProbeSpec::FileReadable {
            path: missing.display().to_string(),
            because: "denied".into(),
        }],
    })
    .await;
    assert!(matches!(got, Observed::Unknown(_)), "{got:?}");
    assert!(matches!(
        observe(&ProbeSpec::AnyOf { probes: vec![] }).await,
        Observed::Unknown(_)
    ));
    let _ = std::fs::remove_file(&here);
}
