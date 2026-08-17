use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use tokio::{io::AsyncWriteExt, process::Command, sync::mpsc, time::Duration};

use crate::{
    context::{CompactionKind, CompactionPolicy, ContextState},
    log::RunLogger,
    openai::{ModelReply, OpenAiClient, Usage},
    protocol::{ActionKind, Step},
};

const SYSTEM_PROMPT: &str = r#"You are a coding agent working iteratively in an assigned repository.

At each step, select exactly one available action. Investigate, implement, and verify the requested change before finishing. Before editing, establish a minimal failing reproduction where practical. When behavior has competing inputs or sources, use distinct values to verify provenance; when a task cites a regression or prior change, search cited identifiers and subsequent fixes in repository history. Before finishing, run the affected tests. Use the optional shell message to give the human concise, useful progress commentary.

Context is a chronological ledger. Each item is followed by an immutable marker in the form [integer kind stable|volatile]. Stable items persist by default. Context keep/drop arrays are sparse advisory signals, not immediate commands: keep marks exact items the task cannot safely lose, while drop marks exact items that are no longer useful. Emit only newly recognized high-confidence signals, at most four IDs in each array; omitted IDs keep their prior signal. Stable items normally do not need keep. Missing, stale, or wrong-class IDs are harmless. A later keep or drop reverses the earlier opinion, and keep wins if both name the same ID in one response. Carry decides when cache-aware minor or major compaction is economical. Human messages are authoritative; mark one drop only when it is satisfied or explicitly superseded. Before marking exact context drop, preserve durable conclusions, constraints, evidence, decisions, or unresolved questions with context.remember. Preserve outcomes, not chain-of-thought.

Work only within the assigned repository. Do not perform destructive or external actions."#;

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub cwd: PathBuf,
    pub prompt: String,
    pub session_dir: PathBuf,
    pub model: String,
    pub max_steps: Option<usize>,
    pub shell_timeout_secs: u64,
}

const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const COMPACTION_HORIZON_TURNS: usize = 8;

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
    pub session_dir: PathBuf,
}

#[derive(Debug)]
pub enum UserInput {
    Message(String),
    Exit,
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
    response_retries: usize,
    minor_compactions: usize,
    major_compactions: usize,
}

impl RunMetrics {
    fn record(&mut self, usage: &Usage, latency_ms: u64, response_retries: usize) {
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
        self.response_retries = self.response_retries.saturating_add(response_retries);
    }

    fn record_compaction(&mut self, kind: CompactionKind) {
        match kind {
            CompactionKind::Minor => {
                self.minor_compactions = self.minor_compactions.saturating_add(1)
            }
            CompactionKind::Major => {
                self.major_compactions = self.major_compactions.saturating_add(1)
            }
        }
    }
}

#[derive(Debug)]
struct CacheTracker {
    implicit_activity: Option<Instant>,
    stable_checkpoint_activity: Option<Instant>,
    checkpoint_pending: bool,
}

impl Default for CacheTracker {
    fn default() -> Self {
        Self {
            implicit_activity: None,
            stable_checkpoint_activity: None,
            checkpoint_pending: true,
        }
    }
}

impl CacheTracker {
    fn observe(&mut self, usage: &Usage) {
        let now = Instant::now();
        let cache_activity = usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0;
        if cache_activity {
            self.implicit_activity = Some(now);
        }
        if self.checkpoint_pending {
            self.stable_checkpoint_activity = cache_activity.then_some(now);
            self.checkpoint_pending = false;
        } else if usage.cached_input_tokens > 0 {
            self.stable_checkpoint_activity = Some(now);
        }
    }

    fn policy(&self) -> CompactionPolicy {
        let now = Instant::now();
        CompactionPolicy {
            horizon_turns: COMPACTION_HORIZON_TURNS,
            implicit_cache_alive: cache_alive(self.implicit_activity, now),
            stable_cache_alive: cache_alive(self.stable_checkpoint_activity, now),
        }
    }

