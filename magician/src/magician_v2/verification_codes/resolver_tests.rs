use std::{
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::super::matching::{MessageEvidence, SourceKind};
use super::super::sources::*;
use super::*;

const CANARY: &str = "canary-body-Q7";

struct FakeClock(AtomicI64);
impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct FakeTransport {
    events: std::sync::Mutex<Vec<RuntimeTransportEvent>>,
}
impl StatusTransport for FakeTransport {
    fn emit(&self, event: RuntimeTransportEvent) {
        self.events.lock().unwrap().push(event);
    }
}
impl FakeTransport {
    fn statuses(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                RuntimeTransportEvent::VerificationRetrievalStatus { status, .. } => {
                    Some(status.clone())
                },
                _ => None,
            })
            .collect()
    }
}

struct FakeRegistry {
    sources: Mutex<Vec<AuthorizedSource>>,
    reads: AtomicUsize,
}
#[async_trait]
impl SourceRegistry for FakeRegistry {
    async fn authorized_sources(&self, _: &str, _: &str) -> Vec<AuthorizedSource> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.sources.lock().await.clone()
    }
}

struct FakeWatch {
    kind: SourceKind,
    /// Messages handed out per poll, in order; a missing entry yields none.
    per_poll: Mutex<Vec<Vec<MessageEvidence>>>,
    polls: AtomicUsize,
    /// What the source says beyond its messages (every poll).
    signal: SourceSignal,
    /// Poll indices that fail; `None` fails every poll.
    fail_on: Option<Vec<usize>>,
}
impl FakeWatch {
    fn new(kind: SourceKind, per_poll: Vec<Vec<MessageEvidence>>) -> Self {
        Self {
            kind,
            per_poll: Mutex::new(per_poll),
            polls: AtomicUsize::new(0),
            signal: SourceSignal::None,
            fail_on: Some(vec![]),
        }
    }
}
#[async_trait]
impl SourceWatch for FakeWatch {
    fn kind(&self) -> SourceKind {
        self.kind
    }
    async fn poll(
        &self,
        _: &AuthorizedSource,
        _: &ChallengeContext,
        _: i64,
        _: usize,
    ) -> Result<WatchPoll, String> {
        let n = self.polls.fetch_add(1, Ordering::SeqCst);
        if self.fail_on.as_ref().is_none_or(|fails| fails.contains(&n)) {
            return Err("provider said no".to_string());
        }
        let mut per_poll = self.per_poll.lock().await;
        let messages = if n < per_poll.len() {
            std::mem::take(&mut per_poll[n])
        } else {
            Vec::new()
        };
        Ok(WatchPoll {
            messages,
            exhausted: self.signal != SourceSignal::None,
            signal: self.signal.clone(),
        })
    }
}

struct FakeSink {
    answers: Mutex<Vec<(String, String)>>,
    outcome: AnswerOutcome,
}
#[async_trait]
impl ChallengeAnswerSink for FakeSink {
    async fn answer(&self, target: &AnswerTarget, code: Zeroizing<String>) -> AnswerOutcome {
        self.answers
            .lock()
            .await
            .push((target.correlation_id.clone(), code.to_string()));
        self.outcome.clone()
    }
}

struct Fixture {
    resolver: Arc<VerificationCodeResolver>,
    transport: Arc<FakeTransport>,
    registry: Arc<FakeRegistry>,
    sink: Arc<FakeSink>,
    clock: Arc<FakeClock>,
}

fn gmail_source() -> AuthorizedSource {
    AuthorizedSource {
        kind: SourceKind::Gmail,
        account: "personal".into(),
        label: "gmail …al".into(),
    }
}

fn mail(id: &str, received_at_ms: i64, body: &str) -> MessageEvidence {
    MessageEvidence {
        source: SourceKind::Gmail,
        account: "personal".into(),
        message_id: id.into(),
        received_at_ms,
        sender_address: Some("no-reply@accounts.example.test".into()),
        authenticated: Some(true),
        subject: Some(CANARY.into()),
        body: format!("{body} {CANARY}"),
    }
}

