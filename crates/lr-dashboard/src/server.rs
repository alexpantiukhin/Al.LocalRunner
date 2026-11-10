use crate::guard::{ct_eq, guard, random_hex, AppState};
use crate::snapshot::{render, render_snapshot, Mode, SCRIPT, STYLE};
use crate::{actions, sse};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use lr_abstractions::{DataError, DataSource};
use lr_core::JobId;
use lr_proto::DashboardSnapshot;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// What the dashboard needs besides its data source.
#[derive(Debug, Clone)]
pub struct DashboardOptions {
    /// OS user of the hub; cancels are recorded as `dashboard:<user>`.
    pub user: String,
}

/// Why the dashboard could not start.
#[derive(Debug)]
pub enum DashboardError {
    /// The OS gave no randomness for the launch token.
    Random(String),
    /// The loopback socket could not be bound.
    Bind(std::io::Error),
    /// The socket ended up on an address that is not loopback.
    NotLoopback(SocketAddr),
}

impl fmt::Display for DashboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DashboardError::Random(e) => write!(f, "no randomness for the launch token: {e}"),
            DashboardError::Bind(e) => write!(f, "cannot bind 127.0.0.1: {e}"),
            DashboardError::NotLoopback(a) => write!(f, "dashboard socket {a} is not loopback"),
        }
    }
}

impl std::error::Error for DashboardError {}

/// Accepts only a loopback socket address (`127.0.0.0/8`, `::1`); wildcard and LAN addresses
/// are refused. There is deliberately no way to widen this (spec 9.4).
pub fn ensure_loopback(addr: SocketAddr) -> Result<(), DashboardError> {
    let ip = match addr.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        ip => ip,
    };
    if ip.is_loopback() {
        Ok(())
    } else {
        Err(DashboardError::NotLoopback(addr))
    }
}

/// The live dashboard over a [`DataSource`]: page, JSON, SSE stream and the cancel action,
/// guarded by a random launch token, a session cookie and an anti-forgery header.
pub struct Dashboard<D> {
    source: Arc<D>,
    options: DashboardOptions,
    launch_token: String,
    session: String,
    csrf: String,
    stop: watch::Sender<bool>,
}

/// A dashboard serving on loopback; dropping it without [`RunningDashboard::shutdown`] leaves
/// the server running until the runtime ends.
pub struct RunningDashboard {
    addr: SocketAddr,
    url: String,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDashboard {
    /// The bound IPv4 loopback address.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The address to open, carrying the launch token.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stops serving, ends live streams and waits for the server tasks.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

#[derive(Deserialize)]
struct IndexQuery {
    token: Option<String>,
}

impl<D> Dashboard<D>
where
    D: DataSource<Snapshot = DashboardSnapshot> + 'static,
    D::Event: Serialize + 'static,
{
    /// A dashboard with fresh random launch token, session and anti-forgery secrets.
    pub fn new(source: Arc<D>, options: DashboardOptions) -> Result<Self, DashboardError> {
        let secret = || random_hex(32).map_err(|e| DashboardError::Random(e.to_string()));
        let (stop, _) = watch::channel(false);
        Ok(Dashboard {
            source,
            options,
            launch_token: secret()?,
            session: secret()?,
            csrf: secret()?,
            stop,
        })
    }

    /// The launch token the URL carries; exchanged once per browser for the session cookie.
    pub fn launch_token(&self) -> &str {
        &self.launch_token
    }

    /// The routes for a dashboard reachable on `port`; requests naming another host or port
    /// are refused.
    pub fn router(&self, port: u16) -> Router {
        let state = Arc::new(AppState {
            source: self.source.clone(),
            user: self.options.user.clone(),
            launch_token: self.launch_token.clone(),
            session: self.session.clone(),
            csrf: self.csrf.clone(),
            port,
            stop: self.stop.subscribe(),
        });
        Router::new()
            .route("/", get(index::<D>))
            .route("/app.js", get(script))
            .route("/app.css", get(style))
            .route("/snapshot.html", get(snapshot_file::<D>))
            .route("/api/snapshot", get(snapshot_json::<D>))
            .route("/api/log/{job}", get(log::<D>))
            .route("/api/events", get(sse::events::<D>))
            .route("/api/cancel", post(actions::cancel::<D>))
            .route("/api/cancel-many", post(actions::cancel_many::<D>))
            .layer(middleware::from_fn_with_state(state.clone(), guard::<D>))
            .with_state(state)
    }

    /// Binds a random free port on `127.0.0.1` (and the same port on `::1` when available),
    /// checks the bound address is loopback and serves until [`RunningDashboard::shutdown`].
    pub async fn serve(self) -> Result<RunningDashboard, DashboardError> {
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(DashboardError::Bind)?;
        let addr = v4.local_addr().map_err(DashboardError::Bind)?;
        ensure_loopback(addr)?;
        let mut listeners = vec![v4];
        if let Ok(v6) =
            TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, addr.port()))).await
        {
            if v6
                .local_addr()
                .map(ensure_loopback)
                .is_ok_and(|r| r.is_ok())
            {
                listeners.push(v6);
            }
        }
        let router = self.router(addr.port());
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                let mut stop = self.stop.subscribe();
                let router = router.clone();
                tokio::spawn(async move {
                    let _ = axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = stop.wait_for(|s| *s).await;
                        })
                        .await;
                })
            })
            .collect();
        Ok(RunningDashboard {
            url: format!(
                "http://127.0.0.1:{}/?token={}",
                addr.port(),
                self.launch_token
            ),
            addr,
            stop: self.stop,
            tasks,
        })
    }
}

