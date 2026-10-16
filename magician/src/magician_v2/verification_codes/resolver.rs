//! The resolver (plan §6.2): one subscription to the HITL lifecycle, one
//! bounded watcher per (challenge, authorised source), one door for the
//! answer, value-free status for the surfaces.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use zeroize::Zeroizing;

use super::{
    extract::ExpectedFormat,
    matching::{decide, host_of, ChallengeContext, MessageIdentity, SourceKind, Verdict},
    sources::{
        AnswerOutcome, AnswerTarget, AuthorizedSource, ChallengeAnswerSink, SourceRegistry,
        SourceSignal, SourceWatch,
    },
};
use crate::config::HitlVerificationCodesSettings;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

/// Cadence of a source poll while the challenge is open.
pub const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Provider calls one watcher may make for one challenge.
pub const MAX_POLLS: u32 = 40;
/// A watcher never outlives this, whatever the ask's own timeout.
pub const MAX_WATCH: Duration = Duration::from_secs(300);
/// Messages fetched per poll.
pub const POLL_LIMIT: usize = 10;
/// Consecutive provider failures that end one watcher.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// How long a message or a code that answered one challenge stays
/// unavailable to every later challenge: longer than any window plus any
/// watch, so a retry inside the lookback never re-reads the rejected code.
pub const CONSUMED_TTL_MS: i64 = 15 * 60 * 1000;
/// Consumed identities remembered at most (in memory, per process).
const MAX_CONSUMED: usize = 2048;
/// A challenge's status stays readable this long after its last change
/// when no `hitl.resolved` ever arrived for it (a lagged feed, an ask that
/// expired without a scoped resolution).
const STATUS_RETENTION_MS: i64 = 30 * 60 * 1000;
/// Digit counts a spec may narrow extraction to.
const EXPECTED_DIGITS_RANGE: std::ops::RangeInclusive<usize> = 4..=8;

/// Wall clock, injectable for tests.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Where status events go: the scoped realtime feed.
pub trait StatusTransport: Send + Sync {
    fn emit(&self, event: RuntimeTransportEvent);
}

impl StatusTransport for RuntimeTransportBroadcaster {
    fn emit(&self, event: RuntimeTransportEvent) {
        RuntimeTransportBroadcaster::emit(self, event);
    }
}

/// The safe status a challenge's retrieval is in. Never the code, never the
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStatus {
    /// Sources are being watched.
    Waiting,
    /// A code was found and answered the ask.
    CodeUsed,
    /// Eligible material could not be decided: the person enters the code.
    Ambiguous,
    /// No authorised source, or every source failed: the person enters the code.
    Unavailable,
    /// The ask resolved (by anyone) or its window closed; watching stopped.
    Stopped,
}

impl RetrievalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::CodeUsed => "code_used",
            Self::Ambiguous => "ambiguous",
            Self::Unavailable => "unavailable",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChallengeStatus {
    pub correlation_id: String,
    pub status: RetrievalStatus,
    /// Source kinds being (or having been) watched, for the UI's wording.
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at_ms: i64,
}

struct ActiveChallenge {
    cancel: CancellationToken,
    status: ChallengeStatus,
    principal: String,
    workspace: String,
    /// The host the challenge bound the ask to, when it bound one. The
    /// lookback clamp is per SERVICE: one site's resolution must not truncate
    /// another site's window, and a code from one host could never answer the
    /// other's challenge anyway (`judge` checks the sender against the bound
    /// destination).
    expected_host: Option<String>,
    /// Digests of the distinct codes the sources offered this challenge;
    /// a second distinct one makes the challenge ambiguous.
    candidates: HashSet<[u8; 32]>,
    /// A first match claimed the answer and is settling: a second source
    /// with the same code adds nothing, one with another code makes the
    /// challenge ambiguous before the answer goes out.
    claimed: bool,
    /// The answer is on its way through the sink: nothing changes it now.
    committed: bool,
    /// Sources watching this challenge.
    watchers: usize,
}

/// Material that answered — or was judged the evidence of — one challenge
/// is never offered to another: message identities, and the codes
/// themselves as salted digests (never the digits), each for
/// [`CONSUMED_TTL_MS`].
struct Consumed {
    salt: [u8; 32],
    messages: HashMap<MessageIdentity, i64>,
    codes: HashMap<[u8; 32], i64>,
}

impl Consumed {
    fn new() -> Self {
        Self {
            salt: rand::random(),
            messages: HashMap::new(),
            codes: HashMap::new(),
        }
    }

