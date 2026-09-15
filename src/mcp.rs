use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo, JsonObject, Tool},
    transport::{
        AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, CredentialStore,
        StoredCredentials, StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::Command,
};

const CONFIG_FILE: &str = "mcp.json";
const AUTH_DIR: &str = "mcp-auth";

#[derive(Debug, Parser)]
#[command(
    name = "carry mcp",
    bin_name = "carry mcp",
    about = "Manage and invoke MCP tools"
)]
pub(crate) struct McpCli {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// Add or replace an MCP server.
    Add {
        /// Stable name used to qualify the server's tools.
        name: String,
        /// Streamable HTTP MCP endpoint.
        #[arg(long, conflicts_with = "command")]
        url: Option<String>,
        /// Stdio server command and arguments (place after `--`).
        #[arg(last = true, required_unless_present = "url")]
        command: Vec<String>,
    },
    /// Authenticate with an HTTP MCP server using OAuth.
    Auth { server: String },
    /// List tools exposed by configured servers.
    List {
        /// List tools from only this server.
        #[arg(long)]
        server: Option<String>,
    },
    /// Show a tool's description and input schema.
    Describe { tool: String },
    /// Invoke a tool with a JSON object containing its arguments.
    Call {
        tool: String,
        /// JSON object containing the tool arguments.
        arguments: Option<String>,
        /// Read the tool arguments as JSON from stdin.
        #[arg(long, conflicts_with = "arguments")]
        stdin: bool,
        /// Print only the value matching this RFC 6901 JSON Pointer.
        #[arg(long)]
        json_pointer: Option<String>,
        /// Preserve JSON encoding when the selected value is a string.
        #[arg(long, requires = "json_pointer")]
        json: bool,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Config {
    #[serde(default)]
    servers: BTreeMap<String, Server>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "transport")]
enum Server {
    Http { url: String },
    Stdio { command: String, args: Vec<String> },
}

pub(crate) async fn run(cli: McpCli, carry_home: &Path) -> Result<()> {
    match cli.command {
        McpCommand::Add { name, url, command } => add(carry_home, name, url, command),
        McpCommand::Auth { server } => authorize(carry_home, &server).await,
        McpCommand::List { server } => list(carry_home, server.as_deref()).await,
        McpCommand::Describe { tool } => inspect(carry_home, &tool, None, None, false).await,
        McpCommand::Call {
            tool,
            arguments,
            stdin,
            json_pointer,
            json,
        } => {
            let arguments = parse_arguments(arguments, stdin, std::io::stdin().lock())?;
            inspect(
                carry_home,
                &tool,
                Some(arguments),
                json_pointer.as_deref(),
                json,
            )
            .await
        }
    }
}

fn add(carry_home: &Path, name: String, url: Option<String>, command: Vec<String>) -> Result<()> {
    validate_name(&name)?;
    let server = if let Some(url) = url {
        let parsed = url::Url::parse(&url).context("MCP server URL must be valid")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("MCP server URL must use http or https");
        }
        Server::Http { url }
    } else {
        let (program, args) = command
            .split_first()
            .context("provide an MCP server command after `--`")?;
        Server::Stdio {
            command: program.clone(),
            args: args.to_vec(),
        }
    };
    let mut config = load(carry_home)?;
    let changed = config.servers.get(&name).is_some_and(|old| old != &server);
    config.servers.insert(name.clone(), server);
    save(carry_home, &config)?;
    if changed {
        clear_credentials(carry_home, &name)?;
    }
    println!("added MCP server {name}");
    Ok(())
}

async fn list(carry_home: &Path, wanted_server: Option<&str>) -> Result<()> {
    let config = load(carry_home)?;
    if let Some(name) = wanted_server
        && !config.servers.contains_key(name)
    {
        bail!("MCP server not found: {name}");
    }
    let mut output = Vec::new();
    for (server_name, server) in &config.servers {
        if wanted_server.is_some_and(|wanted| wanted != server_name) {
            continue;
        }
        let tools = server_tools(carry_home, server_name, server).await?;
        for tool in tools {
            output.push(format!("{server_name}/{}", tool.name));
        }
    }
    print_json(&output)
}

