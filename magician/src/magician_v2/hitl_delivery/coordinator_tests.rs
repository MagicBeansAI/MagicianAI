use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::super::policy::DeliveryPolicy;
use super::super::records::{DeliveryState, DeliveryStore, Destination};
use super::*;
use crate::config::{
    CriticalDeliveryPolicy, EnvoyConfig, HitlCriticalDeliverySettings, MagicianConfig,
    QuietHoursSettings,
};
use crate::magician_v2::realtime_events::RuntimeTransportEvent;

const CANARY: &str = "prompt-canary-Z9";

struct FakeClock(AtomicI64);
impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct FakeTransport {
    offered: std::sync::Mutex<Vec<RuntimeTransportEvent>>,
}
impl ChannelTransport for FakeTransport {
    fn offer(&self, event: RuntimeTransportEvent) {
        self.offered.lock().unwrap().push(event);
    }
}
impl FakeTransport {
    fn alerts(&self) -> Vec<(String, String)> {
        self.offered
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                RuntimeTransportEvent::CriticalRequestAlert {
                    delivery_id,
                    channel_type,
                    ..
                } => Some((delivery_id.clone(), channel_type.clone())),
                _ => None,
            })
            .collect()
    }
    fn retired(&self) -> Vec<String> {
        self.offered
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                RuntimeTransportEvent::CriticalRequestRetired { delivery_id, .. } => {
                    Some(delivery_id.clone())
                },
                _ => None,
            })
            .collect()
    }
}

struct FakePush {
    registrations: usize,
    accept: bool,
    requested: AtomicUsize,
    resolved: AtomicUsize,
}
#[async_trait]
impl PushSink for FakePush {
    async fn attention_requested(&self, _: &str, _: &str, _: &str, _: i64) -> PushWave {
        self.requested.fetch_add(1, Ordering::SeqCst);
        PushWave {
            registrations: self.registrations,
            accepted: if self.accept { self.registrations } else { 0 },
            not_configured: 0,
        }
    }
    async fn attention_resolved(&self, _: &str, _: &str, _: &str, _: i64) {
        self.resolved.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeOracle {
    pending: Mutex<HashSet<String>>,
    origin: Option<(String, String)>,
    checks: AtomicUsize,
}
#[async_trait]
impl RequestOracle for FakeOracle {
    async fn still_pending(&self, request: &RequestIdentity) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.pending.lock().await.contains(&request.correlation_id)
    }
    async fn origin_channel(
        &self,
        _: &RequestIdentity,
        _: Option<&Value>,
    ) -> Option<(String, String)> {
        self.origin.clone()
    }
}

struct Fixture {
    coordinator: Arc<DeliveryCoordinator>,
    transport: Arc<FakeTransport>,
    push: Arc<FakePush>,
    oracle: Arc<FakeOracle>,
    clock: Arc<FakeClock>,
}

fn settings(channels: &[&str]) -> HitlCriticalDeliverySettings {
    HitlCriticalDeliverySettings {
        enabled_channels: channels.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    }
}

fn policy_for(settings: HitlCriticalDeliverySettings) -> DeliveryPolicy {
    let mut config = MagicianConfig::default();
    config.hitl.critical_delivery = settings;
    let mut envoy = EnvoyConfig::default();
    envoy
        .owner_identities
        .insert("telegram".into(), vec!["777001".into()]);
    envoy
        .owner_identities
        .insert("kapso".into(), vec!["919999900000".into()]);
    config.envoy = envoy;
    config.mobile_access.public_origin = Some("https://magician.example.test".into());
    DeliveryPolicy::from_config(&config)
}

fn make_fixture(
    settings: HitlCriticalDeliverySettings,
    origin: Option<(String, String)>,
    push_registrations: usize,
) -> Fixture {
    let transport = Arc::new(FakeTransport::default());
    let push = Arc::new(FakePush {
        registrations: push_registrations,
        accept: true,
        requested: AtomicUsize::new(0),
        resolved: AtomicUsize::new(0),
    });
    let oracle = Arc::new(FakeOracle {
        pending: Mutex::new(HashSet::new()),
        origin,
        checks: AtomicUsize::new(0),
    });
    // 2026-09-22 12:00 UTC.
    let clock = Arc::new(FakeClock(AtomicI64::new(1_790_078_400_000)));
    let coordinator = Arc::new(
        DeliveryCoordinator::with_clock(
            policy_for(settings),
            Arc::new(DeliveryStore::ephemeral()),
            Some(push.clone() as Arc<dyn PushSink>),
            transport.clone() as Arc<dyn ChannelTransport>,
            oracle.clone() as Arc<dyn RequestOracle>,
            clock.clone() as Arc<dyn Clock>,
        )
        .with_timings(
            Duration::from_millis(200),
            Duration::from_millis(300),
            vec![Duration::from_millis(10), Duration::from_millis(20)],
        ),
    );
    Fixture {
        coordinator,
        transport,
        push,
        oracle,
        clock,
    }
}

/// A second coordinator over the SAME store: the next run of the process.
fn fixture_over(store: Arc<DeliveryStore>, settings: HitlCriticalDeliverySettings) -> Fixture {
    let transport = Arc::new(FakeTransport::default());
    let push = Arc::new(FakePush {
        registrations: 0,
        accept: true,
        requested: AtomicUsize::new(0),
        resolved: AtomicUsize::new(0),
    });
    let oracle = Arc::new(FakeOracle {
        pending: Mutex::new(HashSet::new()),
        origin: None,
        checks: AtomicUsize::new(0),
    });
    let clock = Arc::new(FakeClock(AtomicI64::new(1_790_078_400_000)));
    let coordinator = Arc::new(
        DeliveryCoordinator::with_clock(
            policy_for(settings),
            store,
            Some(push.clone() as Arc<dyn PushSink>),
            transport.clone() as Arc<dyn ChannelTransport>,
            oracle.clone() as Arc<dyn RequestOracle>,
            clock.clone() as Arc<dyn Clock>,
        )
        .with_timings(
            Duration::from_millis(200),
            Duration::from_millis(300),
            vec![Duration::from_millis(10), Duration::from_millis(20)],
        ),
    );
    Fixture {
        coordinator,
        transport,
        push,
        oracle,
        clock,
    }
}

fn otp_schema(deadline_ms: Option<i64>) -> Value {
    let mut spec = json!({"kind": "otp", "one_time": true, "expected_destination": "https://accounts.example.test"});
    if let Some(deadline) = deadline_ms {
        spec["collection_deadline_ms"] = json!(deadline);
    }
    json!({"input_type": "otp", "prompt": CANARY, "hint": CANARY, "options": [{"id": "x", "label": CANARY}], "sensitive": spec})
}

async fn request(fixture: &Fixture, correlation_id: &str, schema: Option<Value>) {
    fixture
        .oracle
        .pending
        .lock()
        .await
        .insert(correlation_id.to_string());
    fixture
        .coordinator
        .handle(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.into(),
            source: "user_request".into(),
            input_type: "otp".into(),
            prompt: CANARY.into(),
            hint: Some(CANARY.into()),
            input_schema: schema,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("owner".into()),
            workspace: Some("ws".into()),
            timestamp: fixture.clock.now_ms() - 40,
        })
        .await;
}

