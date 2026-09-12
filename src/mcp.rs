use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo, JsonObject, Tool},
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

const CONFIG_FILE: &str = "mcp.json";

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
    /// List all tools exposed by configured servers.
    List,
    /// Show a tool's description and input schema.
    Describe { tool: String },
    /// Invoke a tool with a JSON object containing its arguments.
    Call {
        tool: String,
        #[arg(default_value = "{}")]
        arguments: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Config {
    #[serde(default)]
    servers: BTreeMap<String, Server>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "transport")]
enum Server {
    Http { url: String },
    Stdio { command: String, args: Vec<String> },
}

pub(crate) async fn run(cli: McpCli, carry_home: &Path) -> Result<()> {
    match cli.command {
        McpCommand::Add { name, url, command } => add(carry_home, name, url, command),
        McpCommand::List => list(carry_home).await,
        McpCommand::Describe { tool } => inspect(carry_home, &tool, None).await,
        McpCommand::Call { tool, arguments } => {
            let arguments: Value = serde_json::from_str(&arguments)
                .with_context(|| "MCP tool arguments must be valid JSON")?;
            let arguments = arguments
                .as_object()
                .cloned()
                .context("MCP tool arguments must be a JSON object")?;
            inspect(carry_home, &tool, Some(arguments)).await
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
    config.servers.insert(name.clone(), server);
    save(carry_home, &config)?;
    println!("added MCP server {name}");
    Ok(())
}

async fn list(carry_home: &Path) -> Result<()> {
    let config = load(carry_home)?;
    let mut output = Vec::new();
    for (server_name, server) in &config.servers {
        let tools = server_tools(server_name, server).await?;
        for tool in tools {
            output.push(json!({
                "name": format!("{server_name}/{}", tool.name),
                "server": server_name,
                "tool": tool.name,
                "description": tool.description,
            }));
        }
    }
    print_json(&output)
}

async fn inspect(carry_home: &Path, reference: &str, arguments: Option<JsonObject>) -> Result<()> {
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
        let tools = server_tools(server_name, server).await?;
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
        call_tool(server_name, server, wanted_tool, arguments).await?
    } else {
        let tool = server_tools(server_name, server)
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
    print_json(&result)
}

fn split_reference(reference: &str) -> (Option<&str>, &str) {
    reference
        .split_once('/')
        .map_or((None, reference), |(server, tool)| (Some(server), tool))
}

async fn server_tools(name: &str, server: &Server) -> Result<Vec<Tool>> {
    match server {
        Server::Http { url } => {
            let transport = StreamableHttpClientTransport::from_uri(url.as_str());
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .with_context(|| format!("failed to connect to MCP server {name}"))?;
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
    name: &str,
    server: &Server,
    tool: &str,
    arguments: JsonObject,
) -> Result<Value> {
    match server {
        Server::Http { url } => {
            let transport = StreamableHttpClientTransport::from_uri(url.as_str());
            let client = ClientInfo::default()
                .serve(transport)
                .await
                .with_context(|| format!("failed to connect to MCP server {name}"))?;
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

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace) {
        bail!("MCP server name must be non-empty and contain no whitespace or `/`");
    }
    Ok(())
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

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
}
