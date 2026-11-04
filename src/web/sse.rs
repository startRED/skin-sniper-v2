//! web/sse — `GET /api/events`: opportunity, gone, state, purchase; keep-alive de 15 s.

use super::AppState;
use super::api::Event;
use axum::extract::State;
use axum::response::sse::{self, KeepAlive, Sse};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

fn to_sse(ev: Event) -> Option<sse::Event> {
    let (name, data) = match ev {
        Event::Opportunity(o) => ("opportunity", serde_json::to_string(&o)),
        Event::Gone { id } => (
            "gone",
            serde_json::to_string(&serde_json::json!({ "id": id })),
        ),
        Event::State(s) => ("state", serde_json::to_string(&s)),
        Event::Purchase(p) => ("purchase", serde_json::to_string(&p)),
    };
    Some(sse::Event::default().event(name).data(data.ok()?))
}

pub async fn events(
    State(app): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<sse::Event, Infallible>>> {
    // Receptor lento perde eventos (Lagged) em vez de travar quem publica; o painel relê via GET.
    let stream = BroadcastStream::new(app.events.subscribe())
        .filter_map(|ev| ev.ok().and_then(to_sse))
        .map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