async fn resolve(fixture: &Fixture, correlation_id: &str, outcome: &str) {
    fixture.oracle.pending.lock().await.remove(correlation_id);
    fixture
        .coordinator
        .handle(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.into(),
            source: "user_request".into(),
            outcome: outcome.into(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("owner".into()),
            workspace: Some("ws".into()),
            timestamp: fixture.clock.now_ms(),
        })
        .await;
}

async fn wait_state(fixture: &Fixture, delivery_id: &str, state: DeliveryState) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let current = fixture
            .coordinator
            .store()
            .get(delivery_id)
            .await
            .map(|r| r.state);
        if current == Some(state) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "delivery {delivery_id} never reached {state:?}: {current:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_alert(fixture: &Fixture, channel_type: &str, nth: usize) -> String {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let alerts: Vec<String> = fixture
                .transport
                .alerts()
                .into_iter()
                .filter(|(_, c)| c == channel_type)
                .map(|(id, _)| id)
                .collect();
            if alerts.len() > nth {
                return alerts[nth].clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("an alert was offered")
}

async fn records_for(fixture: &Fixture, correlation_id: &str) -> Vec<DeliveryRecord> {
    fixture
        .coordinator
        .store()
        .for_correlation("owner", "ws", correlation_id)
        .await
}

