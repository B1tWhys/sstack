//! The agent loop, which carries a Session forward one Turn at a time.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

use crate::provider::{ModelRequest, Provider, ProviderError, StreamEvent, ToolSpec};
use crate::session::{AssistantEntry, Entry, SessionError, SessionLog, StopReason, ToolCall};
use crate::tools::ToolBox;

/// The tool result telling the model that a call was abandoned because its Turn was cut short.
const INTERRUPTED: &str = "not run: the turn was interrupted";
/// The tool result telling the model that a call was ignored because its response ended
/// abnormally.
const NOT_RUN: &str = "not run: the response ended before this call could be used";

/// Something that happened during a Turn, reported as it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A piece of the model's reply.
    Text(String),
    /// A piece of the model's summarized thinking.
    Reasoning(String),
    /// A tool call is about to run.
    ToolStarted(ToolCallSummary),
    /// A tool call finished.
    ToolFinished {
        /// The name of the tool that ran.
        name: String,
        /// Whether the call failed.
        is_error: bool,
    },
    /// The latest request used most of the context window, and requests fail once it is full.
    ContextNearLimit {
        /// Tokens in the most recent request.
        used: u64,
        /// The model's context window, in tokens.
        limit: u64,
    },
}

/// A description of a tool call for the person watching it run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallSummary {
    /// The name of the tool being called.
    pub name: String,
    /// The call's input, as JSON text.
    pub input: String,
}

/// How a Turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The model finished what it was doing.
    Finished,
    /// The response reached the output token limit.
    Truncated,
    /// The model declined the request.
    Refused,
    /// The person cancelled the Turn.
    Cancelled,
    /// The Turn stopped between Steps when asked to, and [`Agent::continue_turn`] picks it up.
    Paused,
}

/// A failure that stops a Turn.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The [`Provider`] failed to produce a response.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The Session could not be recorded.
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// What an [`Agent`] runs on.
pub struct AgentConfig {
    /// The service that runs the model.
    pub provider: Arc<dyn Provider>,
    /// The identifier of the model to request.
    pub model: String,
    /// The system prompt sent with every request.
    pub system: String,
    /// The tools offered to the model.
    pub tools: ToolBox,
    /// The model's context window, in tokens.
    pub context_limit: u64,
}

/// The agent loop for one Session, which keeps every Turn durable and resumable.
pub struct Agent {
    /// What this agent runs on.
    config: AgentConfig,
    /// What the model is told about its tools.
    specs: Vec<ToolSpec>,
    /// Where the Session is made durable.
    log: SessionLog,
    /// The Session so far, which every request carries.
    entries: Vec<Entry>,
}

/// A callback that receives [`AgentEvent`]s as a Turn progresses.
pub type EventSink<'a> = &'a mut (dyn FnMut(AgentEvent) + Send);

/// The caller's controls for watching a Turn and stopping it.
pub struct TurnContext<'a> {
    /// Cancelling this stops the Turn and leaves the Session resumable.
    pub cancel: &'a CancellationToken,
    /// Whether the Turn should stop at the end of the current Step, unlike `cancel`, which
    /// abandons the Step. A Turn that ends by itself in that Step simply ends. Can be
    /// cleared again.
    pub pause: &'a AtomicBool,
    /// Where each event goes as the Turn progresses.
    pub on_event: EventSink<'a>,
}

impl Agent {
    /// Picks up a Session from its log and entries, repairing anything a crash left unfinished.
    ///
    /// # Errors
    /// Returns an error if the repair cannot be recorded.
    pub fn new(
        config: AgentConfig,
        log: SessionLog,
        entries: Vec<Entry>,
    ) -> Result<Self, SessionError> {
        let specs = config.tools.specs();
        let mut agent = Self {
            config,
            specs,
            log,
            entries,
        };
        agent.settle_unanswered_calls(INTERRUPTED)?;
        Ok(agent)
    }