fn make_fixture(sources: Vec<AuthorizedSource>, outcome: AnswerOutcome) -> Fixture {
    let transport = Arc::new(FakeTransport::default());
    let registry = Arc::new(FakeRegistry {
        sources: Mutex::new(sources),
        reads: AtomicUsize::new(0),
    });
    let sink = Arc::new(FakeSink {
        answers: Mutex::new(Vec::new()),
        outcome,
    });
    let clock = Arc::new(FakeClock(AtomicI64::new(1_790_078_400_000)));
    let resolver = Arc::new(VerificationCodeResolver::with_clock(
        HitlVerificationCodesSettings::default(),
        registry.clone() as Arc<dyn SourceRegistry>,
        sink.clone() as Arc<dyn ChallengeAnswerSink>,
        transport.clone() as Arc<dyn StatusTransport>,
        clock.clone() as Arc<dyn Clock>,
        Duration::from_millis(50),
    ));
    Fixture {
        resolver,
        transport,
        registry,
        sink,
        clock,
    }
}

fn otp_schema(deadline_ms: Option<i64>) -> Value {
    let mut spec = json!({"kind": "otp", "one_time": true, "expected_destination": "https://accounts.example.test"});
    if let Some(deadline) = deadline_ms {
        spec["collection_deadline_ms"] = json!(deadline);
    }
    json!({"input_type": "otp", "prompt": "Enter the code", "sensitive": spec})
}

async fn request(fixture: &Fixture, correlation_id: &str, schema: Value) {
    request_in(fixture, "owner", "ws", correlation_id, schema).await;
}

async fn request_in(
    fixture: &Fixture,
    principal: &str,
    workspace: &str,
    correlation_id: &str,
    schema: Value,
) {
    fixture
        .resolver
        .handle(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.into(),
            source: "user_request".into(),
            input_type: "otp".into(),
            prompt: "Enter the code".into(),
            hint: None,
            input_schema: Some(schema),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.into()),
            workspace: Some(workspace.into()),
            timestamp: fixture.clock.now_ms(),
        })
        .await;
}

