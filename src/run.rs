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

At each step, select exactly one available action. Investigate, implement, and verify the requested change before finishing. Before editing, establish a minimal failing reproduction where practical. When behavior has competing inputs or sources, use distinct values to verify provenance; when a task cites a regression or prior change, search cited identifiers and subsequent fixes in repository history. Before finishing, run the affected tests.

You manage context in two generations. The latest tool interaction is available for one decision. Volatile items survive only when explicitly retained and promote after several consecutive rounds. Stable items persist automatically: release them only when stale, contradicted, redundant, or context pressure makes them no longer worth their cost. Preserve exact evidence when its details matter; preserve concise conclusions when it does not.

Work only within the assigned repository. Do not perform destructive or external actions."#;

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub cwd: PathBuf,
    pub task: String,
    pub task_file: PathBuf,
    pub run_dir: PathBuf,
    pub model: String,
    pub max_steps: Option<usize>,
    pub shell_timeout_secs: u64,
    pub promotion_age: usize,
    pub collection_interval: usize,
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

#[derive(Debug, Default, Serialize)]
struct RunMetrics {
    usage: Usage,
    model_latency_ms: u64,
}

impl RunMetrics {
    fn record(&mut self, usage: &Usage, latency_ms: u64) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input_tokens);
        self.usage.cached_input_tokens = self
            .usage
            .cached_input_tokens
            .saturating_add(usage.cached_input_tokens);
        self.usage.cache_write_input_tokens = self
            .usage
            .cache_write_input_tokens
            .saturating_add(usage.cache_write_input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output_tokens);
        self.usage.reasoning_tokens = self
            .usage
            .reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
        self.usage.total_tokens = self.usage.total_tokens.saturating_add(usage.total_tokens);
        self.model_latency_ms = self.model_latency_ms.saturating_add(latency_ms);
    }
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
    let run_started = Instant::now();
    tokio::fs::create_dir_all(config.run_dir.join("tools")).await?;
    let mut logger = RunLogger::create(&config.run_dir)?;
    logger.raw_event(
        "run_started",
        json!({
            "cwd": config.cwd,
            "task_file": config.task_file,
            "model": config.model,
            "max_steps": config.max_steps,
            "promotion_age": config.promotion_age,
            "collection_interval": config.collection_interval
        }),
        &format!("run started in {}", config.cwd.display()),
    )?;

    let mut context_state = ContextState::default();
    let mut latest_tool_result: Option<ContextItem> = None;
    let mut protocol_feedback: Option<String> = None;
    let mut metrics = RunMetrics::default();
    let steps_limit = config
        .max_steps
        .map_or_else(|| "unlimited".to_owned(), |limit| limit.to_string());
    let mut step_index = 0;
    loop {
        if let Some(max_steps) = config.max_steps
            && step_index >= max_steps
        {
            logger.raw_event(
                "run_failed",
                json!({"reason": "max_steps", "max_steps": max_steps}),
                &format!("stopped after reaching {max_steps} steps"),
            )?;
            write_final_artifacts(
                &config,
                false,
                None,
                &metrics,
                run_started.elapsed().as_millis() as u64,
            )
            .await?;
            return Ok(RunOutcome {
                completed: false,
                answer: None,
                run_dir: config.run_dir,
            });
        }
        step_index += 1;
        let history = context_state.input_items(latest_tool_result.as_ref());
        let control = render_control(
            step_index,
            &context_state,
            latest_tool_result.as_ref(),
            protocol_feedback.as_deref(),
            config.promotion_age,
            config.collection_interval,
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
            &format!("[{step_index:02}/{steps_limit}] requesting next step"),
        )?;

        let reply = backend.step(&config.task, &history, &control).await?;
        metrics.record(&reply.usage, reply.latency_ms);
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
                "[{step_index:02}/{steps_limit}] model {}ms in={} cached={} out={} reasoning={}",
                reply.latency_ms,
                reply.usage.input_tokens,
                reply.usage.cached_input_tokens,
                reply.usage.output_tokens,
                reply.usage.reasoning_tokens
            ),
        )?;

        let context_change = match context_state.apply(
            &reply.step.context_management,
            latest_tool_result.as_ref(),
            config.promotion_age,
            config.collection_interval,
        ) {
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
                "  context stable={} volatile={} promoted={} dropped={} released={} added={} bytes={}",
                context_change.stable.len(),
                context_change.volatile.len(),
                context_change.promoted.len(),
                context_change.dropped.len(),
                context_change.released.len(),
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
                write_final_artifacts(
                    &config,
                    true,
                    answer.as_deref(),
                    &metrics,
                    run_started.elapsed().as_millis() as u64,
                )
                .await?;
                return Ok(RunOutcome {
                    completed: true,
                    answer,
                    run_dir: config.run_dir,
                });
            }
        }
    }
}

