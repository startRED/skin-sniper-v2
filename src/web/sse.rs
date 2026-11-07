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
        Event::Gone { id, at, reason } => (
            "gone",
            serde_json::to_string(&serde_json::json!({ "id": id, "at": at, "reason": reason })),
        ),
        Event::State(s) => ("state", serde_json::to_string(&s)),
        Event::Purchase(p) => ("purchase", serde_json::to_string(&p)),
    };
    Some(sse::Event::default().event(name).data(data.ok()?))
}

pub async fn events(
    State(app): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<sse::Event, Infallible>>> {
    // Lagged encerra o stream: o EventSource reconecta e o painel recarrega tudo, em vez de perder gone/purchase.
    let stream = BroadcastStream::new(app.events.subscribe())
        .map_while(|ev| ev.ok())
        .filter_map(to_sse)
        .map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