#[tokio::test(start_paused = true)]
async fn a_critical_request_fans_out_to_every_enabled_destination_with_a_record_before_any_send() {
    let fixture = make_fixture(settings(&["telegram", "kapso"]), None, 2);
    request(
        &fixture,
        "req-1",
        Some(otp_schema(Some(fixture.clock.now_ms() + 600_000))),
    )
    .await;
    let records = records_for(&fixture, "req-1").await;
    assert_eq!(records.len(), 3, "push + telegram + kapso: {records:?}");
    assert!(records
        .iter()
        .all(|r| r.state == DeliveryState::Queued || r.state == DeliveryState::ProviderAccepted));
    // The telegram bot claims and reports; the kapso bot never shows up.
    let telegram = wait_alert(&fixture, "telegram", 0).await;
    let grant = fixture
        .coordinator
        .claim(
            &telegram,
            "owner",
            "ws",
            "telegram",
            "bot:telegram",
            "gen-1",
        )
        .await
        .expect("claim");
    assert_eq!(grant.address, "777001");
    assert_eq!(grant.alert["reason"], "a verification code");
    assert_eq!(grant.alert["service_alias"], "accounts.example.test");
    assert_eq!(
        grant.alert["open_url"],
        "https://magician.example.test/attention?attention=1&attention_item=req-1"
    );
    assert!(grant.deadline_ms.is_some());
    fixture
        .coordinator
        .report(
            &telegram,
            "owner",
            "ws",
            "bot:telegram",
            None,
            DeliveryOutcome::ProviderAccepted {
                provider_message_id: Some("m-9".into()),
            },
        )
        .await
        .expect("report");
    wait_state(&fixture, &telegram, DeliveryState::ProviderAccepted).await;
    let kapso = wait_alert(&fixture, "kapso", 0).await;
    wait_state(&fixture, &kapso, DeliveryState::Unavailable).await;
    let push = records_for(&fixture, "req-1")
        .await
        .into_iter()
        .find(|r| r.destination == Destination::Push)
        .unwrap();
    wait_state(&fixture, &push.id, DeliveryState::ProviderAccepted).await;
    let push = fixture.coordinator.store().get(&push.id).await.unwrap();
    assert_eq!(push.registrations, Some(2));
    assert_eq!(fixture.push.requested.load(Ordering::SeqCst), 1);
    let accepted = fixture.coordinator.store().get(&telegram).await.unwrap();
    assert_eq!(accepted.provider_message_id.as_deref(), Some("m-9"));
    assert!(accepted.accepted_at_ms.is_some());
    assert_eq!(
        accepted
            .claimed_by
            .as_ref()
            .map(|b| b.connection_generation.as_str()),
        Some("gen-1")
    );
    let status = fixture
        .coordinator
        .status("owner", "ws", Some("req-1"))
        .await;
    assert_eq!(status.deliveries.len(), 3);
    assert!(status.channels_last_claimed_ms.contains_key("telegram"));
    assert!(!status.channels_last_claimed_ms.contains_key("kapso"));
    assert_eq!(status.latency.request_to_enqueue.samples, 3);
}