async fn inspect(
    carry_home: &Path,
    reference: &str,
    arguments: Option<JsonObject>,
    json_pointer: Option<&str>,
    json: bool,
) -> Result<()> {
    let config = load(carry_home)?;
    if config.servers.is_empty() {
        bail!("no MCP servers configured; add one with `carry mcp add`");
    }
    let (wanted_server, wanted_tool) = split_reference(reference);
    let mut found = Vec::new();

    for (server_name, server) in &config.servers {
        if wanted_server.is_some_and(|wanted| wanted != server_name) {
            continue;
        }
        let tools = server_tools(carry_home, server_name, server).await?;
        if tools.iter().any(|tool| tool.name == wanted_tool) {
            found.push((server_name, server));
        }
    }
    if found.is_empty() {
        bail!("MCP tool not found: {reference}");
    }
    if found.len() > 1 {
        let names = found
            .iter()
            .map(|(server, _)| format!("{server}/{wanted_tool}"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!("MCP tool name is ambiguous; use one of: {names}");
    }
    let (server_name, server) = found[0];
    let result = if let Some(arguments) = arguments {
        call_tool(carry_home, server_name, server, wanted_tool, arguments).await?
    } else {
        let tool = server_tools(carry_home, server_name, server)
            .await?
            .into_iter()
            .find(|tool| tool.name == wanted_tool)
            .expect("tool was found above");
        json!({
            "name": format!("{server_name}/{}", tool.name),
            "server": server_name,
            "tool": tool,
        })
    };
    let selected = select_output(result, json_pointer)?;
    print_output(&selected, json_pointer.is_some(), json)
}

fn parse_arguments(
    arguments: Option<String>,
    stdin: bool,
    mut reader: impl Read,
) -> Result<JsonObject> {
    let input = if stdin {
        let mut input = String::new();
        reader
            .read_to_string(&mut input)
            .context("failed to read MCP tool arguments from stdin")?;
        input
    } else {
        arguments.unwrap_or_else(|| "{}".into())
    };
    let arguments: Value =
        serde_json::from_str(&input).context("MCP tool arguments must be valid JSON")?;
    arguments
        .as_object()
        .cloned()
        .context("MCP tool arguments must be a JSON object")
}

fn select_output(output: Value, json_pointer: Option<&str>) -> Result<Value> {
    match json_pointer {
        Some(pointer) => output
            .pointer(pointer)
            .cloned()
            .with_context(|| format!("JSON pointer did not match MCP output: {pointer}")),
        None => Ok(output),
    }
}

fn split_reference(reference: &str) -> (Option<&str>, &str) {
    reference
        .split_once('/')
        .map_or((None, reference), |(server, tool)| (Some(server), tool))
}

async fn server_tools(carry_home: &Path, name: &str, server: &Server) -> Result<Vec<Tool>> {
    match server {
        Server::Http { url } => {
            let transport = http_transport(carry_home, name, url).await?;
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .map_err(|error| {
                    if error.is_authorization_required() {
                        anyhow::anyhow!(
                            "MCP server {name} requires authorization; run `carry mcp auth {name}`"
                        )
                    } else {
                        anyhow::anyhow!(error)
                            .context(format!("failed to connect to MCP server {name}"))
                    }
                })?;
            let tools = client
                .list_all_tools()
                .await
                .with_context(|| format!("failed to list tools from MCP server {name}"))?;
            client
                .cancel()
                .await
                .context("failed to close MCP connection")?;
            Ok(tools)
        }
        Server::Stdio { command, args } => {
            let mut process = Command::new(command);
            process.args(args);
            let transport = TokioChildProcess::new(process)
                .with_context(|| format!("failed to start MCP server {name}"))?;
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .with_context(|| format!("failed to initialize MCP server {name}"))?;
            let tools = client
                .list_all_tools()
                .await
                .with_context(|| format!("failed to list tools from MCP server {name}"))?;
            client
                .cancel()
                .await
                .context("failed to close MCP connection")?;
            Ok(tools)
        }
    }
}

async fn call_tool(
    carry_home: &Path,
    name: &str,
    server: &Server,
    tool: &str,
    arguments: JsonObject,
) -> Result<Value> {
    match server {
        Server::Http { url } => {
            let transport = http_transport(carry_home, name, url).await?;
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .map_err(|error| {
                    if error.is_authorization_required() {
                        anyhow::anyhow!(
                            "MCP server {name} requires authorization; run `carry mcp auth {name}`"
                        )
                    } else {
                        anyhow::anyhow!(error)
                            .context(format!("failed to connect to MCP server {name}"))
                    }
                })?;
            let result = client
                .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
                .await
                .with_context(|| format!("MCP tool call failed: {name}/{tool}"))?;
            client
                .cancel()
                .await
                .context("failed to close MCP connection")?;
            serde_json::to_value(result).context("failed to serialize MCP result")
        }
        Server::Stdio { command, args } => {
            let mut process = Command::new(command);
            process.args(args);
            let transport = TokioChildProcess::new(process)
                .with_context(|| format!("failed to start MCP server {name}"))?;
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .with_context(|| format!("failed to initialize MCP server {name}"))?;
            let result = client
                .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
                .await
                .with_context(|| format!("MCP tool call failed: {name}/{tool}"))?;
            client
                .cancel()
                .await
                .context("failed to close MCP connection")?;
            serde_json::to_value(result).context("failed to serialize MCP result")
        }
    }
}

#[derive(Clone, Debug)]
struct FileCredentialStore {
    path: PathBuf,
}

#[async_trait::async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| AuthError::CredentialStoreError(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(AuthError::CredentialStoreError(error.to_string())),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        save_private_json(&self.path, &credentials)
            .map_err(|error| AuthError::CredentialStoreError(error.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AuthError::CredentialStoreError(error.to_string())),
        }
    }
}

