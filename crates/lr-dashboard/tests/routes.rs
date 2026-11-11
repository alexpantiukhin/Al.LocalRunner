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

const READ: &[&str] = &[
    "/",
    "/app.js",
    "/app.css",
    "/snapshot.html",
    "/api/snapshot",
    "/api/log/{job}",
    "/api/events",
];
const CANCEL: &[&str] = &["/api/cancel", "/api/cancel-many"];
const METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "connect", "any", "on",
];

fn sources(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(std::fs::read_to_string(path).unwrap());
        }
    }
}

fn call_of(text: &str) -> &str {
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return &text[..i],
            ')' => depth -= 1,
            _ => {}
        }
    }
    text
}

fn declared_routes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in text.split(".route(").skip(1) {
        let call = call_of(part);
        let path = call.split('"').nth(1).expect("a literal path").to_string();
        let handler = &call[call.find(',').expect("a handler")..];
        let mut found = false;
        for m in METHODS {
            let pattern = format!("{m}(");
            for (at, _) in handler.match_indices(&pattern) {
                let before = handler[..at].chars().last();
                if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':') {
                    out.push((m.to_string(), path.clone()));
                    found = true;
                }
            }
        }
        assert!(found, "no method router in .route({call})");
    }
    out
}

#[test]
fn the_router_declares_exactly_the_read_routes_and_cancel() {
    let mut texts = Vec::new();
    sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut texts,
    );
    let mut declared: Vec<(String, String)> =
        texts.iter().flat_map(|t| declared_routes(t)).collect();
    declared.sort();
    let mut allowed: Vec<(String, String)> = READ
        .iter()
        .map(|p| ("get".to_string(), p.to_string()))
        .chain(CANCEL.iter().map(|p| ("post".to_string(), p.to_string())))
        .collect();
    allowed.sort();
    assert_eq!(declared, allowed);
    for other in [
        ".nest(",
        ".nest_service(",
        ".merge(",
        ".route_service(",
        ".fallback(",
        ".fallback_service(",
    ] {
        assert!(
            texts.iter().all(|t| !t.contains(other)),
            "the router grows outside .route: {other}"
        );
    }
    assert_eq!(
        declared_routes(r#"x.route("/a", get(h).post(p)).route("/b", any(h))"#),
        [
            ("get".to_string(), "/a".to_string()),
            ("post".to_string(), "/a".to_string()),
            ("any".to_string(), "/b".to_string())
        ]
    );
}

#[tokio::test]
async fn router_answers_only_get_on_read_routes_and_post_on_cancel() {
    let source = common::source();
    let dash = dashboard(source.clone());
    let token = dash.launch_token().to_string();
    let router = dash.router(PORT);
    let (cookie, csrf) = login(&router, &token).await;
    let all = [
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
        Method::TRACE,
    ];
    for uri in [
        "/api/submit",
        "/api/cancel/j-run",
        "/api/snapshot/x",
        "/api",
    ] {
        for method in all.iter().chain([&Method::GET]) {
            let (status, _, _) =
                send(&router, authorized(method.clone(), uri, &cookie, &csrf)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        }
    }
    for uri in READ.iter().map(|p| p.replace("{job}", "j-run")) {
        for method in &all {
            let (status, _, _) =
                send(&router, authorized(method.clone(), &uri, &cookie, &csrf)).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }
    for uri in CANCEL {
        for method in all.iter().skip(1).chain([&Method::GET]) {
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
