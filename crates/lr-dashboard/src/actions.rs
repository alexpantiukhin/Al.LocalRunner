use crate::guard::{data_error, AppState};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use lr_abstractions::{CancelRequest, DataSource};
use lr_core::JobId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const REASON_MAX: usize = 200;
const MANY_MAX: usize = 500;

#[derive(Deserialize)]
pub(crate) struct CancelBody {
    job: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CancelManyBody {
    jobs: Vec<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct Outcome {
    job: String,
    ok: bool,
    error: Option<String>,
}

fn request<D>(state: &AppState<D>, reason: Option<String>) -> CancelRequest {
    let reason = reason
        .map(|r| {
            r.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .trim()
                .chars()
                .take(REASON_MAX)
                .collect::<String>()
        })
        .filter(|r| !r.is_empty());
    CancelRequest {
        actor: format!("dashboard:{}", state.user),
        reason,
    }
}

fn forged() -> Response {
    (StatusCode::FORBIDDEN, "missing anti-forgery header").into_response()
}

/// Cancels one job (spec 9.4): `POST` with the anti-forgery header; counts as `--force` and is
/// recorded as `dashboard:<OS user>` with the reason from the dialog.
pub(crate) async fn cancel<D>(
    State(state): State<Arc<AppState<D>>>,
    headers: HeaderMap,
    Json(body): Json<CancelBody>,
) -> Response
where
    D: DataSource + 'static,
{
    if !state.csrf_ok(&headers) {
        return forged();
    }
    let req = request(&state, body.reason);
    match state.source.cancel(&JobId(body.job), req).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => data_error(&e),
    }
}

/// Cancels the jobs listed in the dialog of the separate "cancel filtered" action, one by one,
/// and reports each result.
pub(crate) async fn cancel_many<D>(
    State(state): State<Arc<AppState<D>>>,
    headers: HeaderMap,
    Json(body): Json<CancelManyBody>,
) -> Response
where
    D: DataSource + 'static,
{
    if !state.csrf_ok(&headers) {
        return forged();
    }
    if body.jobs.is_empty() || body.jobs.len() > MANY_MAX {
        return (StatusCode::BAD_REQUEST, "1 to 500 jobs").into_response();
    }
    let mut outcomes = Vec::with_capacity(body.jobs.len());
    for job in body.jobs {
        let req = request(&state, body.reason.clone());
        let result = state.source.cancel(&JobId(job.clone()), req).await;
        outcomes.push(Outcome {
            job,
            ok: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
        });
    }
    Json(outcomes).into_response()
}
