mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::http::{csrf_of, dashboard, get, host, send, session, PORT};
use futures_util::StreamExt;
use lr_abstractions::CancelRequest;
use lr_core::JobId;
use lr_dashboard::{ensure_loopback, CSRF_HEADER};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread")]
async fn binds_loopback_only() {
    let dash = dashboard(common::source());
    let token = dash.launch_token().to_string();
    let running = dash.serve().await.unwrap();
    let addr = running.local_addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_ne!(addr.port(), 0);
    assert!(running
        .url()
        .starts_with(&format!("http://127.0.0.1:{}/?token=", addr.port())));

    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /?token={token} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    tcp.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 303"), "{answer}");

    for wide in [
        "0.0.0.0:8080",
        "[::]:8080",
        "192.168.1.5:8080",
        "10.0.0.1:1",
    ] {
        let wide: SocketAddr = wide.parse().unwrap();
        assert!(ensure_loopback(wide).is_err(), "{wide}");
    }
    for local in ["127.0.0.1:8080", "[::1]:8080"] {
        ensure_loopback(local.parse().unwrap()).unwrap();
    }
    running.shutdown().await;
}

#[tokio::test]
async fn non_loopback_host_header_refused() {
    let dash = dashboard(common::source());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let uri = format!("/?token={token}");

    for bad in [
        "evil.example".to_string(),
        format!("evil.example:{PORT}"),
        format!("192.168.1.5:{PORT}"),
        "127.0.0.1:1".to_string(),
        "127.0.0.1".to_string(),
        format!("localhost.evil.example:{PORT}"),
    ] {
        let (status, _, _) = send(&router, get(&uri, &bad)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "host {bad}");
    }
    let no_host = Request::get(uri.as_str()).body(Body::empty()).unwrap();
    assert_eq!(send(&router, no_host).await.0, StatusCode::FORBIDDEN);

    let foreign_origin = Request::get(uri.as_str())
        .header(header::HOST, host())
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, foreign_origin).await.0, StatusCode::FORBIDDEN);

    let cross_site = Request::get(uri.as_str())
        .header(header::HOST, host())
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, cross_site).await.0, StatusCode::FORBIDDEN);

    let (status, _, _) = send(&router, get(&uri, &format!("localhost:{PORT}"))).await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "refused requests do not spend the token"
    );

    for good in [format!("127.0.0.1:{PORT}"), format!("[::1]:{PORT}")] {
        let fresh = dashboard(common::source());
        let uri = format!("/?token={}", fresh.launch_token());
        let (status, _, _) = send(&fresh.router(PORT), get(&uri, &good)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "host {good}");
    }
}

#[tokio::test]
async fn queue_page_lists_jobs_with_project_source_label() {
    let dash = dashboard(common::source());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);

    let (status, _, _) = send(&router, get("/", &host())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&router, get("/?token=wrong", &host())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = session(&router, &token).await;
    let (status, _, _) = send(&router, get(&format!("/?token={token}"), &host())).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "launch token is single-use"
    );

    let forged = Request::get("/")
        .header(header::HOST, host())
        .header(header::COOKIE, format!("lr_session_{PORT}=00ff"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, forged).await.0, StatusCode::UNAUTHORIZED);
    let other_port = Request::get("/")
        .header(header::HOST, host())
        .header(header::COOKIE, cookie.replace(&PORT.to_string(), "1"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, other_port).await.0, StatusCode::UNAUTHORIZED);

    let req = Request::get("/")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, headers, page) = send(&router, req).await;
    assert_eq!(status, StatusCode::OK);
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");

    let row = page.find("data-job=\"j-queued\"").expect("queued row");
    let row = &page[row..row + page[row..].find("</tr>").unwrap()];
    assert!(row.contains("<td data-col=\"project\">alpha</td>"), "{row}");
    assert!(row.contains("<td data-col=\"source\">github</td>"), "{row}");
    assert!(row.contains("<td data-col=\"label\">agent-1</td>"), "{row}");
    assert!(page.contains("data-job=\"j-run\""));
    assert!(
        !page.contains("<script>alert"),
        "markup from data must be escaped"
    );
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;&quot;&#39;&amp;"));

    let api = Request::get("/api/snapshot")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, json) = send(&router, api).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.contains("\"agent-1\""));
    let (status, _, _) = send(&router, get("/api/snapshot", &host())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cancel_action_requires_token() {
    let source = common::source();
    let dash = dashboard(source.clone());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let cookie = session(&router, &token).await;
    let page = send(
        &router,
        Request::get("/")
            .header(header::HOST, host())
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .2;
    let csrf = csrf_of(&page);
    assert!(csrf.len() >= 32, "{csrf}");

    let body = r#"{"job":"j-queued","reason":"stuck"}"#;
    let cancel = |cookie: Option<&str>, csrf: Option<&str>, origin: Option<&str>| {
        let mut req = Request::post("/api/cancel")
            .header(header::HOST, host())
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        if let Some(t) = csrf {
            req = req.header(CSRF_HEADER, t);
        }
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        req.body(Body::from(body)).unwrap()
    };

    assert_eq!(
        send(&router, cancel(None, Some(&csrf), None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&router, cancel(Some(&cookie), None, None)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&router, cancel(Some(&cookie), Some("forged"), None))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&router, cancel(Some(&cookie), Some(&token), None))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &router,
            cancel(Some(&cookie), Some(&csrf), Some("http://evil.example"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(source.cancels().is_empty());

    let get_cancel = Request::get("/api/cancel")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .header(CSRF_HEADER, &csrf)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&router, get_cancel).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert!(source.cancels().is_empty());

    let origin = format!("http://127.0.0.1:{PORT}");
    let (status, _, _) = send(&router, cancel(Some(&cookie), Some(&csrf), Some(&origin))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        source.cancels(),
        vec![(
            JobId("j-queued".into()),
            CancelRequest {
                actor: "dashboard:alex".into(),
                reason: Some("stuck".into()),
            }
        )]
    );
}

#[tokio::test]
async fn live_events_and_logs_need_the_session() {
    let source = common::source();
    source.set_log(JobId("j-run".into()), b"line <b>1</b>\n".to_vec());
    let dash = dashboard(source.clone());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let cookie = session(&router, &token).await;
    let with_cookie = |uri: &str| {
        Request::get(uri)
            .header(header::HOST, host())
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };

    assert_eq!(
        send(&router, get("/api/events", &host())).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&router, get("/api/log/j-run", &host())).await.0,
        StatusCode::UNAUTHORIZED
    );

    let (status, headers, log) = send(&router, with_cookie("/api/log/j-run")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    assert_eq!(log, "line <b>1</b>\n");
    assert_eq!(
        send(&router, with_cookie("/api/log/nope")).await.0,
        StatusCode::NOT_FOUND
    );

    let resp = router
        .clone()
        .oneshot(with_cookie("/api/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    source.publish(lr_proto::DashboardEvent {
        broker: "home".into(),
        event: lr_proto::QueueEvent::RunnerOnline {
            name: "box-1".into(),
        },
    });
    let mut body = resp.into_body().into_data_stream();
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("event: update"), "{text}");
    assert!(text.contains("\"runner_online\""), "{text}");
}
