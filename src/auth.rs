use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Instant, sleep},
};
use url::Url;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex";
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const DEVICE_LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexAuth {
    pub access_token: String,
    pub account_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCredential {
    version: u8,
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginMethod {
    Browser,
    DeviceCode,
}

pub(crate) fn carry_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("CARRY_HOME") {
        return Ok(PathBuf::from(home));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set; set CARRY_HOME")?)
            .join(".carry"),
    )
}

pub(crate) fn codex_responses_url() -> &'static str {
    CODEX_RESPONSES_URL
}

pub(crate) async fn load_auth(home: &Path) -> Result<Option<CodexAuth>> {
    let path = credential_path(home);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut credential: StoredCredential =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if credential.version != 1
        || credential.access_token.is_empty()
        || credential.refresh_token.is_empty()
    {
        bail!(
            "invalid Codex subscription credential in {}",
            path.display()
        );
    }
    if credential.expires_at_ms <= now_ms().saturating_add(REFRESH_MARGIN.as_millis() as u64) {
        credential = refresh(&credential).await?;
        save_credential(home, &credential).await?;
    }
    Ok(Some(CodexAuth {
        account_id: account_id(&credential.access_token)?,
        access_token: credential.access_token,
    }))
}

pub(crate) async fn login(home: &Path, method: LoginMethod) -> Result<()> {
    let credential = match method {
        LoginMethod::Browser => browser_login().await?,
        LoginMethod::DeviceCode => device_login().await?,
    };
    // Reject an incomplete or mismatched access token before persisting its refresh token.
    let account_id = account_id(&credential.access_token)?;
    save_credential(home, &credential).await?;
    eprintln!("signed in with ChatGPT account {account_id}");
    Ok(())
}

pub(crate) async fn logout(home: &Path) -> Result<bool> {
    let path = credential_path(home);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

async fn browser_login() -> Result<StoredCredential> {
    let verifier = random_urlsafe(32)?;
    let state = random_urlsafe(16)?;
    let auth_url = authorization_url(&verifier, &state)?;
    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .context("bind browser login callback on 127.0.0.1:1455")?;
    eprintln!("Open this URL to sign in with ChatGPT:\n{auth_url}");
    open_browser(&auth_url);
    let code = wait_for_browser_callback(&listener, &state).await?;
    exchange_authorization_code(&code, &verifier, BROWSER_REDIRECT_URI).await
}

async fn device_login() -> Result<StoredCredential> {
    let client = Client::new();
    let response = client
        .post(DEVICE_USER_CODE_URL)
        .json(&serde_json::json!({"client_id": CLIENT_ID}))
        .send()
        .await
        .context("request Codex device code")?;
    if response.status() == StatusCode::NOT_FOUND {
        bail!("Codex device login is unavailable; use browser login instead");
    }
    let response = response
        .error_for_status()
        .context("request Codex device code")?;
    let device: DeviceCodeResponse = response.json().await.context("parse Codex device code")?;
    if device.device_auth_id.is_empty() || device.user_code.is_empty() {
        bail!("Codex device-code response was incomplete");
    }
    eprintln!(
        "Open {DEVICE_VERIFICATION_URL} and enter this code within 15 minutes:\n{}",
        device.user_code
    );
    let device_token = poll_device_code(&client, &device).await?;
    exchange_authorization_code(
        &device_token.authorization_code,
        &device_token.code_verifier,
        DEVICE_REDIRECT_URI,
    )
    .await
}

fn authorization_url(verifier: &str, state: &str) -> Result<String> {
    let mut url = Url::parse(AUTHORIZE_URL).expect("constant authorization URL is valid");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("response_type", "code");
        pairs.append_pair("client_id", CLIENT_ID);
        pairs.append_pair("redirect_uri", BROWSER_REDIRECT_URI);
        pairs.append_pair("scope", "openid profile email offline_access");
        pairs.append_pair("code_challenge", &challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", state);
        pairs.append_pair("id_token_add_organizations", "true");
        pairs.append_pair("codex_cli_simplified_flow", "true");
        pairs.append_pair("originator", "carry");
    }
    Ok(url.into())
}

async fn wait_for_browser_callback(listener: &TcpListener, state: &str) -> Result<String> {
    tokio::time::timeout(
        Duration::from_secs(10 * 60),
        wait_for_browser_callback_until_timeout(listener, state),
    )
    .await
    .context("timed out waiting for the browser OAuth callback")?
}

async fn wait_for_browser_callback_until_timeout(
    listener: &TcpListener,
    state: &str,
) -> Result<String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("accept browser login callback")?;
        let mut request = vec![0_u8; 16 * 1024];
        let size = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut request))
            .await
            .context("timed out reading browser login callback")?
            .context("read browser login callback")?;
        let target = std::str::from_utf8(&request[..size])
            .ok()
            .and_then(|request| request.lines().next())
            .and_then(|line| line.split_whitespace().nth(1));
        let parsed =
            target.and_then(|target| Url::parse(&format!("http://localhost{target}")).ok());
        let valid = parsed.as_ref().is_some_and(|url| {
            url.path() == "/auth/callback"
                && url
                    .query_pairs()
                    .any(|(key, value)| key == "state" && value == state)
        });
        let code = parsed.as_ref().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "code")
                .map(|(_, value)| value.into_owned())
        });
        let (status, body) = if valid && code.as_ref().is_some_and(|code| !code.is_empty()) {
            ("200 OK", "Sign-in complete. You can close this window.")
        } else {
            ("400 Bad Request", "Invalid or incomplete sign-in callback.")
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .context("respond to browser callback")?;
        if valid {
            return code.context("browser callback lacked an authorization code");
        }
    }
}

