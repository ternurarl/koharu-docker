//! GET /events - Server-Sent Events stream.
//!
//! Frames carry no SSE "event:" name; the JSON "event" discriminator in the
//! payload identifies the type. Live frames use the monotonic seq as the SSE
//! "id:" field. A fresh (or reconnecting) client is seeded with a snapshot
//! before the live tail.

use async_stream::try_stream;
use axum::{
    Router,
    extract::State,
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use futures::Stream;
use tokio_stream::{StreamExt as _, wrappers::BroadcastStream};

use crate::state::{AppEvent, AppState, SequencedEvent};

pub fn router() -> Router<AppState> {
    Router::new().route("/events", get(events))
}

fn snapshot_frame(app: &AppState) -> Result<Event, axum::Error> {
    Event::default()
        .json_data(AppEvent::Snapshot {
            jobs: app.jobs_snapshot(),
        })
        .map_err(axum::Error::new)
}

fn sequenced_frame(sequenced: &SequencedEvent) -> Result<Event, axum::Error> {
    Event::default()
        .id(sequenced.seq.to_string())
        .json_data(&sequenced.event)
        .map_err(axum::Error::new)
}

fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

async fn events(
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let _resume_from = last_event_id(&headers);
    let mut receiver = BroadcastStream::new(app.bus.subscribe());
    let app_for_stream = app.clone();

    let stream = try_stream! {
        // Reconnects are re-seeded with a snapshot; this headless port does
        // not retain a replay ring buffer, so Last-Event-ID only chooses the
        // seed-then-live behavior rather than a gap-free replay.
        yield snapshot_frame(&app)?;

        while let Some(message) = receiver.next().await {
            match message {
                Ok(sequenced) => yield sequenced_frame(&sequenced)?,
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_)) => {
                    yield snapshot_frame(&app_for_stream)?;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
