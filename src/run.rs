use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{broadcast, mpsc},
    time::Duration,
};

use crate::{
    context::{
        CompactionPlan, CompactionPolicy, ContextState, FlatRolloutConfig, FlatRolloutEstimate,
        PricedBreakpoint, RenderedBreakpoint, meets_rollout_payback_threshold,
    },
    log::RunLogger,
    openai::{
        ModelProgress, ModelReply, OpenAiClient, PromptCacheCapabilities, Usage,
        prompt_cache_capabilities,
    },
    protocol::{ActionKind, Step},
};

const SYSTEM_PROMPT: &str = r#"You are a coding agent working iteratively in an assigned repository.

Make task progress first: understand the request, investigate, implement, and verify before finishing. Establish a minimal failing reproduction before editing when practical. When practical, identify the root cause and make the smallest correct fix at the appropriate layer; use local history to investigate regressions when it is available. Run affected tests before finishing. Use the optional shell message for concise progress commentary.

Before working in a folder, search for relevant `AGENTS.md` or `CLAUDE.md` files and read them to understand agent-specific guidance; take relevant guidance onboard.

Large stdout and stderr results arrive in separate structured sections. Each text payload is unmodified; truncation, encoding, and artifact-path metadata are outside that payload. Read or slice the relevant stdout/stderr artifact when omitted details matter.

History is a working set, not a complete transcript. Human-authored content is kept by default. All other context is eligible for removal when it no longer fits the working set. After the first removal, a history-status item states that earlier context has been removed.

As required secondary housekeeping, preserve task-critical working state from recently added visible context. This is required, not optional cleanup: protect exact facts, decisions, constraints, diagnoses, and verified results that will matter to later work. If you learned anything from an item that is not already preserved elsewhere, protect it. If only a concise learning must remain, remember it and leave its bulky source removable, or mark it removable if it was protected. Leave or mark an item removable only when it taught you nothing or everything learned from it is preserved elsewhere. Finishing an action or encountering a failure does not by itself preserve its learning.

Retention decisions persist until reversed, applied by compaction, or explicitly noted otherwise. Preserve outcomes, not chain-of-thought."#;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    Economic,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub cwd: PathBuf,
    pub prompt: String,
    pub session_dir: PathBuf,
    pub model: String,
    pub max_steps: Option<usize>,
    pub shell_timeout_secs: u64,
    pub compaction_mode: CompactionMode,
    /// Experimental: revalidate model-requested protected context after this many model turns.
    pub keep_lease_turns: Option<u64>,
    /// Number of future requests used to amortize a compaction rewrite; one is next-request economics.
    pub compaction_payoff_requests: u64,
    /// Zero disables deterministic flat-drop scenario rollouts before compaction.
    pub compaction_rollout_samples: u32,
    /// Per simulated future turn probability (percent) that the task ends.
    pub compaction_rollout_stop_probability_percent: u8,
    /// Eligible neutral token high-water mark before automatic compaction.
    pub compaction_neutral_high_watermark_tokens: usize,
    /// Eligible neutral token target after automatic compaction.
    pub compaction_neutral_low_watermark_tokens: usize,
    pub resume_context: Option<ContextState>,
    pub resume_source: Option<PathBuf>,
    /// Stable provider cache affinity, retained with the resumable state.
    pub prompt_cache_key: Option<String>,
}

const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const CONTEXT_CHECKPOINT_FILE: &str = "context-state.json";
const PATCH_BASELINE_FILE: &str = "patch-baseline-revision";
const CONTEXT_CHECKPOINT_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct ResumeState {
    pub context: ContextState,
    pub model: String,
    pub prompt_cache_key: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ContextCheckpoint {
    version: u32,
    model: String,
    context: ContextState,
    /// Missing from pre-cache-affinity checkpoints; such sessions resume normally.
    #[serde(default)]
    prompt_cache_key: Option<String>,
}

pub fn load_resume_state(session_dir: &Path) -> Result<ResumeState> {
    let path = session_dir.join(CONTEXT_CHECKPOINT_FILE);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("failed to read context checkpoint: {}", path.display()))?;
    let checkpoint: ContextCheckpoint = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid context checkpoint: {}", path.display()))?;
    if checkpoint.version != CONTEXT_CHECKPOINT_VERSION {
        bail!(
            "unsupported context checkpoint version {} in {}",
            checkpoint.version,
            path.display()
        );
    }
    ContextState::decode(&checkpoint.context.encode()?)?;
    Ok(ResumeState {
        context: checkpoint.context,
        model: checkpoint.model,
        prompt_cache_key: checkpoint.prompt_cache_key,
    })
}