fn data_error(err: DataError) -> Response {
    let status = match err {
        DataError::NotFound => StatusCode::NOT_FOUND,
        DataError::Refused(_) => StatusCode::FORBIDDEN,
        DataError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, err.to_string()).into_response()
}

async fn index<D>(
    State(state): State<Arc<AppState<D>>>,
    Query(query): Query<IndexQuery>,
) -> Response
where
    D: DataSource<Snapshot = DashboardSnapshot> + 'static,
{
    if let Some(token) = query.token {
        if !ct_eq(token.as_bytes(), state.launch_token.as_bytes()) {
            return (StatusCode::UNAUTHORIZED, "wrong launch token").into_response();
        }
        let cookie = format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            state.cookie_name(),
            state.session
        );
        return (
            StatusCode::SEE_OTHER,
            [
                (header::LOCATION, "/".to_string()),
                (header::SET_COOKIE, cookie),
            ],
        )
            .into_response();
    }
    match state.source.snapshot().await {
        Ok(snapshot) => html(render(&snapshot, Mode::Live { csrf: &state.csrf })),
        Err(e) => data_error(e),
    }
}

fn html(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

async fn script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SCRIPT,
    )
        .into_response()
}

async fn style() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE).into_response()
}

async fn snapshot_json<D>(State(state): State<Arc<AppState<D>>>) -> Response
where
    D: DataSource<Snapshot = DashboardSnapshot> + 'static,
{
    match state.source.snapshot().await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => data_error(e),
    }
}

async fn snapshot_file<D>(State(state): State<Arc<AppState<D>>>) -> Response
where
    D: DataSource<Snapshot = DashboardSnapshot> + 'static,
{
    match state.source.snapshot().await {
        Ok(snapshot) => {
            let mut resp = html(render_snapshot(&snapshot, &[]));
            resp.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"queue.html\""),
            );
            resp
        }
        Err(e) => data_error(e),
    }
}

async fn log<D>(State(state): State<Arc<AppState<D>>>, Path(job): Path<String>) -> Response
where
    D: DataSource + 'static,
{
    match state.source.log(&JobId(job)).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            String::from_utf8_lossy(&bytes).into_owned(),
        )
            .into_response(),
        Err(e) => data_error(e),
    }
}