#[tokio::test(start_paused = true)]
async fn an_ordinary_request_only_pushes_and_leaves_no_record() {
    let fixture = make_fixture(settings(&["telegram"]), None, 1);
    request(
        &fixture,
        "req-2",
        Some(json!({"input_type": "choice", "prompt": "Pick one", "options": []})),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(records_for(&fixture, "req-2").await.is_empty());
    assert!(fixture.transport.alerts().is_empty());
    assert_eq!(fixture.push.requested.load(Ordering::SeqCst), 1);
    resolve(&fixture, "req-2", "responded").await;
    assert_eq!(fixture.push.resolved.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn the_origin_channel_destination_is_relayed_by_origin_and_never_offered() {
    let fixture = make_fixture(
        settings(&["telegram", "kapso"]),
        Some(("telegram".into(), "777001".into())),
        0,
    );
    request(&fixture, "req-3", Some(otp_schema(None))).await;
    let records = records_for(&fixture, "req-3").await;
    let telegram = records
        .iter()
        .find(|r| r.destination.channel_type() == Some("telegram"))
        .unwrap();
    assert_eq!(telegram.state, DeliveryState::RelayedByOrigin);
    let kapso = records
        .iter()
        .find(|r| r.destination.channel_type() == Some("kapso"))
        .unwrap();
    wait_alert(&fixture, "kapso", 0).await;
    wait_state(&fixture, &kapso.id, DeliveryState::Unavailable).await;
    assert!(
        fixture.transport.alerts().iter().all(|(_, c)| c == "kapso"),
        "the relayed destination is never offered"
    );
    let push = records
        .iter()
        .find(|r| r.destination == Destination::Push)
        .unwrap();
    wait_state(&fixture, &push.id, DeliveryState::Unavailable).await;
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&push.id)
            .await
            .unwrap()
            .reason
            .as_deref(),
        Some("no registered device")
    );
}

#[tokio::test(start_paused = true)]
async fn staged_policy_offers_the_next_destination_only_when_the_first_is_not_accepted() {
    let staged = HitlCriticalDeliverySettings {
        policy: CriticalDeliveryPolicy::Staged,
        staged_fallback_secs: 5,
        push_enabled: false,
        ..settings(&["telegram", "kapso"])
    };
    // First: telegram accepts in time → kapso is skipped.
    let fixture = make_fixture(staged.clone(), None, 0);
    request(&fixture, "req-4", Some(otp_schema(None))).await;
    let telegram = wait_alert(&fixture, "telegram", 0).await;
    fixture
        .coordinator
        .claim(&telegram, "owner", "ws", "telegram", "bot:telegram", "g")
        .await
        .unwrap();
    fixture
        .coordinator
        .report(
            &telegram,
            "owner",
            "ws",
            "bot:telegram",
            None,
            DeliveryOutcome::ProviderAccepted {
                provider_message_id: None,
            },
        )
        .await
        .unwrap();
    let kapso = records_for(&fixture, "req-4")
        .await
        .into_iter()
        .find(|r| r.destination.channel_type() == Some("kapso"))
        .unwrap();
    wait_state(&fixture, &kapso.id, DeliveryState::Skipped).await;
    assert!(fixture
        .transport
        .alerts()
        .iter()
        .all(|(_, c)| c == "telegram"));
    // Second: nobody claims telegram → kapso is offered after the fallback.
    let fixture = make_fixture(staged, None, 0);
    request(&fixture, "req-5", Some(otp_schema(None))).await;
    let telegram = wait_alert(&fixture, "telegram", 0).await;
    wait_state(&fixture, &telegram, DeliveryState::Unavailable).await;
    let kapso = wait_alert(&fixture, "kapso", 0).await;
    wait_state(&fixture, &kapso, DeliveryState::Unavailable).await;
}

#[tokio::test(start_paused = true)]
async fn quiet_hours_hold_until_the_window_ends_unless_the_request_is_time_bound() {
    let quiet = QuietHoursSettings {
        start: "22:00".into(),
        end: "07:00".into(),
        timezone: "UTC".into(),
        interrupt_for_time_bound: true,
    };
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            quiet_hours: Some(quiet),
            push_enabled: false,
            ..settings(&["telegram"])
        },
        None,
        0,
    );
    // 23:00 UTC.
    fixture.clock.0.store(1_790_118_000_000, Ordering::SeqCst);
    request(&fixture, "req-6", Some(otp_schema(None))).await;
    let record = &records_for(&fixture, "req-6").await[0];
    assert_eq!(record.state, DeliveryState::Held);
    assert_eq!(record.reason.as_deref(), Some("held by quiet hours"));
    assert!(
        fixture.transport.alerts().is_empty(),
        "nothing is offered during quiet hours"
    );
    // A time-bound request interrupts.
    request(
        &fixture,
        "req-7",
        Some(otp_schema(Some(fixture.clock.now_ms() + 300_000))),
    )
    .await;
    let urgent = wait_alert(&fixture, "telegram", 0).await;
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&urgent)
            .await
            .unwrap()
            .correlation_id,
        "req-7"
    );
    // When the window ends the held alert goes out (paused time is moved
    // past 07:00 by hand: the poll loops would otherwise creep forward).
    let held = record.id.clone();
    tokio::time::advance(Duration::from_secs(9 * 3600)).await;
    wait_state(&fixture, &held, DeliveryState::Unavailable).await;
    assert!(
        fixture.transport.alerts().iter().any(|(id, _)| id == &held),
        "the held alert was offered after the window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_request_resolved_while_queued_is_retired_without_a_send_and_a_sent_card_is_retired() {
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram", "kapso"])
        },
        None,
        0,
    );
    request(&fixture, "req-8", Some(otp_schema(None))).await;
    let telegram = wait_alert(&fixture, "telegram", 0).await;
    let kapso = wait_alert(&fixture, "kapso", 0).await;
    fixture
        .coordinator
        .claim(&telegram, "owner", "ws", "telegram", "bot:telegram", "g")
        .await
        .unwrap();
    fixture
        .coordinator
        .report(
            &telegram,
            "owner",
            "ws",
            "bot:telegram",
            None,
            DeliveryOutcome::ProviderAccepted {
                provider_message_id: None,
            },
        )
        .await
        .unwrap();
    resolve(&fixture, "req-8", "responded").await;
    let kapso_record = fixture.coordinator.store().get(&kapso).await.unwrap();
    assert_eq!(
        kapso_record.state,
        DeliveryState::Resolved,
        "the unclaimed offer is retired, not left to time out"
    );
    assert_eq!(kapso_record.reason.as_deref(), Some("request responded"));
    assert_eq!(
        fixture.transport.retired(),
        vec![telegram.clone()],
        "only the card that was sent is retired at the bot"
    );
    // A late claim of the retired delivery is refused.
    assert_eq!(
        fixture
            .coordinator
            .claim(&kapso, "owner", "ws", "kapso", "bot:kapso", "g")
            .await
            .unwrap_err(),
        ClaimError::NotClaimable(DeliveryState::Resolved)
    );
    // A replayed request for a resolved correlation alerts nobody.
    let before = fixture.transport.alerts().len();
    request(&fixture, "req-8", Some(otp_schema(None))).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fixture.transport.alerts().len(), before);
}

