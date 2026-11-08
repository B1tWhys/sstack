//! Turns run in the background, so the interface stays live while the agent works.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::{Agent, TurnContext, TurnEnd};
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
    /// Where Turns are queued for the agent's task.
    prompts: mpsc::UnboundedSender<Prompt>,
    /// Cancelling this stops the most recently started Turn.
    cancel: CancellationToken,
    /// Whether every Turn should stop at the end of its Step.
    pause: Arc<AtomicBool>,
}

/// What a queued Turn does.
enum Work {
    /// A new Turn on this input from the person.
    Start(String),
    /// More of a Turn that paused.
    Continue,
}

/// A Turn waiting to be run.
struct Prompt {
    /// What the Turn does.
    work: Work,
    /// Cancelling this stops this Turn, even before it starts.
    cancel: CancellationToken,
    /// Whether this Turn should stop at the end of its Step.
    pause: Arc<AtomicBool>,
}

impl AgentHandle {
    /// Moves `agent` onto its own task, which reports what its Turns do to `reports`.
    ///
    /// The task ends once this handle is dropped and any queued Turns have finished.
    #[must_use]
    pub fn spawn(agent: Agent, reports: ReportSender) -> Self {
        reports.send(SessionReport::Resumed(agent.entries().to_vec()));
        let (prompts, queued) = mpsc::unbounded_channel();
        tokio::spawn(serve(agent, queued, reports));
        Self {
            prompts,
            cancel: CancellationToken::new(),
            pause: Arc::default(),
        }
    }

    /// Starts a Turn on `text`, after any Turn that is already running.
    pub fn prompt(&mut self, text: String) {
        self.queue(Work::Start(text));
    }

    /// Carries on the Turn that paused, after any Turn that is already running.
    pub fn continue_turn(&mut self) {
        self.queue(Work::Continue);
    }

    /// Queues `work` as the new most recent Turn.
    fn queue(&mut self, work: Work) {
        self.cancel = CancellationToken::new();
        let _ = self.prompts.send(Prompt {
            work,
            cancel: self.cancel.clone(),
            pause: self.pause.clone(),
        });
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

/// Runs queued Turns one at a time until no more can arrive.
async fn serve(
    mut agent: Agent,
    mut prompts: mpsc::UnboundedReceiver<Prompt>,
    reports: ReportSender,
) {
    while let Some(prompt) = prompts.recv().await {
        reports.send(match &prompt.work {
            Work::Start(text) => SessionReport::TurnStarted(text.clone()),
            Work::Continue => SessionReport::TurnContinued,
        });
        let ended = run_turn(&mut agent, &prompt, &reports).await;
        reports.send(SessionReport::TurnEnded(ended));
    }
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
        on_event: &mut forward,
    };
    let ended = match &prompt.work {
        Work::Start(text) => agent.run_turn(text, &mut context).await,
        Work::Continue => agent.continue_turn(&mut context).await,
    };
    ended.map_err(|error| error.to_string())
}
