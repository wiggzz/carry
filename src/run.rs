use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use tokio::{io::AsyncWriteExt, process::Command, time::Duration};

use crate::{
    carry::CarryState,
    log::RunLogger,
    openai::{ModelReply, OpenAiClient, Usage},
    protocol::{ActionKind, Step},
};

const SYSTEM_PROMPT: &str = r#"You are a coding agent operating in a repository through a shell.

Each response is one structured Step. Use a shell action to inspect, edit, and test the repository. Use a finish action only when the task is complete or no further useful work is possible.

Carry is your complete persistent working memory. Existing carry entries survive only when their IDs appear in carry.keep. Omit entries you no longer expect to need. Never copy retained entry text into carry.add. Add only concise information that will matter after the current action. The original task is always available and does not need to be carried.

The shell is noninteractive. Prefer commands that terminate on their own. Work only within the assigned repository. Test your work before finishing."#;

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub cwd: PathBuf,
    pub task: String,
    pub task_file: PathBuf,
    pub run_dir: PathBuf,
    pub model: String,
    pub max_steps: usize,
    pub shell_timeout_secs: u64,
    pub max_tool_output_bytes: usize,
    pub carry_budget_bytes: usize,
}

pub enum Backend {
    OpenAi(OpenAiClient),
    Scripted {
        steps: VecDeque<Step>,
        emitted: usize,
    },
}

#[derive(Debug)]
pub struct RunOutcome {
    pub completed: bool,
    pub answer: Option<String>,
    pub run_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct ShellResult {
    call_id: String,
    command: String,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    duration_ms: u64,
    timed_out: bool,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    prompt_output: String,
}

impl Backend {
    pub fn openai(client: OpenAiClient) -> Self {
        Self::OpenAi(client)
    }

    pub async fn scripted(path: &Path) -> Result<Self> {
        let contents = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read scripted steps: {}", path.display()))?;
        let mut steps = VecDeque::new();
        for (index, line) in contents.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let step: Step = serde_json::from_str(line).with_context(|| {
                format!("invalid Step on line {} of {}", index + 1, path.display())
            })?;
            step.action.validate()?;
            steps.push_back(step);
        }
        if steps.is_empty() {
            bail!("scripted steps file is empty: {}", path.display());
        }
        Ok(Self::Scripted { steps, emitted: 0 })
    }

    fn request_body(&self, task: &str, context: &str) -> Option<serde_json::Value> {
        match self {
            Self::OpenAi(client) => Some(client.request_body(SYSTEM_PROMPT, task, context)),
            Self::Scripted { .. } => None,
        }
    }

    async fn step(&mut self, task: &str, context: &str) -> Result<ModelReply> {
        match self {
            Self::OpenAi(client) => client.step(SYSTEM_PROMPT, task, context).await,
            Self::Scripted { steps, emitted } => {
                let step = steps
                    .pop_front()
                    .context("scripted backend ran out of Step objects")?;
                *emitted += 1;
                Ok(ModelReply {
                    response_id: format!("scripted-{emitted:04}"),
                    raw: serde_json::to_value(&step)?,
                    step,
                    usage: Usage::default(),
                    latency_ms: 0,
                })
            }
        }
    }
}