#[tokio::test(start_paused = true)]
async fn failures_retry_with_a_recheck_and_a_deadline_passed_in_the_queue_expires() {
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
        None,
        0,
    );
    // Three failed reports → failed, three attempts, three rechecks.
    request(&fixture, "req-9", Some(otp_schema(None))).await;
    for attempt in 0..3 {
        let id = wait_alert(&fixture, "telegram", attempt).await;
        fixture
            .coordinator
            .claim(&id, "owner", "ws", "telegram", "bot:telegram", "g")
            .await
            .unwrap();
        fixture
            .coordinator
            .report(
                &id,
                "owner",
                "ws",
                "bot:telegram",
                None,
                DeliveryOutcome::Failed {
                    reason: "provider 500".into(),
                },
            )
            .await
            .unwrap();
        if attempt < 2 {
            wait_state(&fixture, &id, DeliveryState::Queued).await;
        }
    }
    let record = &records_for(&fixture, "req-9").await[0];
    wait_state(&fixture, &record.id, DeliveryState::Failed).await;
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&record.id)
            .await
            .unwrap()
            .attempts,
        3
    );
    assert_eq!(fixture.oracle.checks.load(Ordering::SeqCst), 3);
    // A deadline that passes between attempts expires the record before the retry.
    let deadline = fixture.clock.now_ms() + 1_000;
    request(&fixture, "req-10", Some(otp_schema(Some(deadline)))).await;
    let id = wait_alert(&fixture, "telegram", 3).await;
    fixture
        .coordinator
        .claim(&id, "owner", "ws", "telegram", "bot:telegram", "g")
        .await
        .unwrap();
    fixture.clock.0.fetch_add(5_000, Ordering::SeqCst);
    fixture
        .coordinator
        .report(
            &id,
            "owner",
            "ws",
            "bot:telegram",
            None,
            DeliveryOutcome::Failed {
                reason: "provider 500".into(),
            },
        )
        .await
        .unwrap();
    wait_state(&fixture, &id, DeliveryState::Expired).await;
    assert_eq!(
        fixture.transport.alerts().len(),
        4,
        "no retry after the deadline"
    );
}

