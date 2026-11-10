use super::Source;
use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use lr_dashboard::{Dashboard, DashboardOptions};
use std::sync::Arc;
use tower::ServiceExt;

pub const PORT: u16 = 47_001;

pub fn dashboard(source: Arc<Source>) -> Dashboard<Source> {
    Dashboard::new(
        source,
        DashboardOptions {
            user: "alex".into(),
        },
    )
    .unwrap()
}

pub fn get(uri: &str, host: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap()
}

pub async fn send(router: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), 1 << 24).await.unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

pub fn host() -> String {
    format!("127.0.0.1:{PORT}")
}

/// Exchanges the launch token for the session cookie and returns `name=value`.
pub async fn session(router: &Router, token: &str) -> String {
    let (status, headers, _) = send(router, get(&format!("/?token={token}"), &host())).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    cookie.split(';').next().unwrap().to_string()
}

pub fn csrf_of(page: &str) -> String {
    let start = page.find("name=\"lr-csrf\" content=\"").unwrap() + 24;
    let end = start + page[start..].find('"').unwrap();
    page[start..end].to_string()
}

/// A logged-in browser: the session cookie and the page's anti-forgery token.
pub async fn login(router: &Router, token: &str) -> (String, String) {
    let cookie = session(router, token).await;
    let page = Request::get("/")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let csrf = csrf_of(&send(router, page).await.2);
    (cookie, csrf)
}