    fn digest(&self, principal: &str, workspace: &str, code: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.salt);
        hasher.update(principal.as_bytes());
        hasher.update([0]);
        hasher.update(workspace.as_bytes());
        hasher.update([0]);
        hasher.update(code.as_bytes());
        hasher.finalize().into()
    }

    fn sweep(&mut self, now: i64) {
        self.messages.retain(|_, at| now - *at < CONSUMED_TTL_MS);
        self.codes.retain(|_, at| now - *at < CONSUMED_TTL_MS);
        if self.messages.len() > MAX_CONSUMED {
            self.messages.clear();
        }
        if self.codes.len() > MAX_CONSUMED {
            self.codes.clear();
        }
    }
}

/// How one watcher ended.
enum WatchEnd {
    /// The challenge was decided (answered, ambiguous, stopped): nothing
    /// for the join to add.
    Decided,
    /// This source is done without a decision; the reason, when there is
    /// one beyond "nothing arrived".
    Ended(Option<String>),
}

pub struct VerificationCodeResolver {
    settings: RwLock<HitlVerificationCodesSettings>,
    registry: Arc<dyn SourceRegistry>,
    watches: RwLock<HashMap<SourceKind, Arc<dyn SourceWatch>>>,
    sink: Arc<dyn ChallengeAnswerSink>,
    transport: Arc<dyn StatusTransport>,
    clock: Arc<dyn Clock>,
    active: Mutex<HashMap<String, ActiveChallenge>>,
    consumed: Mutex<Consumed>,
    /// When each scope's last code challenge was decided, by anyone. The next
    /// challenge's lookback may not reach behind it: the resolver only knows
    /// the codes IT matched (`consumed`), so a code the owner typed or the
    /// phone deposited would otherwise stay eligible for the next challenge.
    resolutions: Mutex<HashMap<String, i64>>,
    /// Between polls of one source — and how long a first match waits for
    /// the other sources' same round before it answers.
    poll_interval: Duration,
}

fn key(principal: &str, workspace: &str, correlation_id: &str) -> String {
    format!("{principal}|{workspace}|{correlation_id}")
}

fn scope_key(principal: &str, workspace: &str, expected_host: Option<&str>) -> String {
    format!(
        "{principal}|{workspace}|{}",
        expected_host.unwrap_or_default()
    )
}

impl VerificationCodeResolver {
    pub fn new(
        settings: HitlVerificationCodesSettings,
        registry: Arc<dyn SourceRegistry>,
        sink: Arc<dyn ChallengeAnswerSink>,
        transport: Arc<dyn StatusTransport>,
    ) -> Self {
        Self::with_clock(
            settings,
            registry,
            sink,
            transport,
            Arc::new(SystemClock),
            POLL_INTERVAL,
        )
    }

    pub fn with_clock(
        settings: HitlVerificationCodesSettings,
        registry: Arc<dyn SourceRegistry>,
        sink: Arc<dyn ChallengeAnswerSink>,
        transport: Arc<dyn StatusTransport>,
        clock: Arc<dyn Clock>,
        poll_interval: Duration,
    ) -> Self {
        Self {
            settings: RwLock::new(settings),
            registry,
            watches: RwLock::new(HashMap::new()),
            sink,
            transport,
            clock,
            active: Mutex::new(HashMap::new()),
            consumed: Mutex::new(Consumed::new()),
            resolutions: Mutex::new(HashMap::new()),
            poll_interval,
        }
    }

    /// Register the watch for one source kind (the composition root wires
    /// the Gmail, AgentMail, Messages and Android watches).
    pub async fn register_watch(&self, watch: Arc<dyn SourceWatch>) {
        self.watches.write().await.insert(watch.kind(), watch);
    }

    pub async fn reload(&self, settings: HitlVerificationCodesSettings) {
        *self.settings.write().await = settings;
    }

    pub async fn settings(&self) -> HitlVerificationCodesSettings {
        self.settings.read().await.clone()
    }

    /// Whether retrieval would run for a challenge in this scope right now:
    /// enabled, and at least one authorised source has a registered watch.
    /// The delivery coordinator uses this for its short grace before alerting.
    pub async fn retrieval_expected(&self, principal: &str, workspace: &str) -> bool {
        if !self.settings.read().await.enabled {
            return false;
        }
        let watches = self.watches.read().await;
        self.registry
            .authorized_sources(principal, workspace)
            .await
            .iter()
            .any(|source| watches.contains_key(&source.kind))
    }

