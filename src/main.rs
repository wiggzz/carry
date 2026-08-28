mod context;
mod log;
mod openai;
mod protocol;
mod run;
mod web;

use std::{
    io::{BufRead, IsTerminal, Read, Write},
    path::{Component, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use run::{Backend, CompactionMode, RunConfig, UserInput};
use tokio::sync::mpsc;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;

const EXAMPLES: &str = r#"Examples:
  carry fix the failing tests
  carry -p "explain why --release is failing"
  carry --cwd ../project add tests for the parser
  carry --interactive -p "investigate the flaky test"
  carry
  carry --serve --cwd ../project
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

    /// Continue this session interactively from its saved conversation.
    #[arg(long, value_name = "SESSION_DIR")]
    resume: Option<PathBuf>,

    /// Launch a local browser UI and SSE API.
    #[arg(long)]
    serve: bool,

    /// Local port used by --serve.
    #[arg(long, default_value_t = 8765, requires = "serve")]
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
    #[arg(long, env = "OPENAI_MODEL", default_value = "gpt-5.6-luna")]
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

    /// Timeout for each shell command.
    #[arg(long, default_value_t = 300)]
    shell_timeout_secs: u64,

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

    /// JSONL Step objects to use instead of calling a model.
    #[arg(long, hide = true)]
    scripted_steps: Option<PathBuf>,
}

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
    run_command(Cli::parse()).await
}

fn validate_args(_args: &Cli) -> Result<()> {
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
    let interactive = args.interactive
        || (!args.serve
            && args.prompt.is_none()
            && args.prompt_words.is_empty()
            && stdin_is_terminal);
    let mut input = None;
    let prompt = if args.serve {
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
        eprint!("carry> ");
        let _ = std::io::stderr().flush();
        input = Some(spawn_input_reader());
        match input
            .as_mut()
            .expect("interactive input exists")
            .recv()
            .await
        {
            Some(UserInput::Message(prompt)) => prompt,
            Some(UserInput::Exit) | None => return Ok(()),
        }
    };
    if !args.serve && prompt.trim().is_empty() {
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
        eprintln!("resuming session: {}", source.display());
        if source != &session_dir {
            eprintln!("new session: {}", session_dir.display());
        }
    } else {
        eprintln!("session: {}", session_dir.display());
    }

    if args.request_timeout_secs == 0 || args.connect_timeout_secs == 0 {
        bail!("OpenAI request and connect timeouts must be greater than zero");
    }

    let backend = match args.scripted_steps {
        Some(path) => Backend::scripted(&path).await?,
        None => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required unless --scripted-steps is used")?;
            Backend::openai(openai::OpenAiClient::with_timeouts(
                args.api_base,
                api_key,
                model.clone(),
                args.reasoning_effort.as_str().to_owned(),
                Duration::from_secs(args.request_timeout_secs),
                Duration::from_secs(args.connect_timeout_secs),
            )?)
        }
    };

    let config = RunConfig {
        cwd,
        prompt: prompt.trim().to_owned(),
        session_dir: session_dir.clone(),
        model,
        max_steps: args.max_steps,
        shell_timeout_secs: args.shell_timeout_secs,
        compaction_mode: args.compaction_policy.into(),
        resume_context: resume.map(|resume| resume.context),
        resume_source,
    };

    if args.serve {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
        eprintln!("carry web UI: http://{address}");
        tokio::select! {
            result = web::serve(address, config, backend) => result?,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("session interrupted by Ctrl-C");
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
            eprintln!("session interrupted by Ctrl-C");
            print_resume_hint(&session_dir);
            return Ok(());
        }
    };
    print_resume_hint(&outcome.session_dir);
    if outcome.completed {
        if !interactive {
            println!("{}", outcome.answer.unwrap_or_default());
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
    eprintln!(
        "session: {}\nresume with: carry --resume {}",
        session_dir.display(),
        session_dir.display()
    );
}

fn spawn_input_reader() -> mpsc::UnboundedReceiver<UserInput> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        loop {
            let Some(line) = lines.next() else {
                let _ = sender.send(UserInput::Exit);
                break;
            };
            let Ok(line) = line else {
                let _ = sender.send(UserInput::Exit);
                break;
            };
            let line = line.trim();
            match line {
                "" => continue,
                "/quit" | "/exit" => {
                    let _ = sender.send(UserInput::Exit);
                    break;
                }
                "/help" => {
                    eprintln!("Enter steering at any time. Commands: /help, /quit, /exit");
                }
                message => {
                    if sender.send(UserInput::Message(message.to_owned())).is_err() {
                        break;
                    }
                }
            }
        }
    });
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
