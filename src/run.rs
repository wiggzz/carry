use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use tokio::{io::AsyncWriteExt, process::Command, sync::mpsc, time::Duration};

use crate::{
    context::{CompactionPolicy, ContextState, PricedBreakpoint, RenderedBreakpoint},
    log::RunLogger,
    openai::{ModelReply, OpenAiClient, PromptCacheCapabilities, Usage},
    protocol::{ActionKind, Step},
};

// Initial policy hypothesis: keep a meaningful recent working set while leaving ample room in
// the model context. Hysteresis compacts this 32 Ki-token high-water mark toward 24 Ki tokens.
const NEUTRAL_VOLATILE_BUDGET_TOKENS: usize = 32 * 1024;

const SYSTEM_PROMPT: &str = r#"You are a coding agent working iteratively in an assigned repository.

At each step, select one action. Understand the request, investigate, implement, and verify before finishing. Establish a minimal failing reproduction before editing when practical. For regressions, inspect repository history and search cited identifiers and later fixes. Run affected tests before finishing. Use the optional shell message for concise progress commentary.

History is chronological, and context items carry an immutable [context integer stable|volatile] marker. The lifecycle label describes the neutral retention default, not the item's relevance: neutral stable context stays by default, while neutral volatile context remains in the recent working window but may be collected automatically under budget pressure. A volatile label is not an instruction to drop the item. Treat context as evidence and learning, not as a list of completed steps. The normal choice is neutral: leave an ID in neither keep nor drop when its future value is uncertain or ordinary. Mark keep when an item's exact details or an important learning may affect later work. Mark drop without remembering only when you learned nothing useful from the item, or when a newer retained result fully supersedes everything learned from it and the old information is no longer relevant. Do not drop merely because you consumed an item, completed its immediate action, changed course, or its command failed. Failures often establish important repository, environment, and approach constraints. Human messages are authoritative.

When useful learning remains but exact source details are no longer needed, preserve one concise outcome with remember and drop the source. Use keep instead when exact source details may matter again. Do not duplicate an existing memory or kept item; when newer evidence supersedes a memory, drop the old memory and remember the updated conclusion. Preserve outcomes, not chain-of-thought.

Large text shell results arrive as structured `output_head` and `output_tail` previews with an absolute `full_output_path`. Non-text output is omitted from the model payload and available only through its artifact paths. Read or slice those session files when omitted details matter.

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

const TOOL_OUTPUT_INLINE_BYTES: usize = 32 * 1024;
const TOOL_OUTPUT_PREVIEW_BYTES: usize = 16 * 1024;
const BINARY_OUTPUT_OMISSION_REASON: &str =
    "Shell output is not UTF-8 text or appears binary; inspect full_output_path instead.";

#[derive(Clone, Debug, Serialize)]
struct ToolOutputPreview {
    encoding: &'static str,
    head: String,
    tail: Option<String>,
    omitted_bytes: usize,
    omission_reason: Option<&'static str>,
    offloaded: bool,
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
    full_output_path: Option<PathBuf>,
    prompt_output: ToolOutputPreview,
}

#[derive(Debug, Default, Serialize)]
struct RunMetrics {
    usage: Usage,
    model_latency_ms: u64,
    response_retries: usize,
    compactions: usize,
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

    fn record_compaction(&mut self) {
        self.compactions = self.compactions.saturating_add(1);
    }
}

#[derive(Debug, Default)]
struct CacheTracker {
    capabilities: Option<PromptCacheCapabilities>,
    implicit_activity: Option<Instant>,
    implicit_cached_tokens: usize,
    implicit_prefix: Vec<serde_json::Value>,
    breakpoints: HashMap<u64, TrackedBreakpoint>,
    pending: Vec<RenderedBreakpoint>,
    pending_request_tokens: usize,
    pending_history: Vec<serde_json::Value>,
}

#[derive(Debug)]
struct TrackedBreakpoint {
    prefix_tokens: usize,
    cached_tokens: usize,
    activity: Option<Instant>,
}

