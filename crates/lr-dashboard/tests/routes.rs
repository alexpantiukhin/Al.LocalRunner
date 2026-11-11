mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::http::{dashboard, get, host, login, send, PORT};
use lr_dashboard::CSRF_HEADER;

fn authorized(method: Method, uri: &str, cookie: &str, csrf: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host())
        .header(header::ORIGIN, format!("http://127.0.0.1:{PORT}"))
        .header(header::COOKIE, cookie)
        .header(CSRF_HEADER, csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"job":"j-queued","profile":"unit"}"#))
        .unwrap()
}

#[tokio::test]
async fn router_exposes_only_read_routes_and_cancel() {
    let source = common::source();
    let dash = dashboard(source.clone());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let (cookie, csrf) = login(&router, &token).await;
    for uri in [
        "/api/submit",
        "/api/run",
        "/api/profile",
        "/api/profiles",
        "/api/config",
        "/api/policy",
        "/api/runner/start",
        "/api/runner/stop",
        "/api/runners",
        "/api/up",
        "/api/down",
    ] {
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            let (status, _, _) =
                send(&router, authorized(method.clone(), uri, &cookie, &csrf)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        }
    }
    for uri in ["/api/snapshot", "/api/log/j-run", "/api/events", "/"] {
        for method in [Method::POST, Method::PUT, Method::DELETE] {
            let (status, _, _) =
                send(&router, authorized(method.clone(), uri, &cookie, &csrf)).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }
    for uri in ["/api/cancel", "/api/cancel-many"] {
        for method in [Method::PUT, Method::DELETE, Method::PATCH] {
            let (status, _, _) =
                send(&router, authorized(method.clone(), uri, &cookie, &csrf)).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }
    assert!(source.cancels().is_empty());
}

#[tokio::test]
async fn every_answer_is_kept_out_of_indexes_caches_and_type_sniffing() {
    let source = common::source();
    source.set_log(lr_core::JobId("j-run".into()), b"line\n".to_vec());
    let dash = dashboard(source);
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let (cookie, _) = login(&router, &token).await;
    let with_cookie = |uri: &str| {
        Request::get(uri)
            .header(header::HOST, host())
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };
    let answers = [
        send(&router, with_cookie("/")).await,
        send(&router, with_cookie("/api/snapshot")).await,
        send(&router, with_cookie("/api/log/j-run")).await,
        send(&router, get("/api/log/j-run", &host())).await,
    ];
    let statuses: Vec<StatusCode> = answers.iter().map(|a| a.0).collect();
    assert_eq!(
        statuses,
        [
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::UNAUTHORIZED
        ]
    );
    for (status, headers, _) in &answers {
        assert_eq!(headers["x-robots-tag"], "noindex, nofollow", "{status}");
        assert_eq!(
            headers[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "{status}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{status}");
    }
}

#[tokio::test]
async fn queue_row_shows_place_wait_reason_and_where_the_job_runs() {
    let dash = dashboard(common::source());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let (cookie, _) = login(&router, &token).await;
    let page = Request::get("/")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let page = send(&router, page).await.2;
    let start = page.find("<tr data-job=\"j-queued\">").expect("queued row");
    let row = &page[start..start + page[start..].find("</tr>").unwrap() + 5];
    assert_eq!(
        row,
        "<tr data-job=\"j-queued\"><td data-col=\"position\">1</td><td data-col=\"job\">j-queued</td>\
         <td data-col=\"state\">queued</td><td data-col=\"project\">alpha</td>\
         <td data-col=\"source\">github</td><td data-col=\"label\">agent-1</td>\
         <td data-col=\"profile\">unit</td><td data-col=\"cwd\">D:/work/alpha</td>\
         <td data-col=\"runner\"></td><td data-col=\"broker\">home</td>\
         <td data-col=\"wait\">runner busy</td></tr>"
    );
    let start = page.find("<tr data-job=\"j-run\">").expect("running row");
    let row = &page[start..start + page[start..].find("</tr>").unwrap()];
    assert!(row.contains("<td data-col=\"runner\">box-1</td>"), "{row}");
    assert!(row.contains("<td data-col=\"state\">running</td>"), "{row}");
    let api = Request::get("/api/snapshot")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&send(&router, api).await.2).unwrap();
    let card = &json["jobs"][1]["detail"];
    assert_eq!(card["summary"]["job"], "j-queued");
    assert_eq!(card["branch"], "main");
    assert_eq!(card["cwd"], "D:/work/alpha");
    assert_eq!(card["note"], common::XSS);
    assert_eq!(card["summary"]["queued_at"], 1_700_000_000_000u64);
}
