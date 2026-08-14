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
    context::{ContextItem, ContextState},
    log::RunLogger,
    openai::{ModelReply, OpenAiClient, Usage},
    protocol::{ActionKind, Step},
};

const SYSTEM_PROMPT: &str = r#"You are a coding agent working iteratively in an assigned repository.

At each step, select exactly one available action. Investigate, implement, and verify the requested change before finishing.

You select the knowledge that carries forward through the investigation. The latest tool interaction is available for one decision. Anything not explicitly selected for retention is removed from subsequent context and cannot be used in further work. Preserve exact evidence when its details matter; preserve concise conclusions when they do not.

Work only within the assigned repository. Do not perform destructive or external actions."#;

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub cwd: PathBuf,
    pub task: String,
    pub task_file: PathBuf,
    pub run_dir: PathBuf,
    pub model: String,
    pub max_steps: usize,
    pub shell_timeout_secs: u64,
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

    fn request_body(
        &self,
        task: &str,
        history: &[serde_json::Value],
        control: &str,
    ) -> Option<serde_json::Value> {
        match self {
            Self::OpenAi(client) => {
                Some(client.request_body(SYSTEM_PROMPT, task, history, control))
            }
            Self::Scripted { .. } => None,
        }
    }

    async fn step(
        &mut self,
        task: &str,
        history: &[serde_json::Value],
        control: &str,
    ) -> Result<ModelReply> {
        match self {
            Self::OpenAi(client) => client.step(SYSTEM_PROMPT, task, history, control).await,
            Self::Scripted { steps, emitted } => {
                let step = steps
                    .pop_front()
                    .context("scripted backend ran out of Step objects")?;
                *emitted += 1;
                let function_call =
                    step.synthetic_function_call(&format!("scripted-call-{emitted:04}"))?;
                Ok(ModelReply {
                    response_id: format!("scripted-{emitted:04}"),
                    raw: serde_json::to_value(&step)?,
                    step,
                    function_call,
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
            "max_steps": config.max_steps
        }),
        &format!("run started in {}", config.cwd.display()),
    )?;

    let mut context_state = ContextState::default();
    let mut latest_tool_result: Option<ContextItem> = None;
    let mut protocol_feedback: Option<String> = None;
    for step_index in 1..=config.max_steps {
        let history = context_state.input_items(latest_tool_result.as_ref());
        let control = render_control(
            step_index,
            &context_state,
            latest_tool_result.as_ref(),
            protocol_feedback.as_deref(),
        );
        let request = backend.request_body(&config.task, &history, &control);
        logger.raw_event(
            "model_request",
            json!({
                "step": step_index,
                "history": history,
                "control": control,
                "request": request
            }),
            &format!(
                "[{step_index:02}/{}] requesting next step",
                config.max_steps
            ),
        )?;

        let reply = backend.step(&config.task, &history, &control).await?;
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

        let context_change = match context_state
            .apply(&reply.step.context_management, latest_tool_result.as_ref())
        {
            Ok(change) => change,
            Err(error) => {
                let feedback = format!(
                    "Protocol error: {error}. No action was executed. Submit a corrected Step."
                );
                protocol_feedback = Some(feedback.clone());
                logger.raw_event(
                    "protocol_error",
                    json!({"step": step_index, "error": error.to_string()}),
                    &feedback,
                )?;
                continue;
            }
        };
        protocol_feedback = None;
        logger.event(
            "context_updated",
            &context_change,
            &format!(
                "  context retained={} dropped={} added={} bytes={}",
                context_change.retained.len(),
                context_change.dropped.len(),
                context_change.added.len(),
                context_change.bytes
            ),
        )?;

        match reply.step.action.kind {
            ActionKind::Shell => {
                let command = reply.step.action.command.as_deref().unwrap();
                let call_id = format!("t{step_index:04}");
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
                )
                .await?;
                let function_call_output = function_call_output(&reply.function_call, &result)?;
                latest_tool_result = Some(ContextItem::tool_interaction(
                    result.call_id.clone(),
                    reply.function_call.clone(),
                    function_call_output,
                )?);
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
                    json!({
                        "step": step_index,
                        "answer": answer,
                        "retained_context": context_state.snapshot()
                    }),
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

fn render_control(
    step: usize,
    state: &ContextState,
    latest: Option<&ContextItem>,
    protocol_feedback: Option<&str>,
) -> String {
    let feedback = protocol_feedback.unwrap_or("(none)");
    let retained_ids = state.retained_ids();
    let latest_id = latest.map_or("none", |item| item.id.as_str());
    format!(
        "Context status for step {step}: retained_ids={retained_ids:?}; latest_automatic_id={latest_id}; retained_bytes={}; protocol_feedback={feedback}. Select exactly one next action.",
        state.retained_bytes()
    )
}

async fn execute_shell(
    cwd: &Path,
    run_dir: &Path,
    call_id: String,
    command: &str,
    timeout_secs: u64,
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

    let prompt_output = combined_output(&stdout, &stderr);
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

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let header = format!("STDOUT ({} bytes):\n", stdout.len());
    let middle = format!("\nSTDERR ({} bytes):\n", stderr.len());
    format!(
        "{header}{}{middle}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
}

fn render_tool_result(result: &ShellResult) -> String {
    serde_json::to_string_pretty(&json!({
        "context_id": result.call_id,
        "exit_code": result.exit_code,
        "timed_out": result.timed_out,
        "duration_ms": result.duration_ms,
        "stdout_bytes": result.stdout_bytes,
        "stderr_bytes": result.stderr_bytes,
        "full_stdout_path": result.stdout_path,
        "full_stderr_path": result.stderr_path,
        "output": result.prompt_output
    }))
    .expect("serializing a shell result cannot fail")
}

fn function_call_output(
    function_call: &serde_json::Value,
    result: &ShellResult,
) -> Result<serde_json::Value> {
    let call_id = function_call["call_id"]
        .as_str()
        .context("shell function call has no call_id")?;
    Ok(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": render_tool_result(result)
    }))
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
    fn combined_output_includes_complete_streams() {
        let output = combined_output(b"all stdout", b"all stderr");
        assert!(output.contains("STDOUT (10 bytes):\nall stdout"));
        assert!(output.contains("STDERR (10 bytes):\nall stderr"));
    }
}
