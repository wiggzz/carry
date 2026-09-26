mod auth;
mod context;
mod log;
mod mcp;
mod openai;
mod protocol;
mod run;
mod terminal;
mod web;

use std::{
    io::{IsTerminal, Read},
    path::{Component, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use context::DEFAULT_COMPACTION_MIN_PAYBACK_PERCENT;
use run::{Backend, CompactionMode, RunConfig, UserInput};
use tokio::sync::mpsc;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS: usize = 0;
const DEFAULT_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS: usize = 0;

const EXAMPLES: &str = r#"Examples:
  carry fix the failing tests
  carry -p "explain why --release is failing"
  carry --cwd ../project add tests for the parser
  carry --interactive -p "investigate the flaky test"
  carry
  carry --cwd ../project
  carry --print < task.txt
"#;

#[derive(Debug, Parser)]
#[command(
    name = "carry",
    about = "A tiny model-managed-context coding agent",
    after_help = EXAMPLES,
    trailing_var_arg = true
)]
struct Cli {
    /// Prompt for the agent. Useful when the prompt contains option-like text.
    #[arg(short, long, conflicts_with = "prompt_words")]
    prompt: Option<String>,

    /// Prompt words joined with spaces.
    #[arg(value_name = "PROMPT", num_args = 0.., allow_hyphen_values = true)]
    prompt_words: Vec<String>,

    /// Keep the session open for follow-up prompts and steering.
    #[arg(short, long, conflicts_with = "serve")]
    interactive: bool,

    /// Run once in the terminal, reading stdin when no prompt is supplied.
    #[arg(long, conflicts_with_all = ["serve", "interactive"])]
    print: bool,

    /// Continue this session from its saved conversation.
    #[arg(long, value_name = "SESSION_DIR")]
    resume: Option<PathBuf>,

    /// Launch a local browser UI and SSE API (default when no prompt is supplied).
    #[arg(long)]
    serve: bool,

    /// Do not automatically open the browser when serving.
    #[arg(long, conflicts_with_all = ["interactive", "print"])]
    no_open: bool,

    /// Local port used by the browser UI.
    #[arg(long, default_value_t = 8765, conflicts_with_all = ["interactive", "print"])]
    port: u16,

    /// Repository or working directory the shell tool may modify.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    /// Parent directory for generated session data.
    #[arg(long, conflicts_with = "session_dir")]
    session_home: Option<PathBuf>,

    /// Exact directory for this session's trace and artifacts.
    #[arg(long, conflicts_with = "session_home")]
    session_dir: Option<PathBuf>,

    /// OpenAI model name.
    #[arg(long, env = "OPENAI_MODEL", default_value = "gpt-6-sol")]
    model: String,

    /// OpenAI API base URL.
    #[arg(
        long,
        env = "OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    api_base: String,

    /// Reasoning effort sent to the Responses API.
    #[arg(long, value_enum, default_value = "medium")]
    reasoning_effort: ReasoningEffort,

    /// Stop after this many model steps per user turn.
    #[arg(long)]
    max_steps: Option<usize>,

    /// Default timeout for each shell command; the model may override per call (1-300 seconds).
    #[arg(long, alias = "shell-timeout-secs", default_value_t = 60)]
    default_shell_timeout_secs: u64,

    /// Deadline for each OpenAI Responses API attempt.
    #[arg(long, env = "OPENAI_REQUEST_TIMEOUT_SECS", default_value_t = DEFAULT_REQUEST_TIMEOUT_SECS)]
    request_timeout_secs: u64,

    /// Deadline for establishing an OpenAI API connection.
    #[arg(long, env = "OPENAI_CONNECT_TIMEOUT_SECS", default_value_t = DEFAULT_CONNECT_TIMEOUT_SECS)]
    connect_timeout_secs: u64,