/// A restart used to mark every live row `failed` and stop: the owner was
/// never alerted again for a credential ask that outlived the runtime, and
/// nothing else re-announces one. The coordinator's own rows are the record.
#[tokio::test(start_paused = true)]
async fn a_restart_re_offers_the_alerts_the_previous_run_left_live() {
    let first = make_fixture(
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
        None,
        0,
    );
    request(&first, "req-restart", Some(otp_schema(None))).await;
    let stale = wait_alert(&first, "telegram", 0).await;
    let store = Arc::clone(first.coordinator.store());

    // The next run: the request is still open, so the alert is offered again
    // and the row the previous run left behind is retired with its own reason.
    let next = fixture_over(
        Arc::clone(&store),
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
    );
    next.oracle
        .pending
        .lock()
        .await
        .insert("req-restart".to_string());
    next.coordinator.recover_after_restart().await;
    let reoffered = wait_alert(&next, "telegram", 0).await;
    assert_ne!(reoffered, stale, "a fresh row, not the stale one");
    let stale_row = store.get(&stale).await.expect("the stale row is kept");
    assert!(stale_row.state.is_terminal(), "{:?}", stale_row.state);
    assert!(
        stale_row.reason.unwrap().contains("re-offered"),
        "the row says why"
    );
    let fresh = store.get(&reoffered).await.expect("the fresh row");
    assert_eq!(fresh.correlation_id, "req-restart");
    assert_eq!(
        fresh.source, "user_request",
        "the lane is carried so the next restart can ask again"
    );

    // A request that closed while the runtime was down is retired, not alerted.
    // Written straight into the store so the assertion does not race the
    // delivery task of the run above.
    let third = fixture_over(
        Arc::clone(&store),
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
    );
    let orphan = orphan_row("d-closed", "req-gone", "user_request", third.clock.now_ms());
    third.coordinator.store().insert(orphan).await;
    third.coordinator.recover_after_restart().await;
    assert!(
        !third.transport.alerts().iter().any(|(_, _)| true),
        "nothing is offered for a request that closed while the runtime was down"
    );
    let closed = store.get("d-closed").await.expect("the row");
    assert_eq!(closed.state, DeliveryState::Failed);
    assert!(closed
        .reason
        .unwrap()
        .contains("closed while the runtime was down"));

    // A row written before the lane was recorded can only be retired.
    let fourth = fixture_over(
        Arc::clone(&store),
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
    );
    fourth
        .coordinator
        .store()
        .insert(orphan_row(
            "d-legacy",
            "req-legacy",
            "",
            fourth.clock.now_ms(),
        ))
        .await;
    fourth
        .oracle
        .pending
        .lock()
        .await
        .insert("req-legacy".to_string());
    fourth.coordinator.recover_after_restart().await;
    let legacy = store.get("d-legacy").await.expect("the row");
    assert_eq!(legacy.state, DeliveryState::Failed);
    assert!(legacy
        .reason
        .unwrap()
        .contains("does not name the ask's lane"));
}

