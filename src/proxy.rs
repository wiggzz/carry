use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Result, bail};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Off,
    Audit,
    Compact,
}

#[derive(Debug, Parser)]
#[command(name = "carry proxy", about = "Opaque native Responses HTTP proxy")]
pub struct ProxyCli {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Full upstream Responses endpoint, not an API base URL.
    #[arg(long, default_value = "https://api.openai.com/v1/responses")]
    upstream_url: String,
    #[arg(long, default_value = ".carry-proxy")]
    state_dir: PathBuf,
    #[arg(long, value_enum, default_value = "off")]
    mode: Mode,
}

struct Service {
    config: ProxyCli,
    client: reqwest::Client,
    upstream_key: Option<String>,
    auth_token: Option<String>,
}

type Failure = (StatusCode, axum::Json<Value>);

fn failure(status: StatusCode, message: &str) -> Failure {
    (status, axum::Json(json!({"error": {"message": message}})))
}

pub async fn serve(config: ProxyCli) -> Result<()> {
    let url = url::Url::parse(&config.upstream_url)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("upstream URL must be an HTTP endpoint without credentials or query");
    }
    if config.mode != Mode::Off {
        bail!("review modes are not implemented");
    }
    let auth_token = std::env::var("CARRY_PROXY_AUTH_TOKEN").ok();
    if !config.listen.ip().is_loopback() && auth_token.is_none() {
        bail!("non-loopback proxy requires CARRY_PROXY_AUTH_TOKEN");
    }
    std::fs::create_dir_all(&config.state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config.state_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let service = Arc::new(Service {
        upstream_key: std::env::var("CARRY_PROXY_UPSTREAM_KEY")
            .ok()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok()),
        auth_token,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(15))
            .build()?,
        config,
    });
    let listener = tokio::net::TcpListener::bind(service.config.listen).await?;
    let router = Router::new()
        .route(
            "/health",
            get(|| async { axum::Json(json!({"status": "ok"})) }),
        )
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/carry/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(service);
    axum::serve(listener, router).await?;
    Ok(())
}

fn authorize(service: &Service, headers: &HeaderMap) -> Result<(), Failure> {
    if let Some(token) = &service.auth_token
        && headers.get("authorization").and_then(|h| h.to_str().ok())
            != Some(format!("Bearer {token}").as_str())
    {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "gateway authentication required",
        ));
    }
    Ok(())
}

async fn models(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
) -> Result<Response, Failure> {
    authorize(&service, &headers)?;
    let endpoint = service
        .config
        .upstream_url
        .strip_suffix("/responses")
        .ok_or_else(|| {
            failure(
                StatusCode::BAD_GATEWAY,
                "upstream models endpoint unavailable",
            )
        })?;
    let mut request = service.client.get(format!("{endpoint}/models"));
    if let Some(key) = &service.upstream_key {
        request = request.bearer_auth(key);
    }
    relay(
        request
            .send()
            .await
            .map_err(|_| failure(StatusCode::BAD_GATEWAY, "upstream transport failed"))?,
    )
    .await
}

async fn metrics(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
) -> Result<axum::Json<Value>, Failure> {
    authorize(&service, &headers)?;
    Ok(axum::Json(json!({"mode": "off"})))
}

async fn responses(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, Failure> {
    authorize(&service, &headers)?;
    let mut request = service
        .client
        .post(&service.config.upstream_url)
        .header("content-type", "application/json")
        .body(bytes);
    if let Some(key) = &service.upstream_key {
        request = request.bearer_auth(key);
    }
    for name in ["accept", "openai-beta", "x-request-id"] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    relay(
        request
            .send()
            .await
            .map_err(|_| failure(StatusCode::BAD_GATEWAY, "upstream transport failed"))?,
    )
    .await
}

async fn relay(mut upstream: reqwest::Response) -> Result<Response, Failure> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let (sender, receiver) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        loop {
            match upstream.chunk().await {
                Ok(Some(chunk)) => {
                    if sender.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let _ = sender
                        .send(Err(std::io::Error::other("upstream stream failed")))
                        .await;
                    break;
                }
            }
        }
    });
    let mut response = (status, Body::from_stream(ReceiverStream::new(receiver))).into_response();
    for (name, value) in &headers {
        if !matches!(
            name.as_str(),
            "connection" | "transfer-encoding" | "keep-alive" | "content-length"
        ) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    Ok(response)
}