    fn implicit_expired(&self) -> bool {
        self.implicit_activity
            .is_some_and(|activity| activity.elapsed() >= CACHE_TTL)
    }

    fn mark_compaction(&mut self) {
        self.implicit_activity = None;
        self.checkpoint_pending = true;
    }
}

fn cache_alive(activity: Option<Instant>, now: Instant) -> bool {
    activity.is_some_and(|activity| now.duration_since(activity) < CACHE_TTL)
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

    fn request_body(&self, history: &[serde_json::Value]) -> Option<serde_json::Value> {
        match self {
            Self::OpenAi(client) => Some(client.request_body(SYSTEM_PROMPT, history)),
            Self::Scripted { .. } => None,
        }
    }

    async fn step(&mut self, history: &[serde_json::Value]) -> Result<ModelReply> {
        match self {
            Self::OpenAi(client) => client.step(SYSTEM_PROMPT, history).await,
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
                    output_items: vec![function_call.clone()],
                    function_call,
                    usage: Usage::default(),
                    latency_ms: 0,
                    response_retries: 0,
                })
            }
        }
    }
}

pub async fn run(config: RunConfig, mut backend: Backend) -> Result<RunOutcome> {
    run_loop(config, &mut backend, None).await
}

pub async fn run_interactive(
    config: RunConfig,
    mut backend: Backend,
    input: mpsc::UnboundedReceiver<UserInput>,
) -> Result<RunOutcome> {
    run_loop(config, &mut backend, Some(input)).await
}

