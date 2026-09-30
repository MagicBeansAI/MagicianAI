//! Serve a small startup application, then install the complete application on
//! the SAME workers/socket. Requests are dispatched as original Actix requests:
//! no proxy, replay, header rewriting, or special WebSocket/SSE transport.

use std::{
    cell::RefCell,
    future::Future,
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use actix_http::Request;
use actix_service::{boxed, IntoServiceFactory, Service, ServiceExt, ServiceFactory};
use actix_web::{
    body::{BoxBody, MessageBody},
    dev::{AppConfig, ServerHandle, ServiceResponse},
    web, App, Error, HttpMessage, HttpRequest, HttpResponse, HttpServer,
};
use magician::magician_v2::runtime::startup::StartupBarrier;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;
type HttpService = boxed::RcService<Request, ServiceResponse<BoxBody>, Error>;
type WorkerFactory = Box<dyn FnOnce(AppConfig) -> LocalFuture<Result<HttpService, ()>>>;
type PublishedFactory = Box<dyn Fn() -> WorkerFactory + Send>;

struct State {
    factory: Mutex<Option<PublishedFactory>>,
    published: CancellationToken,
    barrier: Arc<StartupBarrier>,
    expected_services: AtomicUsize,
    initialized: AtomicUsize,
    // Keep other workers available for UI/status while a worker compiles routes.
    route_build: Arc<Semaphore>,
    phase: Mutex<&'static str>,
    failed: CancellationToken,
}

pub struct StartupHttp {
    state: Arc<State>,
    handle: ServerHandle,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    signal_task: tokio::task::JoinHandle<()>,
    #[cfg(test)]
    address: std::net::SocketAddr,
}

impl StartupHttp {
    pub fn start(
        host: &str,
        port: u16,
        workers: usize,
        static_dir: Option<Arc<PathBuf>>,
    ) -> std::io::Result<Self> {
        Self::start_with_barrier(host, port, workers, static_dir, StartupBarrier::install())
    }

    fn start_with_barrier(
        host: &str,
        port: u16,
        workers: usize,
        static_dir: Option<Arc<PathBuf>>,
        barrier: Arc<StartupBarrier>,
    ) -> std::io::Result<Self> {
        let state = Arc::new(State {
            factory: Mutex::new(None),
            published: CancellationToken::new(),
            barrier,
            expected_services: AtomicUsize::new(workers.max(1)),
            initialized: AtomicUsize::new(0),
            route_build: Arc::new(Semaphore::new(workers.saturating_sub(1).max(1))),
            phase: Mutex::new("core"),
            failed: CancellationToken::new(),
        });
        let factory_state = Arc::clone(&state);
        let server = HttpServer::new(move || StartupFactory {
            state: Arc::clone(&factory_state),
            static_dir: static_dir.clone(),
        })
        .workers(workers.max(1))
        .disable_signals()
        .bind((host, port))?;
        // A hostname can bind multiple addresses. Actix creates one app per
        // worker AND listener, and all of them must be installed before ready.
        state
            .expected_services
            .store(workers.max(1) * server.addrs().len(), Ordering::Release);
        #[cfg(test)]
        let address = server.addrs()[0];
        let server = server.run();
        let handle = server.handle();
        // Construct before spawning so cancellation before the first poll also
        // releases readiness waiters, rather than leaving startup hung.
        let server_exit = InitializationGuard {
            state: Arc::clone(&state),
            complete: false,
        };
        let task = tokio::spawn(async move {
            let _server_exit = server_exit;
            server.await
        });
        let signal_state = Arc::clone(&state);
        let signal_handle = handle.clone();
        let signal_task = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = signal_state.barrier.shutdown.cancelled() => return,
                result = shutdown_signal() => {
                    if let Err(error) = result { tracing::error!(%error, "startup signal listener failed"); }
                },
            }
            signal_state.barrier.shutdown.cancel();
            signal_handle.stop(true).await;
        });
        tracing::info!(host, port, "startup HTTP listener launched");
        Ok(Self {
            state,
            handle,
            task: Some(task),
            signal_task,
            #[cfg(test)]
            address,
        })
    }

    pub fn phase(&self, phase: &'static str) {
        *self.state.phase.lock().expect("startup phase poisoned") = phase;
        tracing::info!(phase, "startup phase changed");
    }

    pub fn publish<F, I, S, B>(&self, factory: F)
    where
        F: Fn() -> I + Send + Clone + 'static,
        I: IntoServiceFactory<S, Request>,
        S: ServiceFactory<
                Request,
                Config = AppConfig,
                Response = ServiceResponse<B>,
                Error = Error,
                InitError = (),
            > + 'static,
        S::Service: 'static,
        S::Future: 'static,
        <S::Service as Service<Request>>::Future: 'static,
        B: MessageBody + 'static,
    {
        self.phase("http_routes");
        *self.state.factory.lock().expect("startup factory poisoned") = Some(Box::new(move || {
            let factory = factory.clone();
            Box::new(move |config| {
                let service = factory().into_factory().new_service(config);
                Box::pin(async move {
                    let service = service.await?;
                    Ok(boxed::rc_service(
                        service.map(|response| response.map_into_boxed_body()),
                    ))
                })
            })
        }));
        self.state.published.cancel();
    }

    pub async fn ready(&self) -> anyhow::Result<()> {
        tokio::select! {
            biased;
            _ = self.state.failed.cancelled() => anyhow::bail!("HTTP application initialization failed"),
            _ = self.state.barrier.shutdown.cancelled() => anyhow::bail!("startup cancelled"),
            _ = self.state.barrier.ready.cancelled() => Ok(()),
        }
    }

    pub fn handle(&self) -> ServerHandle {
        self.handle.clone()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.state.barrier.shutdown.clone()
    }

    pub async fn finished(&mut self) -> std::io::Result<()> {
        let result = self.task.as_mut().expect("server task present").await;
        self.task.take();
        result.map_err(std::io::Error::other)?
    }

    pub async fn stop(&mut self) {
        self.state.barrier.shutdown.cancel();
        self.handle.stop(true).await;
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        self.signal_task.abort();
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut quit = signal(SignalKind::quit())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
            _ = quit.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

impl Drop for StartupHttp {
    fn drop(&mut self) {
        self.state.barrier.shutdown.cancel();
        self.signal_task.abort();
        if self.task.is_some() {
            let handle = self.handle.clone();
            tokio::spawn(async move {
                handle.stop(false).await;
            });
        }
    }
}

#[derive(Clone)]
struct StartupFactory {
    state: Arc<State>,
    static_dir: Option<Arc<PathBuf>>,
}

struct InitializationGuard {
    state: Arc<State>,
    complete: bool,
}

impl Drop for InitializationGuard {
    fn drop(&mut self) {
        if !self.complete && !self.state.barrier.shutdown.is_cancelled() {
            self.state.failed.cancel();
        }
    }
}

impl ServiceFactory<Request> for StartupFactory {
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Config = AppConfig;
    type Service = StartupService;
    type InitError = ();
    type Future = LocalFuture<Result<Self::Service, ()>>;

    fn new_service(&self, config: AppConfig) -> Self::Future {
        let state = Arc::clone(&self.state);
        let mut app = App::new()
            .wrap(actix_web::middleware::from_fn(
                magician::magician_v2::cors::api_cors_middleware,
            ))
            .app_data(web::Data::from(Arc::clone(&state)))
            .route("/startup", web::get().to(status))
            .route("/api/magician/v2/startup", web::get().to(status))
            .route("/health", web::get().to(starting))
            .route(
                "/live",
                web::get().to(|| async {
                    HttpResponse::Ok()
                        .json(serde_json::json!({"status":"alive", "service":"magician"}))
                }),
            );
        if let Some(dir) = &self.static_dir {
            // Immutable compiled assets may load during boot. API and document
            // requests still go through the explicit starting handler below.
            app = app.service(actix_files::Files::new("/_app", dir.join("_app")));
        }
        let bootstrap = app
            .default_service(web::to(starting))
            .into_factory()
            .new_service(config.clone());
        let has_ui = self.static_dir.is_some();
        Box::pin(async move {
            let bootstrap = boxed::rc_service(
                bootstrap
                    .await?
                    .map(|response| response.map_into_boxed_body()),
            );
            let full = Rc::new(RefCell::new(None));
            let full_writer = Rc::clone(&full);
            let initializer_state = Arc::clone(&state);
            let guard = InitializationGuard {
                state: Arc::clone(&state),
                complete: false,
            };
            actix_web::rt::spawn(async move {
                let state = initializer_state;
                let mut guard = guard;
                tokio::select! {
                    biased;
                    _ = state.barrier.shutdown.cancelled() => return,
                    _ = state.published.cancelled() => {},
                }
                let _permit = tokio::select! {
                    biased;
                    _ = state.barrier.shutdown.cancelled() => return,
                    permit = Arc::clone(&state.route_build).acquire_owned() => permit.expect("route gate open"),
                };
                let factory = {
                    let factory = state.factory.lock().expect("startup factory poisoned");
                    factory.as_ref().expect("published factory")()
                };
                // Clone under the mutex, build outside it: route compilation
                // must not serialize every worker or block workers on a mutex.
                let future = factory(config);
                let initialized = tokio::select! {
                    biased;
                    _ = state.barrier.shutdown.cancelled() => return,
                    result = future => result,
                };
                match initialized {
                    Ok(service) => {
                        *full_writer.borrow_mut() = Some(service);
                        guard.complete = true;
                        if state.initialized.fetch_add(1, Ordering::AcqRel) + 1
                            == state.expected_services.load(Ordering::Acquire)
                            && !state.failed.is_cancelled()
                        {
                            *state.phase.lock().expect("startup phase poisoned") = "ready";
                            state.barrier.ready.cancel();
                            tracing::info!(
                                "HTTP application ready; releasing background startup work"
                            );
                        }
                    },
                    Err(()) => {
                        state.failed.cancel();
                        tracing::error!("HTTP worker application initialization failed");
                    },
                }
            });
            Ok(StartupService {
                bootstrap,
                full,
                state,
                has_ui,
            })
        })
    }
}

struct StartupService {
    bootstrap: HttpService,
    full: Rc<RefCell<Option<HttpService>>>,
    state: Arc<State>,
    has_ui: bool,
}

impl Service<Request> for StartupService {
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = LocalFuture<Result<Self::Response, Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(service) = self
            .full
            .borrow()
            .as_ref()
            .filter(|_| self.state.barrier.ready.is_cancelled())
        {
            service.poll_ready(cx)
        } else {
            self.bootstrap.poll_ready(cx)
        }
    }

    fn call(&self, request: Request) -> Self::Future {
        if !matches!(
            request.path(),
            "/startup" | "/api/magician/v2/startup" | "/live"
        ) && self.state.barrier.ready.is_cancelled()
        {
            if let Some(service) = self.full.borrow().as_ref() {
                return service.call(request);
            }
        }
        request.extensions_mut().insert(HasUi(self.has_ui));
        self.bootstrap.call(request)
    }
}

