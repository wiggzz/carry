mod context;
mod log;
mod openai;
mod protocol;
mod run;
mod web;

use std::{
    io::{BufRead, IsTerminal, Read, Write},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use run::{Backend, RunConfig, UserInput};
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

    /// JSONL Step objects to use instead of calling a model.
    #[arg(long, hide = true)]
    scripted_steps: Option<PathBuf>,
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

struct ResumeState {
    history: Vec<serde_json::Value>,
    model: String,
}

fn validate_args(args: &Cli) -> Result<()> {
    if args.resume.is_some()
        && (args.prompt.is_some()
            || !args.prompt_words.is_empty()
            || args.session_dir.is_some()
            || args.session_home.is_some()
            || args.scripted_steps.is_some())
    {
        bail!(
            "--resume cannot be combined with a prompt, session destination, or --scripted-steps"
        );
    }
    Ok(())
}

async fn run_command(args: Cli) -> Result<()> {
    validate_args(&args)?;
    let resume = args
        .resume
        .as_deref()
        .map(load_resume_history)
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
    if (args.interactive || (args.resume.is_some() && !args.serve)) && !stdin_is_terminal {
        bail!("--interactive and terminal --resume require a terminal on stdin");
    }
    let interactive = args.resume.is_some()
        || args.interactive
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

    let session_dir = match &args.resume {
        Some(session_dir) => session_dir.clone(),
        None => resolve_session_dir(args.session_dir, args.session_home)?,
    };
    if args.resume.is_some() {
        eprintln!("resuming session: {}", session_dir.display());
    } else {
        create_private_session_dir(&session_dir)?;
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
        resume_history: resume.map(|mut resume| {
            // Terminal resume receives its fresh human turn before the runner starts.
            // In --serve mode, web::serve appends the browser's first message instead.
            if !args.serve {
                resume.history.push(user_input_item(prompt.trim()));
            }
            resume.history
        }),
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

fn load_resume_history(session_dir: &std::path::Path) -> Result<ResumeState> {
    let trace_path = session_dir.join("trace.jsonl");
    let trace = std::fs::read_to_string(&trace_path)
        .with_context(|| format!("failed to read session trace: {}", trace_path.display()))?;
    let mut latest_request = None;
    for (line_number, line) in trace.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(line).with_context(|| {
            format!(
                "invalid JSON on line {} of {}",
                line_number + 1,
                trace_path.display()
            )
        })?;
        if event["event"].as_str() == Some("model_request") {
            let history = event["data"]["history"]
                .as_array()
                .context("model_request event has no history array")?
                .clone();
            let model = event["data"]["request"]["model"]
                .as_str()
                .context("model_request event has no request model")?
                .to_owned();
            latest_request = Some(ResumeState { history, model });
        }
    }
    latest_request.context("session has no model request to resume")
}

fn user_input_item(message: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": [{ "type": "input_text", "text": message }]
    })
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
    fn resume_can_be_served() {
        let args = Cli::try_parse_from(["carry", "--resume", "session", "--serve"]).unwrap();
        assert!(validate_args(&args).is_ok());
    }

    #[test]
    fn resume_loads_the_latest_request_even_after_it_was_answered() {
        let temp = tempfile::tempdir().unwrap();
        let trace = [
            serde_json::json!({"event":"model_request","data":{"history":["old"],"request":{"model":"old-model"}}}),
            serde_json::json!({"event":"model_response","data":{}}),
            serde_json::json!({"event":"model_request","data":{"history":["latest"],"request":{"model":"gpt-5.6-terra"}}}),
            serde_json::json!({"event":"model_response","data":{}}),
        ]
        .into_iter()
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(temp.path().join("trace.jsonl"), trace).unwrap();

        let resume = load_resume_history(temp.path()).unwrap();
        assert_eq!(resume.history, vec![serde_json::json!("latest")]);
        assert_eq!(resume.model, "gpt-5.6-terra");
    }

    #[test]
    fn resume_rejects_a_session_without_a_model_request() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("trace.jsonl"),
            serde_json::json!({"event":"model_response","data":{}}).to_string(),
        )
        .unwrap();

        assert!(load_resume_history(temp.path()).is_err());
    }

    #[test]
    fn resumed_message_is_a_responses_api_user_input_item() {
        assert_eq!(
            user_input_item("follow up"),
            serde_json::json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": "follow up" }]
            })
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