pub async fn run(config: RunConfig, mut backend: Backend) -> Result<RunOutcome> {
    tokio::fs::create_dir_all(config.run_dir.join("tools")).await?;
    let mut logger = RunLogger::create(&config.run_dir)?;
    logger.raw_event(
        "run_started",
        json!({
            "cwd": config.cwd,
            "task_file": config.task_file,
            "model": config.model,
            "max_steps": config.max_steps,
            "carry_budget_bytes": config.carry_budget_bytes
        }),
        &format!("run started in {}", config.cwd.display()),
    )?;

    let mut carry = CarryState::default();
    let mut recent = "No shell actions have run yet.".to_owned();
    for step_index in 1..=config.max_steps {
        let context = render_context(step_index, &carry, &recent);
        let request = backend.request_body(&config.task, &context);
        logger.raw_event(
            "model_request",
            json!({"step": step_index, "context": context, "request": request}),
            &format!(
                "[{step_index:02}/{}] requesting next step",
                config.max_steps
            ),
        )?;

        let reply = backend.step(&config.task, &context).await?;
        logger.raw_event(
            "model_response",
            json!({
                "step": step_index,
                "response_id": reply.response_id,
                "latency_ms": reply.latency_ms,
                "usage": reply.usage,
                "parsed": reply.step,
                "raw": reply.raw
            }),
            &format!(
                "[{step_index:02}/{}] model {}ms in={} cached={} out={} reasoning={}",
                config.max_steps,
                reply.latency_ms,
                reply.usage.input_tokens,
                reply.usage.cached_input_tokens,
                reply.usage.output_tokens,
                reply.usage.reasoning_tokens
            ),
        )?;

        if let Err(error) = reply.step.action.validate() {
            recent = format!(
                "Protocol error: {error}. No action was executed. Submit a corrected Step."
            );
            logger.raw_event(
                "protocol_error",
                json!({"step": step_index, "error": error.to_string()}),
                &recent,
            )?;
            continue;
        }

        let carry_change = match carry.apply(&reply.step.carry, config.carry_budget_bytes) {
            Ok(change) => change,
            Err(error) => {
                recent = format!(
                    "Protocol error: {error}. No action was executed. Submit a corrected Step."
                );
                logger.raw_event(
                    "protocol_error",
                    json!({"step": step_index, "error": error.to_string()}),
                    &recent,
                )?;
                continue;
            }
        };
        logger.event(
            "carry_updated",
            &carry_change,
            &format!(
                "  carry kept={} dropped={} added={} bytes={}",
                carry_change.kept.len(),
                carry_change.dropped.len(),
                carry_change.added.len(),
                carry_change.bytes
            ),
        )?;

        match reply.step.action.kind {
            ActionKind::Shell => {
                let command = reply.step.action.command.as_deref().unwrap();
                let call_id = format!("c{step_index:04}");
                logger.raw_event(
                    "shell_started",
                    json!({"step": step_index, "call_id": call_id, "command": command}),
                    &format!("  $ {command}"),
                )?;
                let result = execute_shell(
                    &config.cwd,
                    &config.run_dir,
                    call_id,
                    command,
                    config.shell_timeout_secs,
                    config.max_tool_output_bytes,
                )
                .await?;
                recent = render_recent_result(&result);
                logger.event(
                    "shell_finished",
                    &result,
                    &format!(
                        "  exit={:?} {}ms stdout={}B stderr={}B{}",
                        result.exit_code,
                        result.duration_ms,
                        result.stdout_bytes,
                        result.stderr_bytes,
                        if result.timed_out { " timed-out" } else { "" }
                    ),
                )?;
            }
            ActionKind::Finish => {
                let answer = reply.step.action.answer;
                logger.raw_event(
                    "run_finished",
                    json!({"step": step_index, "answer": answer, "carry": carry.snapshot()}),
                    &format!("finished after {step_index} steps"),
                )?;
                write_final_artifacts(&config, true, answer.as_deref()).await?;
                return Ok(RunOutcome {
                    completed: true,
                    answer,
                    run_dir: config.run_dir,
                });
            }
        }
    }

    logger.raw_event(
        "run_failed",
        json!({"reason": "max_steps", "max_steps": config.max_steps}),
        &format!("stopped after reaching {} steps", config.max_steps),
    )?;
    write_final_artifacts(&config, false, None).await?;
    Ok(RunOutcome {
        completed: false,
        answer: None,
        run_dir: config.run_dir,
    })
}

fn render_context(step: usize, carry: &CarryState, recent: &str) -> String {
    format!(
        "<carry>\n{}\n</carry>\n\n<current_step>{step}</current_step>\n\n<recent>\n{recent}\n</recent>",
        carry.render()
    )
}