#[derive(Clone, Copy)]
struct HasUi(bool);

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;

    struct Echo;

    impl actix::Actor for Echo {
        type Context = actix_web_actors::ws::WebsocketContext<Self>;
    }

    impl
        actix::StreamHandler<
            Result<actix_web_actors::ws::Message, actix_web_actors::ws::ProtocolError>,
        > for Echo
    {
        fn handle(
            &mut self,
            message: Result<actix_web_actors::ws::Message, actix_web_actors::ws::ProtocolError>,
            context: &mut Self::Context,
        ) {
            if let Ok(actix_web_actors::ws::Message::Text(text)) = message {
                context.text(text);
            }
        }
    }

    #[actix_web::test]
    async fn published_application_preserves_websockets_and_incremental_streaming() {
        let barrier = Arc::new(StartupBarrier::new());
        let mut server = StartupHttp::start_with_barrier("127.0.0.1", 0, 2, None, barrier).unwrap();
        let ws_url = format!("ws://{}/ws", server.address);
        let blocked = tokio_tungstenite::connect_async(&ws_url).await.unwrap_err();
        assert!(
            matches!(blocked, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 503)
        );
        let release = CancellationToken::new();
        let app_release = release.clone();
        server.publish(move || {
            App::new()
                .app_data(web::Data::new(app_release.clone()))
                .route(
                    "/ws",
                    web::get().to(|request: HttpRequest, stream: web::Payload| async move {
                        actix_web_actors::ws::start(Echo, &request, stream)
                    }),
                )
                .route(
                    "/events",
                    web::get().to(|release: web::Data<CancellationToken>| async move {
                        let stream = futures_util::stream::unfold(
                            (0, release),
                            |(index, release)| async move {
                                if index == 2 {
                                    return None;
                                }
                                if index == 1 {
                                    release.cancelled().await;
                                }
                                Some((
                                    Ok::<_, Error>(web::Bytes::from(format!("data: {index}\n\n"))),
                                    (index + 1, release),
                                ))
                            },
                        );
                        HttpResponse::Ok()
                            .content_type("text/event-stream")
                            .streaming(stream)
                    }),
                )
        });
        tokio::time::timeout(Duration::from_secs(5), server.ready())
            .await
            .unwrap()
            .unwrap();
        let (mut socket, response) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        assert_eq!(response.status(), 101);
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                "original frame".into(),
            ))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame.into_text().unwrap(), "original frame");
        drop(socket);

        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut response = client
            .get(format!("http://{}/events", server.address))
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "data: 0\n\n"
        );
        assert!(!release.is_cancelled());
        release.cancel();
        assert_eq!(response.chunk().await.unwrap().unwrap(), "data: 1\n\n");
        assert!(response.chunk().await.unwrap().is_none());
        server.stop().await;
    }

    #[actix_web::test]
    async fn startup_serves_ui_gates_mutations_and_preserves_transport() {
        let barrier = Arc::new(StartupBarrier::new());
        let assets = tempfile::tempdir().unwrap();
        let mut server = StartupHttp::start_with_barrier(
            "127.0.0.1",
            0,
            2,
            Some(Arc::new(assets.path().into())),
            Arc::clone(&barrier),
        )
        .unwrap();
        let base = format!("http://{}", server.address);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let status = client.get(format!("{base}/startup")).send().await.unwrap();
        assert_eq!(status.status(), 200);
        assert_eq!(
            status.json::<serde_json::Value>().await.unwrap()["ready"],
            false
        );
        assert_eq!(
            client
                .get(format!("{base}/"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let health = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(health.status(), 503);
        assert_eq!(health.headers()["retry-after"], "1");
        assert_eq!(
            client
                .post(format!("{base}/api/write"))
                .body("must not execute")
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        let writes = Arc::new(AtomicUsize::new(0));
        let app_writes = Arc::clone(&writes);
        server.publish(move || App::new()
            .app_data(web::Data::from(Arc::clone(&app_writes)))
            .route("/health", web::get().to(|| async { HttpResponse::Ok().finish() }))
            .route("/api/write", web::post().to(|request: HttpRequest, body: String, count: web::Data<AtomicUsize>| async move {
                if request.headers().get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer fixture") {
                    return HttpResponse::Unauthorized().finish();
                }
                count.fetch_add(1, Ordering::SeqCst);
                HttpResponse::Ok().json(serde_json::json!({
                    "query": request.query_string(), "body": body,
                    "peer_loopback": request.peer_addr().is_some_and(|a| a.ip().is_loopback()),
                }))
            })));
        tokio::time::timeout(Duration::from_secs(5), server.ready())
            .await
            .unwrap()
            .unwrap();
        assert!(barrier.ready.is_cancelled());
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(
            client
                .post(format!("{base}/api/write"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let response = client
            .post(format!("{base}/api/write?scope=a%2Fb"))
            .bearer_auth("fixture")
            .body("original payload")
            .send()
            .await
            .unwrap();
        let body = response.json::<serde_json::Value>().await.unwrap();
        assert_eq!(body["query"], "scope=a%2Fb");
        assert_eq!(body["body"], "original payload");
        assert_eq!(body["peer_loopback"], true);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            client
                .get(format!("{base}/health"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        server.stop().await;
        assert!(barrier.shutdown.is_cancelled());
    }

    #[actix_web::test]
    async fn startup_can_stop_before_application_publication() {
        let barrier = Arc::new(StartupBarrier::new());
        let mut server =
            StartupHttp::start_with_barrier("127.0.0.1", 0, 2, None, Arc::clone(&barrier)).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server.stop())
            .await
            .unwrap();
        assert!(!barrier.ready.is_cancelled());
        assert!(server.ready().await.is_err());
    }

    #[actix_web::test]
    async fn lost_server_task_releases_readiness_waiters() {
        let barrier = Arc::new(StartupBarrier::new());
        let mut server =
            StartupHttp::start_with_barrier("127.0.0.1", 0, 2, None, Arc::clone(&barrier)).unwrap();
        // Exercise cancellation before the server task's first poll too.
        server.task.as_ref().unwrap().abort();
        assert!(tokio::time::timeout(Duration::from_secs(5), server.ready())
            .await
            .unwrap()
            .is_err());
        assert!(!barrier.ready.is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), server.stop())
            .await
            .unwrap();
    }

    #[actix_web::test]
    async fn application_initialization_error_never_publishes_readiness() {
        let barrier = Arc::new(StartupBarrier::new());
        let mut server =
            StartupHttp::start_with_barrier("127.0.0.1", 0, 2, None, Arc::clone(&barrier)).unwrap();
        server.publish(|| {
            App::new().data_factory(|| async { Err::<String, _>("fixture initialization failure") })
        });
        assert!(tokio::time::timeout(Duration::from_secs(5), server.ready())
            .await
            .unwrap()
            .is_err());
        assert!(!barrier.ready.is_cancelled());
        server.stop().await;
    }

    #[actix_web::test]
    async fn all_workers_must_finish_before_readiness() {
        let barrier = Arc::new(StartupBarrier::new());
        let mut server =
            StartupHttp::start_with_barrier("127.0.0.1", 0, 2, None, Arc::clone(&barrier)).unwrap();
        let release = CancellationToken::new();
        let factory_release = release.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        server.publish(move || {
            let is_second = calls.fetch_add(1, Ordering::SeqCst) == 1;
            let release = factory_release.clone();
            App::new().data_factory(move || {
                let release = release.clone();
                async move {
                    if is_second {
                        release.cancelled().await;
                    }
                    Ok::<_, ()>(String::from("initialized"))
                }
            })
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while server.state.initialized.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!barrier.ready.is_cancelled());
        release.cancel();
        tokio::time::timeout(Duration::from_secs(5), server.ready())
            .await
            .unwrap()
            .unwrap();
        server.stop().await;
    }
}

async fn status(state: web::Data<State>) -> HttpResponse {
    let failed = state.failed.is_cancelled();
    let stopping = state.barrier.shutdown.is_cancelled();
    let ready = state.barrier.ready.is_cancelled() && !failed && !stopping;
    HttpResponse::Ok().insert_header(("Cache-Control", "no-store")).json(serde_json::json!({
        "service": "magician",
        "status": if failed { "failed" } else if stopping { "stopping" } else if ready { "ready" } else { "starting" },
        "phase": *state.phase.lock().expect("startup phase poisoned"),
        "ready": ready,
        "capabilities": { "vector": magician::magician_v2::runtime::ollama_lifecycle::is_available() },
    }))
}

async fn starting(request: HttpRequest) -> HttpResponse {
    use actix_web::HttpMessage;
    let is_document = request
        .extensions()
        .get::<HasUi>()
        .is_some_and(|flag| flag.0)
        && matches!(request.method().as_str(), "GET" | "HEAD")
        && !request.path().starts_with("/api/")
        && !request.path().starts_with("/health");
    if is_document {
        return HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .content_type("text/html; charset=utf-8")
            .body(STARTING_PAGE);
    }
    HttpResponse::ServiceUnavailable().insert_header(("Retry-After", "1"))
        .insert_header(("Cache-Control", "no-store"))
        .json(serde_json::json!({"status":"starting", "service":"magician", "code":"service_starting", "retryable":true}))
}

const STARTING_PAGE: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Starting your workspace</title><style>body{font:16px system-ui;margin:0;min-height:100vh;display:grid;place-items:center;background:#101116;color:#eee}main{max-width:32rem;padding:2rem}p{color:#b8bbc6}h1{font-size:1.5rem}</style><main aria-live="polite"><h1>Starting your workspace…</h1><p>Your workspace will open automatically when it is ready.</p><p id="status"></p></main><script>async function check(){try{const r=await fetch('/startup',{cache:'no-store'});const s=await r.json();if(s.ready){location.reload();return}document.getElementById('status').textContent=s.status==='failed'?'Startup failed. Check the service logs.':''}catch{document.getElementById('status').textContent='Waiting for the service to reconnect…'}setTimeout(check,500)}check()</script></html>"#;