async fn resolve(fixture: &Fixture, correlation_id: &str) {
    fixture
        .resolver
        .handle(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.into(),
            source: "user_request".into(),
            outcome: "responded".into(),
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

async fn wait_status(fixture: &Fixture, correlation_id: &str, wanted: RetrievalStatus) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let current = fixture
            .resolver
            .status("owner", "ws", correlation_id)
            .await
            .map(|s| s.status);
        if current == Some(wanted) {
            return;
        }
        // A stopped challenge leaves the registry; its last status went out
        // on the transport.
        if wanted == RetrievalStatus::Stopped && current.is_none() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{correlation_id} never reached {wanted:?}: {current:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn watch_with(fixture: &Fixture, per_poll: Vec<Vec<MessageEvidence>>) -> Arc<FakeWatch> {
    let watch = Arc::new(FakeWatch::new(SourceKind::Gmail, per_poll));
    fixture.resolver.register_watch(watch.clone()).await;
    watch
}

fn sms(id: &str, received_at_ms: i64, body: &str) -> MessageEvidence {
    MessageEvidence {
        source: SourceKind::Messages,
        account: "mac".into(),
        message_id: id.into(),
        received_at_ms,
        sender_address: Some("VERIFY".into()),
        authenticated: None,
        subject: None,
        body: format!("{body} {CANARY}"),
    }
}

fn messages_source() -> AuthorizedSource {
    AuthorizedSource {
        kind: SourceKind::Messages,
        account: "mac".into(),
        label: "Messages".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_matching_message_answers_the_ask_once_and_the_status_never_carries_the_message() {
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let watch = watch_with(
        &fixture,
        vec![
            vec![],
            vec![mail("m1", now + 1_000, "Your verification code is 042917")],
        ],
    )
    .await;
    request(&fixture, "req-1", otp_schema(Some(now + 300_000))).await;
    assert_eq!(
        fixture
            .resolver
            .status("owner", "ws", "req-1")
            .await
            .map(|s| s.status),
        Some(RetrievalStatus::Waiting)
    );
    wait_status(&fixture, "req-1", RetrievalStatus::CodeUsed).await;
    assert_eq!(
        *fixture.sink.answers.lock().await,
        vec![("req-1".to_string(), "042917".to_string())]
    );
    assert_eq!(
        watch.polls.load(Ordering::SeqCst),
        2,
        "watching stopped once the code answered"
    );
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "code_used"]);
    // The ask's own resolution is the consequence of this answer, and it
    // does not erase the explanation: the decided row stays readable — the
    // retention sweep retires it — so a surface can say the code was used,
    // and nothing further goes out on the transport. Retiring it here (which
    // this resolver did until the P7 lane caught it) left
    // `GET /hitl/{id}/retrieval` answering `none` for every code the runtime
    // retrieved itself.
    resolve(&fixture, "req-1").await;
    assert_eq!(
        fixture
            .resolver
            .status("owner", "ws", "req-1")
            .await
            .map(|s| s.status),
        Some(RetrievalStatus::CodeUsed)
    );
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "code_used"]);
    let everything = format!("{:?}", fixture.transport.events.lock().unwrap());
    assert!(
        !everything.contains(CANARY) && !everything.contains("042917"),
        "{everything}"
    );
    assert!(
        fixture.registry.reads.load(Ordering::SeqCst) >= 4,
        "authority re-read before each fetch and before the answer"
    );
    assert!(fixture.resolver.retrieval_expected("owner", "ws").await);
}

#[tokio::test(start_paused = true)]
async fn the_persons_answer_first_stops_the_watch_without_answering() {
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let watch = watch_with(
        &fixture,
        vec![
            vec![],
            vec![],
            vec![mail("m1", now + 1_000, "Your verification code is 042917")],
        ],
    )
    .await;
    request(&fixture, "req-2", otp_schema(Some(now + 300_000))).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    resolve(&fixture, "req-2").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(fixture.sink.answers.lock().await.is_empty());
    assert!(watch.polls.load(Ordering::SeqCst) <= 2);
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-2")
        .await
        .is_none());
    // A resolution arriving second from the sink is not an error either.
    let fixture = fixture_with_outcome(AnswerOutcome::AlreadyResolved);
    let now = fixture.clock.now_ms();
    watch_with(
        &fixture,
        vec![vec![mail(
            "m1",
            now + 1_000,
            "Your verification code is 042917",
        )]],
    )
    .await;
    request(&fixture, "req-3", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-3", RetrievalStatus::Stopped).await;
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "stopped"]);
}

fn fixture_with_outcome(outcome: AnswerOutcome) -> Fixture {
    make_fixture(vec![gmail_source()], outcome)
}

#[tokio::test(start_paused = true)]
async fn revocation_stops_the_watch_and_no_source_means_the_person_types() {
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let watch = watch_with(
        &fixture,
        vec![
            vec![],
            vec![],
            vec![mail("m1", now + 1_000, "Your verification code is 042917")],
        ],
    )
    .await;
    request(&fixture, "req-4", otp_schema(Some(now + 300_000))).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    fixture.registry.sources.lock().await.clear();
    wait_status(&fixture, "req-4", RetrievalStatus::Unavailable).await;
    assert!(
        fixture.sink.answers.lock().await.is_empty(),
        "a revoked source never answers"
    );
    assert!(watch.polls.load(Ordering::SeqCst) <= 2);
    let status = fixture
        .resolver
        .status("owner", "ws", "req-4")
        .await
        .unwrap();
    assert!(status.reason.as_deref().unwrap().contains("withdrawn"));
    // No permitted source at all: unavailable from the start, nothing polled.
    let bare = make_fixture(vec![], AnswerOutcome::Accepted);
    let watch = watch_with(&bare, vec![]).await;
    request(&bare, "req-5", otp_schema(None)).await;
    assert_eq!(
        bare.resolver
            .status("owner", "ws", "req-5")
            .await
            .unwrap()
            .status,
        RetrievalStatus::Unavailable
    );
    assert_eq!(watch.polls.load(Ordering::SeqCst), 0);
    assert!(!bare.resolver.retrieval_expected("owner", "ws").await);
    // An ask that is not a code ask is ignored entirely.
    request(
        &bare,
        "req-6",
        json!({"input_type": "password", "sensitive": {"kind": "password"}}),
    )
    .await;
    assert!(bare.resolver.status("owner", "ws", "req-6").await.is_none());
}