    /// Select automatic context compaction behavior.
    #[arg(
        long,
        env = "CARRY_COMPACTION_POLICY",
        value_enum,
        default_value = "economic"
    )]
    compaction_policy: CompactionPolicyArg,

    /// Revalidate model-protected context after this many later model turns (batched reviews).
    #[arg(
        long,
        env = "CARRY_KEEP_LEASE_TURNS",
        default_value = "8",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    keep_lease_turns: Option<u64>,

    /// Number of future requests used to amortize a compaction rewrite; defaults to five.
    #[arg(
        long,
        env = "CARRY_COMPACTION_PAYOFF_REQUESTS",
        default_value_t = 5,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    compaction_payoff_requests: u64,

    /// Minimum projected saving (percent of retained-path payoff cost) required to compact.
    #[arg(
        long,
        env = "CARRY_COMPACTION_MIN_PAYBACK_PERCENT",
        default_value_t = DEFAULT_COMPACTION_MIN_PAYBACK_PERCENT,
        value_parser = clap::value_parser!(u8).range(0..=100)
    )]
    compaction_min_payback_percent: u8,

    /// Deterministic flat-drop rollout samples used to gate economic compaction; zero disables it.
    #[arg(
        long,
        env = "CARRY_COMPACTION_ROLLOUT_SAMPLES",
        default_value_t = 0,
        value_parser = clap::value_parser!(u32).range(0..=64)
    )]
    compaction_rollout_samples: u32,

    /// Per simulated future turn probability (percent) that the task ends; defaults to ten.
    #[arg(
        long,
        env = "CARRY_COMPACTION_ROLLOUT_STOP_PROBABILITY_PERCENT",
        default_value_t = 10,
        value_parser = clap::value_parser!(u8).range(0..=100)
    )]
    compaction_rollout_stop_probability_percent: u8,

    /// Eligible neutral context token high-water mark before automatic compaction.
    #[arg(
        long,
        env = "CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS",
        default_value_t = DEFAULT_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS
    )]
    compaction_neutral_high_watermark_tokens: usize,

    /// Eligible neutral context token target after automatic compaction; may be zero.
    #[arg(
        long,
        env = "CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS",
        default_value_t = DEFAULT_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS
    )]
    compaction_neutral_low_watermark_tokens: usize,

    /// JSONL Step objects to use instead of calling a model.
    #[arg(long, hide = true)]
    scripted_steps: Option<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(name = "carry login", about = "Sign in with a ChatGPT subscription")]
