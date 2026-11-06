//! The loop that drives a Session: model requests, tool runs and durable recording.

use std::collections::HashSet;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::provider::{ModelRequest, Provider, ProviderError, StreamEvent, ToolSpec};
use crate::session::{AssistantEntry, Entry, SessionError, SessionLog, StopReason, ToolCall};
use crate::tools::ToolBox;

const INTERRUPTED: &str = "not run: the turn was interrupted";
const NOT_RUN: &str = "not run: the response ended before this call could be used";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Text(String),
    Reasoning(String),
    ToolStarted(ToolCallSummary),
    ToolFinished { name: String, is_error: bool },
    ContextNearLimit { used: u64, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallSummary {
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Finished,
    Truncated,
    Refused,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// What an [`Agent`] runs on.
pub struct AgentConfig {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub system: String,
    pub tools: ToolBox,
    pub context_limit: u64,
}

pub struct Agent {
    config: AgentConfig,
    specs: Vec<ToolSpec>,
    log: SessionLog,
    entries: Vec<Entry>,
}

type EventSink<'a> = &'a mut (dyn FnMut(AgentEvent) + Send);

impl Agent {
    /// Starts from an open log and its existing entries. A crash can leave tool calls without
    /// results, which Providers reject, so those are settled here first.
    ///
    /// # Errors
    /// Returns an error if settling an interrupted turn cannot be recorded.
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

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Runs one Turn to its end. Cancelling `cancel` stops it, leaving the Session resumable.
    ///
    /// # Errors
    /// Returns an error if the Provider fails or the Session cannot be recorded.
    pub async fn run_turn(
        &mut self,
        input: &str,
        cancel: &CancellationToken,
        on_event: EventSink<'_>,
    ) -> Result<TurnEnd, AgentError> {
        self.record(Entry::User {
            text: input.to_owned(),
        })?;
        loop {
            if let Some(end) = self.step(cancel, on_event).await? {
                return Ok(end);
            }
        }
    }

    async fn step(
        &mut self,
        cancel: &CancellationToken,
        on_event: EventSink<'_>,
    ) -> Result<Option<TurnEnd>, AgentError> {
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return self.cancelled().map(Some),
            response = self.request(on_event) => response?,
        };
        self.warn_near_limit(&response, on_event);
        let calls = response.tool_calls.clone();
        let stop = response.stop;
        self.record(Entry::Assistant(response))?;
        if stop != StopReason::ToolUse || calls.is_empty() {
            self.settle_unanswered_calls(NOT_RUN)?;
            return Ok(Some(turn_end(stop)));
        }
        self.run_tools(&calls, cancel, on_event).await
    }

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

    async fn run_tools(
        &mut self,
        calls: &[ToolCall],
        cancel: &CancellationToken,
        on_event: EventSink<'_>,
    ) -> Result<Option<TurnEnd>, AgentError> {
        for call in calls {
            on_event(AgentEvent::ToolStarted(ToolCallSummary {
                name: call.name.clone(),
                input: call.input.to_string(),
            }));
            let output = tokio::select! {
                biased;
                () = cancel.cancelled() => return self.cancelled().map(Some),
                output = self.config.tools.run(call) => output,
            };
            on_event(AgentEvent::ToolFinished {
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

    fn warn_near_limit(&self, response: &AssistantEntry, on_event: EventSink<'_>) {
        let used = response.usage.input_tokens;
        let limit = self.config.context_limit;
        if used.saturating_mul(5) >= limit.saturating_mul(4) {
            on_event(AgentEvent::ContextNearLimit { used, limit });
        }
    }

    fn cancelled(&mut self) -> Result<TurnEnd, AgentError> {
        self.settle_unanswered_calls(INTERRUPTED)?;
        self.record(Entry::TurnCancelled)?;
        Ok(TurnEnd::Cancelled)
    }

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

    fn record(&mut self, entry: Entry) -> Result<(), SessionError> {
        self.log.append(&entry)?;
        self.entries.push(entry);
        Ok(())
    }
}

fn stream_event(event: StreamEvent) -> AgentEvent {
    match event {
        StreamEvent::Text(text) => AgentEvent::Text(text),
        StreamEvent::Reasoning(text) => AgentEvent::Reasoning(text),
    }
}

fn turn_end(stop: StopReason) -> TurnEnd {
    match stop {
        StopReason::EndTurn | StopReason::ToolUse => TurnEnd::Finished,
        StopReason::MaxTokens => TurnEnd::Truncated,
        StopReason::Refused => TurnEnd::Refused,
    }
}

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
        let agent = agent_on(&session, provider.clone(), project, approver);
        Harness {
            _dir: dir,
            agent,
            provider,
            session,
        }
    }

    fn agent_on(
        session: &Path,
        provider: Arc<dyn Provider>,
        project: Arc<crate::tools::project::Project>,
        approver: Arc<dyn Approver>,
    ) -> Agent {
        let log = SessionLog::create(session).unwrap();
        Agent::new(config(provider, project, approver), log, vec![]).unwrap()
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
            tools: ToolBox::standard(project, approver, vec![]),
            context_limit: LIMIT,
        }
    }

    async fn run(agent: &mut Agent) -> (Result<TurnEnd, AgentError>, Vec<AgentEvent>) {
        let mut events = Vec::new();
        let end = agent
            .run_turn("go", &CancellationToken::new(), &mut |e| events.push(e))
            .await;
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

        let end = h
            .agent
            .run_turn("go", &cancel, &mut |event| {
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
        let mut agent = agent_on(
            &session,
            Arc::new(NeverResponds),
            project,
            Arc::new(DenyAll),
        );
        let cancel = CancellationToken::new();
        cancel.cancel();

        let end = agent.run_turn("go", &cancel, &mut |_| {}).await;

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
}