async fn execute_shell(
    cwd: &Path,
    run_dir: &Path,
    call_id: String,
    command: &str,
    timeout_secs: u64,
    max_prompt_bytes: usize,
) -> Result<ShellResult> {
    let started = Instant::now();
    let mut child = Command::new("/bin/sh");
    child
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(Duration::from_secs(timeout_secs), child.output()).await;
    let (exit_code, stdout, stderr, timed_out) = match output {
        Ok(result) => {
            let output = result.context("failed to execute shell command")?;
            (output.status.code(), output.stdout, output.stderr, false)
        }
        Err(_) => (None, Vec::new(), b"command timed out".to_vec(), true),
    };

    let stdout_path = run_dir.join("tools").join(format!("{call_id}.stdout"));
    let stderr_path = run_dir.join("tools").join(format!("{call_id}.stderr"));
    write_bytes(&stdout_path, &stdout).await?;
    write_bytes(&stderr_path, &stderr).await?;

    let prompt_output = bounded_output(&stdout, &stderr, max_prompt_bytes);
    Ok(ShellResult {
        call_id,
        command: command.to_owned(),
        exit_code,
        stdout_bytes: stdout.len(),
        stderr_bytes: stderr.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
        stdout_path,
        stderr_path,
        prompt_output,
    })
}

async fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(bytes).await?;
    Ok(())
}

fn bounded_output(stdout: &[u8], stderr: &[u8], budget: usize) -> String {
    let header = format!("STDOUT ({} bytes):\n", stdout.len());
    let middle = format!("\nSTDERR ({} bytes):\n", stderr.len());
    let overhead = header.len() + middle.len();
    let available = budget.saturating_sub(overhead);
    let stdout_budget = available / 2;
    let stderr_budget = available - stdout_budget;
    format!(
        "{header}{}{middle}{}",
        clip(stdout, stdout_budget),
        clip(stderr, stderr_budget)
    )
}

fn clip(bytes: &[u8], budget: usize) -> String {
    if bytes.len() <= budget {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let marker = b"\n...[cut]...\n";
    if budget < marker.len() + 2 {
        return String::from_utf8_lossy(&bytes[..budget.min(bytes.len())]).into_owned();
    }
    let remaining = budget.saturating_sub(marker.len());
    let head = remaining / 2;
    let tail = remaining - head;
    format!(
        "{}{}{}",
        String::from_utf8_lossy(&bytes[..head]),
        String::from_utf8_lossy(marker),
        String::from_utf8_lossy(&bytes[bytes.len() - tail..])
    )
}

fn render_recent_result(result: &ShellResult) -> String {
    format!(
        "[{}] shell: {}\nexit_code={:?} timed_out={} duration_ms={}\nfull stdout: {}\nfull stderr: {}\n\n{}",
        result.call_id,
        result.command,
        result.exit_code,
        result.timed_out,
        result.duration_ms,
        result.stdout_path.display(),
        result.stderr_path.display(),
        result.prompt_output
    )
}

async fn write_final_artifacts(
    config: &RunConfig,
    completed: bool,
    answer: Option<&str>,
) -> Result<()> {
    let patch = Command::new("git")
        .args(["diff", "--binary", "--no-ext-diff"])
        .current_dir(&config.cwd)
        .output()
        .await;
    let patch = match patch {
        Ok(output) if output.status.success() => output.stdout,
        _ => Vec::new(),
    };
    write_bytes(&config.run_dir.join("final.patch"), &patch).await?;
    let result = json!({
        "completed": completed,
        "answer": answer,
        "model": config.model,
        "steps_limit": config.max_steps,
        "patch_bytes": patch.len()
    });
    tokio::fs::write(
        config.run_dir.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_preserves_both_ends() {
        let bytes = b"abcdefghijklmnopqrstuvwxyz";
        let clipped = clip(bytes, 20);
        assert!(clipped.starts_with("a"));
        assert!(clipped.ends_with("z"));
    }
}