#[tokio::test(start_paused = true)]
async fn ambiguity_and_a_stale_code_leave_the_ask_to_the_person() {
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let stale = mail("old", now - 200_000, "Your verification code is 979797");
    let a = mail("a", now + 1_000, "Your verification code is 131313");
    let b = mail("b", now + 1_500, "Your verification code is 242424");
    let watch = watch_with(&fixture, vec![vec![stale], vec![a, b]]).await;
    request(&fixture, "req-7", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-7", RetrievalStatus::Ambiguous).await;
    assert!(
        fixture.sink.answers.lock().await.is_empty(),
        "neither the stale code nor the newest is chosen"
    );
    assert_eq!(watch.polls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "ambiguous"]);
    // The person's later answer closes it quietly.
    resolve(&fixture, "req-7").await;
    assert_eq!(
        fixture.transport.statuses(),
        vec!["waiting", "ambiguous"],
        "no extra status after a decided state"
    );
}

#[tokio::test(start_paused = true)]
async fn the_watch_is_bounded_by_the_deadline_the_poll_cap_and_failures() {
    // Deadline: the watcher stops and the ask is left to the person.
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let watch = watch_with(&fixture, vec![]).await;
    request(&fixture, "req-8", otp_schema(Some(now + 1_000))).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    fixture.clock.0.fetch_add(5_000, Ordering::SeqCst);
    wait_status(&fixture, "req-8", RetrievalStatus::Unavailable).await;
    assert!(watch.polls.load(Ordering::SeqCst) <= 4);
    // Poll cap without a deadline.
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let watch = watch_with(&fixture, vec![]).await;
    request(&fixture, "req-9", otp_schema(None)).await;
    wait_status(&fixture, "req-9", RetrievalStatus::Unavailable).await;
    assert_eq!(watch.polls.load(Ordering::SeqCst) as u32, MAX_POLLS);
    // A source that keeps failing is given three chances, then reported.
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let failing = Arc::new(FakeWatch {
        fail_on: None,
        ..FakeWatch::new(SourceKind::Gmail, vec![])
    });
    fixture.resolver.register_watch(failing.clone()).await;
    request(&fixture, "req-10", otp_schema(None)).await;
    wait_status(&fixture, "req-10", RetrievalStatus::Unavailable).await;
    assert_eq!(failing.polls.load(Ordering::SeqCst), 3);
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-10")
        .await
        .unwrap()
        .reason
        .unwrap()
        .contains("could not be read"));
    // Three failures must be consecutive: a success in between starts over.
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let flaky = Arc::new(FakeWatch {
        fail_on: Some(vec![0, 2, 3, 5, 6, 7]),
        ..FakeWatch::new(SourceKind::Gmail, vec![])
    });
    fixture.resolver.register_watch(flaky.clone()).await;
    request(&fixture, "req-10b", otp_schema(None)).await;
    wait_status(&fixture, "req-10b", RetrievalStatus::Unavailable).await;
    assert_eq!(
        flaky.polls.load(Ordering::SeqCst),
        8,
        "fail, ok, fail, fail, ok, fail, fail, fail"
    );
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-10b")
        .await
        .unwrap()
        .reason
        .unwrap()
        .contains("could not be read"));
}