struct LoginCli {
    /// Use a device code instead of a localhost browser callback.
    #[arg(long)]
    device_auth: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "carry logout",
    about = "Remove the stored ChatGPT subscription credential"
)]
struct LogoutCli {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CompactionPolicyArg {
    Economic,
    Disabled,
}

impl From<CompactionPolicyArg> for CompactionMode {
    fn from(value: CompactionPolicyArg) -> Self {
        match value {
            CompactionPolicyArg::Economic => Self::Economic,
            CompactionPolicyArg::Disabled => Self::Disabled,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ReasoningEffort {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut argv = std::env::args_os().collect::<Vec<_>>();
    match argv.get(1).and_then(|argument| argument.to_str()) {
        Some("login") => {
            argv.remove(1);
            let login = LoginCli::parse_from(argv);
            let method = if login.device_auth {
                auth::LoginMethod::DeviceCode
            } else {
                auth::LoginMethod::Browser
            };
            return auth::login(&auth::carry_home()?, method).await;
        }
        Some("mcp") => {
            argv.remove(1);
            return mcp::run(mcp::McpCli::parse_from(argv), &auth::carry_home()?).await;
        }
        Some("logout") => {
            argv.remove(1);
            LogoutCli::parse_from(argv);
            if auth::logout(&auth::carry_home()?).await? {
                terminal::output("signed out of ChatGPT subscription");
            } else {
                terminal::output("no ChatGPT subscription credential was stored");
            }
            return Ok(());
        }
        _ => run_command(Cli::parse()).await,
    }
}

fn validate_args(args: &Cli) -> Result<()> {
    if args.compaction_neutral_low_watermark_tokens > args.compaction_neutral_high_watermark_tokens
    {
        bail!("compaction neutral low watermark must not exceed the high watermark");
    }
    Ok(())
}

async fn run_command(args: Cli) -> Result<()> {
    validate_args(&args)?;
    let resume_source = args
        .resume
        .as_deref()
        .map(|reference| resolve_resume_session(reference, args.session_home.as_deref()))
        .transpose()?;
    let resume = resume_source
        .as_deref()
        .map(run::load_resume_state)
        .transpose()?;
    let model = resume
        .as_ref()
        .map_or_else(|| args.model.clone(), |resume| resume.model.clone());
    let prompt_cache_key = resume
        .as_ref()
        .and_then(|resume| resume.prompt_cache_key.clone())
        .unwrap_or_else(openai::new_prompt_cache_key);
    let cwd = args
        .cwd
        .canonicalize()
        .with_context(|| format!("working directory does not exist: {}", args.cwd.display()))?;
    if !cwd.is_dir() {
        bail!("working directory is not a directory: {}", cwd.display());
    }

    let stdin_is_terminal = std::io::stdin().is_terminal();
    if args.interactive && !stdin_is_terminal {
        bail!("--interactive requires a terminal on stdin");
    }
    let interactive = args.interactive;
    let serve = args.serve
        || (!interactive && !args.print && args.prompt.is_none() && args.prompt_words.is_empty());
    let mut input = None;
    let prompt = if serve {
        String::new()
    } else if let Some(prompt) = args.prompt {
        prompt
    } else if !args.prompt_words.is_empty() {
        args.prompt_words.join(" ")
    } else if !stdin_is_terminal {
        let mut prompt = String::new();
        std::io::stdin()
            .read_to_string(&mut prompt)
            .context("failed to read prompt from stdin")?;
        prompt
    } else {
        input = Some(spawn_input_reader());
        match input
            .as_mut()
            .expect("interactive input exists")
            .recv()
            .await
        {
            Some(UserInput::Message {
                message: prompt, ..
            }) => prompt,
            Some(UserInput::Exit) | None => return Ok(()),
        }
    };
    if !serve && prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    }

    if interactive && input.is_none() {
        input = Some(spawn_input_reader());
    }

    let session_dir = match (args.session_dir.clone(), resume_source.as_ref()) {
        (Some(session_dir), _) => session_dir,
        (None, Some(source)) => source.clone(),
        (None, None) => resolve_session_dir(None, args.session_home.clone())?,
    };
    let session_dir = if session_dir.exists() {
        session_dir.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize session directory: {}",
                session_dir.display()
            )
        })?
    } else {
        session_dir
    };
    if resume_source.as_ref() != Some(&session_dir) {
        create_private_session_dir(&session_dir)?;
    }
    if let Some(source) = &resume_source {
        terminal::output(&format!("resuming session: {}", source.display()));
        if source != &session_dir {
            terminal::output(&format!("new session: {}", session_dir.display()));
        }
    } else {
        terminal::output(&format!("session: {}", session_dir.display()));
    }

    if args.request_timeout_secs == 0 || args.connect_timeout_secs == 0 {
        bail!("OpenAI request and connect timeouts must be greater than zero");
    }

    let backend = match args.scripted_steps {
        Some(path) => Backend::scripted(&path).await?,
        None => {
            let api_key = match std::env::var("OPENAI_API_KEY") {
                Ok(api_key) if !api_key.trim().is_empty() => Some(api_key),
                Ok(_) | Err(std::env::VarError::NotPresent) => None,
                Err(error) => return Err(error).context("read OPENAI_API_KEY"),
            };
            let (api_base, request_auth) = match api_key {
                Some(api_key) => (args.api_base.clone(), openai::RequestAuth::ApiKey(api_key)),
                None => {
                    if args.api_base != "https://api.openai.com/v1" {
                        bail!(
                            "OPENAI_BASE_URL requires OPENAI_API_KEY; ChatGPT subscription credentials are sent only to the Codex endpoint"
                        );
                    }
                    let credential = auth::load_auth(&auth::carry_home()?)
                        .await?
                        .context("OPENAI_API_KEY is required, or run `carry login` to use a ChatGPT subscription")?;
                    (
                        auth::codex_responses_url().to_owned(),
                        openai::RequestAuth::CodexSubscription {
                            access_token: credential.access_token,
                            account_id: credential.account_id,
                            credential_home: Some(auth::carry_home()?),
                        },
                    )
                }
            };
            Backend::openai(openai::OpenAiClient::with_timeouts_and_prompt_cache_key(
                api_base,
                request_auth,
                model.clone(),
                args.reasoning_effort.as_str().to_owned(),
                prompt_cache_key.clone(),
                Duration::from_secs(args.request_timeout_secs),
                Duration::from_secs(args.connect_timeout_secs),
            )?)
        }
    };