    /// The value-free status of one challenge's retrieval, if the resolver
    /// knows it.
    /// The value-free status of one challenge, while it is still readable.
    ///
    /// A decided challenge stays readable for [`STATUS_RETENTION_MS`] so a
    /// surface can explain an answer that arrived without the person — and no
    /// longer: the retention is applied here as well as at the next
    /// `on_challenge`, so a scope that raises no further challenge does not
    /// keep an answered row readable forever.
    pub async fn status(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
    ) -> Option<ChallengeStatus> {
        let now = self.clock.now_ms();
        self.active
            .lock()
            .await
            .get(&key(principal, workspace, correlation_id))
            .filter(|active| now.saturating_sub(active.status.updated_at_ms) < STATUS_RETENTION_MS)
            .map(|active| active.status.clone())
    }

    pub fn start(self: Arc<Self>, broadcaster: Arc<RuntimeTransportBroadcaster>) {
        let mut events = broadcaster.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "[VERIFICATION-CODES] event subscriber lagged");
                        continue;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let resolver = Arc::clone(&self);
                tokio::spawn(async move { resolver.handle(event).await });
            }
        });
    }

    pub async fn handle(self: &Arc<Self>, event: RuntimeTransportEvent) {
        match event {
            RuntimeTransportEvent::HitlRequested {
                correlation_id,
                source,
                input_schema,
                execution_id,
                principal: Some(principal),
                workspace: Some(workspace),
                timestamp,
                ..
            } => {
                let Some(challenge) = challenge_from_schema(
                    &principal,
                    &workspace,
                    &correlation_id,
                    input_schema.as_ref(),
                    timestamp,
                    self.settings.read().await.lookback_secs,
                ) else {
                    return;
                };
                let target = AnswerTarget {
                    principal,
                    workspace,
                    correlation_id,
                    source,
                    execution_id,
                };
                self.on_challenge(challenge, target).await;
            },
            RuntimeTransportEvent::HitlResolved {
                correlation_id,
                principal: Some(principal),
                workspace: Some(workspace),
                ..
            } => {
                self.stop(
                    &principal,
                    &workspace,
                    &correlation_id,
                    RetrievalStatus::Stopped,
                    None,
                )
                .await;
            },
            _ => {},
        }
    }

    async fn on_challenge(self: &Arc<Self>, mut challenge: ChallengeContext, target: AnswerTarget) {
        if !self.settings.read().await.enabled {
            return;
        }
        challenge.lane = target.source.clone();
        challenge.not_before_ms = self
            .resolutions
            .lock()
            .await
            .get(&scope_key(
                &challenge.principal,
                &challenge.workspace,
                challenge.expected_host.as_deref(),
            ))
            .copied();
        let challenge = challenge;
        let key = key(
            &challenge.principal,
            &challenge.workspace,
            &challenge.correlation_id,
        );
        let watches = self.watches.read().await.clone();
        let sources: Vec<AuthorizedSource> = self
            .registry
            .authorized_sources(&challenge.principal, &challenge.workspace)
            .await
            .into_iter()
            .filter(|source| watches.contains_key(&source.kind))
            .collect();
        let now = self.clock.now_ms();
        let cancel = CancellationToken::new();
        let source_names: Vec<String> = sources
            .iter()
            .map(|s| s.kind.as_str().to_string())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        // Registered under one lock: a duplicate announcement finds the
        // entry and adds nothing; a second live code ask in the same scope
        // makes both ambiguous — the evidence a source yields names no
        // challenge, and choosing is how the wrong code gets typed.
        let (status, concurrent) = {
            let mut active = self.active.lock().await;
            active.retain(|_, entry| now - entry.status.updated_at_ms < STATUS_RETENTION_MS);
            if active.contains_key(&key) {
                return;
            }
            let concurrent: Vec<(String, String, ChallengeStatus)> = active
                .iter_mut()
                .filter(|(_, entry)| {
                    entry.principal == challenge.principal
                        && entry.workspace == challenge.workspace
                        && entry.status.status == RetrievalStatus::Waiting
                        && !entry.committed
                })
                .map(|(_, entry)| {
                    entry.cancel.cancel();
                    entry.status.status = RetrievalStatus::Ambiguous;
                    entry.status.reason = Some("another code request opened in the meantime; the person decides which code is which".to_string());
                    entry.status.updated_at_ms = now;
                    (entry.principal.clone(), entry.workspace.clone(), entry.status.clone())
                })
                .collect();
            let (status, reason) = if !concurrent.is_empty() {
                (RetrievalStatus::Ambiguous, Some("another code request is already open; the person decides which code is which".to_string()))
            } else if sources.is_empty() {
                (
                    RetrievalStatus::Unavailable,
                    Some("no source is permitted for verification codes".to_string()),
                )
            } else {
                (RetrievalStatus::Waiting, None)
            };
            let status = ChallengeStatus {
                correlation_id: challenge.correlation_id.clone(),
                status,
                sources: source_names,
                reason,
                updated_at_ms: now,
            };
            active.insert(
                key.clone(),
                ActiveChallenge {
                    cancel: cancel.clone(),
                    status: status.clone(),
                    principal: challenge.principal.clone(),
                    workspace: challenge.workspace.clone(),
                    expected_host: challenge.expected_host.clone(),
                    candidates: HashSet::new(),
                    claimed: false,
                    committed: false,
                    // Counted HERE, under the same lock that publishes the
                    // challenge — never after the watchers are spawned. A
                    // watcher that reached `watcher_count()` first read `0`,
                    // skipped the one-round settle that exists to catch two
                    // sources disagreeing, and submitted its code unchallenged.
                    watchers: sources.len(),
                },
            );
            (status, concurrent)
        };
        for (principal, workspace, status) in concurrent {
            self.publish(&principal, &workspace, &status);
        }
        self.publish(&challenge.principal, &challenge.workspace, &status);
        if status.status != RetrievalStatus::Waiting {
            // Nothing to watch; the ask stays a person's. Keep the status
            // readable until the ask resolves.
            return;
        }
        let challenge = Arc::new(challenge);
        let target = Arc::new(target);
        // One watcher per authorised source, and one live challenge per
        // scope: the provider calls a challenge may cost are bounded by
        // construction (`MAX_POLLS` × the sources the owner permitted).
        let mut tasks = Vec::new();
        for source in sources {
            let Some(watch) = watches.get(&source.kind).cloned() else {
                continue;
            };
            let resolver = Arc::clone(self);
            let (challenge, target, cancel) =
                (Arc::clone(&challenge), Arc::clone(&target), cancel.clone());
            tasks.push(tokio::spawn(async move {
                resolver
                    .run_watch(watch, source, challenge, target, cancel)
                    .await
            }));
        }
        let resolver = Arc::clone(self);
        tokio::spawn(async move {
            let mut reasons = Vec::new();
            for task in tasks {
                if let Ok(WatchEnd::Ended(Some(reason))) = task.await {
                    reasons.push(reason);
                }
            }
            // Every watcher ended without answering: the person's ask stays
            // open; say so once, with what the sources reported.
            let now = resolver.clock.now_ms();
            let unanswered = {
                let mut active = resolver.active.lock().await;
                match active.get_mut(&key) {
                    Some(entry) if entry.status.status == RetrievalStatus::Waiting => {
                        entry.status.status = RetrievalStatus::Unavailable;
                        entry.status.reason = Some(if reasons.is_empty() {
                            "no matching code arrived while the sources were watched".to_string()
                        } else {
                            reasons.join("; ")
                        });
                        entry.status.updated_at_ms = now;
                        Some((
                            entry.principal.clone(),
                            entry.workspace.clone(),
                            entry.status.clone(),
                        ))
                    },
                    _ => None,
                }
            };
            if let Some((principal, workspace, status)) = unanswered {
                resolver.publish(&principal, &workspace, &status);
            }
        });
    }

    async fn run_watch(
        self: &Arc<Self>,
        watch: Arc<dyn SourceWatch>,
        source: AuthorizedSource,
        challenge: Arc<ChallengeContext>,
        target: Arc<AnswerTarget>,
        cancel: CancellationToken,
    ) -> WatchEnd {
        let (source, challenge, target, cancel) = (&source, &challenge, &target, &cancel);
        let started = tokio::time::Instant::now();
        let mut seen = HashSet::new();
        let mut polls = 0u32;
        let mut consecutive_failures = 0u32;
        let since_ms = challenge.window_start_ms();
        let kind = source.kind.as_str();
        loop {
            if cancel.is_cancelled() {
                return WatchEnd::Decided;
            }
            if started.elapsed() >= MAX_WATCH || polls >= MAX_POLLS {
                return WatchEnd::Ended(None);
            }
            if challenge
                .deadline_ms
                .is_some_and(|deadline| self.clock.now_ms() > deadline)
            {
                return WatchEnd::Ended(None);
            }
            // Authority is re-read before every fetch: a source the owner
            // disabled meanwhile is not read again.
            if !self.still_authorized(challenge, source).await {
                return WatchEnd::Ended(Some(format!("{kind}: the permission was withdrawn")));
            }
            polls += 1;
            let poll = tokio::select! {
                _ = cancel.cancelled() => return WatchEnd::Decided,
                poll = watch.poll(source, challenge, since_ms, POLL_LIMIT) => poll,
            };
            let poll = match poll {
                Ok(poll) => {
                    consecutive_failures = 0;
                    poll
                },
                Err(error) => {
                    consecutive_failures += 1;
                    warn!(kind, %error, consecutive_failures, "[VERIFICATION-CODES] source poll failed");
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        return WatchEnd::Ended(Some(format!("{kind} could not be read")));
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => return WatchEnd::Decided,
                        _ = tokio::time::sleep(self.poll_interval) => continue,
                    }
                },
            };
            match poll.signal {
                SourceSignal::None => {},
                SourceSignal::AnsweredBySource => {
                    // The companion answered the ask over its own authority:
                    // the code was received and used; the lifecycle's
                    // resolution follows. Nothing here saw the code.
                    info!(correlation_id = %challenge.correlation_id, kind, "[VERIFICATION-CODES] the source answered the ask itself");
                    self.finish(challenge, RetrievalStatus::CodeUsed, None)
                        .await;
                    return WatchEnd::Decided;
                },
                SourceSignal::AlreadyResolved => {
                    self.stop(
                        &challenge.principal,
                        &challenge.workspace,
                        &challenge.correlation_id,
                        RetrievalStatus::Stopped,
                        None,
                    )
                    .await;
                    return WatchEnd::Decided;
                },
                SourceSignal::Ambiguous(reason) => {
                    self.decide_ambiguous(challenge, format!("{kind}: {reason}"))
                        .await;
                    return WatchEnd::Decided;
                },
                SourceSignal::Unavailable(reason) => {
                    return WatchEnd::Ended(Some(format!("{kind}: {reason}")));
                },
            }
            // Material another challenge already took is not this one's:
            // neither the message nor the code.
            let decision = {
                let consumed = self.consumed.lock().await;
                let messages = poll
                    .messages
                    .into_iter()
                    .filter(|message| !consumed.messages.contains_key(&message.identity()))
                    .collect::<Vec<_>>();
                let is_stale = |code: &str| {
                    consumed.codes.contains_key(&consumed.digest(
                        &challenge.principal,
                        &challenge.workspace,
                        code,
                    ))
                };
                decide(challenge, &messages, &mut seen, &is_stale)
            };
            match decision.verdict {
                Verdict::Match { code } => {
                    // And once more before use: enablement and ownership are
                    // checked before reading and before the answer.
                    if !self.still_authorized(challenge, source).await {
                        return WatchEnd::Ended(Some(format!(
                            "{kind}: the permission was withdrawn"
                        )));
                    }
                    match self.claim_answer(challenge, &code, &decision.matched).await {
                        Claim::Answer => {},
                        Claim::Stale => {
                            // A code that already answered an earlier challenge
                            // (a retry after the service rejected it) is never
                            // offered again; keep watching for the fresh one.
                            info!(correlation_id = %challenge.correlation_id, kind, "[VERIFICATION-CODES] a code from an earlier challenge was skipped");
                            if poll.exhausted {
                                return WatchEnd::Ended(None);
                            }
                            tokio::select! {
                                _ = cancel.cancelled() => return WatchEnd::Decided,
                                _ = tokio::time::sleep(self.poll_interval) => continue,
                            }
                        },
                        Claim::Decided => return WatchEnd::Decided,
                    }
                    // The other sources' same round may still be in flight:
                    // give them one interval to disagree before the answer
                    // goes out. A different code meanwhile cancels this. A
                    // lone watcher has nobody to wait for.
                    if self.watcher_count(challenge).await > 1 {
                        tokio::select! {
                            _ = cancel.cancelled() => return WatchEnd::Decided,
                            _ = tokio::time::sleep(self.poll_interval) => {},
                        }
                    }
                    if !self.commit_answer(challenge).await {
                        return WatchEnd::Decided;
                    }
                    let outcome = self.sink.answer(target, Zeroizing::new(code)).await;
                    match outcome {
                        AnswerOutcome::Accepted => {
                            info!(correlation_id = %challenge.correlation_id, kind, "[VERIFICATION-CODES] a retrieved code answered the ask");
                            self.finish(challenge, RetrievalStatus::CodeUsed, None)
                                .await;
                        },
                        AnswerOutcome::AlreadyResolved => {
                            // Somebody answered first. This challenge is
                            // decided either way, so the row is finished (and
                            // stays readable) rather than dropped — a `stop`
                            // would be refused on a committed entry.
                            self.finish(challenge, RetrievalStatus::Stopped, None).await;
                        },
                        AnswerOutcome::Refused(reason) => {
                            warn!(correlation_id = %challenge.correlation_id, %reason, "[VERIFICATION-CODES] the ask refused the retrieved answer");
                            self.finish(
                                challenge,
                                RetrievalStatus::Unavailable,
                                Some("the request could not take the code".to_string()),
                            )
                            .await;
                        },
                    }
                    return WatchEnd::Decided;
                },
                Verdict::Ambiguous { reason } => {
                    // The person decides; every source stops.
                    self.decide_ambiguous(challenge, format!("{kind}: {reason}"))
                        .await;
                    return WatchEnd::Decided;
                },
                Verdict::NoMatch { .. } => {},
            }
            if poll.exhausted {
                return WatchEnd::Ended(None);
            }
            tokio::select! {
                _ = cancel.cancelled() => return WatchEnd::Decided,
                _ = tokio::time::sleep(self.poll_interval) => {},
            }
        }
    }

    async fn watcher_count(&self, challenge: &ChallengeContext) -> usize {
        let key = key(
            &challenge.principal,
            &challenge.workspace,
            &challenge.correlation_id,
        );
        self.active
            .lock()
            .await
            .get(&key)
            .map_or(0, |entry| entry.watchers)
    }

    /// After the settle: the answer goes out unless the challenge was
    /// decided meanwhile. From here nothing makes it ambiguous.
    async fn commit_answer(&self, challenge: &ChallengeContext) -> bool {
        let key = key(
            &challenge.principal,
            &challenge.workspace,
            &challenge.correlation_id,
        );
        let mut active = self.active.lock().await;
        match active.get_mut(&key) {
            Some(entry) if entry.status.status == RetrievalStatus::Waiting => {
                entry.committed = true;
                true
            },
            _ => false,
        }
    }

    /// One answer per challenge, across sources: the first distinct code a
    /// source offers claims the answer and consumes its evidence; the same
    /// code from another source adds nothing; a different code makes the
    /// challenge ambiguous instead — before or during the claimant's settle;
    /// a code that answered an earlier challenge is stale. All under one
    /// lock, so two sources arriving together cannot both answer.
    async fn claim_answer(
        &self,
        challenge: &ChallengeContext,
        code: &str,
        matched: &[MessageIdentity],
    ) -> Claim {
        let key = key(
            &challenge.principal,
            &challenge.workspace,
            &challenge.correlation_id,
        );
        let now = self.clock.now_ms();
        let mut consumed = self.consumed.lock().await;
        consumed.sweep(now);
        let digest = consumed.digest(&challenge.principal, &challenge.workspace, code);
        if consumed.codes.contains_key(&digest) {
            for identity in matched {
                consumed.messages.insert(identity.clone(), now);
            }
            return Claim::Stale;
        }
        let (claim, ambiguous) = {
            let mut active = self.active.lock().await;
            match active.get_mut(&key) {
                Some(entry)
                    if entry.status.status == RetrievalStatus::Waiting && !entry.committed =>
                {
                    entry.candidates.insert(digest);
                    if entry.candidates.len() > 1 {
                        entry.status.status = RetrievalStatus::Ambiguous;
                        entry.status.reason =
                            Some("the sources offered different codes".to_string());
                        entry.status.updated_at_ms = now;
                        entry.cancel.cancel();
                        (
                            Claim::Decided,
                            Some((
                                entry.principal.clone(),
                                entry.workspace.clone(),
                                entry.status.clone(),
                            )),
                        )
                    } else if entry.claimed {
                        // The same code, from another source: the first
                        // claimant answers.
                        for identity in matched {
                            consumed.messages.insert(identity.clone(), now);
                        }
                        (Claim::Decided, None)
                    } else {
                        entry.claimed = true;
                        consumed.codes.insert(digest, now);
                        for identity in matched {
                            consumed.messages.insert(identity.clone(), now);
                        }
                        (Claim::Answer, None)
                    }
                },
                _ => (Claim::Decided, None),
            }
        };
        drop(consumed);
        if let Some((principal, workspace, status)) = ambiguous {
            self.publish(&principal, &workspace, &status);
        }
        claim
    }

    async fn decide_ambiguous(&self, challenge: &ChallengeContext, reason: String) {
        self.finish(challenge, RetrievalStatus::Ambiguous, Some(reason))
            .await;
    }

    /// The challenge is decided: every watcher ends, the status stays
    /// readable until the ask's own resolution retires it. A decided state
    /// is never overwritten, and an answer in flight is never made
    /// ambiguous behind its back.
    async fn finish(
        &self,
        challenge: &ChallengeContext,
        status: RetrievalStatus,
        reason: Option<String>,
    ) {
        let key = key(
            &challenge.principal,
            &challenge.workspace,
            &challenge.correlation_id,
        );
        let now = self.clock.now_ms();
        let snapshot = {
            let mut active = self.active.lock().await;
            match active.get_mut(&key) {
                Some(entry)
                    if entry.status.status == RetrievalStatus::Waiting
                        && (status != RetrievalStatus::Ambiguous || !entry.committed) =>
                {
                    entry.status.status = status;
                    entry.status.reason = reason;
                    entry.status.updated_at_ms = now;
                    entry.cancel.cancel();
                    Some(entry.status.clone())
                },
                _ => None,
            }
        };
        if let Some(snapshot) = snapshot {
            self.note_resolution(
                &challenge.principal,
                &challenge.workspace,
                challenge.expected_host.as_deref(),
                now,
            )
            .await;
            self.publish(&challenge.principal, &challenge.workspace, &snapshot);
        }
    }

    /// Remember that this scope's challenge was decided at `at_ms`, so the
    /// next one's lookback starts after it. Monotonic: a late ending never
    /// moves the mark backwards.
    async fn note_resolution(
        &self,
        principal: &str,
        workspace: &str,
        expected_host: Option<&str>,
        at_ms: i64,
    ) {
        let mut resolutions = self.resolutions.lock().await;
        let mark = resolutions
            .entry(scope_key(principal, workspace, expected_host))
            .or_insert(at_ms);
        *mark = (*mark).max(at_ms);
    }

    async fn still_authorized(
        &self,
        challenge: &ChallengeContext,
        source: &AuthorizedSource,
    ) -> bool {
        self.settings.read().await.enabled
            && self
                .registry
                .authorized_sources(&challenge.principal, &challenge.workspace)
                .await
                .iter()
                .any(|current| current.kind == source.kind && current.account == source.account)
    }

    /// End every watcher of the challenge with a final status.
    /// Retire a challenge on somebody else's account: the ask resolved, or
    /// its window closed.
    ///
    /// A challenge this resolver **committed** is the exception, and the
    /// automatic case is exactly that: the resolution now arriving was
    /// produced by our own answer, and the committing watch is a few
    /// milliseconds behind it with the outcome (`code_used`) the surfaces
    /// explain the answer with. Removing the row here would delete that
    /// explanation before it was ever written — which is what the
    /// qualification lane found: `GET /hitl/{id}/retrieval` answered `none`
    /// for every code the runtime retrieved itself. So a committed entry is
    /// left to its watch, and the retention sweep in
    /// [`Self::on_challenge`] retires it. Retention is safe to hold now that
    /// each ask of a step has its own correlation id
    /// (`AgenticPauseState::ask_id`): a retained row can no longer stand in
    /// the way of the step's next challenge.
    ///
    /// A watch that already decided is the same exception reached by the
    /// other door. When the **companion** answers the ask over its own
    /// credential, this resolver never commits — `committed` stays false —
    /// so the committed test alone let the resolution event delete a row
    /// whose watch had just written `code_used`, and every phone-answered
    /// code read back as `none`. A decided outcome is therefore kept too:
    /// the stop that follows it "adds nothing the surfaces need" (see the
    /// publish rule below), and deleting the row is not nothing.
    async fn stop(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        status: RetrievalStatus,
        reason: Option<String>,
    ) {
        let key = key(principal, workspace, correlation_id);
        let removed = {
            let mut active = self.active.lock().await;
            if status == RetrievalStatus::Stopped
                && active.get(&key).is_some_and(|entry| {
                    entry.committed
                        || matches!(
                            entry.status.status,
                            RetrievalStatus::CodeUsed
                                | RetrievalStatus::Ambiguous
                                | RetrievalStatus::Unavailable
                        )
                })
            {
                return;
            }
            active.remove(&key)
        };
        let Some(mut entry) = removed else { return };
        entry.cancel.cancel();
        let publish = match (entry.status.status, status) {
            // A stop after the code was used, or after the person decided,
            // adds nothing the surfaces need.
            (
                RetrievalStatus::CodeUsed
                | RetrievalStatus::Ambiguous
                | RetrievalStatus::Unavailable,
                RetrievalStatus::Stopped,
            ) => false,
            _ => true,
        };
        entry.status.status = status;
        entry.status.reason = reason;
        entry.status.updated_at_ms = self.clock.now_ms();
        // Whoever answered — the owner from the Attention sheet, the phone over
        // its own credential, or the window simply closing — this challenge is
        // decided, and the next one for the same service starts its lookback
        // here.
        self.note_resolution(
            principal,
            workspace,
            entry.expected_host.as_deref(),
            entry.status.updated_at_ms,
        )
        .await;
        if publish {
            self.publish(principal, workspace, &entry.status);
        }
    }

    fn publish(&self, principal: &str, workspace: &str, status: &ChallengeStatus) {
        self.transport
            .emit(RuntimeTransportEvent::VerificationRetrievalStatus {
                correlation_id: status.correlation_id.clone(),
                status: status.status.as_str().to_string(),
                sources: status.sources.clone(),
                reason: status.reason.clone(),
                principal: Some(principal.to_string()),
                workspace: Some(workspace.to_string()),
                timestamp: status.updated_at_ms,
            });
    }
}