fn credential_store(carry_home: &Path, name: &str) -> FileCredentialStore {
    FileCredentialStore {
        path: carry_home.join(AUTH_DIR).join(format!("{name}.json")),
    }
}

fn clear_credentials(carry_home: &Path, name: &str) -> Result<()> {
    let path = credential_store(carry_home, name).path;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to clear MCP credentials: {}", path.display())),
    }
}

async fn authorization_manager(
    carry_home: &Path,
    name: &str,
    url: &str,
) -> Result<AuthorizationManager> {
    let mut manager = AuthorizationManager::new(url)
        .await
        .context("failed to initialize MCP OAuth")?;
    manager.set_credential_store(credential_store(carry_home, name));
    manager
        .initialize_from_store()
        .await
        .context("failed to load MCP OAuth credentials")?;
    Ok(manager)
}

fn install_mcp_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn http_transport(
    carry_home: &Path,
    name: &str,
    url: &str,
) -> Result<StreamableHttpClientTransport<AuthClient<reqwest_mcp::Client>>> {
    install_mcp_crypto_provider();
    let manager = authorization_manager(carry_home, name, url).await?;
    let client = AuthClient::new(reqwest_mcp::Client::new(), manager);
    Ok(StreamableHttpClientTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(url),
    ))
}

async fn authorize(carry_home: &Path, name: &str) -> Result<()> {
    let config = load(carry_home)?;
    let server = config
        .servers
        .get(name)
        .with_context(|| format!("MCP server not found: {name}"))?;
    let Server::Http { url } = server else {
        bail!("stdio MCP servers do not use HTTP OAuth");
    };

    let challenge = authorization_challenge(url, name).await?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind OAuth callback listener")?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr()?.port()
    );
    let manager = authorization_manager(carry_home, name, url).await?;
    let mut oauth = rmcp::transport::auth::OAuthState::Unauthorized(manager);
    oauth
        .start_authorization(
            AuthorizationRequest::new(&redirect_uri)
                .with_client_name("Carry")
                .with_challenge(challenge),
        )
        .await
        .context("failed to start MCP OAuth authorization")?;
    let auth_url = oauth
        .get_authorization_url()
        .await
        .context("failed to build MCP OAuth authorization URL")?;

    eprintln!(
        "Open this URL to authorize {name}:
{auth_url}
"
    );
    open_browser(&auth_url);
    let callback = receive_oauth_callback(listener).await?;
    oauth
        .handle_callback_url(&callback)
        .await
        .context("MCP OAuth callback failed")?;
    println!("authorized MCP server {name}");
    Ok(())
}

