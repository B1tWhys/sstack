//! The agent loop, which carries a Session forward one Turn at a time.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use tokio_util::sync::CancellationToken;

use crate::agent::QueuedPrompts;
use crate::connection::{Effort, ModelRequest, Provider, ProviderError, StreamEvent, ToolSpec};
use crate::protocol::{AgentEvent, ToolCallSummary, TurnEnd};
use crate::session::{AssistantEntry, Entry, SessionError, SessionLog, StopReason, ToolCall};
use crate::tools::ToolBox;

/// The tool result telling the model that a call was abandoned because its Turn was cut short.
const INTERRUPTED: &str = "not run: the turn was interrupted";
/// The tool result telling the model that a call was ignored because its response ended
/// abnormally.
const NOT_RUN: &str = "not run: the response ended before this call could be used";

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

/// The model a Session moves to when the person switches its Connection, and what serves it.
pub struct ModelService {
    /// The service that runs the model.
    pub provider: Arc<dyn Provider>,
    /// The identifier of the model to request.
    pub model: String,
    /// How much effort the model is asked to spend, which the model can learn about the Session.
    pub effort: Effort,
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
    /// When the person was first asked what the next call to run asks, if that call is the one a
    /// Rebuild put off. The person has seen that call already, so it is not shown again.
    resumed_since: Option<SystemTime>,
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
    /// The prompts the person sent while the Turn runs, which the Turn takes at the end of each
    /// Step that does not end it.
    pub queued: &'a QueuedPrompts,
    /// Where each event goes as the Turn progresses.
    pub on_event: EventSink<'a>,
}

