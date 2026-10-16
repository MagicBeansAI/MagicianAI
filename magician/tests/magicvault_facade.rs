//! Product ownership and shared-type identity across the extraction boundary.
use std::sync::{Arc, Barrier};

use magician::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    secrets::{
        InMemoryKeyProvider, OneTimeBinding, OneTimeClaim, OneTimeError, OneTimeState,
        PreDispatchFailure, SecretRuntimeCapabilities, SecretStore, SecretStoreResolver,
    },
};
// The manual clock is a `test-fixtures` item of the shared core (magician
// re-exports it as `magician_v2::secrets::ManualClock` under the same feature).
use magicvault_core::store::ManualClock;

#[test]
fn facade_uses_the_existing_product_scope_layout_and_the_actual_core_store() {
    let temp = tempfile::tempdir().unwrap();
    let layout = ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(temp.path()));
    let resolver = SecretStoreResolver::new_with_capabilities(
        Box::new(InMemoryKeyProvider::new()),
        temp.path().to_path_buf(),
        SecretRuntimeCapabilities::fully_available("in_memory"),
    );
    let store = resolver.resolve_for_scope("owner", "default").unwrap();
    assert_eq!(store.base_dir(), layout.secrets_root("owner", "default"));
    assert_eq!(
        resolver.workspace_layout().secrets_root("owner", "default"),
        store.base_dir()
    );

    // These assignments deliberately require type identity, not a conversion
    // that could allocate a parallel store or copy plaintext into a wrapper.
    let shared: Arc<magicvault_core::store::SecretStore> = Arc::clone(&store);
    let facade: Arc<SecretStore> = Arc::clone(&shared);
    assert!(Arc::ptr_eq(&facade, &store));
    store
        .register_ephemeral("task", "input", "SYNTHETIC-FACADE-CANARY".into())
        .unwrap();
    assert_eq!(
        shared.get_ephemeral_scoped("task", "input").as_deref(),
        Some("SYNTHETIC-FACADE-CANARY")
    );
    let other = resolver.resolve_for_scope("owner", "other").unwrap();
    assert!(!Arc::ptr_eq(&other, &store));
    assert!(other.get_ephemeral_scoped("task", "input").is_none());
}

#[test]
fn cloned_product_resolvers_still_share_one_first_load_slot() {
    const CONTENDERS: usize = 16;
    let temp = tempfile::tempdir().unwrap();
    let resolver = SecretStoreResolver::new_with_capabilities(
        Box::new(InMemoryKeyProvider::new()),
        temp.path().to_path_buf(),
        SecretRuntimeCapabilities::fully_available("in_memory"),
    );
    let barrier = Arc::new(Barrier::new(CONTENDERS));
    let handles = (0..CONTENDERS)
        .map(|_| {
            let resolver = resolver.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                resolver.resolve_for_scope("owner", "default").unwrap()
            })
        })
        .collect::<Vec<_>>();
    let stores = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert!(stores.iter().all(|store| Arc::ptr_eq(&stores[0], store)));
}

#[test]
fn captured_session_types_remain_identical_through_the_old_import_path() {
    let original = magician::magician_v2::api_mining::types::SessionContext::default();
    let shared: magicvault_core::session::SessionContext = original;
    let wire = serde_json::to_value(&shared).unwrap();
    assert_eq!(wire, serde_json::json!({}));
    assert!(wire.get("cookie_metadata").is_none());
}

