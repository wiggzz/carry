mod context;
mod log;
mod openai;
mod protocol;
mod run;

use std::{
    io::{BufRead, IsTerminal, Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use run::{Backend, RunConfig, UserInput};
use tokio::sync::mpsc;

const EXAMPLES: &str = r#"Examples:
  carry fix the failing tests
  carry -p "explain why --release is failing"
  carry --cwd ../project add tests for the parser
  carry --interactive -p "investigate the flaky test"
  carry
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
    #[arg(short, long)]
    interactive: bool,

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

async fn run_command(args: Cli) -> Result<()> {
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
        || (args.prompt.is_none() && args.prompt_words.is_empty() && stdin_is_terminal);
    let mut input = None;
    let prompt = if let Some(prompt) = args.prompt {
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
    if prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    }

    if interactive && input.is_none() {
        input = Some(spawn_input_reader());
    }

    let session_dir = resolve_session_dir(args.session_dir, args.session_home)?;
    create_private_session_dir(&session_dir)?;
    eprintln!("session: {}", session_dir.display());

    let backend = match args.scripted_steps {
        Some(path) => Backend::scripted(&path).await?,
        None => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required unless --scripted-steps is used")?;
            Backend::openai(openai::OpenAiClient::new(
                args.api_base,
                api_key,
                args.model.clone(),
                args.reasoning_effort.as_str().to_owned(),
            ))
        }
    };

    let config = RunConfig {
        cwd,
        prompt: prompt.trim().to_owned(),
        session_dir: session_dir.clone(),
        model: args.model,
        max_steps: args.max_steps,
        shell_timeout_secs: args.shell_timeout_secs,
    };

    let outcome = if interactive {
        run::run_interactive(config, backend, input.expect("interactive input exists")).await?
    } else {
        run::run(config, backend).await?
    };
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
    fn exact_session_directory_wins_without_home_expansion() {
        let exact = PathBuf::from("custom-session");
        assert_eq!(
            resolve_session_dir(Some(exact.clone()), None).unwrap(),
            exact
        );
    }
}