enum Claim {
    /// This source answers.
    Answer,
    /// The code answered an earlier challenge; not this one's.
    Stale,
    /// The challenge is decided (answered, ambiguous, stopped).
    Decided,
}

#[async_trait::async_trait]
impl crate::magician_v2::hitl_delivery::RetrievalOracle for VerificationCodeResolver {
    async fn retrieval_expected(&self, principal: &str, workspace: &str) -> bool {
        VerificationCodeResolver::retrieval_expected(self, principal, workspace).await
    }

    async fn retrieval_expected_for(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
    ) -> bool {
        match self.status(principal, workspace, correlation_id).await {
            Some(status) => status.status == RetrievalStatus::Waiting,
            None => VerificationCodeResolver::retrieval_expected(self, principal, workspace).await,
        }
    }
}

/// The challenge a `hitl.requested` is, or `None` when the ask collects no
/// one-time code. Only a single-value `otp` ask is a challenge: a form that
/// carries a code field beside a password is the person's whole form, and
/// a bare code could never answer it.
pub fn challenge_from_schema(
    principal: &str,
    workspace: &str,
    correlation_id: &str,
    input_schema: Option<&Value>,
    timestamp: i64,
    lookback_secs: u64,
) -> Option<ChallengeContext> {
    let schema = input_schema?;
    let spec = schema.get("sensitive").filter(|v| v.is_object())?;
    if spec.get("kind").and_then(Value::as_str) != Some("otp") {
        return None;
    }
    if spec
        .get("fields")
        .and_then(Value::as_array)
        .is_some_and(|fields| !fields.is_empty())
    {
        return None;
    }
    let deadline_ms = spec
        .get("collection_deadline_ms")
        .and_then(Value::as_i64)
        .filter(|d| *d > 0);
    let expected_host = spec
        .get("expected_destination")
        .and_then(Value::as_str)
        .map(host_of)
        .filter(|host| !host.is_empty());
    // A spec may name the code's length (`expected_digits`); none does
    // today, so extraction accepts every approved length until one does.
    let digits = spec
        .get("expected_digits")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| EXPECTED_DIGITS_RANGE.contains(n));
    Some(ChallengeContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        correlation_id: correlation_id.to_string(),
        started_at_ms: timestamp,
        deadline_ms,
        expected_host,
        expected: ExpectedFormat { digits },
        lookback_ms: (lookback_secs as i64) * 1000,
        // Both filled in by `on_challenge`, which holds the resolver's record
        // of when this service's last challenge was decided and the answer
        // target naming the lane that owns the ask.
        not_before_ms: None,
        lane: String::new(),
    })
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