fn render_control(
    step: usize,
    state: &ContextState,
    latest: Option<&ContextItem>,
    protocol_feedback: Option<&str>,
    promotion_age: usize,
    collection_interval: usize,
) -> String {
    let feedback = protocol_feedback.unwrap_or("(none)");
    let stable_ids = state.stable_ids();
    let volatile = state.volatile_status();
    let latest_id = latest.map_or("none", |item| item.id.as_str());
    format!(
        "Context status for step {step}: stable_ids={stable_ids:?} (kept by default; release only when stale, redundant, contradicted, or under context pressure); volatile_ids_and_ages={volatile:?} (must be retained explicitly); latest_automatic_id={latest_id} (retain only when this is an actual tNNNN ID; if none, use no placeholder); promotion_age={}; collection_interval={}; stable_bytes={}; volatile_bytes={}; retained_bytes={}; protocol_feedback={feedback}. Select exactly one next action.",
        promotion_age,
        collection_interval,
        state.stable_bytes(),
        state.volatile_bytes(),
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
    metrics: &RunMetrics,
    elapsed_ms: u64,
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
        "promotion_age": config.promotion_age,
        "collection_interval": config.collection_interval,
        "patch_bytes": patch.len(),
        "usage": &metrics.usage,
        "model_latency_ms": metrics.model_latency_ms,
        "elapsed_ms": elapsed_ms
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
    fn system_prompt_requires_reproduction_provenance_and_history_checks() {
        assert!(SYSTEM_PROMPT.contains("minimal failing reproduction"));
        assert!(SYSTEM_PROMPT.contains("distinct values"));
        assert!(SYSTEM_PROMPT.contains("repository history"));
        assert!(SYSTEM_PROMPT.contains("search cited identifiers"));
        assert!(SYSTEM_PROMPT.contains("affected tests"));
    }

    #[test]
    fn combined_output_includes_complete_streams() {
        let output = combined_output(b"all stdout", b"all stderr");
        assert!(output.contains("STDOUT (10 bytes):\nall stdout"));
        assert!(output.contains("STDERR (10 bytes):\nall stderr"));
    }

    #[test]
    fn run_metrics_accumulate_usage_and_model_latency() {
        let mut metrics = RunMetrics::default();
        metrics.record(
            &Usage {
                input_tokens: 10,
                cached_input_tokens: 4,
                cache_write_input_tokens: 5,
                output_tokens: 3,
                reasoning_tokens: 1,
                total_tokens: 13,
            },
            25,
        );
        metrics.record(
            &Usage {
                input_tokens: 7,
                cached_input_tokens: 2,
                cache_write_input_tokens: 3,
                output_tokens: 5,
                reasoning_tokens: 2,
                total_tokens: 12,
            },
            15,
        );

        assert_eq!(metrics.usage.input_tokens, 17);
        assert_eq!(metrics.usage.cached_input_tokens, 6);
        assert_eq!(metrics.usage.cache_write_input_tokens, 8);
        assert_eq!(metrics.usage.output_tokens, 8);
        assert_eq!(metrics.usage.reasoning_tokens, 3);
        assert_eq!(metrics.usage.total_tokens, 25);
        assert_eq!(metrics.model_latency_ms, 40);
    }

    #[tokio::test]
    async fn scripted_run_writes_aggregate_metrics_to_result() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let run_dir = temp.path().join("run");
        let task_file = temp.path().join("task.md");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(&task_file, "Finish the task.")
            .await
            .unwrap();
        tokio::fs::write(
            &steps_file,
            r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context_management":{"retain_volatile_ids":[],"release_stable_ids":[],"add_memories":[]}}"#,
        )
        .await
        .unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                task: "Finish the task.".into(),
                task_file,
                run_dir: run_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(1),
                shell_timeout_secs: 1,
                promotion_age: 3,
                collection_interval: 3,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(outcome.completed);
        let result: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(run_dir.join("result.json")).await.unwrap())
                .unwrap();
        assert_eq!(result["usage"]["input_tokens"], 0);
        assert_eq!(result["usage"]["output_tokens"], 0);
        assert_eq!(result["model_latency_ms"], 0);
        assert!(result["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn unlimited_scripted_run_can_finish_after_thirty_shell_actions() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let run_dir = temp.path().join("run");
        let task_file = temp.path().join("task.md");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(&task_file, "Finish the task.")
            .await
            .unwrap();

        let shell_step = format!(
            "{}\n",
            r#"{"action":{"kind":"shell","command":"true","answer":null},"context_management":{"retain_volatile_ids":[],"release_stable_ids":[],"add_memories":[]}}"#
        );
        let mut steps = shell_step.repeat(31);
        steps.push_str(r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context_management":{"retain_volatile_ids":[],"release_stable_ids":[],"add_memories":[]}}"#);
        steps.push('\n');
        tokio::fs::write(&steps_file, steps).await.unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                task: "Finish the task.".into(),
                task_file,
                run_dir: run_dir.clone(),
                model: "scripted".into(),
                max_steps: None,
                shell_timeout_secs: 1,
                promotion_age: 3,
                collection_interval: 3,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(outcome.completed);
        let result: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(run_dir.join("result.json")).await.unwrap())
                .unwrap();
        assert!(result["steps_limit"].is_null());
        let trace = tokio::fs::read_to_string(run_dir.join("trace.log"))
            .await
            .unwrap();
        assert!(trace.contains("[01/unlimited] requesting next step"));
    }

    #[tokio::test]
    async fn explicit_step_limit_stops_before_the_next_model_request() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let run_dir = temp.path().join("run");
        let task_file = temp.path().join("task.md");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(&task_file, "Finish the task.")
            .await
            .unwrap();
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"true","answer":null},"context_management":{"retain_volatile_ids":[],"release_stable_ids":[],"add_memories":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context_management":{"retain_volatile_ids":[],"release_stable_ids":[],"add_memories":[]}}"#,
                "\n"
            ),
        )
        .await
        .unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                task: "Finish the task.".into(),
                task_file,
                run_dir: run_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(1),
                shell_timeout_secs: 1,
                promotion_age: 3,
                collection_interval: 3,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(!outcome.completed);
        let result: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(run_dir.join("result.json")).await.unwrap())
                .unwrap();
        assert_eq!(result["steps_limit"], 1);
        assert!(!result["completed"].as_bool().unwrap());
        let trace = tokio::fs::read_to_string(run_dir.join("trace.jsonl"))
            .await
            .unwrap();
        assert!(trace.contains(r#""reason":"max_steps""#));
    }
}
