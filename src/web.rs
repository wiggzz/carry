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
        // The resumed history is restored as one server-side item, so the fresh
        // browser message is not emitted by run_loop's initial-prompt path.
        let _ = runner_state.events.send(json!({
            "event": "human_message",
            "data": {"message": prompt}
        }));
        let mut config = config;
        config.prompt = prompt.clone();
        if let Some(history) = config.resume_history.as_mut() {
            history.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}]
            }));
        }
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
    let history = history.into_iter().map(Ok).chain(std::iter::once(Ok(json!({
        "event": "history_complete",
        "data": {}
    }))));
    let history = tokio_stream::iter(history);
    let live = BroadcastStream::new(live_receiver).filter_map(|event| match event {
        Ok(value) => Some(Ok(value)),
        Err(_) => None,
    });
    let stream = history.chain(live).map(|event: Result<Value, Infallible>| {
        event.map(|value| Event::default().json_data(value).expect("event is JSON"))
    });
    Ok(Sse::new(stream))
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
    fn ui_flows_top_to_bottom_with_a_bottom_composer_and_inline_context_pills() {
        assert!(INDEX.contains("EventSource"));
        assert!(INDEX.contains("position:sticky;bottom:0"));
        assert!(INDEX.contains("event.key === 'Enter'"));
        assert!(INDEX.contains("event.shiftKey"));
        assert!(INDEX.contains("renderMarkdown"));
        assert!(INDEX.contains("shell-command"));
        assert!(INDEX.contains("shell-output"));
        assert!(INDEX.contains("context-pill"));
        assert!(INDEX.contains("Session statistics"));
        assert!(INDEX.contains("stat-tokens"));
        assert!(INDEX.contains("stat-cached"));
        assert!(INDEX.contains("model_response"));
        assert!(INDEX.contains("context_compacted"));
        assert!(INDEX.contains("session_ended"));
        assert!(INDEX.contains("formatDuration"));
        assert!(INDEX.contains("model_progress"));
        assert!(INDEX.contains("Model streaming"));
        assert!(INDEX.contains("contextCompacted"));
        assert!(INDEX.contains("Context compacted"));
        assert!(INDEX.contains("followTail"));
        assert!(INDEX.contains("addEventListener('scroll'"));
        assert!(INDEX.contains("scheduleScrollToTail"));
        assert!(INDEX.contains("history_complete"));
        assert!(INDEX.contains("behavior:'auto'"));
        assert!(!INDEX.contains("Context ledger"));
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