async fn run_loop(
    config: RunConfig,
    backend: &mut Backend,
    mut input: Option<mpsc::UnboundedReceiver<UserInput>>,
) -> Result<RunOutcome> {
    let run_started = Instant::now();
    tokio::fs::create_dir_all(config.session_dir.join("tools")).await?;
    let mut logger = RunLogger::create(&config.session_dir)?;
    logger.raw_event(
        "run_started",
        json!({
            "cwd": config.cwd,
            "prompt": config.prompt,
            "model": config.model,
            "max_steps": config.max_steps,
            "cache_ttl_seconds": CACHE_TTL.as_secs(),
            "compaction_horizon_turns": COMPACTION_HORIZON_TURNS
        }),
        &format!("carry · {} · {}", config.model, config.cwd.display()),
    )?;

    let mut context_state = ContextState::new(config.prompt.clone());
    let mut metrics = RunMetrics::default();
    let mut cache = CacheTracker::default();
    let mut step_index = 0;
    let mut turn_step = 0;
    loop {
        if cache.implicit_expired() {
            maybe_compact(
                &mut context_state,
                &[],
                &mut cache,
                &mut metrics,
                &mut logger,
                "cache expired",
            )?;
        }
        if let Some(max_steps) = config.max_steps
            && turn_step >= max_steps
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
                session_dir: config.session_dir,
            });
        }
        step_index += 1;
        turn_step += 1;
        let history = context_state.input_items();
        let request = backend.request_body(&history);
        logger.raw_event_silent(
            "model_request",
            json!({
                "step": step_index,
                "history": history,
                "request": request
            }),
        )?;

        let reply = backend.step(&history).await?;
        metrics.record(&reply.usage, reply.latency_ms, reply.response_retries);
        cache.observe(&reply.usage);
        logger.raw_event(
            "model_response",
            json!({
                "step": step_index,
                "response_id": reply.response_id,
                "latency_ms": reply.latency_ms,
                "response_retries": reply.response_retries,
                "usage": reply.usage,
                "parsed": &reply.step,
                "raw": reply.raw
            }),
            &terminal_usage(
                turn_step,
                reply.latency_ms,
                reply.response_retries,
                &reply.usage,
            ),
        )?;

        match reply.step.action.kind {
            ActionKind::Shell => {
                if let Some(message) = reply.step.action.message.as_deref() {
                    logger.raw_event(
                        "assistant_message",
                        json!({"step": step_index, "message": message}),
                        &format!("  {message}"),
                    )?;
                }
                let command = reply.step.action.command.as_deref().unwrap();
                let call_id = format!("tool-{step_index}");
                logger.raw_event(
                    "shell_started",
                    json!({"step": step_index, "call_id": call_id, "command": command}),
                    &format!("  $ {command}"),
                )?;
                let result = execute_shell(
                    &config.cwd,
                    &config.session_dir,
                    call_id,
                    command,
                    config.shell_timeout_secs,
                )
                .await?;
                let output = function_call_output(&reply.function_call, &result)?;
                let item_id = context_state.add_tool(reply.output_items.clone(), output)?;
                logger.event("shell_finished", &result, &terminal_shell_result(&result))?;
                let signals = context_state.record_signals(&reply.step.context);
                logger.event_silent("context_signals", &signals)?;
                maybe_compact(
                    &mut context_state,
                    &[item_id],
                    &mut cache,
                    &mut metrics,
                    &mut logger,
                    "economic",
                )?;

                if let Some(receiver) = input.as_mut()
                    && drain_user_input(receiver, &mut context_state, &mut logger)?
                {
                    write_final_artifacts(
                        &config,
                        true,
                        None,
                        &metrics,
                        run_started.elapsed().as_millis() as u64,
                    )
                    .await?;
                    return Ok(RunOutcome {
                        completed: true,
                        answer: None,
                        session_dir: config.session_dir,
                    });
                }
            }
            ActionKind::Finish => {
                let answer = reply.step.action.answer.clone();
                let output = function_output(
                    &reply.function_call,
                    "The answer was delivered to the human; the session may continue.",
                )?;
                let item_id = context_state.add_tool(reply.output_items.clone(), output)?;
                let signals = context_state.record_signals(&reply.step.context);
                logger.event_silent("context_signals", &signals)?;
                maybe_compact(
                    &mut context_state,
                    &[item_id],
                    &mut cache,
                    &mut metrics,
                    &mut logger,
                    "economic",
                )?;
                logger.raw_event(
                    if input.is_some() {
                        "turn_finished"
                    } else {
                        "run_finished"
                    },
                    json!({
                        "step": step_index,
                        "answer": answer,
                        "retained_context": context_state.snapshot()
                    }),
                    &terminal_finished(step_index, &metrics.usage),
                )?;
                if let Some(receiver) = input.as_mut() {
                    println!("{}", answer.as_deref().unwrap_or_default());
                    let mut should_exit =
                        drain_user_input(receiver, &mut context_state, &mut logger)?;
                    if !should_exit {
                        eprint!("carry> ");
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                        match receiver.recv().await {
                            Some(UserInput::Message(message)) => {
                                append_user_message(&mut context_state, &mut logger, message)?;
                            }
                            Some(UserInput::Exit) | None => should_exit = true,
                        }
                    }
                    if !should_exit {
                        turn_step = 0;
                        continue;
                    }
                }

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
                    session_dir: config.session_dir,
                });
            }
        }
    }
}

fn drain_user_input(
    receiver: &mut mpsc::UnboundedReceiver<UserInput>,
    state: &mut ContextState,
    logger: &mut RunLogger,
) -> Result<bool> {
    let mut should_exit = false;
    while let Ok(input) = receiver.try_recv() {
        match input {
            UserInput::Message(message) => {
                append_user_message(state, logger, message)?;
            }
            UserInput::Exit => should_exit = true,
        }
    }
    Ok(should_exit)
}

fn append_user_message(
    state: &mut ContextState,
    logger: &mut RunLogger,
    message: String,
) -> Result<()> {
    let id = state.add_user(message.clone());
    logger.raw_event(
        "human_message",
        json!({"context_id": id, "message": message}),
        &format!("  steering [{id}] queued"),
    )
}