/// One live row as a previous run would have left it.
fn orphan_row(
    id: &str,
    correlation_id: &str,
    source: &str,
    now_ms: i64,
) -> crate::magician_v2::hitl_delivery::records::DeliveryRecord {
    crate::magician_v2::hitl_delivery::records::DeliveryRecord {
        id: id.into(),
        principal: "owner".into(),
        workspace: "ws".into(),
        correlation_id: correlation_id.into(),
        revision: 1,
        kind: "request".into(),
        destination: Destination::Channel {
            channel_type: "telegram".into(),
            address: "777001".into(),
        },
        state: DeliveryState::Queued,
        attempts: 1,
        requested_at_ms: now_ms - 1_000,
        enqueued_at_ms: now_ms - 900,
        claimed_at_ms: None,
        accepted_at_ms: None,
        updated_at_ms: now_ms - 900,
        provider_message_id: None,
        reason: None,
        claimed_by: None,
        registrations: None,
        source: source.into(),
        execution_id: None,
        deadline_ms: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_claimed_but_unreported_delivery_is_ambiguous_and_not_retried() {
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
        None,
        0,
    );
    request(&fixture, "req-11", Some(otp_schema(None))).await;
    let id = wait_alert(&fixture, "telegram", 0).await;
    fixture
        .coordinator
        .claim(&id, "owner", "ws", "telegram", "bot:telegram", "g")
        .await
        .unwrap();
    wait_state(&fixture, &id, DeliveryState::Ambiguous).await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(fixture.transport.alerts().len(), 1);
    // A late report on an ambiguous row is refused: the state is honest.
    assert_eq!(
        fixture
            .coordinator
            .report(
                &id,
                "owner",
                "ws",
                "bot:telegram",
                None,
                DeliveryOutcome::ProviderAccepted {
                    provider_message_id: None
                }
            )
            .await
            .unwrap_err(),
        ReportError::NotReportable(DeliveryState::Ambiguous)
    );
}

#[tokio::test(start_paused = true)]
async fn claims_and_reports_are_checked_for_scope_channel_and_claimant() {
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            push_enabled: false,
            ..settings(&["telegram"])
        },
        None,
        0,
    );
    request(&fixture, "req-12", Some(otp_schema(None))).await;
    let id = wait_alert(&fixture, "telegram", 0).await;
    assert_eq!(
        fixture
            .coordinator
            .claim("nope", "owner", "ws", "telegram", "b", "g")
            .await
            .unwrap_err(),
        ClaimError::UnknownDelivery
    );
    assert_eq!(
        fixture
            .coordinator
            .claim(&id, "intruder", "ws", "telegram", "b", "g")
            .await
            .unwrap_err(),
        ClaimError::WrongScope
    );
    assert_eq!(
        fixture
            .coordinator
            .claim(&id, "owner", "ws", "kapso", "b", "g")
            .await
            .unwrap_err(),
        ClaimError::WrongChannel
    );
    fixture
        .coordinator
        .claim(&id, "owner", "ws", "Telegram", "bot:a", "g")
        .await
        .expect("channel type is case-insensitive");
    assert_eq!(
        fixture
            .coordinator
            .claim(&id, "owner", "ws", "telegram", "bot:b", "g")
            .await
            .unwrap_err(),
        ClaimError::NotClaimable(DeliveryState::Claimed)
    );
    assert_eq!(
        fixture
            .coordinator
            .report(
                &id,
                "owner",
                "ws",
                "bot:b",
                None,
                DeliveryOutcome::ConfirmedDelivered
            )
            .await
            .unwrap_err(),
        ReportError::NotTheClaimant
    );
    assert_eq!(
        fixture
            .coordinator
            .report(
                &id,
                "owner",
                "other",
                "bot:a",
                None,
                DeliveryOutcome::ConfirmedDelivered
            )
            .await
            .unwrap_err(),
        ReportError::WrongScope
    );
    // One bot NAME can be two processes. The orphan reporting for its
    // replacement's send would take the delivery terminal on a send that never
    // happened, so a report naming another connection is not the claimant's.
    assert_eq!(
        fixture
            .coordinator
            .report(
                &id,
                "owner",
                "ws",
                "bot:a",
                Some("g-orphan"),
                DeliveryOutcome::ConfirmedDelivered
            )
            .await
            .unwrap_err(),
        ReportError::NotTheClaimant
    );
    fixture
        .coordinator
        .report(
            &id,
            "owner",
            "ws",
            "bot:a",
            Some("g"),
            DeliveryOutcome::ProviderAccepted {
                provider_message_id: None,
            },
        )
        .await
        .unwrap();
    // An older SDK names no connection: admitted, as before.
    let delivered = fixture
        .coordinator
        .report(
            &id,
            "owner",
            "ws",
            "bot:a",
            None,
            DeliveryOutcome::ConfirmedDelivered,
        )
        .await
        .unwrap();
    assert_eq!(delivered.state, DeliveryState::ConfirmedDelivered);
}

#[tokio::test(start_paused = true)]
async fn nothing_the_producer_wrote_reaches_a_record_an_offer_or_a_grant() {
    let fixture = make_fixture(settings(&["telegram"]), None, 1);
    request(&fixture, "req-13", Some(otp_schema(None))).await;
    let id = wait_alert(&fixture, "telegram", 0).await;
    let grant = fixture
        .coordinator
        .claim(&id, "owner", "ws", "telegram", "bot", "g")
        .await
        .unwrap();
    let everything = format!(
        "{:?} {:?} {}",
        records_for(&fixture, "req-13").await,
        fixture.transport.offered.lock().unwrap(),
        serde_json::to_string(&grant).unwrap()
    );
    assert!(!everything.contains(CANARY), "{everything}");
    let status =
        serde_json::to_string(&fixture.coordinator.status("owner", "ws", None).await).unwrap();
    assert!(
        !status.contains("777001"),
        "the status masks the address: {status}"
    );
    assert!(status.contains("telegram:…01"));
}

#[tokio::test(start_paused = true)]
async fn the_owners_test_reaches_every_destination_and_ignores_quiet_hours() {
    let quiet = QuietHoursSettings {
        start: "00:00".into(),
        end: "23:59".into(),
        timezone: "UTC".into(),
        interrupt_for_time_bound: false,
    };
    let fixture = make_fixture(
        HitlCriticalDeliverySettings {
            quiet_hours: Some(quiet),
            ..settings(&["telegram", "kapso"])
        },
        None,
        1,
    );
    let records = fixture.coordinator.send_test("owner", "ws").await;
    assert_eq!(records.len(), 3);
    assert!(records
        .iter()
        .all(|r| r.kind == "test" && r.state != DeliveryState::Held));
    let id = wait_alert(&fixture, "telegram", 0).await;
    let grant = fixture
        .coordinator
        .claim(&id, "owner", "ws", "telegram", "bot", "g")
        .await
        .unwrap();
    assert_eq!(grant.kind, "test");
    assert!(grant.alert["text"].as_str().unwrap().contains("test alert"));
    assert_eq!(fixture.push.requested.load(Ordering::SeqCst), 1);
}

