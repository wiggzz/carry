mod context;
mod log;
mod openai;
mod protocol;
mod run;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use run::{Backend, RunConfig};

#[derive(Debug, Parser)]
#[command(name = "carry", about = "A tiny model-managed-context agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run an agent against a local working directory.
    Run(RunArgs),
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

#[derive(Debug, clap::Args)]
struct RunArgs {
    /// Repository or working directory the shell tool may modify.
    #[arg(long)]
    cwd: PathBuf,

    /// File containing the coding task.
    #[arg(long)]
    task_file: PathBuf,

    /// Directory for trace.jsonl, trace.log, tool output, and final.patch.
    #[arg(long)]
    run_dir: PathBuf,

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

    /// Stop after this many model steps. Omit for no step limit.
    #[arg(long)]
    max_steps: Option<usize>,

    /// Timeout for each shell command.
    #[arg(long, default_value_t = 300)]
    shell_timeout_secs: u64,

    /// Consecutive volatile retention rounds required for stable promotion.
    #[arg(long, default_value_t = 3)]
    promotion_age: usize,

    /// Successful context updates between batched promotion collections.
    #[arg(long, default_value_t = 3)]
    collection_interval: usize,

    /// JSONL Step objects to use instead of calling a model (integration tests).
    #[arg(long)]
    scripted_steps: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run_command(args).await,
    }
}

async fn run_command(args: RunArgs) -> Result<()> {
    if args.promotion_age == 0 || args.collection_interval == 0 {
        bail!("--promotion-age and --collection-interval must be at least 1");
    }
    let cwd = args
        .cwd
        .canonicalize()
        .with_context(|| format!("working directory does not exist: {}", args.cwd.display()))?;
    if !cwd.is_dir() {
        bail!("working directory is not a directory: {}", cwd.display());
    }

    let task = tokio::fs::read_to_string(&args.task_file)
        .await
        .with_context(|| format!("failed to read task file: {}", args.task_file.display()))?;

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
        task,
        task_file: args.task_file,
        run_dir: args.run_dir,
        model: args.model,
        max_steps: args.max_steps,
        shell_timeout_secs: args.shell_timeout_secs,
        promotion_age: args.promotion_age,
        collection_interval: args.collection_interval,
    };

    let outcome = run::run(config, backend).await?;
    if outcome.completed {
        println!("{}", outcome.answer.unwrap_or_default());
        Ok(())
    } else {
        bail!(
            "run stopped without a finish action; see {}",
            outcome.run_dir.display()
        )
    }
}