    let config = RunConfig {
        cwd,
        prompt,
        session_dir: session_dir.clone(),
        model,
        max_steps: args.max_steps,
        default_shell_timeout_secs: args.default_shell_timeout_secs,
        compaction_mode: args.compaction_policy.into(),
        keep_lease_turns: args.keep_lease_turns,
        compaction_payoff_requests: args.compaction_payoff_requests,
        compaction_min_payback_percent: args.compaction_min_payback_percent,
        compaction_rollout_samples: args.compaction_rollout_samples,
        compaction_rollout_stop_probability_percent: args
            .compaction_rollout_stop_probability_percent,
        compaction_neutral_high_watermark_tokens: args.compaction_neutral_high_watermark_tokens,
        compaction_neutral_low_watermark_tokens: args.compaction_neutral_low_watermark_tokens,
        resume_context: resume.map(|resume| resume.context),
        resume_source,
        prompt_cache_key: Some(prompt_cache_key),
    };

    if serve {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
        tokio::select! {
            result = web::serve(address, config, backend, !args.no_open) => result?,
            _ = tokio::signal::ctrl_c() => {
                terminal::output("session interrupted by Ctrl-C");
                print_resume_hint(&session_dir);
                return Ok(());
            }
        }
        print_resume_hint(&session_dir);
        return Ok(());
    }

    let outcome = tokio::select! {
        result = async {
            if interactive {
                run::run_interactive(config, backend, input.expect("interactive input exists")).await
            } else {
                run::run(config, backend).await
            }
        } => result?,
        _ = tokio::signal::ctrl_c() => {
            terminal::output("session interrupted by Ctrl-C");
            print_resume_hint(&session_dir);
            return Ok(());
        }
    };
    print_resume_hint(&outcome.session_dir);
    if outcome.completed {
        if !interactive && !outcome.answer_streamed {
            terminal::print_answer(&outcome.answer.unwrap_or_default());
        }
        Ok(())
    } else {
        bail!(
            "session stopped without a finish action; see {}",
            outcome.session_dir.display()
        )
    }
}

fn print_resume_hint(session_dir: &std::path::Path) {
    terminal::output(&format!(
        "session: {}\nresume with: carry --resume {}",
        session_dir.display(),
        session_dir.display()
    ));
}

fn spawn_input_reader() -> mpsc::UnboundedReceiver<UserInput> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || terminal::read_input(sender));
    receiver
}

fn resolve_resume_session(
    reference: &std::path::Path,
    session_home: Option<&std::path::Path>,
) -> Result<PathBuf> {
    if reference == std::path::Path::new("..") {
        bail!(
            "resume session ID must be a single directory name: {}",
            reference.display()
        );
    }
    if reference.is_dir() {
        return reference.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize resume session: {}",
                reference.display()
            )
        });
    }
    let mut components = reference.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!(
            "resume session ID must be a single directory name: {}",
            reference.display()
        );
    }
    let home = if let Some(home) = session_home {
        home.to_path_buf()
    } else if let Some(home) = std::env::var_os("CARRY_HOME") {
        PathBuf::from(home)
    } else {
        PathBuf::from(
            std::env::var_os("HOME")
                .context("HOME is not set; use --session-home or a session path")?,
        )
        .join(".carry")
    };
    let sessions = home.join("sessions");
    let sessions_root = sessions.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize session home: {}",
            sessions.display()
        )
    })?;
    let session = sessions_root.join(reference);
    if !session.is_dir() {
        bail!("resume session does not exist: {}", session.display());
    }
    let session = session.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize resume session: {}",
            session.display()
        )
    })?;
    if !session.starts_with(&sessions_root) {
        bail!(
            "resume session escapes configured session home: {}",
            session.display()
        );
    }
    Ok(session)
}