struct FakeRetrieval(bool);
#[async_trait]
impl RetrievalOracle for FakeRetrieval {
    async fn retrieval_expected(&self, _: &str, _: &str) -> bool {
        self.0
    }
}

#[tokio::test(start_paused = true)]
async fn a_code_ask_under_automatic_retrieval_gives_the_sources_a_grace_before_channel_alerts() {
    let settings = HitlCriticalDeliverySettings {
        push_enabled: true,
        ..settings(&["telegram"])
    };
    let fixture = make_fixture(settings, None, 1);
    let mut policy = fixture.coordinator.policy().await;
    policy.retrieval_grace_secs = 20;
    fixture.coordinator.reload(policy).await;
    fixture
        .coordinator
        .set_retrieval_oracle(Arc::new(FakeRetrieval(true)))
        .await;
    // A long deadline: the channel alert waits, the push does not.
    request(
        &fixture,
        "req-g1",
        Some(otp_schema(Some(fixture.clock.now_ms() + 600_000))),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        fixture.push.requested.load(Ordering::SeqCst),
        1,
        "push goes out at once"
    );
    assert!(
        fixture.transport.alerts().is_empty(),
        "the channel waits for retrieval"
    );
    let telegram = records_for(&fixture, "req-g1")
        .await
        .into_iter()
        .find(|r| r.destination.channel_type() == Some("telegram"))
        .unwrap();
    assert_eq!(
        telegram.reason.as_deref(),
        Some("waiting briefly for automatic code retrieval")
    );
    // The code was retrieved and the ask resolved inside the grace: no alert ever goes out.
    resolve(&fixture, "req-g1", "responded").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(fixture.transport.alerts().is_empty());
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&telegram.id)
            .await
            .unwrap()
            .state,
        DeliveryState::Resolved
    );
    // Retrieval that does not deliver: the alert goes out after the grace.
    request(
        &fixture,
        "req-g2",
        Some(otp_schema(Some(fixture.clock.now_ms() + 600_000))),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(21)).await;
    let id = wait_alert(&fixture, "telegram", 0).await;
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&id)
            .await
            .unwrap()
            .correlation_id,
        "req-g2"
    );
    // A short deadline skips the grace; so does a scope with nothing to retrieve from.
    request(
        &fixture,
        "req-g3",
        Some(otp_schema(Some(fixture.clock.now_ms() + 60_000))),
    )
    .await;
    wait_alert(&fixture, "telegram", 1).await;
    // Retrieval that gives up early (ambiguous, unavailable) ends the grace
    // early: the alert goes out at once, not at the end of the grace.
    request(
        &fixture,
        "req-g5",
        Some(otp_schema(Some(fixture.clock.now_ms() + 600_000))),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        fixture.transport.alerts().len(),
        2,
        "still inside the grace"
    );
    fixture
        .coordinator
        .handle(RuntimeTransportEvent::VerificationRetrievalStatus {
            correlation_id: "req-g5".into(),
            status: "ambiguous".into(),
            sources: vec!["gmail".into()],
            reason: Some("two codes".into()),
            principal: Some("owner".into()),
            workspace: Some("ws".into()),
            timestamp: fixture.clock.now_ms(),
        })
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        fixture.transport.alerts().len(),
        3,
        "the alert went out 5 s into a 20 s grace"
    );
    let id = wait_alert(&fixture, "telegram", 2).await;
    assert_eq!(
        fixture
            .coordinator
            .store()
            .get(&id)
            .await
            .unwrap()
            .correlation_id,
        "req-g5"
    );
    assert!(
        fixture
            .coordinator
            .store()
            .get(&id)
            .await
            .unwrap()
            .reason
            .as_deref()
            != Some("waiting briefly for automatic code retrieval")
    );
    fixture
        .coordinator
        .set_retrieval_oracle(Arc::new(FakeRetrieval(false)))
        .await;
    request(
        &fixture,
        "req-g4",
        Some(otp_schema(Some(fixture.clock.now_ms() + 600_000))),
    )
    .await;
    wait_alert(&fixture, "telegram", 3).await;
}