    /// Returns every entry in the Session so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Runs one Turn to its end, unless `context` cancels it first.
    ///
    /// # Errors
    /// Returns an error if the [`Provider`] fails or the Session cannot be recorded.
    pub async fn run_turn(
        &mut self,
        input: &str,
        context: &mut TurnContext<'_>,
    ) -> Result<TurnEnd, AgentError> {
        self.record(Entry::User {
            text: input.to_owned(),
        })?;
        self.run_steps(context).await
    }

    /// Carries on a Turn that stopped with [`TurnEnd::Paused`], in this run or an earlier one.
    ///
    /// # Errors
    /// Returns an error if the [`Provider`] fails or the Session cannot be recorded.
    pub async fn continue_turn(
        &mut self,
        context: &mut TurnContext<'_>,
    ) -> Result<TurnEnd, AgentError> {
        self.run_steps(context).await
    }

    /// Runs Steps until one ends the Turn, or until the Turn is asked to pause between them.
    async fn run_steps(&mut self, context: &mut TurnContext<'_>) -> Result<TurnEnd, AgentError> {
        loop {
            if let Some(end) = self.step(context).await? {
                return Ok(end);
            }
            if context.pause.load(Ordering::SeqCst) {
                self.record(Entry::TurnParked)?;
                return Ok(TurnEnd::Paused);
            }
        }
    }

    /// Runs one Step, returning how the Turn ended if this Step ended it.
    async fn step(&mut self, context: &mut TurnContext<'_>) -> Result<Option<TurnEnd>, AgentError> {
        let response = tokio::select! {
            biased;
            () = context.cancel.cancelled() => return self.cancelled().map(Some),
            response = self.request(context.on_event) => response?,
        };
        self.warn_near_limit(&response, context.on_event);
        let calls = response.tool_calls.clone();
        let stop = response.stop;
        self.record(Entry::Assistant(response))?;
        if stop != StopReason::ToolUse || calls.is_empty() {
            self.settle_unanswered_calls(NOT_RUN)?;
            return Ok(Some(TurnEnd::from(stop)));
        }
        self.run_tools(&calls, context).await
    }

    /// Asks the [`Provider`] for the next response, reporting it as it arrives.
    async fn request(&self, on_event: EventSink<'_>) -> Result<AssistantEntry, ProviderError> {
        let request = ModelRequest {
            model: &self.config.model,
            system: &self.config.system,
            entries: &self.entries,
            tools: &self.specs,
        };
        let mut forward = |event| on_event(stream_event(event));
        self.config.provider.respond(request, &mut forward).await
    }

    /// Runs the tools a response asked for, stopping early if the Turn is cancelled.
    async fn run_tools(
        &mut self,
        calls: &[ToolCall],
        context: &mut TurnContext<'_>,
    ) -> Result<Option<TurnEnd>, AgentError> {
        for call in calls {
            (context.on_event)(AgentEvent::ToolStarted(ToolCallSummary::from(call)));
            let output = tokio::select! {
                biased;
                () = context.cancel.cancelled() => return self.cancelled().map(Some),
                output = self.config.tools.run(call) => output,
            };
            (context.on_event)(AgentEvent::ToolFinished {
                name: call.name.clone(),
                is_error: output.is_error,
            });
            self.record(Entry::ToolResult {
                call_id: call.id.clone(),
                output: output.text,
                is_error: output.is_error,
            })?;
        }
        Ok(None)
    }

    /// Reports when a response used at least 80% of the context window.
    fn warn_near_limit(&self, response: &AssistantEntry, on_event: EventSink<'_>) {
        let used = response.usage.input_tokens;
        let limit = self.config.context_limit;
        if used.saturating_mul(5) >= limit.saturating_mul(4) {
            on_event(AgentEvent::ContextNearLimit { used, limit });
        }
    }

    /// Records that the Turn was cancelled, leaving the Session valid for the next one.
    fn cancelled(&mut self) -> Result<TurnEnd, AgentError> {
        self.settle_unanswered_calls(INTERRUPTED)?;
        self.record(Entry::TurnCancelled)?;
        Ok(TurnEnd::Cancelled)
    }