#[tokio::test(start_paused = true)]
async fn a_source_that_answers_the_ask_itself_ends_the_watch_and_source_signals_are_honest() {
    let device = AuthorizedSource {
        kind: SourceKind::AndroidNotification,
        account: "pixel".into(),
        label: "Pixel".into(),
    };
    let fixture = make_fixture(vec![device.clone()], AnswerOutcome::Accepted);
    let watch = Arc::new(FakeWatch {
        signal: SourceSignal::AnsweredBySource,
        ..FakeWatch::new(SourceKind::AndroidNotification, vec![])
    });
    fixture.resolver.register_watch(watch.clone()).await;
    request(&fixture, "req-11", otp_schema(None)).await;
    wait_status(&fixture, "req-11", RetrievalStatus::CodeUsed).await;
    assert_eq!(watch.polls.load(Ordering::SeqCst), 1);
    assert!(
        fixture.sink.answers.lock().await.is_empty(),
        "the companion answered over its own authority"
    );
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "code_used"]);
    resolve(&fixture, "req-11").await;
    assert_eq!(
        fixture.transport.statuses(),
        vec!["waiting", "code_used"],
        "the lifecycle's resolution adds nothing"
    );
    // Adding nothing is not the same as taking the outcome away. The
    // companion answered over its own credential, so this resolver never
    // committed, and the resolution's stop used to delete the row the watch
    // had just decided — on a paired handset every phone-answered code read
    // back `none` from `GET /hitl/{id}/retrieval`.
    assert_eq!(
        fixture
            .resolver
            .status("owner", "ws", "req-11")
            .await
            .map(|status| status.status),
        Some(RetrievalStatus::CodeUsed),
        "the outcome must still be readable after the lifecycle resolves",
    );
    // The companion's own ambiguity is the person's decision.
    let fixture = make_fixture(vec![device.clone()], AnswerOutcome::Accepted);
    let watch = Arc::new(FakeWatch {
        signal: SourceSignal::Ambiguous("two codes arrived".into()),
        ..FakeWatch::new(SourceKind::AndroidNotification, vec![])
    });
    fixture.resolver.register_watch(watch.clone()).await;
    request(&fixture, "req-11b", otp_schema(None)).await;
    wait_status(&fixture, "req-11b", RetrievalStatus::Ambiguous).await;
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-11b")
        .await
        .unwrap()
        .reason
        .unwrap()
        .contains("two codes arrived"));
    // A companion without notification access says so, and the status says why.
    let fixture = make_fixture(vec![device.clone()], AnswerOutcome::Accepted);
    let watch = Arc::new(FakeWatch {
        signal: SourceSignal::Unavailable("notification access is not granted".into()),
        ..FakeWatch::new(SourceKind::AndroidNotification, vec![])
    });
    fixture.resolver.register_watch(watch.clone()).await;
    request(&fixture, "req-11c", otp_schema(None)).await;
    wait_status(&fixture, "req-11c", RetrievalStatus::Unavailable).await;
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-11c")
        .await
        .unwrap()
        .reason
        .unwrap()
        .contains("notification access"));
    // A companion that learned the ask was answered already ends the watch quietly.
    let fixture = make_fixture(vec![device], AnswerOutcome::Accepted);
    let watch = Arc::new(FakeWatch {
        signal: SourceSignal::AlreadyResolved,
        ..FakeWatch::new(SourceKind::AndroidNotification, vec![])
    });
    fixture.resolver.register_watch(watch.clone()).await;
    request(&fixture, "req-11d", otp_schema(None)).await;
    wait_status(&fixture, "req-11d", RetrievalStatus::Stopped).await;
    assert_eq!(fixture.transport.statuses(), vec!["waiting", "stopped"]);
}