async fn poll_device_code(
    client: &Client,
    device: &DeviceCodeResponse,
) -> Result<DeviceTokenResponse> {
    let deadline = Instant::now() + DEVICE_LOGIN_TIMEOUT;
    let interval = Duration::from_secs(device.interval.max(1));
    loop {
        let response = client
            .post(DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }))
            .send()
            .await
            .context("poll Codex device code")?;
        if response.status().is_success() {
            let token: DeviceTokenResponse =
                response.json().await.context("parse Codex device token")?;
            if token.authorization_code.is_empty() || token.code_verifier.is_empty() {
                bail!("Codex device-code authorization response was incomplete");
            }
            return Ok(token);
        }
        if response.status() != StatusCode::FORBIDDEN && response.status() != StatusCode::NOT_FOUND
        {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Codex device-code login failed with {status}: {body}");
        }
        if Instant::now() >= deadline {
            bail!("Codex device-code login timed out after 15 minutes");
        }
        sleep(interval.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<StoredCredential> {
    token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ])
    .await
}

async fn refresh(credential: &StoredCredential) -> Result<StoredCredential> {
    token_request(&[
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", &credential.refresh_token),
    ])
    .await
}

async fn token_request(form: &[(&str, &str)]) -> Result<StoredCredential> {
    let response = Client::new()
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .context("request Codex OAuth token")?
        .error_for_status()
        .context("request Codex OAuth token")?;
    let token: TokenResponse = response.json().await.context("parse Codex OAuth token")?;
    if token.access_token.is_empty() || token.refresh_token.is_empty() || token.expires_in == 0 {
        bail!("Codex OAuth token response was incomplete");
    }
    Ok(StoredCredential {
        version: 1,
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_ms: now_ms().saturating_add(token.expires_in.saturating_mul(1_000)),
    })
}

async fn save_credential(home: &Path, credential: &StoredCredential) -> Result<()> {
    tokio::fs::create_dir_all(home)
        .await
        .with_context(|| format!("create {}", home.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))
            .await
            .with_context(|| format!("protect {}", home.display()))?;
    }
    let path = credential_path(home);
    let temp = home.join(format!(".auth-{}.tmp", random_urlsafe(12)?));
    let body = serde_json::to_vec(credential).context("serialize Codex subscription credential")?;
    tokio::fs::write(&temp, body)
        .await
        .with_context(|| format!("write {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .await
            .with_context(|| format!("protect {}", temp.display()))?;
    }
    tokio::fs::rename(&temp, &path)
        .await
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn credential_path(home: &Path) -> PathBuf {
    home.join("auth.json")
}

fn account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .context("Codex access token is not a JWT")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode Codex access-token payload")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse Codex access-token payload")?;
    value["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .context("Codex access token has no ChatGPT account ID")
}

fn random_urlsafe(size: usize) -> Result<String> {
    let mut bytes = vec![0_u8; size];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("read secure random bytes: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn deserialize_interval<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Interval {
        Number(u64),
        Text(String),
    }
    match Interval::deserialize(deserializer)? {
        Interval::Number(value) => Ok(value),
        Interval::Text(value) => value.trim().parse().map_err(serde::de::Error::custom),
    }
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(target_os = "windows")]
    let command = "cmd";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let command = "xdg-open";

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new(command)
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let result = std::process::Command::new(command).arg(url).spawn();
    if let Err(error) = result {
        eprintln!("could not open a browser ({error}); paste the URL above into one");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_authorization_url_uses_pkce_and_state() {
        let url = Url::parse(&authorization_url("verifier", "expected-state").unwrap()).unwrap();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            pairs.get("client_id").map(|value| value.as_ref()),
            Some(CLIENT_ID)
        );
        assert_eq!(
            pairs.get("state").map(|value| value.as_ref()),
            Some("expected-state")
        );
        assert_eq!(
            pairs
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert!(
            pairs
                .get("code_challenge")
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[tokio::test]
    async fn stored_credential_loads_with_its_chatgpt_account() {
        let home = tempfile::tempdir().unwrap();
        let payload = URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-1"}}"#);
        let credential = StoredCredential {
            version: 1,
            access_token: format!("header.{payload}.signature"),
            refresh_token: "refresh-token".into(),
            expires_at_ms: now_ms() + 3_600_000,
        };
        save_credential(home.path(), &credential).await.unwrap();

        let auth = load_auth(home.path()).await.unwrap().unwrap();
        assert_eq!(auth.account_id, "account-1");
        assert_eq!(auth.access_token, credential.access_token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(credential_path(home.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn extracts_chatgpt_account_id_from_jwt_payload() {
        let payload = URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-1"}}"#);
        assert_eq!(
            account_id(&format!("header.{payload}.signature")).unwrap(),
            "account-1"
        );
    }
}