#[test]
fn recreated_product_resolver_reads_the_same_scoped_vault_without_migration() {
    use magician::magician_v2::secrets::{
        InjectionTarget, MasterKeyProvider, SecretEncryptionError, SecretPolicy,
    };
    use std::collections::HashMap;

    // Share only the test key, not a resolver cache, so the second resolver
    // must actually reopen the product-layout encrypted partition.
    struct SharedFixtureKey(Arc<InMemoryKeyProvider>);
    impl MasterKeyProvider for SharedFixtureKey {
        fn get_or_create_key(&self) -> Result<[u8; 32], SecretEncryptionError> {
            self.0.get_or_create_key()
        }
        fn delete_key(&self) -> Result<(), SecretEncryptionError> {
            self.0.delete_key()
        }
        fn provider_name(&self) -> &str {
            "in_memory"
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let key = Arc::new(InMemoryKeyProvider::new());
    let new_resolver = || {
        SecretStoreResolver::new_with_capabilities(
            Box::new(SharedFixtureKey(Arc::clone(&key))),
            temp.path().to_path_buf(),
            SecretRuntimeCapabilities::fully_available("in_memory"),
        )
    };
    let resolver = new_resolver();
    let store = resolver.resolve_for_scope("owner", "default").unwrap();
    store
        .store_provisioned(
            "fixture",
            "Fixture",
            HashMap::from([("value".into(), "SYNTHETIC-RELOAD-CANARY".into())]),
            InjectionTarget::Header {
                name: "Authorization".into(),
                prefix: Some("Bearer ".into()),
            },
            SecretPolicy::default(),
        )
        .unwrap();
    assert!(store.base_dir().join("provisioned_secrets.vault").is_file());
    let metadata: magician::magician_v2::secrets::ProvisionedSecretMetadata =
        store.provisioned_metadata("fixture").unwrap();
    let shared: magicvault_core::store::ProvisionedSecretMetadata = metadata;
    assert_eq!(shared.field_names, ["value"]);
    assert!(!serde_json::to_string(&shared)
        .unwrap()
        .contains("SYNTHETIC-RELOAD-CANARY"));
    drop(store);
    drop(resolver);

    let reopened = new_resolver();
    let same_scope = reopened.resolve_for_scope("owner", "default").unwrap();
    assert_eq!(
        same_scope.get_provisioned("fixture").unwrap().fields["value"],
        "SYNTHETIC-RELOAD-CANARY"
    );
    assert!(reopened
        .resolve_for_scope("owner", "other")
        .unwrap()
        .get_provisioned("fixture")
        .is_none());
}

/// Fixture 0.4 of the secure HITL credentials plan (§2 "OTP lifetime", §4,
/// §8 "OTP expires while UI is open" / "Two submissions or two consumers race"),
/// now against the actual core this branch links (P2; `magicvault-core`
/// 0.1.4 through the workspace `[patch]`).
///
/// P0 finding: an ephemeral entry had execution-scoped cleanup only — no
/// deadline, no reservation, no consumed state — so nothing prevented a
/// one-time code from being submitted twice. The generic read
/// (`get_ephemeral_scoped`) stays a plain read for inline injection; a one-time
/// code lives in a separate partition the store drives through
/// `reserve → consume | release | expire` with an injectable clock.
#[test]
fn p0_a_one_time_code_cannot_be_used_twice_from_ephemeral_custody() {
    const OTP_CANARY: &str = "p0-otp-custody-canary-5";
    const T0: i64 = 1_700_000_000_000;
    let temp = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let resolver = SecretStoreResolver::new_with_capabilities(
        Box::new(InMemoryKeyProvider::new()),
        temp.path().to_path_buf(),
        SecretRuntimeCapabilities::fully_available("in_memory"),
    )
    .with_clock(clock.clone());
    let store = resolver.resolve_for_scope("owner", "default").unwrap();
    let binding = OneTimeBinding {
        challenge_id: Some("challenge-1".into()),
        destination: Some("https://login.example.test".into()),
    };
    let claim = || OneTimeClaim {
        operation: "browser:submit_login".into(),
        destination: Some("https://login.example.test".into()),
        challenge_id: Some("challenge-1".into()),
    };
    let held = |store: &SecretStore| {
        store
            .redaction_values()
            .iter()
            .any(|value| value.as_str() == OTP_CANARY)
    };

    // 1. registered as one-time material with a deadline, not `register_ephemeral`;
    //    a plain read never sees it, the redaction pass does.
    let registered = store
        .register_one_time(
            "execution:run-1",
            "otp",
            OTP_CANARY.into(),
            T0 + 60_000,
            binding,
        )
        .unwrap();
    assert_eq!(registered.state, OneTimeState::Available);
    assert_eq!(store.get_ephemeral_scoped("execution:run-1", "otp"), None);
    assert!(store.ephemeral_scope_holds_user_typed_secret("execution:run-1"));
    assert!(held(&store));
    assert_eq!(
        store
            .reserve_one_time("execution:run-2", "otp", claim())
            .unwrap_err(),
        OneTimeError::NotFound,
        "another run cannot claim this run's code"
    );

    // 2. `reserve` for one bound authentication attempt succeeds once ...
    let first = store
        .reserve_one_time("execution:run-1", "otp", claim())
        .unwrap();
    assert_eq!(first.value.as_str(), OTP_CANARY);
    let first_id = first.receipt.reservation_id.clone().unwrap();
    // 3. ... and a second `reserve` while reserved is refused.
    assert_eq!(
        store
            .reserve_one_time("execution:run-1", "otp", claim())
            .unwrap_err(),
        OneTimeError::AlreadyReserved
    );

    // 5. `release` on a proven pre-dispatch failure returns it to available ...
    let released = store
        .release_one_time(
            &first_id,
            PreDispatchFailure {
                reason: "field not present".into(),
            },
        )
        .unwrap();
    assert_eq!(released.state, OneTimeState::Available);
    let second = store
        .reserve_one_time("execution:run-1", "otp", claim())
        .unwrap();
    let second_id = second.receipt.reservation_id.clone().unwrap();
    assert_ne!(second_id, first_id);

    // 4. `consume` after dispatch starts makes the entry permanently unusable —
    //    for the stale id, the live id, and any later reserve — and the value
    //    is gone from the store, not merely flagged.
    assert_eq!(
        store.consume_one_time(&first_id).unwrap_err(),
        OneTimeError::NotReserved
    );
    assert_eq!(
        store.consume_one_time(&second_id).unwrap().state,
        OneTimeState::Consumed
    );
    assert!(!held(&store), "a consumed code is dropped at consume");
    assert_eq!(
        store.consume_one_time(&second_id).unwrap_err(),
        OneTimeError::Consumed
    );
    assert_eq!(
        store
            .release_one_time(
                &second_id,
                PreDispatchFailure {
                    reason: "too late".into()
                }
            )
            .unwrap_err(),
        OneTimeError::Consumed
    );
    assert_eq!(
        store
            .reserve_one_time("execution:run-1", "otp", claim())
            .unwrap_err(),
        OneTimeError::Consumed
    );
    assert!(!store.ephemeral_scope_holds_user_typed_secret("execution:run-1"));

    // 6. advancing the injected clock past the deadline expires a fresh code:
    //    one still waiting (the UI left open) and one already reserved (queued
    //    behind an approval), and an untouched expiry stops pinning the run.
    let unbound = || OneTimeClaim {
        operation: "http:post".into(),
        destination: None,
        challenge_id: None,
    };
    store
        .register_one_time(
            "execution:run-2",
            "otp",
            OTP_CANARY.into(),
            T0 + 30_000,
            OneTimeBinding::default(),
        )
        .unwrap();
    store
        .register_one_time(
            "execution:run-2",
            "queued",
            OTP_CANARY.into(),
            T0 + 30_000,
            OneTimeBinding::default(),
        )
        .unwrap();
    let queued = store
        .reserve_one_time("execution:run-2", "queued", unbound())
        .unwrap();
    clock.set(T0 + 30_000);
    assert!(
        !store.ephemeral_scope_holds_user_typed_secret("execution:run-2"),
        "expired without a touch"
    );
    assert_eq!(
        store
            .reserve_one_time("execution:run-2", "otp", unbound())
            .unwrap_err(),
        OneTimeError::Expired,
        "a code nobody claimed before the deadline cannot be claimed after it"
    );
    assert_eq!(
        store
            .one_time_state("execution:run-2", "otp")
            .unwrap()
            .state,
        OneTimeState::Expired
    );
    assert_eq!(
        store
            .consume_one_time(queued.receipt.reservation_id.as_deref().unwrap())
            .unwrap_err(),
        OneTimeError::Expired,
        "the pre-dispatch recheck refuses a code that expired while queued"
    );
    assert_eq!(
        store
            .one_time_state("execution:run-2", "queued")
            .unwrap()
            .state,
        OneTimeState::Expired
    );
    assert!(
        !held(&store),
        "expired codes are dropped, not kept for later"
    );

    // Two consumers race: exactly one bound submission, every loser told why.
    clock.set(T0);
    store
        .register_one_time(
            "execution:run-3",
            "otp",
            OTP_CANARY.into(),
            T0 + 60_000,
            OneTimeBinding::default(),
        )
        .unwrap();
    let racers = 16;
    let barrier = Arc::new(Barrier::new(racers));
    let outcomes: Vec<_> = (0..racers)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .reserve_one_time("execution:run-3", "otp", unbound())
                    .map(|_| ())
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == Err(OneTimeError::AlreadyReserved))
            .count(),
        racers - 1
    );

    // 7. `clear_ephemeral` still retires everything for the scope.
    assert_eq!(store.clear_ephemeral("execution:run-3"), 1);
    assert!(store.one_time_state("execution:run-3", "otp").is_none());
    assert_eq!(
        store
            .reserve_one_time("execution:run-3", "otp", unbound())
            .unwrap_err(),
        OneTimeError::NotFound
    );

    // A restart loses the code and fails closed: a fresh resolver over the same
    // scope directory knows nothing, and no file under it carries the value.
    store
        .register_one_time(
            "execution:run-4",
            "otp",
            OTP_CANARY.into(),
            T0 + 60_000,
            OneTimeBinding::default(),
        )
        .unwrap();
    let reopened = SecretStoreResolver::new_with_capabilities(
        Box::new(InMemoryKeyProvider::new()),
        temp.path().to_path_buf(),
        SecretRuntimeCapabilities::fully_available("in_memory"),
    )
    .with_clock(clock.clone())
    .resolve_for_scope("owner", "default")
    .unwrap();
    assert!(!Arc::ptr_eq(&reopened, &store));
    assert!(reopened.one_time_state("execution:run-4", "otp").is_none());
    assert_eq!(
        reopened
            .reserve_one_time("execution:run-4", "otp", unbound())
            .unwrap_err(),
        OneTimeError::NotFound
    );
    for entry in std::fs::read_dir(store.base_dir()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(OTP_CANARY),
            "{} carries the value",
            path.display()
        );
    }

    // The journal under the product scope layout names every transition and
    // never the value.
    let journal = std::fs::read_to_string(store.base_dir().join("secret_audit.jsonl")).unwrap();
    for event in [
        "one_time_registered",
        "one_time_reserved",
        "one_time_released",
        "one_time_consumed",
        "one_time_expired",
    ] {
        assert!(journal.contains(event), "journal lacks {event}");
    }
}