#[tokio::test(start_paused = true)]
async fn two_live_code_asks_in_one_scope_are_both_the_persons_and_a_duplicate_announcement_is_one()
{
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let watch = watch_with(
        &fixture,
        vec![
            vec![],
            vec![],
            vec![],
            vec![mail("m1", now + 1_000, "Your verification code is 482913")],
        ],
    )
    .await;
    request(&fixture, "req-12", otp_schema(Some(now + 300_000))).await;
    request(&fixture, "req-12", otp_schema(Some(now + 300_000))).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        fixture.transport.statuses(),
        vec!["waiting"],
        "a republished announcement adds nothing"
    );
    // A second code ask while the first is live: the evidence cannot name
    // its challenge, so both are the person's — no source keeps watching.
    request(&fixture, "req-13", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-12", RetrievalStatus::Ambiguous).await;
    assert_eq!(
        fixture
            .resolver
            .status("owner", "ws", "req-13")
            .await
            .unwrap()
            .status,
        RetrievalStatus::Ambiguous
    );
    let polled = watch.polls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        watch.polls.load(Ordering::SeqCst),
        polled,
        "watching stopped for both"
    );
    assert!(fixture.sink.answers.lock().await.is_empty());
    assert_eq!(
        fixture.transport.statuses(),
        vec!["waiting", "ambiguous", "ambiguous"]
    );
    // Another scope is another person's business entirely.
    request_in(
        &fixture,
        "other",
        "ws",
        "req-14",
        otp_schema(Some(now + 300_000)),
    )
    .await;
    assert_eq!(
        fixture
            .resolver
            .status("other", "ws", "req-14")
            .await
            .unwrap()
            .status,
        RetrievalStatus::Waiting
    );
    for id in ["req-12", "req-13"] {
        resolve(&fixture, id).await;
    }
    // Once the first ask resolved, the next code ask in the scope is watched again.
    request(&fixture, "req-15", otp_schema(Some(now + 300_000))).await;
    assert_eq!(
        fixture
            .resolver
            .status("owner", "ws", "req-15")
            .await
            .unwrap()
            .status,
        RetrievalStatus::Waiting
    );
}