    /// Keeps the Session valid, because Providers reject tool calls left without a result.
    fn settle_unanswered_calls(&mut self, reason: &str) -> Result<(), SessionError> {
        for call_id in unanswered_calls(&self.entries) {
            self.record(Entry::ToolResult {
                call_id,
                output: reason.to_owned(),
                is_error: true,
            })?;
        }
        Ok(())
    }

    /// Adds an entry to the Session.
    fn record(&mut self, entry: Entry) -> Result<(), SessionError> {
        self.log.append(&entry)?;
        self.entries.push(entry);
        Ok(())
    }
}

/// Converts a [`Provider`] stream event into the equivalent agent event.
fn stream_event(event: StreamEvent) -> AgentEvent {
    match event {
        StreamEvent::Text(text) => AgentEvent::Text(text),
        StreamEvent::Reasoning(text) => AgentEvent::Reasoning(text),
    }
}

/// Tells how a Turn ended from why its last response stopped.
impl From<StopReason> for TurnEnd {
    fn from(stop: StopReason) -> Self {
        match stop {
            StopReason::EndTurn | StopReason::ToolUse => TurnEnd::Finished,
            StopReason::MaxTokens => TurnEnd::Truncated,
            StopReason::Refused => TurnEnd::Refused,
        }
    }
}

/// Describes a recorded tool call the same way as one that is about to run.
impl From<&ToolCall> for ToolCallSummary {
    fn from(call: &ToolCall) -> Self {
        Self {
            name: call.name.clone(),
            input: call.input.to_string(),
        }
    }
}