impl Agent {
    /// Picks up a Session from its log and entries, repairing anything a crash left unfinished.
    /// A Turn that parked keeps the calls it had not run yet, for [`Self::continue_turn`].
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
            resumed_since: None,
        };
        if !agent.entries.last().is_some_and(Entry::is_parked) {
            agent.settle_unanswered_calls(INTERRUPTED)?;
        }
        Ok(agent)
    }

    /// Has every request from now on go to `service`. Call it between Turns, so that each Turn
    /// is served by one model. The system prompt and tools stay as they are.
    pub fn switch(&mut self, service: ModelService) {
        self.config
            .tools
            .served_by(&service.model, Some(service.effort));
        self.config.provider = service.provider;
        self.config.model = service.model;
        self.config.context_limit = service.context_limit;
    }

    /// Returns every entry in the Session so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Runs one Turn to its end, unless `context` cancels it first. A parked Turn that still had
    /// calls to run is given up, and the model is told they were not run.
    ///
    /// # Errors
    /// Returns an error if the [`Provider`] fails or the Session cannot be recorded.
    pub async fn run_turn(
        &mut self,
        input: &str,
        context: &mut TurnContext<'_>,
    ) -> Result<TurnEnd, AgentError> {
        self.settle_unanswered_calls(INTERRUPTED)?;
        self.record(Entry::User {
            text: input.to_owned(),
        })?;
        self.run_steps(context).await
    }

    /// Carries on a Turn that stopped with [`TurnEnd::Paused`] or [`TurnEnd::PutOff`], in this
    /// run or an earlier one. Any call it stopped before running runs first, so a question that
    /// was put off is asked again, without the call being shown a second time.
    ///
    /// # Errors
    /// Returns an error if the [`Provider`] fails or the Session cannot be recorded.
    pub async fn continue_turn(
        &mut self,
        context: &mut TurnContext<'_>,
    ) -> Result<TurnEnd, AgentError> {
        if let Some(end) = self.finish_parked_step(context).await? {
            return Ok(end);
        }
        self.run_steps(context).await
    }

    /// Runs the calls of a Step that parked before running them all, returning how the Turn
    /// ended if it stopped again. Does nothing for a Turn that parked between Steps.
    async fn finish_parked_step(
        &mut self,
        context: &mut TurnContext<'_>,
    ) -> Result<Option<TurnEnd>, AgentError> {
        let unfinished = unanswered_calls(&self.entries);
        if unfinished.is_empty() {
            return Ok(None);
        }
        if let Some(Entry::TurnPutOff {
            waiting_since,
            queued,
        }) = self.entries.last()
        {
            self.resumed_since = Some(*waiting_since);
            if let Some(queued) = queued {
                context.queued.put_back(queued.clone());
            }
        }
        if let Some(end) = self.run_tools(&unfinished, context).await? {
            return Ok(Some(end));
        }
        self.admit_queued(context)?;
        Ok(self.park_if_asked(context)?)
    }

    /// Runs Steps until one ends the Turn, or until the Turn is asked to pause between them.
    /// Prompts queued during a Step go to the model in the next one, and are recorded before the
    /// Turn parks so that they are not lost to a relaunch.
    async fn run_steps(&mut self, context: &mut TurnContext<'_>) -> Result<TurnEnd, AgentError> {
        loop {
            if let Some(end) = self.step(context).await? {
                return Ok(end);
            }
            self.admit_queued(context)?;
            if let Some(end) = self.park_if_asked(context)? {
                return Ok(end);
            }
        }
    }

    /// Adds the prompts the person queued during the Step to the Session, if there are any, so
    /// that the model reads them in the next Step. Called between Steps.
    fn admit_queued(&mut self, context: &mut TurnContext<'_>) -> Result<(), SessionError> {
        let Some(text) = context.queued.take() else {
            return Ok(());
        };
        self.record(Entry::User { text: text.clone() })?;
        (context.on_event)(AgentEvent::Admitted(text));
        Ok(())
    }

    /// Parks the Turn if `context` asks it to pause, returning how it ended if it did. Called
    /// between Steps.
    fn park_if_asked(
        &mut self,
        context: &TurnContext<'_>,
    ) -> Result<Option<TurnEnd>, SessionError> {
        if !context.pause.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.parked().map(Some)
    }

    /// Records that the Turn stopped between Steps to carry on later.
    fn parked(&mut self) -> Result<TurnEnd, SessionError> {
        self.record(Entry::TurnParked)?;
        Ok(TurnEnd::Paused)
    }

    /// Records that the Turn stopped to carry on later because a Rebuild put off what it asked
    /// the person, who was first asked at `waiting_since`. The prompts queued for the Turn go
    /// with it, so that a relaunch does not lose them.
    fn put_off(
        &mut self,
        waiting_since: SystemTime,
        queued: &QueuedPrompts,
    ) -> Result<TurnEnd, SessionError> {
        let queued = queued.take();
        self.record(Entry::TurnPutOff {
            waiting_since,
            queued,
        })?;
        Ok(TurnEnd::PutOff)
    }

    /// Shows `call` as about to run, unless it is the put-off call the person saw already, and
    /// returns from when the person is asked about it, if it asks.
    fn start_call(&mut self, call: &ToolCall, on_event: EventSink<'_>) -> SystemTime {
        self.resumed_since.take().unwrap_or_else(|| {
            on_event(AgentEvent::ToolStarted(ToolCallSummary::from(call)));
            SystemTime::now()
        })
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

    /// Runs the tools a response asked for, stopping early if the Turn is cancelled, or parking
    /// it if a call's question is put off. Calls after that one stay unrun.
    async fn run_tools(
        &mut self,
        calls: &[ToolCall],
        context: &mut TurnContext<'_>,
    ) -> Result<Option<TurnEnd>, AgentError> {
        for call in calls {
            let asked_since = self.start_call(call, context.on_event);
            let output = tokio::select! {
                biased;
                () = context.cancel.cancelled() => return self.cancelled().map(Some),
                output = self.config.tools.run(call) => output,
            };
            let Ok(output) = output else {
                return Ok(Some(self.put_off(asked_since, context.queued)?));
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
        for call in unanswered_calls(&self.entries) {
            self.record(Entry::ToolResult {
                call_id: call.id,
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

/// Returns the ids of the tool calls in `entries` that were never shown as starting, for
/// [`replay`](fn@super::replay) to leave out when a resumed Session is shown. They are the calls
/// after the one whose question a Rebuild put off, which the agent shows as they start once the
/// Turn carries on.
#[must_use]
pub(super) fn calls_not_started(entries: &[Entry]) -> HashSet<String> {
    if !matches!(entries.last(), Some(Entry::TurnPutOff { .. })) {
        return HashSet::new();
    }
    let unanswered = unanswered_calls(entries).into_iter();
    unanswered.skip(1).map(|call| call.id).collect()
}

/// Returns the tool calls that have no result in `entries`, oldest first.
fn unanswered_calls(entries: &[Entry]) -> Vec<ToolCall> {
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
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::scripted::{Scripted, ScriptedProvider};
    use crate::session::Usage;
    use crate::tools::test_support::{AllowAll, DeferAll, fake_tools};
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
        dir: tempfile::TempDir,
        agent: Agent,
        provider: Arc<ScriptedProvider>,
        session: std::path::PathBuf,
    }

    fn harness(script: Vec<Scripted>, approver: Arc<dyn Approver>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("ignored/session.jsonl");
        let provider = Arc::new(ScriptedProvider::new(script));
        let agent = agent_on(&session, config(provider.clone(), dir.path(), approver));
        Harness {
            dir,
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
            queued: &QueuedPrompts::default(),
            on_event,
        };
        agent.run_turn("go", &mut context).await
    }

    fn config(
        provider: Arc<dyn Provider>,
        root: &Path,
        approver: Arc<dyn Approver>,
    ) -> AgentConfig {
        AgentConfig {
            provider,
            model: "scripted".into(),
            system: "system".into(),
            tools: fake_tools(root, approver),
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
        let stored = crate::session::lock_free_read(&h.session).unwrap();
        assert_eq!(stored, h.agent.entries());
    }

    #[tokio::test]
    async fn after_a_switch_the_next_turn_goes_to_the_new_model_with_its_limit() {
        let mut h = harness(vec![Ok(reply("first"))], Arc::new(DenyAll));
        run(&mut h.agent).await.0.unwrap();
        let mut answer = reply("second");
        answer.usage.input_tokens = 90;
        let other = Arc::new(ScriptedProvider::new(vec![Ok(answer)]));

        h.agent.switch(ModelService {
            provider: other.clone(),
            model: "other".into(),
            effort: Effort::Low,
            context_limit: 100,
        });
        let (end, events) = run(&mut h.agent).await;

        assert_eq!(end.unwrap(), TurnEnd::Finished);
        assert_eq!(h.provider.requests().len(), 1);
        assert_eq!(
            other.requests()[0].len(),
            3,
            "the new model sees the Session so far"
        );
        assert!(events.contains(&AgentEvent::ContextNearLimit {
            used: 90,
            limit: 100
        }));
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
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("ignored/session.jsonl");
        let config = config(Arc::new(NeverResponds), dir.path(), Arc::new(DenyAll));
        let mut agent = agent_on(&session, config);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let end = turn(&mut agent, &cancel, &mut |_| {}).await;

        assert_eq!(end.unwrap(), TurnEnd::Cancelled);
    }

    #[test]
    fn resuming_after_a_crash_answers_dangling_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ignored/session.jsonl");
        let mut log = SessionLog::create(&path).unwrap();
        let dangling = Entry::Assistant(calling("1", "read", json!({"path": "a"})));
        log.append(&dangling).unwrap();
        drop(log);

        let (log, entries) = SessionLog::open(&path).unwrap();
        let provider = Arc::new(ScriptedProvider::default());
        let config = config(provider, dir.path(), Arc::new(DenyAll));
        let agent = Agent::new(config, log, entries).unwrap();

        assert_eq!(tool_results(agent.entries()), [("1", INTERRUPTED, true)]);
        let stored = crate::session::lock_free_read(&path).unwrap();
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
            queued: &QueuedPrompts::default(),
            on_event: &mut ignore,
        };

        let end = h.agent.run_turn("go", &mut context).await.unwrap();

        assert_eq!(end, TurnEnd::Paused);
        assert_eq!(h.agent.entries().last(), Some(&Entry::TurnParked));
        assert!(matches!(
            h.agent.entries()[h.agent.entries().len() - 2],
            Entry::ToolResult { .. }
        ));

        drop(h.agent);
        pause.store(false, Ordering::SeqCst);
        let (log, entries) = SessionLog::open(&h.session).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = config(h.provider.clone(), dir.path(), Arc::new(AllowAll));
        let mut resumed = Agent::new(config, log, entries).unwrap();
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            queued: &QueuedPrompts::default(),
            on_event: &mut ignore,
        };

        let end = resumed.continue_turn(&mut context).await.unwrap();

        assert_eq!(end, TurnEnd::Finished);
        drop(dir);
    }

    /// Replaces the agent of `h` with a new one on the same Session and project, as a relaunch
    /// does, whose shell commands `approver` decides on.
    fn relaunched(h: Harness, approver: Arc<dyn Approver>) -> Harness {
        let Harness {
            dir,
            agent,
            provider,
            session,
        } = h;
        drop(agent);
        let (log, entries) = SessionLog::open(&session).unwrap();
        let config = config(provider.clone(), dir.path(), approver);
        Harness {
            agent: Agent::new(config, log, entries).unwrap(),
            dir,
            provider,
            session,
        }
    }

    /// Carries on the parked Turn of `agent`, with nothing asking it to pause, returning how it
    /// ended and what it reported.
    async fn carry_on(agent: &mut Agent) -> (TurnEnd, Vec<AgentEvent>) {
        let cancel = CancellationToken::new();
        let pause = AtomicBool::new(false);
        let mut events = Vec::new();
        let mut record = |event| events.push(event);
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            queued: &QueuedPrompts::default(),
            on_event: &mut record,
        };
        let end = agent.continue_turn(&mut context).await.unwrap();
        (end, events)
    }

    /// Returns a call of `name` with `input` and id `id`.
    fn tool_call(id: &str, name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    /// Returns a response that reads a file that is not there, runs `touch ran` in the shell,
    /// which asks the person, then reads another file that is not there.
    fn read_touch_read() -> AssistantEntry {
        AssistantEntry {
            tool_calls: vec![
                tool_call("1", "read", json!({"path": "missing"})),
                tool_call("2", "bash", json!({"command": "touch ran"})),
                tool_call("3", "read", json!({"path": "absent"})),
            ],
            ..calling("", "", json!({}))
        }
    }

    /// Returns the names of the tools `events` show as starting, in order.
    fn started(events: &[AgentEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolStarted(call) => Some(call.name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Returns when the person was first asked what the parked Turn of `agent` waits on.
    fn put_off_since(agent: &Agent) -> SystemTime {
        match agent.entries().last() {
            Some(Entry::TurnPutOff { waiting_since, .. }) => *waiting_since,
            last => panic!("the Turn should be put off, not end with {last:?}"),
        }
    }

    #[tokio::test]
    async fn a_put_off_question_parks_the_turn_and_is_asked_again_after_a_relaunch() {
        let script = vec![Ok(read_touch_read()), Ok(reply("done"))];
        let mut h = harness(script, Arc::new(DeferAll));
        let before = SystemTime::now();

        let (end, events) = run(&mut h.agent).await;

        assert_eq!(end.unwrap(), TurnEnd::PutOff);
        assert_eq!(started(&events), ["read", "bash"]);
        assert!(put_off_since(&h.agent) >= before);
        assert_eq!(tool_results(h.agent.entries()).len(), 1);
        assert!(crate::session::is_parked(&h.session).unwrap());

        let mut h = relaunched(h, Arc::new(AllowAll));
        assert_eq!(tool_results(h.agent.entries()).len(), 1);
        let (end, events) = carry_on(&mut h.agent).await;

        assert_eq!(end, TurnEnd::Finished);
        assert_eq!(started(&events), ["read"]);
        assert!(h.dir.path().join("ran").exists());
        let results = tool_results(h.agent.entries());
        let ids: Vec<&str> = results.iter().map(|result| result.0).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        let last_request = h.provider.requests().pop().unwrap();
        assert!(matches!(
            last_request.last(),
            Some(Entry::ToolResult { .. })
        ));
    }

    #[tokio::test]
    async fn only_the_calls_after_a_put_off_question_were_never_started() {
        let mut h = harness(vec![Ok(read_touch_read())], Arc::new(DeferAll));
        assert_eq!(calls_not_started(h.agent.entries()), HashSet::new());

        let _ = run(&mut h.agent).await;

        let not_started = calls_not_started(h.agent.entries());
        assert_eq!(not_started, HashSet::from(["3".to_owned()]));
    }

    #[tokio::test]
    async fn a_prompt_queued_before_a_question_is_put_off_reaches_the_model_after_its_call() {
        let script = vec![Ok(read_touch_read()), Ok(reply("done"))];
        let mut h = harness(script, Arc::new(DeferAll));
        let (end, _) = run_with_queued(&mut h, "also this", false).await;
        assert_eq!(end, TurnEnd::PutOff);
        assert!(matches!(
            h.agent.entries().last(),
            Some(Entry::TurnPutOff { queued: Some(text), .. }) if text == "also this"
        ));

        let mut h = relaunched(h, Arc::new(AllowAll));
        let (end, events) = carry_on(&mut h.agent).await;

        assert_eq!(end, TurnEnd::Finished);
        assert!(events.contains(&AgentEvent::Admitted("also this".into())));
        let queued = Entry::User {
            text: "also this".into(),
        };
        assert_eq!(h.provider.requests()[1].last(), Some(&queued));
    }

    #[tokio::test]
    async fn a_question_put_off_again_keeps_when_it_was_first_asked() {
        let mut h = harness(vec![Ok(read_touch_read())], Arc::new(DeferAll));
        let _ = run(&mut h.agent).await;
        let first_asked = put_off_since(&h.agent);

        let mut h = relaunched(h, Arc::new(DeferAll));

        let (end, events) = carry_on(&mut h.agent).await;
        assert_eq!(end, TurnEnd::PutOff);
        assert_eq!(started(&events), Vec::<&str>::new());
        assert_eq!(put_off_since(&h.agent), first_asked);
        assert_eq!(tool_results(h.agent.entries()).len(), 1);
        assert_eq!(h.provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_new_turn_in_place_of_a_put_off_question_says_its_calls_were_not_run() {
        let script = vec![Ok(read_touch_read()), Ok(reply("fine"))];
        let mut h = harness(script, Arc::new(DeferAll));
        let _ = run(&mut h.agent).await;

        let (end, _) = run(&mut h.agent).await;

        assert_eq!(end.unwrap(), TurnEnd::Finished);
        let results = tool_results(h.agent.entries());
        assert_eq!(
            results[1..],
            [("2", INTERRUPTED, true), ("3", INTERRUPTED, true)]
        );
        assert!(!h.dir.path().join("ran").exists());
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
            queued: &QueuedPrompts::default(),
            on_event: &mut ignore,
        };
        let end = h.agent.run_turn("go", &mut context).await.unwrap();
        assert_eq!(end, TurnEnd::Finished);
    }
    /// Runs a Turn on `h` in which the person queued `text` before its first Step ended, asking
    /// it to pause between Steps if `paused`, and returns how it ended with what it reported.
    async fn run_with_queued(
        h: &mut Harness,
        text: &str,
        paused: bool,
    ) -> (TurnEnd, Vec<AgentEvent>) {
        let cancel = CancellationToken::new();
        let pause = AtomicBool::new(paused);
        let queued = QueuedPrompts::default();
        queued.push(text.into());
        let mut events = Vec::new();
        let mut context = TurnContext {
            cancel: &cancel,
            pause: &pause,
            queued: &queued,
            on_event: &mut |event| events.push(event),
        };
        let end = h.agent.run_turn("go", &mut context).await.unwrap();
        (end, events)
    }

    #[tokio::test]
    async fn a_prompt_queued_during_a_step_reaches_the_model_in_the_next_one() {
        let script = vec![
            Ok(calling("1", "read", json!({"path": "missing"}))),
            Ok(reply("done")),
        ];
        let mut h = harness(script, Arc::new(AllowAll));

        let (end, events) = run_with_queued(&mut h, "also this", false).await;

        assert_eq!(end, TurnEnd::Finished);
        let queued = Entry::User {
            text: "also this".into(),
        };
        assert_eq!(h.provider.requests()[1].last(), Some(&queued));
        assert!(events.contains(&AgentEvent::Admitted("also this".into())));
    }

    #[tokio::test]
    async fn a_prompt_queued_before_the_turn_parks_is_recorded_first() {
        let script = vec![Ok(calling("1", "read", json!({"path": "missing"})))];
        let mut h = harness(script, Arc::new(AllowAll));

        let (end, _) = run_with_queued(&mut h, "also this", true).await;

        assert_eq!(end, TurnEnd::Paused);
        let entries = h.agent.entries();
        let queued = Entry::User {
            text: "also this".into(),
        };
        assert_eq!(entries[entries.len() - 2], queued);
    }
}