#[tokio::test(start_paused = true)]
async fn a_code_that_answered_an_earlier_challenge_never_answers_the_retry() {
    let fixture = make_fixture(vec![gmail_source()], AnswerOutcome::Accepted);
    let now = fixture.clock.now_ms();
    let first = mail("m1", now + 1_000, "Your verification code is 482913");
    // The retry's first poll sees the rejected code again (inside its
    // lookback), a second copy of it under another id, and the fresh one
    // beside them: the fresh one answers, the stale one is not a candidate.
    let again = mail("m1", now + 1_000, "Your verification code is 482913");
    let copy = mail("m1-copy", now + 1_100, "Your verification code is 482913");
    let fresh = mail("m2", now + 45_000, "Your verification code is 573920");
    let watch = watch_with(&fixture, vec![vec![first], vec![again, copy, fresh]]).await;
    request(&fixture, "req-16", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-16", RetrievalStatus::CodeUsed).await;
    resolve(&fixture, "req-16").await;
    // The service rejected it; the adapter raises a fresh challenge 40 s later.
    fixture.clock.0.fetch_add(40_000, Ordering::SeqCst);
    request(&fixture, "req-17", otp_schema(Some(now + 340_000))).await;
    wait_status(&fixture, "req-17", RetrievalStatus::CodeUsed).await;
    assert_eq!(
        *fixture.sink.answers.lock().await,
        vec![
            ("req-16".to_string(), "482913".to_string()),
            ("req-17".to_string(), "573920".to_string())
        ],
        "neither the same message nor the same code answers twice"
    );
    assert_eq!(watch.polls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn two_sources_offering_different_codes_make_the_challenge_ambiguous_and_the_same_code_answers_once(
) {
    let fixture = make_fixture(
        vec![gmail_source(), messages_source()],
        AnswerOutcome::Accepted,
    );
    let now = fixture.clock.now_ms();
    let gmail = watch_with(
        &fixture,
        vec![vec![mail(
            "m1",
            now + 1_000,
            "Your verification code is 482913",
        )]],
    )
    .await;
    let texts = Arc::new(FakeWatch::new(
        SourceKind::Messages,
        vec![vec![sms("s1", now + 1_200, "Your login code is 573920")]],
    ));
    fixture.resolver.register_watch(texts.clone()).await;
    request(&fixture, "req-18", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-18", RetrievalStatus::Ambiguous).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        fixture.sink.answers.lock().await.is_empty(),
        "a first match waits one round for the other sources"
    );
    assert!(fixture
        .resolver
        .status("owner", "ws", "req-18")
        .await
        .unwrap()
        .reason
        .unwrap()
        .contains("different codes"));
    assert_eq!(gmail.polls.load(Ordering::SeqCst), 1);
    assert_eq!(texts.polls.load(Ordering::SeqCst), 1);
    // The same code from two sources is one answer.
    let fixture = make_fixture(
        vec![gmail_source(), messages_source()],
        AnswerOutcome::Accepted,
    );
    let now = fixture.clock.now_ms();
    watch_with(
        &fixture,
        vec![vec![mail(
            "m1",
            now + 1_000,
            "Your verification code is 482913",
        )]],
    )
    .await;
    let texts = Arc::new(FakeWatch::new(
        SourceKind::Messages,
        vec![vec![sms("s1", now + 1_200, "Your login code is 482913")]],
    ));
    fixture.resolver.register_watch(texts).await;
    request(&fixture, "req-19", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-19", RetrievalStatus::CodeUsed).await;
    assert_eq!(
        *fixture.sink.answers.lock().await,
        vec![("req-19".to_string(), "482913".to_string())]
    );
    // One source's failure does not end the watch while another still reads.
    let fixture = make_fixture(
        vec![gmail_source(), messages_source()],
        AnswerOutcome::Accepted,
    );
    let now = fixture.clock.now_ms();
    let failing = Arc::new(FakeWatch {
        fail_on: None,
        ..FakeWatch::new(SourceKind::Gmail, vec![])
    });
    fixture.resolver.register_watch(failing).await;
    let texts = Arc::new(FakeWatch::new(
        SourceKind::Messages,
        vec![
            vec![],
            vec![],
            vec![],
            vec![],
            vec![sms("s2", now + 2_000, "Your login code is 664422")],
        ],
    ));
    fixture.resolver.register_watch(texts).await;
    request(&fixture, "req-20", otp_schema(Some(now + 300_000))).await;
    wait_status(&fixture, "req-20", RetrievalStatus::CodeUsed).await;
    assert_eq!(
        fixture.transport.statuses(),
        vec!["waiting", "code_used"],
        "no unavailable status while a source still watches"
    );
}

#[test]
fn a_challenge_is_read_from_the_published_spec_only() {
    let schema = json!({
        "input_type": "otp", "prompt": "Enter the 6-digit code",
        "sensitive": {"kind": "otp", "one_time": true, "expected_destination": "https://Accounts.Example.test:8443", "collection_deadline_ms": 5_000, "expected_digits": 6}
    });
    let challenge = challenge_from_schema("owner", "ws", "req", Some(&schema), 1_000, 90).unwrap();
    assert_eq!(
        challenge.expected_host.as_deref(),
        Some("accounts.example.test")
    );
    assert_eq!(challenge.deadline_ms, Some(5_000));
    assert_eq!(challenge.expected.digits, Some(6));
    assert_eq!(challenge.window_start_ms(), 1_000 - 90_000);
    let bare = json!({"input_type": "otp", "prompt": "Enter the code", "expected_length": 6, "sensitive": {"kind": "otp"}});
    assert_eq!(
        challenge_from_schema("owner", "ws", "req", Some(&bare), 1_000, 90)
            .unwrap()
            .expected
            .digits,
        None,
        "only the spec names a length"
    );
    // A form is the person's whole form: a bare code could never answer it.
    let form = json!({"sensitive": {"kind": "otp", "fields": [{"id": "u", "kind": "login_identifier"}, {"id": "c", "kind": "otp"}]}});
    assert!(
        challenge_from_schema("owner", "ws", "req", Some(&form), 1_000, 90).is_none(),
        "a form with a code field is not a challenge"
    );
    let password_form = json!({"sensitive": {"kind": "password", "fields": [{"id": "u", "kind": "login_identifier"}, {"id": "c", "kind": "otp"}]}});
    assert!(challenge_from_schema("owner", "ws", "req", Some(&password_form), 1_000, 90).is_none());
    assert!(challenge_from_schema(
        "owner",
        "ws",
        "req",
        Some(&json!({"sensitive": {"kind": "password"}})),
        1_000,
        90
    )
    .is_none());
    assert!(
        challenge_from_schema(
            "owner",
            "ws",
            "req",
            Some(&json!({"input_type": "otp"})),
            1_000,
            90
        )
        .is_none(),
        "no spec, no challenge — wording is not authority"
    );
}