/// Returns the ids of tool calls that have no result in `entries`.
fn unanswered_calls(entries: &[Entry]) -> Vec<String> {
    let answered: HashSet<&str> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Assistant(assistant) => Some(&assistant.tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| !answered.contains(call.id.as_str()))
        .map(|call| call.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::scripted::{Scripted, ScriptedProvider};
    use crate::session::Usage;
    use crate::tools::test_support::{AllowAll, git_project};
    use crate::tools::{Approver, DenyAll};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::Path;

    const LIMIT: u64 = 1000;

    fn reply(text: &str) -> AssistantEntry {
        AssistantEntry {
            text: text.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        }
    }

    fn calling(id: &str, name: &str, input: serde_json::Value) -> AssistantEntry {
        AssistantEntry {
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                input,
            }],
            stop: StopReason::ToolUse,
            ..reply("")
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        agent: Agent,
        provider: Arc<ScriptedProvider>,
        session: std::path::PathBuf,
    }

    fn harness(script: Vec<Scripted>, approver: Arc<dyn Approver>) -> Harness {
        let (dir, project) = git_project();
        let session = dir.path().join("ignored/session.jsonl");
        let provider = Arc::new(ScriptedProvider::new(script));
        let agent = agent_on(&session, config(provider.clone(), project, approver));
        Harness {
            _dir: dir,
            agent,
            provider,
            session,
        }
    }

    fn agent_on(session: &Path, config: AgentConfig) -> Agent {
        let log = SessionLog::create(session).unwrap();
        Agent::new(config, log, vec![]).unwrap()
    }

    async fn turn(
        agent: &mut Agent,
        cancel: &CancellationToken,
        on_event: EventSink<'_>,
    ) -> Result<TurnEnd, AgentError> {
        let pause = AtomicBool::new(false);
        let mut context = TurnContext {
            cancel,
            pause: &pause,
            on_event,
        };
        agent.run_turn("go", &mut context).await
    }

    fn config(
        provider: Arc<dyn Provider>,
        project: Arc<crate::tools::project::Project>,
        approver: Arc<dyn Approver>,
    ) -> AgentConfig {
        AgentConfig {
            provider,
            model: "scripted".into(),
            system: "system".into(),
            tools: ToolBox::standard()
                .project(project)
                .sandbox(crate::tools::test_support::disabled())
                .approver(approver)
                .call(),
            context_limit: LIMIT,
        }
    }

    async fn run(agent: &mut Agent) -> (Result<TurnEnd, AgentError>, Vec<AgentEvent>) {
        let mut events = Vec::new();
        let cancel = CancellationToken::new();
        let end = turn(agent, &cancel, &mut |event| events.push(event)).await;
        (end, events)
    }

    fn tool_results(entries: &[Entry]) -> Vec<(&str, &str, bool)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => Some((call_id.as_str(), output.as_str(), *is_error)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn runs_tools_until_the_model_finishes_and_records_everything() {
        let script = vec![
            Ok(calling(
                "1",
                "write",
                json!({"path": "a.txt", "content": "x"}),
            )),
            Ok(reply("done")),
        ];
        let mut h = harness(script, Arc::new(DenyAll));

        let (end, events) = run(&mut h.agent).await;

        assert_eq!(end.unwrap(), TurnEnd::Finished);
        assert_eq!(
            tool_results(h.agent.entries()),
            [("1", "wrote a.txt", false)]
        );
        assert_eq!(h.provider.requests()[1].len(), 3);
        assert!(events.contains(&AgentEvent::Text("done".into())));
        assert!(events.contains(&AgentEvent::ToolFinished {
            name: "write".into(),
            is_error: false
        }));
        let (_, stored) = SessionLog::open(&h.session).unwrap();
        assert_eq!(stored, h.agent.entries());
    }

    #[tokio::test]
    async fn a_refused_or_truncated_response_leaves_its_tool_calls_answered_but_unrun() {
        for (stop, expected) in [
            (StopReason::Refused, TurnEnd::Refused),
            (StopReason::MaxTokens, TurnEnd::Truncated),
        ] {
            let response = AssistantEntry {
                stop,
                ..calling("1", "write", json!({"path": "a.txt", "content": "x"}))
            };
            let mut h = harness(vec![Ok(response)], Arc::new(DenyAll));

            let (end, _) = run(&mut h.agent).await;

            assert_eq!(end.unwrap(), expected);
            assert_eq!(tool_results(h.agent.entries()), [("1", NOT_RUN, true)]);
        }
    }

    #[tokio::test]
    async fn tool_use_with_no_calls_ends_the_turn() {
        let response = AssistantEntry {
            stop: StopReason::ToolUse,
            ..reply("odd")
        };
        let mut h = harness(vec![Ok(response)], Arc::new(DenyAll));
        assert_eq!(run(&mut h.agent).await.0.unwrap(), TurnEnd::Finished);
    }

    #[tokio::test]
    async fn provider_failures_surface_and_keep_the_user_input() {
        let mut h = harness(vec![Err("down".to_owned())], Arc::new(DenyAll));
        let (end, _) = run(&mut h.agent).await;
        assert_eq!(end.unwrap_err().to_string(), "down");
        assert_eq!(h.agent.entries(), [Entry::User { text: "go".into() }]);
    }

    #[tokio::test]
    async fn warns_when_the_context_is_nearly_full() {
        let response = |input_tokens| AssistantEntry {
            usage: Usage {
                input_tokens,
                output_tokens: 0,
            },
            ..reply("")
        };
        let script = vec![Ok(response(799)), Ok(response(800))];
        let mut h = harness(script, Arc::new(DenyAll));

        let warned = |events: &[AgentEvent]| {
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ContextNearLimit { .. }))
        };
        assert!(!warned(&run(&mut h.agent).await.1));
        assert!(warned(&run(&mut h.agent).await.1));
    }

    #[tokio::test]
    async fn cancelling_a_running_tool_keeps_the_session_valid() {
        let script = vec![Ok(calling("1", "bash", json!({"command": "sleep 30"})))];
        let mut h = harness(script, Arc::new(AllowAll));
        let cancel = CancellationToken::new();
        let on_start = cancel.clone();

        let end = turn(&mut h.agent, &cancel, &mut |event| {
            if matches!(event, AgentEvent::ToolStarted(_)) {
                on_start.cancel();
            }
        })
        .await;

        assert_eq!(end.unwrap(), TurnEnd::Cancelled);
        assert_eq!(tool_results(h.agent.entries()), [("1", INTERRUPTED, true)]);
        assert_eq!(h.agent.entries().last(), Some(&Entry::TurnCancelled));
    }

    struct NeverResponds;

    #[async_trait]
    impl Provider for NeverResponds {
        async fn respond(
            &self,
            _request: ModelRequest<'_>,
            _on_event: &mut (dyn FnMut(StreamEvent) + Send),
        ) -> Result<AssistantEntry, ProviderError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn cancelling_a_model_request_ends_the_turn() {
        let (dir, project) = git_project();
        let session = dir.path().join("ignored/session.jsonl");
        let config = config(Arc::new(NeverResponds), project, Arc::new(DenyAll));
        let mut agent = agent_on(&session, config);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let end = turn(&mut agent, &cancel, &mut |_| {}).await;

        assert_eq!(end.unwrap(), TurnEnd::Cancelled);
    }

    #[test]
    fn resuming_after_a_crash_answers_dangling_tool_calls() {
        let (dir, project) = git_project();
        let path = dir.path().join("ignored/session.jsonl");
        let mut log = SessionLog::create(&path).unwrap();
        let dangling = Entry::Assistant(calling("1", "read", json!({"path": "a"})));
        log.append(&dangling).unwrap();
        drop(log);

        let (log, entries) = SessionLog::open(&path).unwrap();
        let provider = Arc::new(ScriptedProvider::default());
        let config = config(provider, project, Arc::new(DenyAll));
        let agent = Agent::new(config, log, entries).unwrap();

        assert_eq!(tool_results(agent.entries()), [("1", INTERRUPTED, true)]);
        let (_, stored) = SessionLog::open(&path).unwrap();
        assert_eq!(stored.len(), 2);
    }

    /// Runs a Turn that is told to pause, then carries it on in a new agent, as a relaunch does.
    #[tokio::test]
    async fn a_paused_turn_stops_between_steps_and_carries_on_after_a_restart() {
        let script = vec![
            Ok(calling("1", "read", json!({"path": "missing"}))),
            Ok(reply("done")),
        ];
        let mut h = harness(script, Arc::new(AllowAll));
        let cancel = CancellationToken::new();
        let pause = AtomicBool::new(true);
        let mut ignore = |_| {};
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            on_event: &mut ignore,
        };

        let end = h.agent.run_turn("go", &mut context).await.unwrap();

        assert_eq!(end, TurnEnd::Paused);
        assert_eq!(h.agent.entries().last(), Some(&Entry::TurnParked));
        assert!(matches!(
            h.agent.entries()[h.agent.entries().len() - 2],
            Entry::ToolResult { .. }
        ));

        pause.store(false, Ordering::SeqCst);
        let (log, entries) = SessionLog::open(&h.session).unwrap();
        let (dir, project) = git_project();
        let config = config(h.provider.clone(), project, Arc::new(AllowAll));
        let mut resumed = Agent::new(config, log, entries).unwrap();
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            on_event: &mut ignore,
        };

        let end = resumed.continue_turn(&mut context).await.unwrap();

        assert_eq!(end, TurnEnd::Finished);
        drop(dir);
    }

    #[tokio::test]
    async fn a_turn_that_finishes_in_its_step_ends_normally_even_when_asked_to_pause() {
        let mut h = harness(vec![Ok(reply("done"))], Arc::new(AllowAll));
        let cancel = CancellationToken::new();
        let pause = AtomicBool::new(true);
        let mut ignore = |_| {};
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            on_event: &mut ignore,
        };
        let end = h.agent.run_turn("go", &mut context).await.unwrap();
        assert_eq!(end, TurnEnd::Finished);
    }
}