fn resolve_session_dir(exact: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(exact) = exact {
        return Ok(exact);
    }
    let home = match home {
        Some(home) => home,
        None => match std::env::var_os("CARRY_HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .context("HOME is not set; use --session-home or --session-dir")?,
            )
            .join(".carry"),
        },
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(home
        .join("sessions")
        .join(format!("{now}-{}", std::process::id())))
}

fn create_private_session_dir(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create session directory: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn interactive_default_is_gpt6_sol_but_benchmark_model_can_be_explicit() {
        let defaulted = Cli::try_parse_from(["carry", "-p", "fix it"]).unwrap();
        assert_eq!(defaulted.model, "gpt-6-sol");
        let benchmark =
            Cli::try_parse_from(["carry", "--model", "gpt-6-luna", "-p", "fix it"]).unwrap();
        assert_eq!(benchmark.model, "gpt-6-luna");
    }

    #[test]
    fn default_shell_timeout_flag_is_primary_and_legacy_name_still_works() {
        let args = Cli::try_parse_from(["carry", "-p", "fix it"]).unwrap();
        assert_eq!(args.default_shell_timeout_secs, 60);
        let explicit = Cli::try_parse_from([
            "carry",
            "--default-shell-timeout-secs",
            "17",
            "-p",
            "fix it",
        ])
        .unwrap();
        assert_eq!(explicit.default_shell_timeout_secs, 17);
        let legacy =
            Cli::try_parse_from(["carry", "--shell-timeout-secs", "19", "-p", "fix it"]).unwrap();
        assert_eq!(legacy.default_shell_timeout_secs, 19);
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--default-shell-timeout-secs"));
        assert!(!help.contains("--shell-timeout-secs"));
    }

    #[tokio::test]
    async fn shell_timeout_cli_override_terminates_slow_scripted_command() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("session");
        let steps_file = temp.path().join("steps.jsonl");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"sleep 3","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
            ),
        )
        .unwrap();
        let args = Cli::try_parse_from([
            "carry",
            "--default-shell-timeout-secs",
            "1",
            "--max-steps",
            "2",
            "--cwd",
            workspace.to_str().unwrap(),
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--scripted-steps",
            steps_file.to_str().unwrap(),
            "-p",
            "run the command",
        ])
        .unwrap();
        assert_eq!(args.default_shell_timeout_secs, 1);

        run_command(args).await.unwrap();
        let trace = std::fs::read_to_string(session_dir.join("trace.jsonl")).unwrap();
        let shell_finished = trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["event"] == "shell_finished")
            .unwrap();
        assert_eq!(shell_finished["data"]["result"]["timed_out"], true);
    }

    #[tokio::test]
    async fn model_shell_timeout_overrides_session_default() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("session");
        let steps_file = temp.path().join("steps.jsonl");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"sleep 3","timeout_secs":1,"answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
            ),
        )
        .unwrap();
        let args = Cli::try_parse_from([
            "carry",
            "--default-shell-timeout-secs",
            "60",
            "--max-steps",
            "2",
            "--cwd",
            workspace.to_str().unwrap(),
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--scripted-steps",
            steps_file.to_str().unwrap(),
            "-p",
            "run the command",
        ])
        .unwrap();
        run_command(args).await.unwrap();
        let trace = std::fs::read_to_string(session_dir.join("trace.jsonl")).unwrap();
        let shell_finished = trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["event"] == "shell_finished")
            .unwrap();
        assert_eq!(shell_finished["data"]["result"]["timed_out"], true);
    }

    #[tokio::test]
    async fn model_can_extend_short_session_timeout_for_one_command() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("session");
        let steps_file = temp.path().join("steps.jsonl");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"sleep 2 && printf done","timeout_secs":3,"answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
            ),
        )
        .unwrap();
        let args = Cli::try_parse_from([
            "carry",
            "--default-shell-timeout-secs",
            "1",
            "--max-steps",
            "2",
            "--cwd",
            workspace.to_str().unwrap(),
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--scripted-steps",
            steps_file.to_str().unwrap(),
            "-p",
            "run the command",
        ])
        .unwrap();
        run_command(args).await.unwrap();
        let trace = std::fs::read_to_string(session_dir.join("trace.jsonl")).unwrap();
        let shell_finished = trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["event"] == "shell_finished")
            .unwrap();
        assert_eq!(shell_finished["data"]["result"]["timed_out"], false);
        assert_eq!(shell_finished["data"]["result"]["exit_code"], 0);
    }

    #[test]
    fn default_web_port_does_not_require_serve_flag() {
        assert!(Cli::try_parse_from(["carry", "--port", "9000"]).is_ok());
    }

    #[test]
    fn print_mode_is_explicit_and_exclusive() {
        assert!(Cli::try_parse_from(["carry", "--print", "-p", "hello"]).is_ok());
        assert!(Cli::try_parse_from(["carry", "--print", "--serve"]).is_err());
        assert!(Cli::try_parse_from(["carry", "--print", "--interactive"]).is_err());
    }

    #[test]
    fn positional_and_explicit_prompts_are_supported() {
        let positional = Cli::try_parse_from(["carry", "fix", "the", "tests"]).unwrap();
        assert_eq!(positional.prompt_words, ["fix", "the", "tests"]);
        let explicit = Cli::try_parse_from(["carry", "-p", "fix --release"]).unwrap();
        assert_eq!(explicit.prompt.as_deref(), Some("fix --release"));
    }

    #[test]
    fn prompt_sources_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["carry", "-p", "one", "two"]).is_err());
    }

    #[test]
    fn help_contains_real_examples() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("carry fix the failing tests"));
        assert!(help.contains("carry --interactive"));
    }

    #[test]
    fn keep_lease_turns_defaults_to_eight_and_requires_positive_value() {
        let defaulted = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(defaulted.keep_lease_turns, Some(8));
        let enabled =
            Cli::try_parse_from(["carry", "--keep-lease-turns", "3", "continue"]).unwrap();
        assert_eq!(enabled.keep_lease_turns, Some(3));
        assert!(Cli::try_parse_from(["carry", "--keep-lease-turns", "0", "continue"]).is_err());
    }

    #[test]
    fn compaction_payoff_requests_defaults_to_five_and_requires_positive_value() {
        let defaulted = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(defaulted.compaction_payoff_requests, 5);
        let configured =
            Cli::try_parse_from(["carry", "--compaction-payoff-requests", "5", "continue"])
                .unwrap();
        assert_eq!(configured.compaction_payoff_requests, 5);
        assert!(
            Cli::try_parse_from(["carry", "--compaction-payoff-requests", "0", "continue",])
                .is_err()
        );
    }

    #[test]
    fn compaction_min_payback_percent_defaults_to_twenty_five_and_is_bounded() {
        let defaulted = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(defaulted.compaction_min_payback_percent, 25);
        let configured = Cli::try_parse_from([
            "carry",
            "--compaction-min-payback-percent",
            "25",
            "continue",
        ])
        .unwrap();
        assert_eq!(configured.compaction_min_payback_percent, 25);
        assert!(
            Cli::try_parse_from([
                "carry",
                "--compaction-min-payback-percent",
                "101",
                "continue",
            ])
            .is_err()
        );
    }

    #[test]
    fn compaction_rollout_samples_are_opt_in_and_bounded() {
        let disabled = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(disabled.compaction_rollout_samples, 0);
        let enabled =
            Cli::try_parse_from(["carry", "--compaction-rollout-samples", "16", "continue"])
                .unwrap();
        assert_eq!(enabled.compaction_rollout_samples, 16);
        assert!(
            Cli::try_parse_from(["carry", "--compaction-rollout-samples", "65", "continue",])
                .is_err()
        );
    }

    #[test]
    fn neutral_watermarks_default_to_zero_and_accept_nonzero() {
        let defaulted = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(defaulted.compaction_neutral_high_watermark_tokens, 0);
        assert_eq!(defaulted.compaction_neutral_low_watermark_tokens, 0);
        let configured_budget = Cli::try_parse_from([
            "carry",
            "--compaction-neutral-high-watermark-tokens",
            "32768",
            "--compaction-neutral-low-watermark-tokens",
            "24576",
            "continue",
        ])
        .unwrap();
        assert_eq!(
            configured_budget.compaction_neutral_high_watermark_tokens,
            32 * 1024
        );
        assert_eq!(
            configured_budget.compaction_neutral_low_watermark_tokens,
            24 * 1024
        );
        let invalid = Cli::try_parse_from([
            "carry",
            "--compaction-neutral-high-watermark-tokens",
            "0",
            "--compaction-neutral-low-watermark-tokens",
            "1",
            "continue",
        ])
        .unwrap();
        assert!(validate_args(&invalid).is_err());
    }

    #[test]
    fn compaction_rollout_stop_probability_defaults_to_ten_and_is_bounded() {
        let defaulted = Cli::try_parse_from(["carry", "continue"]).unwrap();
        assert_eq!(defaulted.compaction_rollout_stop_probability_percent, 10);
        let configured = Cli::try_parse_from([
            "carry",
            "--compaction-rollout-stop-probability-percent",
            "25",
            "continue",
        ])
        .unwrap();
        assert_eq!(configured.compaction_rollout_stop_probability_percent, 25);
        assert!(
            Cli::try_parse_from([
                "carry",
                "--compaction-rollout-stop-probability-percent",
                "101",
                "continue",
            ])
            .is_err()
        );
    }

    #[test]
    fn compaction_policy_disabled_is_accepted() {
        let args =
            Cli::try_parse_from(["carry", "--compaction-policy", "disabled", "continue"]).unwrap();
        assert_eq!(args.compaction_policy, CompactionPolicyArg::Disabled);
    }

    #[test]
    fn resume_can_be_served() {
        let args = Cli::try_parse_from(["carry", "--resume", "session", "--serve"]).unwrap();
        assert!(validate_args(&args).is_ok());
    }

    #[test]
    fn resume_allows_a_fresh_session_destination() {
        let args = Cli::try_parse_from([
            "carry",
            "--resume",
            "session",
            "--session-dir",
            "new-session",
            "continue with a new task",
        ])
        .unwrap();
        assert!(validate_args(&args).is_ok());
    }

    #[test]
    fn resume_session_path_is_canonicalized() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(temp.path().join("alias")).unwrap();
        std::fs::create_dir(&source).unwrap();
        let alias = temp.path().join("alias").join("..").join("source");

        assert_eq!(
            resolve_resume_session(&alias, None).unwrap(),
            source.canonicalize().unwrap()
        );
    }

    #[test]
    fn resume_session_id_rejects_path_traversal() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_resume_session(std::path::Path::new(".."), Some(temp.path())).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resume_session_id_rejects_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let sessions = temp.path().join("sessions");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&sessions).unwrap();
        std::os::unix::fs::symlink(&outside, sessions.join("escaped")).unwrap();

        assert!(
            resolve_resume_session(std::path::Path::new("escaped"), Some(temp.path())).is_err()
        );
    }

    #[test]
    fn resume_session_id_resolves_in_configured_home() {
        let temp = tempfile::tempdir().unwrap();
        let expected = temp.path().join("sessions").join("run-42");
        std::fs::create_dir_all(&expected).unwrap();

        assert_eq!(
            resolve_resume_session(std::path::Path::new("run-42"), Some(temp.path())).unwrap(),
            expected
        );
    }

    #[test]
    fn exact_session_directory_wins_without_home_expansion() {
        let exact = PathBuf::from("custom-session");
        assert_eq!(
            resolve_session_dir(Some(exact.clone()), None).unwrap(),
            exact
        );
    }
}
