use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tokio::sync::watch;

/// Header that every state-changing request must carry with the page's anti-forgery token.
pub const CSRF_HEADER: &str = "x-lr-csrf";

pub(crate) struct AppState<D> {
    pub(crate) source: Arc<D>,
    pub(crate) user: String,
    pub(crate) launch_token: String,
    pub(crate) session: String,
    pub(crate) csrf: String,
    pub(crate) port: u16,
    pub(crate) stop: watch::Receiver<bool>,
}

impl<D> AppState<D> {
    pub(crate) fn cookie_name(&self) -> String {
        format!("lr_session_{}", self.port)
    }

    pub(crate) fn csrf_ok(&self, headers: &HeaderMap) -> bool {
        headers
            .get(CSRF_HEADER)
            .map(|v| ct_eq(v.as_bytes(), self.csrf.as_bytes()))
            .unwrap_or(false)
    }

    fn session_ok(&self, headers: &HeaderMap) -> bool {
        let name = self.cookie_name();
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .any(|(k, v)| k == name && ct_eq(v.as_bytes(), self.session.as_bytes()))
    }
}

pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn random_hex(bytes: usize) -> Result<String, getrandom::Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn allowed_hosts(port: u16) -> [String; 3] {
    [
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ]
}

fn host_ok(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    allowed_hosts(port).contains(&host)
}

fn origin_ok(headers: &HeaderMap, port: u16) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let origin = origin.to_ascii_lowercase();
    allowed_hosts(port)
        .iter()
        .any(|h| origin == format!("http://{h}"))
}

fn fetch_site_ok(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(site) => matches!(site, "same-origin" | "none"),
    }
}

fn is_token_exchange(req: &Request) -> bool {
    req.uri().path() == "/"
        && req
            .uri()
            .query()
            .map(|q| q.split('&').any(|p| p.starts_with("token=")))
            .unwrap_or(false)
}

/// Refuses requests with a foreign `Host` or `Origin` (DNS rebinding, cross-site requests) and
/// requests without the session cookie, then adds the headers every answer carries.
pub(crate) async fn guard<D: Send + Sync + 'static>(
    State(state): State<Arc<AppState<D>>>,
    req: Request,
    next: Next,
) -> Response {
    let headers = req.headers();
    let mut resp = if !host_ok(headers, state.port)
        || !origin_ok(headers, state.port)
        || !fetch_site_ok(headers)
    {
        (StatusCode::FORBIDDEN, "foreign host or origin").into_response()
    } else if is_token_exchange(&req) || state.session_ok(headers) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "open the address printed by `localrunner dashboard`",
        )
            .into_response()
    };
    secure_headers(resp.headers_mut());
    resp
}

fn secure_headers(headers: &mut HeaderMap) {
    let fixed = [
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
    ];
    for (name, value) in fixed {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
}