impl CacheTracker {
    fn new(capabilities: Option<PromptCacheCapabilities>) -> Self {
        Self {
            capabilities,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn begin_request(&mut self, breakpoints: Vec<RenderedBreakpoint>) {
        self.begin_request_with_tokens(breakpoints, 0);
    }

    #[cfg(test)]
    fn begin_request_with_tokens(
        &mut self,
        breakpoints: Vec<RenderedBreakpoint>,
        request_tokens: usize,
    ) {
        self.begin_request_with_history(breakpoints, &[], request_tokens);
    }

    fn begin_request_with_history(
        &mut self,
        breakpoints: Vec<RenderedBreakpoint>,
        history: &[serde_json::Value],
        request_tokens: usize,
    ) {
        for breakpoint in &breakpoints {
            self.breakpoints
                .entry(breakpoint.generation)
                .and_modify(|tracked| tracked.prefix_tokens = breakpoint.prefix_tokens)
                .or_insert(TrackedBreakpoint {
                    prefix_tokens: breakpoint.prefix_tokens,
                    cached_tokens: 0,
                    activity: None,
                });
        }
        self.pending = breakpoints;
        self.pending_request_tokens = request_tokens;
        self.pending_history = history.to_vec();
    }

    fn observe(&mut self, usage: &Usage) {
        let Some(capabilities) = self.capabilities else {
            self.pending.clear();
            self.pending_request_tokens = 0;
            self.pending_history.clear();
            return;
        };
        let now = Instant::now();
        let cache_activity = usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0;
        if cache_activity && self.pending_request_tokens >= capabilities.minimum_prefix_tokens {
            self.implicit_activity = Some(now);
            self.implicit_cached_tokens = self.pending_request_tokens;
            self.implicit_prefix.clone_from(&self.pending_history);
        }
        let readable = self
            .pending
            .iter()
            .rev()
            .take(capabilities.max_read_breakpoints)
            .filter(|breakpoint| breakpoint.prefix_tokens <= usage.cached_input_tokens as usize)
            .filter(|breakpoint| {
                self.breakpoints
                    .get(&breakpoint.generation)
                    .is_some_and(|tracked| cache_alive(tracked.activity, now))
            })
            .max_by_key(|breakpoint| breakpoint.prefix_tokens)
            .map(|breakpoint| breakpoint.generation);
        if let Some(generation) = readable
            && let Some(tracked) = self.breakpoints.get_mut(&generation)
        {
            tracked.activity = Some(now);
        }

        if usage.cache_write_input_tokens > 0 {
            let explicit_write_slots =
                capabilities
                    .max_write_breakpoints
                    .saturating_sub(usize::from(
                        capabilities.implicit_breakpoint_uses_write_slot,
                    ));
            for breakpoint in self
                .pending
                .iter()
                .filter(|breakpoint| breakpoint.prefix_tokens >= capabilities.minimum_prefix_tokens)
                .rev()
                .take(explicit_write_slots)
            {
                let tracked = self
                    .breakpoints
                    .get_mut(&breakpoint.generation)
                    .expect("pending breakpoints are tracked");
                tracked.cached_tokens = breakpoint.prefix_tokens;
                tracked.activity = Some(now);
            }
        }
        self.pending.clear();
        self.pending_request_tokens = 0;
        self.pending_history.clear();
    }

    #[cfg(test)]
    fn policy(&self) -> CompactionPolicy {
        self.policy_with_implicit_compatibility(true)
    }

    fn policy_for_history(&self, history: &[serde_json::Value]) -> CompactionPolicy {
        self.policy_with_implicit_compatibility(history.starts_with(&self.implicit_prefix))
    }

    fn policy_with_implicit_compatibility(
        &self,
        implicit_prefix_compatible: bool,
    ) -> CompactionPolicy {
        let now = Instant::now();
        let mut breakpoints = self
            .breakpoints
            .iter()
            .filter(|(_, tracked)| cache_alive(tracked.activity, now))
            .map(|(generation, tracked)| PricedBreakpoint {
                generation: *generation,
                cached_tokens: tracked.cached_tokens,
            })
            .collect::<Vec<_>>();
        breakpoints.sort_by_key(|breakpoint| breakpoint.generation);
        let max_read_breakpoints = self
            .capabilities
            .map_or(0, |capabilities| capabilities.max_read_breakpoints);
        if breakpoints.len() > max_read_breakpoints {
            breakpoints.drain(..breakpoints.len() - max_read_breakpoints);
        }
        CompactionPolicy {
            implicit_cached_tokens: if implicit_prefix_compatible
                && cache_alive(self.implicit_activity, now)
            {
                self.implicit_cached_tokens
            } else {
                0
            },
            breakpoints,
        }
    }

    fn implicit_expired(&self) -> bool {
        self.implicit_activity
            .is_some_and(|activity| activity.elapsed() >= CACHE_TTL)
    }

    fn mark_compaction(&mut self, invalidated_generations: &[u64]) {
        self.implicit_activity = None;
        self.implicit_cached_tokens = 0;
        self.implicit_prefix.clear();
        for generation in invalidated_generations {
            self.breakpoints.remove(generation);
        }
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

    fn prompt_cache_capabilities(&self) -> Option<PromptCacheCapabilities> {
        match self {
            Self::OpenAi(client) => client.prompt_cache_capabilities(),
            Self::Scripted { .. } => None,
        }
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
    let prompt_cache_capabilities = backend.prompt_cache_capabilities();
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
            "prompt_cache_capabilities": prompt_cache_capabilities,
            "compaction_decision": "next_request"
        }),
        &format!("carry · {} · {}", config.model, config.cwd.display()),
    )?;

    let mut context_state = ContextState::new_with_max_read_breakpoints(
        config.prompt.clone(),
        prompt_cache_capabilities.map_or(0, |capabilities| capabilities.max_read_breakpoints),
    );
    let mut metrics = RunMetrics::default();
    let mut cache = CacheTracker::new(prompt_cache_capabilities);
    let mut step_index = 0;
    let mut turn_step = 0;
    let mut protected_until_request = Vec::new();
    loop {
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
        let trigger = if cache.implicit_expired() {
            "cache expired"
        } else {
            "economic"
        };
        maybe_compact(
            &mut context_state,
            &protected_until_request,
            &mut cache,
            &mut metrics,
            &mut logger,
            trigger,
        )?;
        step_index += 1;
        turn_step += 1;
        let history = context_state.input_items();
        cache.begin_request_with_history(
            context_state.rendered_breakpoints(),
            &history,
            context_state.estimated_tokens(),
        );
        protected_until_request.clear();
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
                let signals = context_state.record_signals(&reply.step.context, item_id);
                protected_until_request.push(item_id);
                protected_until_request.extend(signals.added.iter().copied());
                logger.event_silent("context_signals", &signals)?;

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
                let signals = context_state.record_signals(&reply.step.context, item_id);
                protected_until_request.push(item_id);
                protected_until_request.extend(signals.added.iter().copied());
                logger.event_silent("context_signals", &signals)?;
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
    let policy = cache.policy_for_history(&state.input_items());
    let Some(plan) = state.plan_compaction_with_neutral_budget(
        protected,
        policy,
        NEUTRAL_VOLATILE_BUDGET_TOKENS,
    ) else {
        return Ok(false);
    };
    let change = state.compact(plan);
    cache.mark_compaction(&change.invalidated_generations);
    metrics.record_compaction();
    logger.raw_event(
        "context_compacted",
        json!({"trigger": trigger, "compaction": &change}),
        &format!(
            "  compact · -{} items / ~{} tok · {} retained · {} rewritten · reuse {} · invalidate {} generations / {} cached tok · next request saves ~{} input-equivalent tok",
            change.dropped.len(),
            compact_number(change.dropped_tokens as u64),
            compact_number(change.retained_tokens as u64),
            compact_number(change.rewrite_tokens as u64),
            change.reused_generation.map_or_else(|| "cold".to_owned(), |generation| generation.to_string()),
            change.invalidated_generations.len(),
            compact_number(change.invalidated_cache_tokens as u64),
            compact_number(change.estimated_savings_input_units.round().max(0.0) as u64),
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
    let stdout_path = tokio::fs::canonicalize(stdout_path).await?;
    let stderr_path = tokio::fs::canonicalize(stderr_path).await?;

    let full_output = combined_output(&stdout, &stderr);
    let prompt_output = preview_tool_output(&full_output);
    let full_output_path = if prompt_output.offloaded {
        let path = run_dir.join("tools").join(format!("{call_id}.output"));
        write_bytes(&path, &full_output).await?;
        Some(tokio::fs::canonicalize(path).await?)
    } else {
        None
    };
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
        full_output_path,
        prompt_output,
    })
}

async fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(bytes).await?;
    Ok(())
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
    let mut output = format!("STDOUT ({} bytes):\n", stdout.len()).into_bytes();
    output.extend_from_slice(stdout);
    output.extend_from_slice(format!("\nSTDERR ({} bytes):\n", stderr.len()).as_bytes());
    output.extend_from_slice(stderr);
    output
}

fn preview_tool_output(output: &[u8]) -> ToolOutputPreview {
    let Ok(text) = std::str::from_utf8(output) else {
        return omitted_binary_output(output.len());
    };

    if output.len() <= TOOL_OUTPUT_INLINE_BYTES
        && json_string_size(text) <= TOOL_OUTPUT_INLINE_BYTES + 2
    {
        return ToolOutputPreview {
            encoding: "utf-8",
            head: text.to_owned(),
            tail: None,
            omitted_bytes: 0,
            omission_reason: None,
            offloaded: false,
        };
    }

    let head_end = bounded_utf8_head_end(text);
    let tail_start = bounded_utf8_tail_start(text);
    if let (Some(head_end), Some(tail_start)) = (head_end, tail_start)
        && head_end <= tail_start
    {
        return ToolOutputPreview {
            encoding: "utf-8",
            head: text[..head_end].to_owned(),
            tail: Some(text[tail_start..].to_owned()),
            omitted_bytes: tail_start - head_end,
            omission_reason: None,
            offloaded: true,
        };
    }

    omitted_binary_output(output.len())
}

fn omitted_binary_output(output_bytes: usize) -> ToolOutputPreview {
    ToolOutputPreview {
        encoding: "binary",
        head: String::new(),
        tail: None,
        omitted_bytes: output_bytes,
        omission_reason: Some(BINARY_OUTPUT_OMISSION_REASON),
        offloaded: true,
    }
}

fn bounded_utf8_head_end(text: &str) -> Option<usize> {
    let mut end = TOOL_OUTPUT_PREVIEW_BYTES.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    for _ in 0..=256 {
        if json_string_size(&text[..end]) <= TOOL_OUTPUT_PREVIEW_BYTES + 2 {
            return Some(end);
        }
        if end == 0 {
            break;
        }
        end -= 1;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
    }
    None
}

fn bounded_utf8_tail_start(text: &str) -> Option<usize> {
    let mut start = text.len().saturating_sub(TOOL_OUTPUT_PREVIEW_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    for _ in 0..=256 {
        if json_string_size(&text[start..]) <= TOOL_OUTPUT_PREVIEW_BYTES + 2 {
            return Some(start);
        }
        if start == text.len() {
            break;
        }
        start += 1;
        while !text.is_char_boundary(start) {
            start += 1;
        }
    }
    None
}

fn json_string_size(value: &str) -> usize {
    serde_json::to_string(value)
        .expect("serializing a string cannot fail")
        .len()
}

fn render_tool_result(result: &ShellResult) -> String {
    let mut payload = json!({
        "context_id": result.call_id,
        "exit_code": result.exit_code,
        "timed_out": result.timed_out,
        "duration_ms": result.duration_ms,
        "stdout_bytes": result.stdout_bytes,
        "stderr_bytes": result.stderr_bytes,
        "full_stdout_path": result.stdout_path,
        "full_stderr_path": result.stderr_path,
        "output_truncated": result.prompt_output.offloaded,
    });
    if let Some(reason) = result.prompt_output.omission_reason {
        payload["output_omitted_reason"] = json!(reason);
        payload["output_omitted_bytes"] = json!(result.prompt_output.omitted_bytes);
        payload["full_output_path"] = json!(
            result
                .full_output_path
                .as_ref()
                .expect("omitted shell output must have a full output path")
        );
    } else if let Some(tail) = result.prompt_output.tail.as_ref() {
        payload["output_encoding"] = json!(result.prompt_output.encoding);
        payload["output_head"] = json!(result.prompt_output.head);
        payload["output_tail"] = json!(tail);
        payload["output_omitted_bytes"] = json!(result.prompt_output.omitted_bytes);
        payload["full_output_path"] = json!(
            result
                .full_output_path
                .as_ref()
                .expect("truncated shell output must have a full output path")
        );
    } else {
        payload["output"] = json!(result.prompt_output.head);
    }
    serde_json::to_string_pretty(&payload).expect("serializing a shell result cannot fail")
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
        "compactions": metrics.compactions,
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
    use crate::openai::PromptCacheCapabilities;

    fn openai_cache_capabilities() -> PromptCacheCapabilities {
        PromptCacheCapabilities {
            minimum_prefix_tokens: 1_024,
            max_read_breakpoints: 50,
            max_write_breakpoints: 4,
            implicit_breakpoint_uses_write_slot: true,
        }
    }

    #[test]
    fn tracker_respects_resolved_minimum_and_write_slots() {
        let mut cache = CacheTracker::new(Some(PromptCacheCapabilities {
            minimum_prefix_tokens: 2_000,
            max_read_breakpoints: 7,
            max_write_breakpoints: 2,
            implicit_breakpoint_uses_write_slot: true,
        }));
        cache.begin_request(vec![
            RenderedBreakpoint {
                generation: 0,
                prefix_tokens: 1_999,
            },
            RenderedBreakpoint {
                generation: 1,
                prefix_tokens: 2_100,
            },
            RenderedBreakpoint {
                generation: 2,
                prefix_tokens: 2_200,
            },
        ]);
        cache.observe(&Usage {
            cache_write_input_tokens: 9_000,
            ..Usage::default()
        });

        assert_eq!(
            cache.policy().breakpoints,
            vec![PricedBreakpoint {
                generation: 2,
                cached_tokens: 2_200,
            }]
        );
    }

    #[test]
    fn unknown_cache_capabilities_disable_cache_assumptions() {
        let mut cache = CacheTracker::new(None);
        cache.begin_request_with_tokens(
            vec![RenderedBreakpoint {
                generation: 1,
                prefix_tokens: 4_000,
            }],
            4_000,
        );
        cache.observe(&Usage {
            cached_input_tokens: 4_000,
            cache_write_input_tokens: 4_000,
            ..Usage::default()
        });

        let policy = cache.policy();
        assert_eq!(policy.implicit_cached_tokens, 0);
        assert!(policy.breakpoints.is_empty());
    }

    #[test]
    fn system_prompt_requires_reproduction_and_history_checks() {
        assert!(SYSTEM_PROMPT.contains("minimal failing reproduction"));
        assert!(SYSTEM_PROMPT.contains("repository history"));
        assert!(SYSTEM_PROMPT.contains("search cited identifiers"));
        assert!(SYSTEM_PROMPT.contains("affected tests"));
    }

    #[test]
    fn small_combined_output_stays_inline() {
        let full = combined_output(b"all stdout", b"all stderr");
        let preview = preview_tool_output(&full);
        assert_eq!(preview.encoding, "utf-8");
        assert_eq!(preview.head, String::from_utf8(full).unwrap());
        assert!(preview.tail.is_none());
        assert_eq!(preview.omitted_bytes, 0);
    }

    #[test]
    fn exact_inline_byte_boundary_is_not_offloaded() {
        let exact = vec![b'x'; TOOL_OUTPUT_INLINE_BYTES];
        assert!(preview_tool_output(&exact).tail.is_none());

        let over = vec![b'x'; TOOL_OUTPUT_INLINE_BYTES + 1];
        assert!(preview_tool_output(&over).tail.is_some());
    }

    #[test]
    fn utf8_preview_boundaries_do_not_split_code_points() {
        let full = "🦀".repeat((TOOL_OUTPUT_INLINE_BYTES / 4) + 1).into_bytes();
        let preview = preview_tool_output(&full);
        assert_eq!(preview.encoding, "utf-8");
        let tail = preview.tail.as_ref().unwrap();
        assert!(!preview.head.contains('�'));
        assert!(!tail.contains('�'));
        assert_eq!(
            preview.head.len() + preview.omitted_bytes + tail.len(),
            full.len()
        );
    }

    #[test]
    fn binary_output_is_omitted_from_the_model_payload() {
        let full = vec![0; TOOL_OUTPUT_INLINE_BYTES + 1];
        let preview = preview_tool_output(&full);
        assert_eq!(preview.encoding, "binary");
        assert!(preview.head.is_empty());
        assert!(preview.tail.is_none());
        assert_eq!(preview.omitted_bytes, full.len());
        assert_eq!(preview.omission_reason, Some(BINARY_OUTPUT_OMISSION_REASON));
        assert!(preview.offloaded);
    }

    #[test]
    fn invalid_utf8_output_is_omitted_from_the_model_payload() {
        let mut full = vec![b'x'; TOOL_OUTPUT_INLINE_BYTES + 1];
        full[100] = 0xff;
        let preview = preview_tool_output(&full);
        assert_eq!(preview.encoding, "binary");
        assert!(preview.head.is_empty());
        assert!(preview.tail.is_none());
        assert_eq!(preview.omitted_bytes, full.len());
        assert_eq!(preview.omission_reason, Some(BINARY_OUTPUT_OMISSION_REASON));
        assert!(preview.offloaded);
    }

    #[tokio::test]
    async fn large_shell_output_is_offloaded_with_structured_head_and_tail() {
        let current_dir = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(&current_dir).unwrap();
        let relative_root = temp.path().strip_prefix(&current_dir).unwrap();
        let run_dir = relative_root.join("session");
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(run_dir.join("tools"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let result = execute_shell(
            &workspace,
            &run_dir,
            "tool-1".into(),
            "python3 -c \"import sys; sys.stdout.write('h'*16384 + 'm'*4096 + 't'*16384)\"",
            30,
        )
        .await
        .unwrap();

        let rendered: serde_json::Value =
            serde_json::from_str(&render_tool_result(&result)).unwrap();
        assert_eq!(rendered["output_truncated"], true);
        assert_eq!(rendered["output_encoding"], "utf-8");
        let head = rendered["output_head"].as_str().unwrap();
        let tail = rendered["output_tail"].as_str().unwrap();
        assert!(head.len() <= 16 * 1024 && head.len() > 15 * 1024);
        assert!(tail.len() <= 16 * 1024 && tail.len() > 15 * 1024);
        assert!(serde_json::to_string(head).unwrap().len() <= 16 * 1024 + 2);
        assert!(serde_json::to_string(tail).unwrap().len() <= 16 * 1024 + 2);
        assert!(rendered.get("output").is_none());
        assert!(rendered["output_omitted_bytes"].as_u64().unwrap() > 0);
        let full_path = rendered["full_output_path"].as_str().unwrap();
        assert!(std::path::Path::new(full_path).is_absolute());
        assert!(std::path::Path::new(rendered["full_stdout_path"].as_str().unwrap()).is_absolute());
        assert!(std::path::Path::new(rendered["full_stderr_path"].as_str().unwrap()).is_absolute());
        let full = tokio::fs::read(full_path).await.unwrap();
        assert!(full.starts_with(b"STDOUT ("));
        assert!(
            full.windows(32)
                .any(|window| window == b"mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm")
        );
        assert!(
            !rendered["output_head"]
                .as_str()
                .unwrap()
                .contains("truncat")
        );
        assert!(
            !rendered["output_tail"]
                .as_str()
                .unwrap()
                .contains("truncat")
        );
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
    fn policy_prices_the_exact_previous_implicit_prefix() {
        let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
        cache.begin_request_with_tokens(Vec::new(), 2_400);
        cache.observe(&Usage {
            cache_write_input_tokens: 2_400,
            ..Usage::default()
        });

        assert_eq!(cache.policy().implicit_cached_tokens, 2_400);
    }

    #[test]
    fn policy_rejects_a_mutated_previous_implicit_prefix() {
        let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
        let original = vec![json!({"role": "user", "content": "original"})];
        cache.begin_request_with_history(Vec::new(), &original, 2_400);
        cache.observe(&Usage {
            cache_write_input_tokens: 2_400,
            ..Usage::default()
        });

        let extended = vec![
            json!({"role": "user", "content": "original"}),
            json!({"type": "function_call_output", "output": "new"}),
        ];
        assert_eq!(
            cache.policy_for_history(&extended).implicit_cached_tokens,
            2_400
        );
        let mutated = vec![json!({"role": "user", "content": "changed"})];
        assert_eq!(cache.policy_for_history(&mutated).implicit_cached_tokens, 0);
    }

    #[test]
    fn eligible_explicit_breakpoint_writes_are_tracked_by_generation() {
        let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
        cache.begin_request(vec![
            RenderedBreakpoint {
                generation: 0,
                prefix_tokens: 1_023,
            },
            RenderedBreakpoint {
                generation: 1,
                prefix_tokens: 1_200,
            },
            RenderedBreakpoint {
                generation: 2,
                prefix_tokens: 1_400,
            },
        ]);
        cache.observe(&Usage {
            cache_write_input_tokens: 7_200,
            ..Usage::default()
        });

        let mut priced = cache.policy().breakpoints;
        priced.sort_by_key(|breakpoint| breakpoint.generation);
        assert_eq!(
            priced,
            vec![
                PricedBreakpoint {
                    generation: 1,
                    cached_tokens: 1_200,
                },
                PricedBreakpoint {
                    generation: 2,
                    cached_tokens: 1_400,
                },
            ]
        );
    }

    #[test]
    fn one_explicit_write_is_not_divided_with_the_implicit_slot() {
        let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
        cache.begin_request(vec![RenderedBreakpoint {
            generation: 1,
            prefix_tokens: 1_200,
        }]);
        cache.observe(&Usage {
            cache_write_input_tokens: 1_200,
            ..Usage::default()
        });

        assert_eq!(
            cache.policy().breakpoints,
            vec![PricedBreakpoint {
                generation: 1,
                cached_tokens: 1_200,
            }]
        );
    }

    #[test]
    fn implicit_slot_limits_explicit_writes_to_latest_three_generations() {
        let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
        cache.begin_request(
            (0..4)
                .map(|generation| RenderedBreakpoint {
                    generation,
                    prefix_tokens: 1_100,
                })
                .collect(),
        );
        cache.observe(&Usage {
            cache_write_input_tokens: 8_000,
            ..Usage::default()
        });

        let mut generations = cache
            .policy()
            .breakpoints
            .iter()
            .map(|breakpoint| breakpoint.generation)
            .collect::<Vec<_>>();
        generations.sort_unstable();
        assert_eq!(generations, vec![1, 2, 3]);
    }

    #[test]
    fn planner_prices_only_the_resolved_number_of_readable_breakpoints() {
        let now = Instant::now();
        let cache = CacheTracker {
            capabilities: Some(PromptCacheCapabilities {
                max_read_breakpoints: 2,
                ..openai_cache_capabilities()
            }),
            implicit_activity: Some(now),
            implicit_cached_tokens: 1_500,
            implicit_prefix: Vec::new(),
            breakpoints: (0..3)
                .map(|generation| {
                    (
                        generation,
                        TrackedBreakpoint {
                            prefix_tokens: 1_200,
                            cached_tokens: 1_100,
                            activity: Some(now),
                        },
                    )
                })
                .collect(),
            pending: Vec::new(),
            pending_request_tokens: 0,
            pending_history: Vec::new(),
        };

        let mut generations = cache
            .policy()
            .breakpoints
            .iter()
            .map(|breakpoint| breakpoint.generation)
            .collect::<Vec<_>>();
        generations.sort_unstable();
        assert_eq!(generations, vec![1, 2]);
    }

    #[test]
    fn aggregate_reads_do_not_resurrect_an_expired_generation() {
        let mut cache = CacheTracker {
            capabilities: Some(openai_cache_capabilities()),
            implicit_activity: None,
            implicit_cached_tokens: 0,
            implicit_prefix: Vec::new(),
            breakpoints: HashMap::from([(
                7,
                TrackedBreakpoint {
                    prefix_tokens: 1_200,
                    cached_tokens: 1_100,
                    activity: Some(Instant::now() - CACHE_TTL),
                },
            )]),
            pending: Vec::new(),
            pending_request_tokens: 0,
            pending_history: Vec::new(),
        };
        cache.begin_request(vec![RenderedBreakpoint {
            generation: 7,
            prefix_tokens: 1_200,
        }]);
        cache.observe(&Usage {
            cached_input_tokens: 2_048,
            ..Usage::default()
        });

        assert!(cache.policy().breakpoints.is_empty());
    }

    #[tokio::test]
    async fn binary_shell_output_is_only_exposed_through_artifact_paths() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("session");
        tokio::fs::create_dir_all(run_dir.join("tools"))
            .await
            .unwrap();
        let result = execute_shell(
            temp.path(),
            &run_dir,
            "tool-binary".into(),
            "python3 -c \"import sys; sys.stdout.buffer.write(bytes([0xff, 0, 1]))\"",
            30,
        )
        .await
        .unwrap();

        let rendered: serde_json::Value =
            serde_json::from_str(&render_tool_result(&result)).unwrap();
        assert_eq!(rendered["output_truncated"], true);
        assert_eq!(
            rendered["output_omitted_reason"],
            BINARY_OUTPUT_OMISSION_REASON
        );
        assert!(rendered.get("output").is_none());
        assert!(rendered.get("output_head").is_none());
        assert!(rendered.get("output_tail").is_none());
        let full_path = rendered["full_output_path"].as_str().unwrap();
        let full = tokio::fs::read(full_path).await.unwrap();
        assert!(full.windows(3).any(|window| window == [0xff, 0, 1]));
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
        assert_eq!(result["compactions"], 0);
        assert!(result["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn finish_does_not_compact_without_another_model_request() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"keep":[],"drop":[1],"remember":[]}}"#,
        )
        .await
        .unwrap();

        run(
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

        let trace = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap();
        assert!(!trace.contains("context_compacted"));
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
        assert_eq!(result["compactions"], 0);
        let trace_jsonl = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap();
        assert!(!trace_jsonl.contains("context_compacted"));
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
        let tool_result = history
            .iter()
            .position(|item| {
                item["type"] == "function_call_output"
                    && item["output"]
                        .as_str()
                        .is_some_and(|output| output.ends_with("[context 2 volatile]"))
            })
            .unwrap();
        let steering = history
            .iter()
            .position(|item| item["content"][0]["text"] == "do not change the JSON format")
            .unwrap();
        assert!(tool_result < steering);
        assert_eq!(
            history[steering + 1]["content"][0]["text"],
            "[context 3 stable]"
        );
    }
}
