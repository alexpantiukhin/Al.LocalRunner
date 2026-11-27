//! The lines `wait` prints to standard error while an order is queued or running: the reason
//! it waits and one line per change, taken from the event stream of the hub, so a monitor or an
//! agent sees progress without polling `status`.

use super::jobs::hub;
use super::read_cmd::explain;
use crate::paths::Layout;
use lr_core::{JobId, StepState};
use lr_proto::{CliReply, CliRequest, QueueEvent};
use std::io::Write;

/// The line for `event` when it concerns `job`: it started, a step started, it finished.
pub fn event_line(event: &QueueEvent, job: &JobId) -> Option<String> {
    match event {
        QueueEvent::JobStarted { job: j, runner } if j == job => {
            Some(format!("running on runner {runner}"))
        }
        QueueEvent::StepChanged { job: j, step }
            if j == job && step.state == StepState::Running =>
        {
            Some(format!("step {}", step.name))
        }
        QueueEvent::JobFinished { job: j, state, .. } if j == job => {
            Some(format!("finished: {}", format!("{state:?}").to_lowercase()))
        }
        _ => None,
    }
}

/// Whether `event` can change why a waiting order waits: another order started or finished, a
/// runner came or went.
pub fn needs_reexplain(event: &QueueEvent) -> bool {
    matches!(
        event,
        QueueEvent::JobStarted { .. }
            | QueueEvent::JobFinished { .. }
            | QueueEvent::RunnerOnline { .. }
            | QueueEvent::RunnerOffline { .. }
            | QueueEvent::RunnerChanged { .. }
    )
}

/// One JSON object for `text`.
pub fn line_json(job: &JobId, text: &str) -> String {
    serde_json::json!({ "job": job, "event": text }).to_string()
}

fn print(job: &JobId, text: &str, json: bool) {
    let line = if json {
        line_json(job, text)
    } else {
        format!("{job}: {text}")
    };
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Prints the state of `job` and every change of it until the caller drops this future: the
/// reason it waits now (asked again when the queue changes, printed only when it differs), then
/// the lines of [`event_line`]. A hub or an event stream that does not answer ends the printing
/// quietly: `wait` itself goes on.
pub async fn watch(layout: &Layout, job: JobId, json: bool) {
    let Ok(client) = hub(layout).await else {
        return std::future::pending().await;
    };
    if client.send(&CliRequest::Events).await.is_err() {
        return std::future::pending().await;
    }
    let mut last = String::new();
    let ask = |state: &mut String, text: String| {
        if *state != text {
            print(&job, &text, json);
            *state = text;
        }
    };
    if let Ok((_, text)) = explain_now(layout, &job).await {
        ask(&mut last, text);
    }
    while let Ok(reply) = client.recv::<CliReply>().await {
        let CliReply::Event(e) = reply else { continue };
        if let Some(line) = event_line(&e.event, &job) {
            ask(&mut last, line);
        } else if needs_reexplain(&e.event) && last.starts_with("waits") {
            if let Ok((_, text)) = explain_now(layout, &job).await {
                ask(&mut last, text);
            }
        }
    }
    std::future::pending::<()>().await
}

async fn explain_now(layout: &Layout, job: &JobId) -> Result<(String, String), String> {
    let client = hub(layout).await?;
    let found = explain(&client, job, None).await;
    client.close().await;
    found
}