fn maybe_compact(
    state: &mut ContextState,
    protected: &[u64],
    cache: &mut CacheTracker,
    metrics: &mut RunMetrics,
    logger: &mut RunLogger,
    trigger: &str,
) -> Result<bool> {
    let Some(plan) = state.plan_compaction(protected, cache.policy()) else {
        return Ok(false);
    };
    let change = state.compact(plan);
    cache.mark_compaction();
    metrics.record_compaction(change.kind);
    logger.raw_event(
        "context_compacted",
        json!({"trigger": trigger, "compaction": &change}),
        &format!(
            "  compact {} · -{} items / ~{} tok · {} retained · {} rewritten · break-even {} turns",
            change.kind.label(),
            change.dropped.len(),
            compact_number(change.dropped_tokens as u64),
            compact_number(change.retained_tokens as u64),
            compact_number(change.rewrite_tokens as u64),
            change.break_even_turns,
        ),
    )?;
    Ok(true)
}

fn terminal_usage(step: usize, latency_ms: u64, retries: usize, usage: &Usage) -> String {
    let retry = if retries == 0 {
        String::new()
    } else {
        format!(" · {retries} retries")
    };
    format!(
        "[{step:02}] {} · in {} ({} cached, {} write) · out {}{retry}",
        compact_duration(latency_ms),
        compact_number(usage.input_tokens),
        compact_number(usage.cached_input_tokens),
        compact_number(usage.cache_write_input_tokens),
        compact_number(usage.output_tokens),
    )
}

fn terminal_shell_result(result: &ShellResult) -> String {
    let status = if result.timed_out {
        "timeout".to_owned()
    } else {
        result
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| format!("exit {code}"))
    };
    format!(
        "  {status} · {} · {} stdout · {} stderr",
        compact_duration(result.duration_ms),
        compact_bytes(result.stdout_bytes),
        compact_bytes(result.stderr_bytes),
    )
}

fn terminal_finished(steps: usize, usage: &Usage) -> String {
    format!(
        "done · {steps} steps · {} input ({} cached, {} write) · {} output",
        compact_number(usage.input_tokens),
        compact_number(usage.cached_input_tokens),
        compact_number(usage.cache_write_input_tokens),
        compact_number(usage.output_tokens),
    )
}

fn compact_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    }
}

fn compact_number(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    }
}