fn persist_context_checkpoint(config: &RunConfig, state: &ContextState) -> Result<()> {
    std::fs::create_dir_all(&config.session_dir).with_context(|| {
        format!(
            "failed to create context checkpoint directory: {}",
            config.session_dir.display()
        )
    })?;
    let path = config.session_dir.join(CONTEXT_CHECKPOINT_FILE);
    let checkpoint = ContextCheckpoint {
        version: CONTEXT_CHECKPOINT_VERSION,
        model: config.model.clone(),
        context: state.clone(),
        prompt_cache_key: config.prompt_cache_key.clone(),
    };
    let bytes = serde_json::to_vec(&checkpoint)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = config.session_dir.join(format!(
        ".{CONTEXT_CHECKPOINT_FILE}.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("failed to create context checkpoint: {}", temp.display()))?;
    use std::io::Write as _;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write context checkpoint: {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync context checkpoint: {}", temp.display()))?;
    std::fs::rename(&temp, &path)
        .with_context(|| format!("failed to publish context checkpoint: {}", path.display()))?;
    Ok(())
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
    pub session_dir: PathBuf,
}

#[derive(Debug)]
pub enum UserInput {
    Message(String),
    Exit,
}

const TOOL_OUTPUT_INLINE_BYTES: usize = 10 * 1024;
const TOOL_OUTPUT_PREVIEW_BYTES: usize = TOOL_OUTPUT_INLINE_BYTES / 2;
const BINARY_OUTPUT_OMISSION_REASON: &str =
    "Shell stream is not UTF-8 text or appears binary; inspect its artifact path instead.";

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
struct ShellOutputPreview {
    stdout: ToolOutputPreview,
    stderr: ToolOutputPreview,
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
    prompt_output: ShellOutputPreview,
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
    implicit_minimum_prefix_tokens: Option<usize>,
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
    #[cfg(test)]
    fn new(capabilities: Option<PromptCacheCapabilities>) -> Self {
        let implicit_minimum_prefix_tokens =
            capabilities.map(|capabilities| capabilities.minimum_prefix_tokens);
        Self::new_with_implicit_minimum(capabilities, implicit_minimum_prefix_tokens)
    }

    fn new_with_implicit_minimum(
        capabilities: Option<PromptCacheCapabilities>,
        implicit_minimum_prefix_tokens: Option<usize>,
    ) -> Self {
        Self {
            capabilities,
            implicit_minimum_prefix_tokens,
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
        let now = Instant::now();
        let cache_activity = usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0;
        let estimated_cache_write = self
            .implicit_minimum_prefix_tokens
            .is_some_and(|minimum| self.pending_request_tokens >= minimum);
        if cache_activity || estimated_cache_write {
            self.implicit_activity = Some(now);
            self.implicit_cached_tokens = self.pending_request_tokens;
            self.implicit_prefix.clone_from(&self.pending_history);
        }
        let Some(capabilities) = self.capabilities else {
            self.pending.clear();
            self.pending_request_tokens = 0;
            self.pending_history.clear();
            return;
        };
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
        self.policy_with_implicit_compatibility(true, 1)
    }

    fn policy_for_history(
        &self,
        history: &[serde_json::Value],
        payoff_requests: u64,
    ) -> CompactionPolicy {
        self.policy_with_implicit_compatibility(
            history.starts_with(&self.implicit_prefix),
            payoff_requests,
        )
    }

    fn policy_with_implicit_compatibility(
        &self,
        implicit_prefix_compatible: bool,
        payoff_requests: u64,
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
            payoff_requests,
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

    fn implicit_cache_minimum_prefix_tokens(&self, model: &str) -> Option<usize> {
        match self {
            Self::OpenAi(_) => prompt_cache_capabilities(model)
                .map(|capabilities| capabilities.minimum_prefix_tokens),
            Self::Scripted { .. } => None,
        }
    }

    fn request_body(&self, history: &[serde_json::Value]) -> Option<serde_json::Value> {
        match self {
            Self::OpenAi(client) => Some(client.request_body(SYSTEM_PROMPT, history)),
            Self::Scripted { .. } => None,
        }
    }

    fn request_metadata(&self) -> serde_json::Value {
        match self {
            Self::OpenAi(client) => json!({
                "request_timeout_ms": client.request_timeout().as_millis(),
                "connect_timeout_ms": client.connect_timeout().as_millis(),
            }),
            Self::Scripted { .. } => json!({"scripted": true}),
        }
    }

    async fn step_with_progress<F>(
        &mut self,
        history: &[serde_json::Value],
        progress: F,
    ) -> Result<ModelReply>
    where
        F: FnMut(ModelProgress),
    {
        match self {
            Self::OpenAi(client) => {
                client
                    .step_with_progress(SYSTEM_PROMPT, history, progress)
                    .await
            }
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
    run_loop(config, &mut backend, None, None).await
}

pub async fn run_interactive(
    config: RunConfig,
    mut backend: Backend,
    input: mpsc::UnboundedReceiver<UserInput>,
) -> Result<RunOutcome> {
    run_loop(config, &mut backend, Some(input), None).await
}

pub async fn run_interactive_with_events(
    config: RunConfig,
    mut backend: Backend,
    input: mpsc::UnboundedReceiver<UserInput>,
    events: broadcast::Sender<serde_json::Value>,
) -> Result<RunOutcome> {
    run_loop(config, &mut backend, Some(input), Some(events)).await
}

async fn run_loop(
    config: RunConfig,
    backend: &mut Backend,
    mut input: Option<mpsc::UnboundedReceiver<UserInput>>,
    events: Option<broadcast::Sender<serde_json::Value>>,
) -> Result<RunOutcome> {
    let run_started = Instant::now();
    let prompt_cache_capabilities = backend.prompt_cache_capabilities();
    let implicit_cache_minimum_prefix_tokens =
        backend.implicit_cache_minimum_prefix_tokens(&config.model);
    tokio::fs::create_dir_all(config.session_dir.join("tools")).await?;
    let patch_baseline = capture_patch_baseline(&config).await?;
    let resumed = config.resume_context.is_some();
    let mut trace_recovery = None;
    let mut logger = if config.resume_source.as_ref() == Some(&config.session_dir) {
        match RunLogger::resume_with_events(&config.session_dir, events.clone()) {
            Ok(logger) => logger,
            Err(error) => {
                trace_recovery = Some(error.to_string());
                RunLogger::recovery_segment_with_events(&config.session_dir, events.clone())?
            }
        }
    } else {
        RunLogger::create_with_events(&config.session_dir, events.clone())?
    };
    if let Some(reason) = trace_recovery {
        logger.raw_event_silent("trace_recovery", json!({"reason": reason}))?;
    }
    if let Some(context) = config.resume_context.as_ref() {
        logger.raw_event(
            "session_resumed",
            json!({
                "cwd": config.cwd,
                "model": config.model,
                "history_items": context.input_items().len(),
                "cache_ttl_seconds": CACHE_TTL.as_secs(),
                "prompt_cache_capabilities": prompt_cache_capabilities,
                "implicit_cache_minimum_prefix_tokens": implicit_cache_minimum_prefix_tokens,
                "compaction_policy": config.compaction_mode,
                "source_session": config.resume_source,
            }),
            &format!(
                "resumed · {} · {} history items",
                config.model,
                context.input_items().len()
            ),
        )?;
    } else {
        logger.raw_event(
            "run_started",
            json!({
                "cwd": config.cwd,
                "prompt": config.prompt,
                "model": config.model,
                "max_steps": config.max_steps,
                "cache_ttl_seconds": CACHE_TTL.as_secs(),
                "prompt_cache_capabilities": prompt_cache_capabilities,
                "implicit_cache_minimum_prefix_tokens": implicit_cache_minimum_prefix_tokens,
                "compaction_policy": config.compaction_mode,
                "compaction_decision": "next_request"
            }),
            &format!("carry · {} · {}", config.model, config.cwd.display()),
        )?;
    }

    let max_read_breakpoints =
        prompt_cache_capabilities.map_or(0, |capabilities| capabilities.max_read_breakpoints);
    let mut context_state = config.resume_context.clone().unwrap_or_else(|| {
        ContextState::new_with_max_read_breakpoints(config.prompt.clone(), max_read_breakpoints)
    });
    if resumed && !config.prompt.trim().is_empty() {
        let id = context_state.add_user(config.prompt.clone());
        logger.raw_event(
            "human_message",
            json!({"context_id": id, "message": config.prompt}),
            &format!("  prompt [{id}] submitted"),
        )?;
    } else if !resumed {
        logger.raw_event(
            "human_message",
            json!({"context_id": 1, "message": config.prompt}),
            "  prompt [1] submitted",
        )?;
    }
    persist_context_checkpoint(&config, &context_state)?;
    let mut metrics = RunMetrics::default();
    let mut cache = CacheTracker::new_with_implicit_minimum(
        prompt_cache_capabilities,
        implicit_cache_minimum_prefix_tokens,
    );
    // A resumed provider session gets one unmodified request to reuse its persisted cache key.
    let mut sent_model_request = false;
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
                patch_baseline.as_deref(),
                false,
                None,
                &metrics,
                run_started.elapsed().as_millis() as u64,
            )
            .await?;
            persist_context_checkpoint(&config, &context_state)?;
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
        if config.compaction_mode == CompactionMode::Economic && (!resumed || sent_model_request) {
            maybe_compact(
                &mut context_state,
                &protected_until_request,
                &mut cache,
                &mut metrics,
                &mut logger,
                &config,
                trigger,
            )?;
            persist_context_checkpoint(&config, &context_state)?;
        }
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
        let request_started = Instant::now();
        logger.raw_event_silent(
            "model_request",
            json!({
                "step": step_index,
                "history": history,
                "request": request,
                "transport": backend.request_metadata()
            }),
        )?;

        let progress_events = events.clone();
        let mut last_progress = None;
        let reply = match backend
            .step_with_progress(&history, |progress| {
                if last_progress
                    .as_ref()
                    .is_some_and(|previous: &ModelProgress| {
                        previous.output_tokens == progress.output_tokens
                            && previous.reasoning_output_tokens == progress.reasoning_output_tokens
                            && previous.output_events == progress.output_events
                    })
                {
                    return;
                }
                eprint!(
                    "\r  model streaming · ~{} output tokens · {} events",
                    progress.output_tokens, progress.output_events
                );
                if let Some(events) = &progress_events {
                    let _ = events.send(json!({"event":"model_progress", "data": {
                        "step": step_index,
                        "output_tokens": progress.output_tokens,
                        "reasoning_output_tokens": progress.reasoning_output_tokens,
                        "output_events": progress.output_events,
                        "estimated": true,
                    }}));
                }
                last_progress = Some(progress);
            })
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                logger.raw_event(
                    "model_error",
                    json!({
                        "step": step_index,
                        "latency_ms": request_started.elapsed().as_millis(),
                        "transport": backend.request_metadata(),
                        "error": format!("{error:#}"),
                    }),
                    &format!(
                        "  model request failed after {}ms: {error}",
                        request_started.elapsed().as_millis()
                    ),
                )?;
                return Err(error);
            }
        };
        if last_progress.is_some() {
            eprintln!();
        }
        metrics.record(&reply.usage, reply.latency_ms, reply.response_retries);
        sent_model_request = true;
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
                logger.raw_event(
                    "shell_finished",
                    json!({"result": &result, "context_id": item_id}),
                    &terminal_shell_result(&result),
                )?;
                let signals = context_state.record_signals(&reply.step.context, item_id);
                let expired_keep_leases = if let Some(lease_turns) = config.keep_lease_turns {
                    context_state.advance_retention_turn();
                    let expired = context_state.resolve_keep_lease_review(&signals.keep);
                    context_state.arm_keep_leases(&signals.keep, lease_turns);
                    expired
                } else {
                    Vec::new()
                };
                protected_until_request.push(item_id);
                protected_until_request.extend(signals.added.iter().copied());
                maybe_attach_keep_lease_review(
                    &mut context_state,
                    &protected_until_request,
                    &mut cache,
                    &mut logger,
                    &config,
                    item_id,
                )?;
                logger.raw_event_silent(
                    "context_signals",
                    json!({"source_id": item_id, "signals": &signals, "expired_keep_leases": expired_keep_leases}),
                )?;
                persist_context_checkpoint(&config, &context_state)?;

                if let Some(receiver) = input.as_mut()
                    && drain_user_input(receiver, &mut context_state, &mut logger, &config)?
                {
                    write_final_artifacts(
                        &config,
                        patch_baseline.as_deref(),
                        true,
                        None,
                        &metrics,
                        run_started.elapsed().as_millis() as u64,
                    )
                    .await?;
                    persist_context_checkpoint(&config, &context_state)?;
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
                let expired_keep_leases = if let Some(lease_turns) = config.keep_lease_turns {
                    context_state.advance_retention_turn();
                    let expired = context_state.resolve_keep_lease_review(&signals.keep);
                    context_state.arm_keep_leases(&signals.keep, lease_turns);
                    expired
                } else {
                    Vec::new()
                };
                protected_until_request.push(item_id);
                protected_until_request.extend(signals.added.iter().copied());
                if input.is_some() {
                    maybe_attach_keep_lease_review(
                        &mut context_state,
                        &protected_until_request,
                        &mut cache,
                        &mut logger,
                        &config,
                        item_id,
                    )?;
                }
                logger.raw_event_silent(
                    "context_signals",
                    json!({"source_id": item_id, "signals": &signals, "expired_keep_leases": expired_keep_leases}),
                )?;
                persist_context_checkpoint(&config, &context_state)?;
                logger.raw_event(
                    if input.is_some() {
                        "turn_finished"
                    } else {
                        "run_finished"
                    },
                    json!({
                        "step": step_index,
                        "answer": answer,
                        "context_id": item_id,
                        "retained_context": context_state.snapshot()
                    }),
                    &terminal_finished(step_index, &metrics.usage),
                )?;
                if let Some(receiver) = input.as_mut() {
                    println!("{}", answer.as_deref().unwrap_or_default());
                    let mut should_exit =
                        drain_user_input(receiver, &mut context_state, &mut logger, &config)?;
                    if !should_exit {
                        eprint!("carry> ");
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                        match receiver.recv().await {
                            Some(UserInput::Message(message)) => {
                                append_user_message(
                                    &config,
                                    &mut context_state,
                                    &mut logger,
                                    message,
                                    false,
                                )?;
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
                    patch_baseline.as_deref(),
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
    config: &RunConfig,
) -> Result<bool> {
    let mut should_exit = false;
    while let Ok(input) = receiver.try_recv() {
        match input {
            UserInput::Message(message) => {
                append_user_message(config, state, logger, message, true)?;
            }
            UserInput::Exit => should_exit = true,
        }
    }
    Ok(should_exit)
}

fn append_user_message(
    config: &RunConfig,
    state: &mut ContextState,
    logger: &mut RunLogger,
    message: String,
    steering: bool,
) -> Result<()> {
    let id = state.add_user(message.clone());
    let terminal = if steering {
        format!("  steering [{id}] queued")
    } else {
        format!("  prompt [{id}] submitted")
    };
    logger.raw_event(
        "human_message",
        json!({"context_id": id, "message": message}),
        &terminal,
    )?;
    persist_context_checkpoint(config, state)
}

fn select_compaction_plan(
    state: &ContextState,
    protected: &[u64],
    cache: &CacheTracker,
    config: &RunConfig,
) -> Option<(CompactionPlan, Option<FlatRolloutEstimate>)> {
    let policy = cache.policy_for_history(&state.input_items(), config.compaction_payoff_requests);
    if config.compaction_rollout_samples > 0 {
        let rollout_config = FlatRolloutConfig {
            samples: config.compaction_rollout_samples,
            horizon: config.compaction_payoff_requests,
            seed: 0,
            stop_probability_percent: config.compaction_rollout_stop_probability_percent,
        };
        state
            .compaction_candidates_with_neutral_watermarks(
                protected,
                policy.clone(),
                config.compaction_neutral_high_watermark_tokens,
                config.compaction_neutral_low_watermark_tokens,
            )
            .into_iter()
            .map(|plan| {
                let rollout =
                    state.flat_rollout_estimate(&plan, protected, policy.clone(), rollout_config);
                (plan, rollout)
            })
            .filter(|(_, rollout)| meets_rollout_payback_threshold(rollout))
            .max_by(|(_, left), (_, right)| {
                left.expected_savings_input_units
                    .total_cmp(&right.expected_savings_input_units)
            })
            .map(|(plan, rollout)| (plan, Some(rollout)))
    } else {
        state
            .plan_compaction_with_neutral_watermarks(
                protected,
                policy,
                config.compaction_neutral_high_watermark_tokens,
                config.compaction_neutral_low_watermark_tokens,
            )
            .map(|plan| (plan, None))
    }
}

/// Attach a keep-lease review to the tool result that was just returned, while
/// it is still fresh trailing content. Rewriting an older, already-rendered
/// result instead would invalidate the cached prefix. This keeps the planner
/// gate from `select_compaction_plan`: the review only fires when releasing
/// the due leases would make a compaction worthwhile.
fn maybe_attach_keep_lease_review(
    state: &mut ContextState,
    protected_until_request: &[u64],
    cache: &mut CacheTracker,
    logger: &mut RunLogger,
    config: &RunConfig,
    host_id: u64,
) -> Result<()> {
    if config.compaction_mode != CompactionMode::Economic || config.keep_lease_turns.is_none() {
        return Ok(());
    }
    if select_compaction_plan(state, protected_until_request, cache, config).is_some() {
        return Ok(());
    }
    let Some((virtual_release, virtually_released_ids)) = state.virtual_release_due_keep_leases()
    else {
        return Ok(());
    };
    let virtual_protected = protected_until_request
        .iter()
        .copied()
        .filter(|id| !virtually_released_ids.contains(id))
        .collect::<Vec<_>>();
    if select_compaction_plan(&virtual_release, &virtual_protected, cache, config).is_none() {
        return Ok(());
    }
    let review = state.attach_due_keep_lease_review_to(host_id);
    if !review.item_ids.is_empty() {
        logger.raw_event_silent(
            "retention_revalidation_requested",
            json!({
                "item_ids": review.item_ids,
                "selection_scope": "all_due_virtual_release"
            }),
        )?;
    }
    Ok(())
}

fn maybe_compact(
    state: &mut ContextState,
    protected: &[u64],
    cache: &mut CacheTracker,
    metrics: &mut RunMetrics,
    logger: &mut RunLogger,
    config: &RunConfig,
    trigger: &str,
) -> Result<bool> {
    let Some((plan, rollout)) = select_compaction_plan(state, protected, cache, config) else {
        return Ok(false);
    };
    let change = state.compact(plan);
    cache.mark_compaction(&change.invalidated_generations);
    metrics.record_compaction();
    logger.raw_event(
        "context_compacted",
        json!({
            "trigger": trigger,
            "compaction": &change,
            "retained_context": state.snapshot(),
            "rollout": rollout,
        }),
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
    let mut command_builder = Command::new("/bin/sh");
    command_builder
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_shell_process_group(&mut command_builder);

    let mut child = command_builder
        .spawn()
        .context("failed to start shell command")?;
    let stdout = child
        .stdout
        .take()
        .context("shell stdout was not captured")?;
    let stderr = child
        .stderr
        .take()
        .context("shell stderr was not captured")?;
    let stdout_reader = tokio::spawn(read_shell_stream(stdout));
    let stderr_reader = tokio::spawn(read_shell_stream(stderr));

    let status = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
    let (exit_code, timed_out) = match status {
        Ok(status) => (
            status
                .context("failed while waiting for shell command")?
                .code(),
            false,
        ),
        Err(_) => {
            terminate_shell_process_group(&mut child)?;
            (
                child
                    .wait()
                    .await
                    .context("failed to reap timed-out shell command")?
                    .code(),
                true,
            )
        }
    };
    let stdout = stdout_reader.await.context("stdout reader task failed")??;
    let mut stderr = stderr_reader.await.context("stderr reader task failed")??;
    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with(b"\n") {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(b"command timed out\n");
    }

    let stdout_path = run_dir.join("tools").join(format!("{call_id}.stdout"));
    let stderr_path = run_dir.join("tools").join(format!("{call_id}.stderr"));
    write_bytes(&stdout_path, &stdout).await?;
    write_bytes(&stderr_path, &stderr).await?;
    let stdout_path = tokio::fs::canonicalize(stdout_path).await?;
    let stderr_path = tokio::fs::canonicalize(stderr_path).await?;

    let prompt_output = ShellOutputPreview {
        stdout: preview_tool_output(&stdout),
        stderr: preview_tool_output(&stderr),
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
        prompt_output,
    })
}

async fn read_shell_stream<R>(mut stream: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

#[cfg(unix)]
fn configure_shell_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_shell_process_group(_: &mut Command) {}

#[cfg(unix)]
fn terminate_shell_process_group(child: &mut tokio::process::Child) -> Result<()> {
    let pid = child
        .id()
        .context("timed-out shell command has no process ID")? as i32;
    // The shell is the process-group leader, so a negative PID targets only its descendants.
    let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error).context("failed to terminate timed-out shell process group");
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_shell_process_group(child: &mut tokio::process::Child) -> Result<()> {
    child
        .start_kill()
        .context("failed to terminate timed-out shell command")
}

async fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(bytes).await?;
    Ok(())
}

fn preview_tool_output(output: &[u8]) -> ToolOutputPreview {
    let Ok(text) = std::str::from_utf8(output) else {
        return omitted_binary_output(output.len());
    };
    if text.contains('\0') {
        return omitted_binary_output(output.len());
    }

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
    loop {
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
    loop {
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

fn render_stream_output(preview: &ToolOutputPreview) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    if preview.encoding != "utf-8" {
        metadata.insert("encoding".into(), json!(preview.encoding));
    }
    if preview.offloaded {
        metadata.insert("truncated".into(), json!(true));
    }
    if preview.omitted_bytes > 0 {
        metadata.insert("omitted_bytes".into(), json!(preview.omitted_bytes));
    }
    if let Some(reason) = preview.omission_reason {
        metadata.insert("omission_reason".into(), json!(reason));
    }
    let mut stream = serde_json::Map::new();
    if !metadata.is_empty() {
        stream.insert("metadata".into(), serde_json::Value::Object(metadata));
    }
    if preview.omission_reason.is_none() {
        if let Some(tail) = preview.tail.as_ref() {
            stream.insert("output_head".into(), json!(preview.head));
            stream.insert("output_tail".into(), json!(tail));
        } else {
            stream.insert("output".into(), json!(preview.head));
        }
    }
    serde_json::Value::Object(stream)
}

fn render_tool_result(result: &ShellResult) -> String {
    let mut payload = serde_json::Map::new();
    payload.insert("context_id".into(), json!(result.call_id));
    if let Some(exit_code) = result.exit_code.filter(|code| *code != 0) {
        payload.insert("exit_code".into(), json!(exit_code));
    }
    if result.timed_out {
        payload.insert("timed_out".into(), json!(true));
    }
    if result.duration_ms > 0 {
        payload.insert("duration_ms".into(), json!(result.duration_ms));
    }
    if result.stdout_bytes > 0 {
        payload.insert("stdout_bytes".into(), json!(result.stdout_bytes));
    }
    if result.stderr_bytes > 0 {
        payload.insert("stderr_bytes".into(), json!(result.stderr_bytes));
    }
    if result.prompt_output.stdout.offloaded {
        payload.insert("full_stdout_path".into(), json!(result.stdout_path));
    }
    if result.prompt_output.stderr.offloaded {
        payload.insert("full_stderr_path".into(), json!(result.stderr_path));
    }
    payload.insert(
        "stdout".into(),
        render_stream_output(&result.prompt_output.stdout),
    );
    payload.insert(
        "stderr".into(),
        render_stream_output(&result.prompt_output.stderr),
    );
    serde_json::to_string_pretty(&serde_json::Value::Object(payload))
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

async fn capture_patch_baseline(config: &RunConfig) -> Result<Option<String>> {
    let path = config.session_dir.join(PATCH_BASELINE_FILE);
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        let revision = existing.trim();
        if is_git_revision(revision) {
            return Ok(Some(revision.to_owned()));
        }
    }

    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&config.cwd)
        .output()
        .await;
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let Ok(revision) = String::from_utf8(output.stdout) else {
        return Ok(None);
    };
    let revision = revision.trim();
    if !is_git_revision(revision) {
        return Ok(None);
    }
    tokio::fs::write(&path, format!("{revision}\n")).await?;
    Ok(Some(revision.to_owned()))
}

fn is_git_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn write_final_artifacts(
    config: &RunConfig,
    patch_baseline: Option<&str>,
    completed: bool,
    answer: Option<&str>,
    metrics: &RunMetrics,
    elapsed_ms: u64,
) -> Result<()> {
    let mut command = Command::new("git");
    command
        .args(["diff", "--binary", "--no-ext-diff"])
        .current_dir(&config.cwd);
    if let Some(baseline) = patch_baseline {
        command.arg(baseline);
    }
    let patch = command.output().await;
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
        "compaction_policy": config.compaction_mode,
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
    fn provider_reported_implicit_cache_is_tracked_without_explicit_capabilities() {
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
            ..Usage::default()
        });

        let policy = cache.policy();
        assert_eq!(policy.implicit_cached_tokens, 4_000);
        assert!(policy.breakpoints.is_empty());
    }

    #[test]
    fn system_prompt_requires_reproduction_and_root_cause_investigation() {
        assert!(SYSTEM_PROMPT.contains("minimal failing reproduction"));
        assert!(SYSTEM_PROMPT.contains("affected tests"));
        assert!(SYSTEM_PROMPT.contains(
            "identify the root cause and make the smallest correct fix at the appropriate layer"
        ));
        assert!(SYSTEM_PROMPT.contains("use local history to investigate regressions"));
        assert!(!SYSTEM_PROMPT.contains("later fixes"));
        assert!(!SYSTEM_PROMPT.contains("upstream fix"));
    }

    #[test]
    fn system_prompt_prioritizes_task_progress_and_requires_critical_state_preservation() {
        assert!(SYSTEM_PROMPT.contains("Make task progress first"));
        assert!(SYSTEM_PROMPT.contains("As required secondary housekeeping"));
        assert!(
            SYSTEM_PROMPT.find("Make task progress first")
                < SYSTEM_PROMPT.find("As required secondary housekeeping")
        );
        assert!(SYSTEM_PROMPT.contains("task-critical working state"));
        assert!(SYSTEM_PROMPT.contains("This is required, not optional cleanup"));
        assert!(
            SYSTEM_PROMPT
                .contains("exact facts, decisions, constraints, diagnoses, and verified results")
        );
        assert!(SYSTEM_PROMPT.contains("If you learned anything"));
        assert!(SYSTEM_PROMPT.contains("not already preserved elsewhere"));
        assert!(SYSTEM_PROMPT.contains("remember it"));
        assert!(SYSTEM_PROMPT.contains("History is a working set"));
        assert!(SYSTEM_PROMPT.contains("Human-authored content is kept by default"));
        assert!(SYSTEM_PROMPT.contains("All other context is eligible for removal"));
        assert!(!SYSTEM_PROMPT.contains("Select one action"));
        assert!(!SYSTEM_PROMPT.contains("At each step:"));
        assert!(SYSTEM_PROMPT.contains("leave its bulky source removable"));
        assert!(SYSTEM_PROMPT.contains("or mark it removable if it was protected"));
        assert!(SYSTEM_PROMPT.contains("Leave or mark an item removable only"));
        assert!(SYSTEM_PROMPT.contains("or explicitly noted otherwise"));
        assert!(!SYSTEM_PROMPT.contains("Make an item removable only"));
        assert!(!SYSTEM_PROMPT.contains("stable"));
        assert!(!SYSTEM_PROMPT.contains("volatile"));
    }

    #[test]
    fn system_prompt_requires_relevant_repository_instruction_review() {
        assert!(SYSTEM_PROMPT.contains(
            "Before working in a folder, search for relevant `AGENTS.md` or `CLAUDE.md` files"
        ));
        assert!(SYSTEM_PROMPT.contains("understand agent-specific guidance"));
        assert!(SYSTEM_PROMPT.contains("take relevant guidance onboard"));
    }

    #[test]
    fn small_stream_output_stays_inline() {
        let output = b"all stdout";
        let preview = preview_tool_output(output);
        assert_eq!(preview.encoding, "utf-8");
        assert_eq!(preview.head, String::from_utf8(output.to_vec()).unwrap());
        assert!(preview.tail.is_none());
        assert_eq!(preview.omitted_bytes, 0);
    }

    #[test]
    fn exact_inline_byte_boundary_is_not_offloaded() {
        let exact = vec![b'x'; 10 * 1024];
        assert!(preview_tool_output(&exact).tail.is_none());

        let over = vec![b'x'; 10 * 1024 + 1];
        assert!(preview_tool_output(&over).tail.is_some());
    }

    #[test]
    fn newline_rich_text_uses_a_text_preview_instead_of_the_binary_guard() {
        let full = "ordinary search result\n".repeat(2_000).into_bytes();
        let preview = preview_tool_output(&full);

        assert_eq!(preview.encoding, "utf-8");
        assert!(preview.tail.is_some());
        assert!(preview.omission_reason.is_none());
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
    async fn shell_result_keeps_stdout_and_empty_stderr_as_separate_raw_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("session");
        tokio::fs::create_dir_all(run_dir.join("tools"))
            .await
            .unwrap();
        let result = execute_shell(
            temp.path(),
            &run_dir,
            "tool-streams".into(),
            "printf 'stdout text\\n'",
            30,
        )
        .await
        .unwrap();

        let rendered: serde_json::Value =
            serde_json::from_str(&render_tool_result(&result)).unwrap();
        assert_eq!(rendered["stdout"]["output"], "stdout text\n");
        assert_eq!(rendered["stderr"]["output"], "");
        assert!(rendered["stdout"].get("metadata").is_none());
        assert!(rendered["stderr"].get("metadata").is_none());
        assert!(rendered.get("full_stdout_path").is_none());
        assert!(rendered.get("full_stderr_path").is_none());
        assert!(!render_tool_result(&result).contains("STDOUT ("));
        assert!(!render_tool_result(&result).contains("STDERR ("));
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
        assert_eq!(rendered["stdout"]["metadata"]["truncated"], true);
        assert!(rendered["stdout"]["metadata"].get("encoding").is_none());
        let head = rendered["stdout"]["output_head"].as_str().unwrap();
        let tail = rendered["stdout"]["output_tail"].as_str().unwrap();
        assert!(head.len() <= 5 * 1024 && head.len() > 4 * 1024);
        assert!(tail.len() <= 5 * 1024 && tail.len() > 4 * 1024);
        assert!(
            serde_json::to_string(head).unwrap().len() + serde_json::to_string(tail).unwrap().len()
                <= 10 * 1024 + 4
        );
        assert!(rendered["stdout"].get("output").is_none());
        assert!(
            rendered["stdout"]["metadata"]["omitted_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(rendered.get("full_output_path").is_none());
        let stdout_path = rendered["full_stdout_path"].as_str().unwrap();
        assert!(std::path::Path::new(stdout_path).is_absolute());
        assert!(rendered.get("full_stderr_path").is_none());
        let stdout = tokio::fs::read(stdout_path).await.unwrap();
        assert!(stdout.starts_with(b"hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh"));
        assert!(
            stdout
                .windows(32)
                .any(|window| window == b"mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm")
        );
        assert!(!head.contains("truncat"));
        assert!(!tail.contains("truncat"));
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
    fn known_implicit_cache_estimates_an_unreported_write() {
        let mut cache = CacheTracker::new_with_implicit_minimum(None, Some(1_024));
        cache.begin_request_with_tokens(Vec::new(), 2_400);
        cache.observe(&Usage::default());

        assert_eq!(cache.policy().implicit_cached_tokens, 2_400);
        assert!(cache.policy().breakpoints.is_empty());
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
            cache
                .policy_for_history(&extended, 1)
                .implicit_cached_tokens,
            2_400
        );
        let mutated = vec![json!({"role": "user", "content": "changed"})];
        assert_eq!(
            cache.policy_for_history(&mutated, 1).implicit_cached_tokens,
            0
        );
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
            implicit_minimum_prefix_tokens: Some(openai_cache_capabilities().minimum_prefix_tokens),
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
            implicit_minimum_prefix_tokens: Some(openai_cache_capabilities().minimum_prefix_tokens),
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
        assert_eq!(rendered["stdout"]["metadata"]["truncated"], true);
        assert_eq!(
            rendered["stdout"]["metadata"]["omission_reason"],
            BINARY_OUTPUT_OMISSION_REASON
        );
        assert!(rendered["stdout"].get("output").is_none());
        assert!(rendered["stdout"].get("output_head").is_none());
        assert!(rendered["stdout"].get("output_tail").is_none());
        let stdout_path = rendered["full_stdout_path"].as_str().unwrap();
        let stdout = tokio::fs::read(stdout_path).await.unwrap();
        assert_eq!(stdout, [0xff, 0, 1]);
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
    async fn disabled_compaction_does_not_request_keep_lease_revalidation() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"printf first","answer":null},"context":{"protected":[2],"removable":[],"remember":[]}}"#, "\n",
                r#"{"action":{"kind":"shell","command":"printf second","answer":null},"context":{"protected":[],"removable":[],"remember":["the second tool result is relevant"]}}"#, "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#, "\n",
            ),
        )
        .await
        .unwrap();
        run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(3),
                shell_timeout_secs: 1,
                compaction_mode: CompactionMode::Disabled,
                keep_lease_turns: Some(1),
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: Some("carry-test-cache-key".into()),
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();
        let histories = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap()
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|event| event["event"] == "model_request")
            .map(|event| event["data"]["history"].as_array().unwrap().clone())
            .collect::<Vec<_>>();
        assert_eq!(histories.len(), 3);
        assert!(histories.iter().all(|history| !history.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("Review protected items"))
        })));
    }

    #[tokio::test]
    async fn final_patch_includes_changes_committed_during_run() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("file.txt"), "before\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "file.txt"],
            vec![
                "-c",
                "user.name=Carry test",
                "-c",
                "user.email=carry-test@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .status()
                .unwrap();
            assert!(status.success());
        }
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"printf 'after\\n' > file.txt && git add file.txt && git -c user.name=Carry -c user.email=carry@example.invalid commit -qm agent-change","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#, "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
            ),
        )
        .await
        .unwrap();

        run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(2),
                shell_timeout_secs: 5,
                compaction_mode: CompactionMode::Disabled,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: Some("carry-test-cache-key".into()),
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        let patch = tokio::fs::read_to_string(session_dir.join("final.patch"))
            .await
            .unwrap();
        assert!(
            patch.contains("+after"),
            "committed changes must remain in final.patch"
        );
    }

    #[tokio::test]
    async fn scripted_run_emits_flat_rollout_telemetry_before_a_compaction_decision() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session_dir = temp.path().join("run");
        let steps_file = temp.path().join("steps.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        tokio::fs::write(
            &steps_file,
            concat!(
                r#"{"action":{"kind":"shell","command":"true","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
            ),
        )
        .await
        .unwrap();
        let mut context = ContextState::new("initial task".into());
        for call in 0..3 {
            context
                .add_tool(
                    vec![json!({
                        "type": "function_call", "call_id": format!("call-{call}"),
                        "name": "shell", "arguments": "{}"
                    })],
                    json!({
                        "type": "function_call_output", "call_id": format!("call-{call}"),
                        "output": "large output ".repeat(7_000)
                    }),
                )
                .unwrap();
        }

        run(
            RunConfig {
                cwd: workspace,
                prompt: "Finish the task.".into(),
                session_dir: session_dir.clone(),
                model: "scripted".into(),
                max_steps: Some(2),
                shell_timeout_secs: 1,
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 5,
                compaction_rollout_samples: 4,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: Some(context),
                resume_source: None,
                prompt_cache_key: Some("carry-test-cache-key".into()),
            },
            Backend::scripted(&steps_file).await.unwrap(),
        )
        .await
        .unwrap();

        let trace = tokio::fs::read_to_string(session_dir.join("trace.jsonl"))
            .await
            .unwrap();
        let decision = trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| {
                matches!(
                    event["event"].as_str(),
                    Some("context_compacted") | Some("compaction_rollout_rejected")
                )
            })
            .expect("large resumed context should make a compaction decision");
        assert_eq!(decision["data"]["rollout"]["samples"], 4);
        assert_eq!(decision["data"]["rollout"]["horizon"], 5);
        assert_eq!(decision["data"]["rollout"]["stop_probability_percent"], 10);
        assert!(
            decision["data"]["rollout"]["average_simulated_followup_turns"]
                .as_f64()
                .is_some()
        );
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
            r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
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
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: Some("carry-test-cache-key".into()),
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
        let resumed = load_resume_state(&session_dir).unwrap();
        assert_eq!(resumed.model, "scripted");
        assert_eq!(
            resumed.prompt_cache_key.as_deref(),
            Some("carry-test-cache-key")
        );
        let restored = resumed.context.input_items();
        assert!(restored.iter().any(|item| item["type"] == "function_call"));
        assert!(restored.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("answer was delivered"))
        }));
    }

    #[test]
    fn legacy_checkpoint_without_cache_key_remains_resumable() {
        let temp = tempfile::tempdir().unwrap();
        let checkpoint = ContextCheckpoint {
            version: CONTEXT_CHECKPOINT_VERSION,
            model: "gpt-5.6-luna".into(),
            context: ContextState::new("first task".into()),
            prompt_cache_key: Some("newer-checkpoint-key".into()),
        };
        let mut legacy = serde_json::to_value(checkpoint).unwrap();
        legacy.as_object_mut().unwrap().remove("prompt_cache_key");
        std::fs::write(
            temp.path().join(CONTEXT_CHECKPOINT_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let resumed = load_resume_state(temp.path()).unwrap();
        assert_eq!(resumed.model, "gpt-5.6-luna");
        assert_eq!(resumed.prompt_cache_key, None);
    }

    #[tokio::test]
    async fn resumed_run_sends_the_prior_terminal_response_to_the_next_model_request() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let first_session = temp.path().join("first");
        let second_session = temp.path().join("second");
        let first_steps = temp.path().join("first.jsonl");
        let second_steps = temp.path().join("second.jsonl");
        tokio::fs::create_dir(&workspace).await.unwrap();
        let finish = r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[1],"remember":[]}}"#;
        tokio::fs::write(&first_steps, finish).await.unwrap();
        tokio::fs::write(&second_steps, finish).await.unwrap();

        run(
            RunConfig {
                cwd: workspace.clone(),
                prompt: "first task".into(),
                session_dir: first_session.clone(),
                model: "scripted".into(),
                max_steps: Some(1),
                shell_timeout_secs: 1,
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: Some("resumable-cache-affinity".into()),
            },
            Backend::scripted(&first_steps).await.unwrap(),
        )
        .await
        .unwrap();
        let resume = load_resume_state(&first_session).unwrap();
        let prompt_cache_key = resume.prompt_cache_key.clone();

        run(
            RunConfig {
                cwd: workspace,
                prompt: "second task".into(),
                session_dir: second_session.clone(),
                model: resume.model,
                max_steps: Some(1),
                shell_timeout_secs: 1,
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: Some(resume.context),
                resume_source: Some(first_session),
                prompt_cache_key,
            },
            Backend::scripted(&second_steps).await.unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            load_resume_state(&second_session)
                .unwrap()
                .prompt_cache_key
                .as_deref(),
            Some("resumable-cache-affinity"),
        );

        let trace = tokio::fs::read_to_string(second_session.join("trace.jsonl"))
            .await
            .unwrap();
        assert!(
            !trace.contains("\"event\":\"context_compacted\""),
            "a resumed run must not rewrite restored history before its first provider request"
        );
        let event: serde_json::Value = trace
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<Vec<serde_json::Value>, _>>()
            .unwrap()
            .into_iter()
            .find(|event| event["event"] == "model_request")
            .unwrap();
        assert!(
            event["data"]["history"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "function_call_output")
        );
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
            r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[1],"remember":[]}}"#,
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
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: None,
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
            r#"{"action":{"kind":"shell","command":"true","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#
        );
        let mut steps = shell_step.repeat(31);
        steps.push_str(r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#);
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
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: None,
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
                r#"{"action":{"kind":"shell","command":"true","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#,
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
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: None,
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
    async fn timed_out_shell_preserves_partial_output_and_terminates_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        tokio::fs::create_dir_all(run_dir.join("tools"))
            .await
            .unwrap();
        let survivor = temp.path().join("survivor");
        let command =
            "printf stdout-before; printf stderr-before >&2; (sleep 2; touch survivor) & wait";

        let result = execute_shell(temp.path(), &run_dir, "timeout".into(), command, 1)
            .await
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(
            tokio::fs::read(&result.stdout_path).await.unwrap(),
            b"stdout-before"
        );
        assert!(
            tokio::fs::read_to_string(&result.stderr_path)
                .await
                .unwrap()
                .contains("stderr-before")
        );
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(
            !survivor.exists(),
            "a descendant continued modifying the workspace after timeout"
        );
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
                r#"{"action":{"kind":"shell","command":"true","message":"Checking first.","answer":null},"context":{"protected":[],"removable":[],"remember":[]}}"#,
                "\n",
                r#"{"action":{"kind":"finish","command":null,"answer":"done"},"context":{"protected":[2],"removable":[],"remember":[]}}"#,
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
                compaction_mode: CompactionMode::Economic,
                keep_lease_turns: None,
                compaction_payoff_requests: 1,
                compaction_rollout_samples: 0,
                compaction_rollout_stop_probability_percent: 10,
                compaction_neutral_high_watermark_tokens: 32 * 1024,
                compaction_neutral_low_watermark_tokens: 24 * 1024,
                resume_context: None,
                resume_source: None,
                prompt_cache_key: None,
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
        let initial_prompt = events
            .iter()
            .find(|event| {
                event["event"] == "human_message" && event["data"]["message"] == "initial task"
            })
            .expect("initial prompt is logged as a human message");
        assert_eq!(initial_prompt["data"]["context_id"], 1);
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
                        .is_some_and(|output| output.ends_with("[context 2]"))
            })
            .unwrap();
        let steering = history
            .iter()
            .position(|item| item["content"][0]["text"] == "do not change the JSON format")
            .unwrap();
        assert!(tool_result < steering);
        assert_eq!(history[steering + 1]["content"][0]["text"], "[context 3]");
    }
}
