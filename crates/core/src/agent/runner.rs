//! Turns run in the background, so the interface stays live while the agent works.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::{Agent, ModelService, QueuedPrompts, TurnContext, TurnEnd};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session_report::{ReportSender, SessionReport};

/// An interface's handle on an agent that runs on its own task, for starting and cancelling Turns.
///
/// Everything comes back as [`SessionReport`]s. First [`SessionReport::Resumed`] tells what the
/// Session already holds. Then each Turn sends [`SessionReport::TurnStarted`] or
/// [`SessionReport::TurnContinued`], any number of [`SessionReport::Agent`], and
/// [`SessionReport::TurnEnded`].
pub struct AgentHandle {
    /// Where Turns, and switches between them, are queued for the agent's task.
    jobs: mpsc::UnboundedSender<Job>,
    /// Cancelling this stops the most recently started Turn.
    cancel: CancellationToken,
    /// Whether every Turn should stop at the end of its Step.
    pause: Arc<AtomicBool>,
    /// The prompts the person sent while a Turn was running, which that Turn takes at the end of
    /// its Step.
    queued: QueuedPrompts,
}

/// What a queued Turn does.
enum Work {
    /// A new Turn on this input from the person.
    Start(String),
    /// More of a Turn that paused.
    Continue,
}

/// Something the agent's task does, in the order it was asked for.
enum Job {
    /// Runs a Turn.
    Turn(Prompt),
    /// Moves the Session to another model before the next Turn.
    Switch(ModelService),
}

/// A Turn waiting to be run.
struct Prompt {
    /// What the Turn does.
    work: Work,
    /// Cancelling this stops this Turn, even before it starts.
    cancel: CancellationToken,
    /// Whether this Turn should stop at the end of its Step.
    pause: Arc<AtomicBool>,
    /// The prompts this Turn takes at the end of each Step.
    queued: QueuedPrompts,
}

impl AgentHandle {
    /// Moves `agent` onto its own task, which reports what its Turns do to `reports`.
    ///
    /// The task ends once this handle is dropped and any queued Turns have finished.
    #[must_use]
    pub fn spawn(agent: Agent, reports: ReportSender) -> Self {
        reports.send(SessionReport::Resumed(agent.entries().to_vec()));
        let (jobs, queued) = mpsc::unbounded_channel();
        tokio::spawn(serve(agent, queued, reports));
        Self {
            jobs,
            cancel: CancellationToken::new(),
            pause: Arc::default(),
            queued: QueuedPrompts::default(),
        }
    }

    /// Starts a Turn on `text`, after any Turn that is already running.
    pub fn prompt(&mut self, text: String) {
        self.queue(Work::Start(text));
    }

    /// Queues `text` for the running Turn, which takes it at the end of its current Step. If the
    /// Turn ends first, [`Self::take_queued`] gets it back to start the next one.
    pub fn queue_prompt(&self, text: String) {
        self.queued.push(text);
    }

    /// Takes every prompt [`Self::queue_prompt`] queued that no Turn has taken yet, joined into
    /// one message, or returns `None` if there are none.
    #[must_use]
    pub fn take_queued(&self) -> Option<String> {
        self.queued.take()
    }

    /// Carries on the Turn that paused, after any Turn that is already running.
    pub fn continue_turn(&mut self) {
        self.queue(Work::Continue);
    }

    /// Moves the Session to `service` once the Turn running now, and any queued before this,
    /// have ended. Turns started after this call go to `service`.
    pub fn switch(&self, service: ModelService) {
        let _ = self.jobs.send(Job::Switch(service));
    }

    /// Queues `work` as the new most recent Turn.
    fn queue(&mut self, work: Work) {
        self.cancel = CancellationToken::new();
        let _ = self.jobs.send(Job::Turn(Prompt {
            work,
            cancel: self.cancel.clone(),
            pause: self.pause.clone(),
            queued: self.queued.clone(),
        }));
    }

    /// Asks Turns to stop at the end of their Step, or stops asking. A Turn that has already
    /// stopped needs [`Self::continue_turn`].
    pub fn set_paused(&self, paused: bool) {
        self.pause.store(paused, Ordering::SeqCst);
    }

    /// Cancels the most recently started Turn, leaving the Session resumable. Does nothing if
    /// that Turn has already ended.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Does queued jobs one at a time until no more can arrive.
async fn serve(mut agent: Agent, mut jobs: mpsc::UnboundedReceiver<Job>, reports: ReportSender) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Turn(prompt) => take_turn(&mut agent, &prompt, &reports).await,
            Job::Switch(service) => agent.switch(service),
        }
    }
}

/// Runs one Turn, reporting that it started, what it does and how it ended.
async fn take_turn(agent: &mut Agent, prompt: &Prompt, reports: &ReportSender) {
    reports.send(match &prompt.work {
        Work::Start(text) => SessionReport::TurnStarted(text.clone()),
        Work::Continue => SessionReport::TurnContinued,
    });
    let ended = run_turn(agent, prompt, reports).await;
    reports.send(SessionReport::TurnEnded(ended));
}

/// Runs one Turn, passing on its events as they happen.
async fn run_turn(
    agent: &mut Agent,
    prompt: &Prompt,
    reports: &ReportSender,
) -> Result<TurnEnd, String> {
    let mut forward = |event| reports.send(SessionReport::Agent(event));
    let mut context = TurnContext {
        cancel: &prompt.cancel,
        pause: &prompt.pause,
        queued: &prompt.queued,
        on_event: &mut forward,
    };
    let ended = match &prompt.work {
        Work::Start(text) => agent.run_turn(text, &mut context).await,
        Work::Continue => agent.continue_turn(&mut context).await,
    };
    ended.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentConfig;
    use crate::provider::Effort;
    use crate::provider::scripted::ScriptedProvider;
    use crate::session::{AssistantEntry, SessionLog, StopReason, Usage};
    use crate::tools::project::Project;
    use crate::tools::test_support::{disabled, git_project};
    use crate::tools::{DenyAll, ToolBox};

    /// Returns a model that says `text` once.
    fn model(text: &str) -> Arc<ScriptedProvider> {
        let reply = AssistantEntry {
            text: text.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        };
        Arc::new(ScriptedProvider::new([Ok(reply)]))
    }

    #[tokio::test]
    async fn a_switch_waits_for_the_turns_queued_before_it() {
        let (dir, _) = git_project();
        let (first, second) = (model("one"), model("two"));
        let config = AgentConfig {
            provider: first.clone(),
            model: "first".into(),
            system: String::new(),
            tools: ToolBox::standard()
                .project(Arc::new(Project::new(dir.path()).unwrap()))
                .sandbox(disabled())
                .approver(Arc::new(DenyAll))
                .call(),
            context_limit: 1000,
        };
        let log = SessionLog::create(&dir.path().join("1.jsonl")).unwrap();
        let (sender, mut received) = mpsc::unbounded_channel();
        let reports = ReportSender::new(move |report| {
            let _ = sender.send(report);
        });
        let mut handle = AgentHandle::spawn(Agent::new(config, log, vec![]).unwrap(), reports);

        handle.prompt("first".into());
        handle.switch(ModelService {
            provider: second.clone(),
            model: "second".into(),
            effort: Effort::Low,
            context_limit: 1000,
        });
        handle.prompt("second".into());
        let mut ended = 0;
        while ended < 2 {
            if let Some(SessionReport::TurnEnded(end)) = received.recv().await {
                assert_eq!(end, Ok(TurnEnd::Finished));
                ended += 1;
            }
        }

        assert_eq!(first.requests().len(), 1);
        assert_eq!(second.requests().len(), 1);
    }
}