fn compact_bytes(value: usize) -> String {
    if value < 1_024 {
        format!("{value}B")
    } else if value < 1_048_576 {
        format!("{:.1}KB", value as f64 / 1_024.0)
    } else {
        format!("{:.1}MB", value as f64 / 1_048_576.0)
    }
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

fn function_output(function_call: &serde_json::Value, output: &str) -> Result<serde_json::Value> {
    let call_id = function_call["call_id"]
        .as_str()
        .context("function call has no call_id")?;
    Ok(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
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
    write_bytes(&config.session_dir.join("final.patch"), &patch).await?;
    let result = json!({
        "completed": completed,
        "answer": answer,
        "model": config.model,
        "steps_limit": config.max_steps,
        "patch_bytes": patch.len(),
        "usage": &metrics.usage,
        "model_latency_ms": metrics.model_latency_ms,
        "response_retries": metrics.response_retries,
        "compactions": {
            "minor": metrics.minor_compactions,
            "major": metrics.major_compactions
        },
        "elapsed_ms": elapsed_ms
    });
    tokio::fs::write(
        config.session_dir.join("result.json"),
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
    fn terminal_usage_is_compact_and_includes_cache_writes() {
        let usage = Usage {
            input_tokens: 12_345,
            cached_input_tokens: 10_000,
            cache_write_input_tokens: 2_000,
            output_tokens: 678,
            reasoning_tokens: 400,
            total_tokens: 13_023,
        };

        assert_eq!(
            terminal_usage(3, 1_250, 0, &usage),
            "[03] 1.2s · in 12.3k (10.0k cached, 2.0k write) · out 678"
        );
    }

    #[test]
    fn cached_reads_refresh_the_stable_checkpoint_ttl() {
        let mut cache = CacheTracker {
            implicit_activity: None,
            stable_checkpoint_activity: Some(Instant::now() - CACHE_TTL),
            checkpoint_pending: false,
        };
        cache.observe(&Usage {
            cached_input_tokens: 10,
            ..Usage::default()
        });

        assert!(cache.policy().stable_cache_alive);
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
            2,
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
            3,
        );

        assert_eq!(metrics.usage.input_tokens, 17);
        assert_eq!(metrics.usage.cached_input_tokens, 6);
        assert_eq!(metrics.usage.cache_write_input_tokens, 8);
        assert_eq!(metrics.usage.output_tokens, 8);
        assert_eq!(metrics.usage.reasoning_tokens, 3);
        assert_eq!(metrics.usage.total_tokens, 25);
        assert_eq!(metrics.model_latency_ms, 40);
        assert_eq!(metrics.response_retries, 5);
    }

    #[tokio::test]
    async fn scripted_run_writes_aggregate_metrics_to_result() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"keep":[],"drop":[],"remember":[]}}"#,
        )
        .await
        .unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(1),
                shell_timeout_secs: 1,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(outcome.completed);
        let result: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(session_dir.join("result.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["usage"]["input_tokens"], 0);
        assert_eq!(result["usage"]["output_tokens"], 0);
        assert_eq!(result["model_latency_ms"], 0);
        assert_eq!(result["response_retries"], 0);
        assert!(result["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn unlimited_scripted_run_can_finish_after_thirty_shell_actions() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();

        let shell_step = format!(
            "{}\n",
            r#"{"action":{"kind":"shell","command":"true","answer":null},"context":{"keep":[],"drop":[],"remember":[]}}"#
        );
        let mut steps = shell_step.repeat(31);
        steps.push_str(r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"keep":[],"drop":[],"remember":[]}}"#);
        steps.push('\n');
        tokio::fs::write(&steps_file, steps).await.unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: None,
                shell_timeout_secs: 1,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(outcome.completed);
        let result: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(session_dir.join("result.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(result["steps_limit"].is_null());
        let trace = tokio::fs::read_to_string(session_dir.join("trace.log"))
            .await
            .unwrap();
        assert!(trace.contains("[01] 0ms · in 0 (0 cached, 0 write) · out 0"));
    }

    #[tokio::test]
    async fn explicit_step_limit_stops_before_the_next_model_request() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"true","answer":null},"context":{"keep":[],"drop":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"keep":[],"drop":[],"remember":[]}}"#,
                "\n"
            ),
        )
        .await
        .unwrap();

        let outcome = run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(1),
                shell_timeout_secs: 1,
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(!outcome.completed);
        let result: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(session_dir.join("result.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["steps_limit"], 1);
        assert!(!result["completed"].as_bool().unwrap());
        let trace = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap();
        assert!(trace.contains(r#""reason":"max_steps""#));
    }

    #[tokio::test]
    async fn interactive_steering_is_appended_after_the_completed_tool_result() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("session");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"true","message":"Checking first.","answer":null},"context":{"keep":[],"drop":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"keep":[2],"drop":[],"remember":[]}}"#,
                "\n"
            ),
        )
        .await
        .unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        sender
            .send(UserInput::Message("do not change the JSON format".into()))
            .unwrap();
        drop(sender);

        let outcome = run_interactive(
            RunConfig {
                cwd: workspace,
                prompt: "initial task".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: None,
                shell_timeout_secs: 1,
            },
            Backend::scripted(&steps_file).await.unwrap(),
            receiver,
        )
        .await
        .unwrap();

        assert!(outcome.completed);
        let events = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| event["event"] == "human_message"));
        let requests = events
            .iter()
            .filter(|event| event["event"] == "model_request")
            .collect::<Vec<_>>();
        let history = requests[1]["data"]["history"].as_array().unwrap();
        let tool_marker = history
            .iter()
            .position(|item| item["content"][0]["text"] == "[2 tool volatile]")
            .unwrap();
        let steering = history
            .iter()
            .position(|item| item["content"][0]["text"] == "do not change the JSON format")
            .unwrap();
        assert!(tool_marker < steering);
        assert_eq!(
            history[steering + 1]["content"][0]["text"],
            "[3 user stable]"
        );
    }
}
