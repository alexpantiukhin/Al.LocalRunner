use crate::guard::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use lr_abstractions::{DataSource, EventStream};
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::watch;

struct Feed<E> {
    events: Box<dyn EventStream<E>>,
    stop: watch::Receiver<bool>,
}

async fn next_event<E: Serialize + Send>(mut feed: Feed<E>) -> Option<(Event, Feed<E>)> {
    if *feed.stop.borrow() {
        return None;
    }
    let event = tokio::select! {
        e = feed.events.next() => e?,
        _ = feed.stop.changed() => return None,
    };
    let data = serde_json::to_string(&event).ok()?;
    Some((Event::default().event("update").data(data), feed))
}

/// The live stream of the page: one SSE connection carrying the data source's events as
/// `update` events; it ends when the source ends or the dashboard stops.
pub(crate) async fn events<D>(State(state): State<Arc<AppState<D>>>) -> Response
where
    D: DataSource + 'static,
    D::Event: Serialize,
{
    let events = match state.source.subscribe().await {
        Ok(events) => events,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    let feed = Feed {
        events,
        stop: state.stop.clone(),
    };
    let stream = stream::unfold(feed, |feed| async move {
        next_event(feed)
            .await
            .map(|(event, feed)| (Ok::<_, Infallible>(event), feed))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