async fn authorization_challenge(url: &str, name: &str) -> Result<String> {
    install_mcp_crypto_provider();
    let transport = StreamableHttpClientTransport::from_uri(url);
    match ClientInfo::default().serve(transport).await {
        Ok(client) => {
            client.cancel().await.ok();
            bail!("MCP server {name} does not require authorization");
        }
        Err(error) => error.auth_challenge().map(str::to_owned).with_context(|| {
            format!("failed to get an OAuth challenge from MCP server {name}: {error}")
        }),
    }
}

async fn receive_oauth_callback(listener: TcpListener) -> Result<String> {
    let port = listener.local_addr()?.port();
    let (mut stream, _) = listener
        .accept()
        .await
        .context("failed to accept OAuth callback")?;
    let mut request_line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut request_line)
        .await
        .context("failed to read OAuth callback")?;
    let mut request = request_line.split_whitespace();
    let method = request.next().context("invalid OAuth callback request")?;
    let target = request.next().context("invalid OAuth callback request")?;
    if method != "GET" || !target.starts_with("/callback?") {
        bail!("invalid OAuth callback request");
    }
    let callback = format!("http://127.0.0.1:{port}{target}");
    let body = "Authorization complete. You can close this window.";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("failed to respond to OAuth callback")?;
    Ok(callback)
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let command = ("", Vec::<&str>::new());

    if !command.0.is_empty() {
        let _ = std::process::Command::new(command.0)
            .args(command.1)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn save_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("credential path has no parent")?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let temporary = parent.join(format!(".credentials.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace) {
        bail!("MCP server name must be non-empty and contain no whitespace or `/`");
    }
    Ok(())
}

pub(crate) fn configured_server_names(carry_home: &Path) -> Result<Vec<String>> {
    Ok(load(carry_home)?.servers.into_keys().collect())
}

fn load(carry_home: &Path) -> Result<Config> {
    let path = carry_home.join(CONFIG_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse MCP configuration: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read MCP configuration: {}", path.display())),
    }
}

fn save(carry_home: &Path, config: &Config) -> Result<()> {
    std::fs::create_dir_all(carry_home)
        .with_context(|| format!("failed to create Carry home: {}", carry_home.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(carry_home, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect Carry home: {}", carry_home.display()))?;
    }
    let path = carry_home.join(CONFIG_FILE);
    let temporary = carry_home.join(format!(".{CONFIG_FILE}.tmp-{}", std::process::id()));
    let bytes =
        serde_json::to_vec_pretty(config).context("failed to serialize MCP configuration")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).with_context(|| {
        format!(
            "failed to create MCP configuration: {}",
            temporary.display()
        )
    })?;
    file.write_all(&bytes)
        .context("failed to write MCP configuration")?;
    file.sync_all()
        .context("failed to sync MCP configuration")?;
    std::fs::rename(&temporary, &path)
        .with_context(|| format!("failed to publish MCP configuration: {}", path.display()))?;
    Ok(())
}

fn format_output(value: &Value, selected: bool, json: bool) -> Result<String> {
    if selected
        && !json
        && let Some(text) = value.as_str()
    {
        return Ok(text.to_owned());
    }
    serde_json::to_string_pretty(value).context("failed to serialize MCP output")
}

fn print_output(value: &Value, selected: bool, json: bool) -> Result<()> {
    println!("{}", format_output(value, selected, json)?);
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_call_arguments_from_stdin() {
        let arguments = parse_arguments(None, true, br#"{"query":"roadmap"}"#.as_slice()).unwrap();
        assert_eq!(arguments["query"], "roadmap");
    }

    #[test]
    fn parses_server_scoped_list() {
        let cli = McpCli::try_parse_from(["carry", "list", "--server", "notion"]).unwrap();
        assert!(matches!(
            cli.command,
            McpCommand::List { server: Some(ref server) } if server == "notion"
        ));
    }

    #[test]
    fn pointer_selected_strings_are_raw_unless_json_is_requested() {
        assert_eq!(
            format_output(&json!("done\n"), true, false).unwrap(),
            "done\n"
        );
        assert_eq!(
            format_output(&json!("done\n"), true, true).unwrap(),
            "\"done\\n\""
        );
        assert_eq!(
            format_output(&json!({"done": true}), true, false).unwrap(),
            "{\n  \"done\": true\n}"
        );
    }

    #[test]
    fn reads_configured_server_names_without_connecting() {
        let home = tempdir().unwrap();
        let config = Config {
            servers: BTreeMap::from([
                (
                    "notion".into(),
                    Server::Http {
                        url: "https://example.test".into(),
                    },
                ),
                (
                    "github".into(),
                    Server::Stdio {
                        command: "server".into(),
                        args: vec![],
                    },
                ),
            ]),
        };
        save(home.path(), &config).unwrap();
        assert_eq!(
            configured_server_names(home.path()).unwrap(),
            vec!["github", "notion"]
        );
    }

    #[test]
    fn selects_call_output_with_json_pointer() {
        let output = json!({"content": [{"text": "done"}]});
        assert_eq!(
            select_output(output, Some("/content/0/text")).unwrap(),
            json!("done")
        );
    }

    #[test]
    fn rejects_missing_json_pointer() {
        let error = select_output(json!({"content": []}), Some("/content/0/text")).unwrap_err();
        assert!(error.to_string().contains("JSON pointer did not match"));
    }

    #[test]
    fn stores_http_and_stdio_servers() {
        let home = tempdir().unwrap();
        add(
            home.path(),
            "remote".into(),
            Some("https://example.test/mcp".into()),
            vec![],
        )
        .unwrap();
        add(
            home.path(),
            "local".into(),
            None,
            vec!["server".into(), "--flag".into()],
        )
        .unwrap();
        let config = load(home.path()).unwrap();
        assert!(matches!(config.servers["remote"], Server::Http { .. }));
        assert!(matches!(config.servers["local"], Server::Stdio { .. }));
    }

    #[test]
    fn rejects_non_http_server_urls() {
        let home = tempdir().unwrap();
        let error = add(
            home.path(),
            "remote".into(),
            Some("file:///tmp/mcp".into()),
            vec![],
        )
        .unwrap_err();
        assert!(error.to_string().contains("must use http or https"));
    }

    #[test]
    fn splits_qualified_and_bare_tool_names() {
        assert_eq!(
            split_reference("github/create_issue"),
            (Some("github"), "create_issue")
        );
        assert_eq!(split_reference("create_issue"), (None, "create_issue"));
    }

    #[tokio::test]
    async fn receives_oauth_callback_url_and_responds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let callback = tokio::spawn(receive_oauth_callback(listener));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        use tokio::io::AsyncReadExt;
        stream
            .write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert_eq!(
            callback.await.unwrap().unwrap(),
            format!(
                "http://127.0.0.1:{}/callback?code=abc&state=xyz",
                address.port()
            )
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn replacing_server_clears_credentials() {
        let home = tempdir().unwrap();
        add(
            home.path(),
            "remote".into(),
            Some("https://one.example/mcp".into()),
            vec![],
        )
        .unwrap();
        let credentials = credential_store(home.path(), "remote").path;
        save_private_json(
            &credentials,
            &StoredCredentials::new("client".into(), None, vec![], None),
        )
        .unwrap();

        add(
            home.path(),
            "remote".into(),
            Some("https://two.example/mcp".into()),
            vec![],
        )
        .unwrap();
        assert!(!credentials.exists());
    }
}
