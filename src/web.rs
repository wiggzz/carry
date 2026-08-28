use std::{
    convert::Infallible,
    io::{BufRead, IsTerminal},
    net::SocketAddr,
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::run::{Backend, RunConfig, UserInput, run_interactive_with_events};

const INDEX: &str = include_str!("web/index.html");

#[derive(Clone)]
struct AppState {
    input: mpsc::UnboundedSender<UserInput>,
    events: broadcast::Sender<Value>,
    session_dir: Arc<std::path::PathBuf>,
    status: Arc<Mutex<&'static str>>,
}

#[derive(Deserialize)]
struct MessageRequest {
    message: String,
}

#[derive(Serialize)]
struct Session {
    state: &'static str,
}

pub async fn serve(address: SocketAddr, config: RunConfig, backend: Backend) -> Result<()> {
    let (input, receiver) = mpsc::unbounded_channel();
    let (events, _) = broadcast::channel(256);
    let shutdown = Arc::new(Notify::new());
    let shutdown_runner = shutdown.clone();
    // Keep a served session controllable from the terminal as well as the browser.
    let console_input = input.clone();
    if std::io::stdin().is_terminal() {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let command = if matches!(line, "/exit" | "/quit") {
                    UserInput::Exit
                } else if line == "/help" {
                    eprintln!("Commands: /help, /quit, /exit");
                    continue;
                } else {
                    UserInput::Message(line.to_owned())
                };
                if console_input.send(command).is_err() {
                    break;
                }
                if matches!(line, "/exit" | "/quit") {
                    break;
                }
            }
        });
    }
    let state = AppState {
        input,
        events: events.clone(),
        session_dir: Arc::new(config.session_dir.clone()),
        status: Arc::new(Mutex::new("waiting")),
    };
    let runner_state = state.clone();
    tokio::spawn(async move {
        let mut receiver = receiver;
        let Some(first_input) = receiver.recv().await else {
            shutdown_runner.notify_one();
            return;
        };
        let UserInput::Message(prompt) = first_input else {
            *runner_state.status.lock().await = "finished";
            let _ = runner_state
                .events
                .send(json!({"event":"session_ended", "data":{"success":true}}));
            shutdown_runner.notify_one();
            return;
        };
        *runner_state.status.lock().await = "running";
        let _ = runner_state
            .events
            .send(json!({"event":"session_started", "data":{"prompt":prompt}}));
        let mut config = config;
        config.prompt = prompt.clone();
        let outcome =
            run_interactive_with_events(config, backend, receiver, runner_state.events.clone())
                .await;
        let completed = outcome.as_ref().is_ok_and(|outcome| outcome.completed);
        *runner_state.status.lock().await = if completed { "finished" } else { "failed" };
        let _ = runner_state
            .events
            .send(json!({"event":"session_ended", "data":{"success":completed}}));
        shutdown_runner.notify_one();
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(health))
        .route("/api/v1/session", get(session))
        .route("/api/v1/messages", post(message))
        .route("/api/v1/events", get(sse_events))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}
async fn health() -> &'static str {
    "ok\n"
}
async fn session(State(state): State<AppState>) -> Json<Session> {
    Json(Session {
        state: *state.status.lock().await,
    })
}
async fn message(
    State(state): State<AppState>,
    Json(request): Json<MessageRequest>,
) -> impl IntoResponse {
    let message = request.message.trim();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"message must not be empty"})),
        )
            .into_response();
    }
    // Keep the lifecycle lock through the enqueue.  Otherwise the runner can
    // observe the session ending after this check but before the message is
    // submitted, producing a successful response for input that nobody reads.
    let status = state.status.lock().await;
    if *status == "finished" || *status == "failed" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"session has ended"})),
        )
            .into_response();
    }
    if state
        .input
        .send(UserInput::Message(message.to_owned()))
        .is_err()
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"session is unavailable"})),
        )
            .into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({"accepted":true}))).into_response()
}
async fn sse_events(
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    // Subscribe before reading the trace. Events emitted while the history is being
    // replayed remain buffered by the broadcast receiver instead of being lost in
    // the snapshot/live handoff.
    let live_receiver = state.events.subscribe();
    let history = load_trace(&state.session_dir).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to load session trace: {error:#}")})),
        )
    })?;
    let highest_replayed_sequence = history
        .iter()
        .filter_map(|event| event["seq"].as_u64())
        .max()
        .unwrap_or_default();
    let history = history.into_iter().map(Ok).chain(std::iter::once(Ok(json!({
        "event": "history_complete",
        "data": {}
    }))));
    let history = tokio_stream::iter(history);
    let live = BroadcastStream::new(live_receiver).filter_map(move |event| match event {
        Ok(value) if should_emit_live_event(&value, highest_replayed_sequence) => Some(Ok(value)),
        Ok(_) | Err(_) => None,
    });
    let stream = history.chain(live).map(|event: Result<Value, Infallible>| {
        event.map(|value| Event::default().json_data(value).expect("event is JSON"))
    });
    Ok(Sse::new(stream))
}

fn should_emit_live_event(event: &Value, highest_replayed_sequence: u64) -> bool {
    event
        .get("seq")
        .and_then(Value::as_u64)
        .is_none_or(|sequence| sequence > highest_replayed_sequence)
}

fn load_trace(session_dir: &Path) -> Result<Vec<Value>> {
    let path = session_dir.join("trace.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read session trace: {}", path.display()))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<serde_json::Result<Vec<_>>>()
        .with_context(|| format!("invalid JSON in session trace: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_handoff_skips_replayed_events_but_keeps_ephemeral_progress() {
        assert!(!should_emit_live_event(&json!({"seq": 7}), 7));
        assert!(should_emit_live_event(&json!({"seq": 8}), 7));
        assert!(should_emit_live_event(
            &json!({"event": "model_progress"}),
            7
        ));
    }

    #[tokio::test]
    async fn message_endpoint_enqueues_nonempty_input() {
        let (input, mut receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(1);
        let state = AppState {
            input,
            events,
            session_dir: Arc::new(std::path::PathBuf::from("unused")),
            status: Arc::new(Mutex::new("waiting")),
        };

        let response = message(
            State(state),
            Json(MessageRequest {
                message: "  steer right  ".to_owned(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            matches!(receiver.recv().await, Some(UserInput::Message(message)) if message == "steer right")
        );
    }

    #[test]
    fn trace_loader_replays_existing_session_events() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("trace.jsonl"),
            "{\"event\":\"human_message\"}\n",
        )
        .unwrap();
        assert_eq!(
            load_trace(temp.path()).unwrap(),
            vec![json!({"event":"human_message"})]
        );
    }
}
