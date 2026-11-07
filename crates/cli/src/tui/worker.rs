//! Running Turns in the background, so the screen stays live while the agent works.

use sstack_core::agent::{Agent, TurnContext, TurnEnd};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::event::{Event, EventSender};

/// Lets the App start and cancel Turns on an agent that runs on its own task.
///
/// Everything the Turn does comes back as events: [`Event::TurnStarted`], then any number of
/// [`Event::Agent`], then [`Event::TurnEnded`].
pub struct AgentHandle {
    /// Where Turns are queued for the agent's task.
    prompts: mpsc::UnboundedSender<Prompt>,
    /// Stops the most recently started Turn.
    cancel: CancellationToken,
}

/// A Turn waiting to be run.
struct Prompt {
    /// The person's input for the model.
    text: String,
    /// Stops this Turn, even before it starts.
    cancel: CancellationToken,
}

impl AgentHandle {
    /// Moves `agent` onto its own task, which reports what its Turns do to `events`.
    ///
    /// The task ends once this handle is dropped and any queued Turns have finished.
    pub fn spawn(agent: Agent, events: EventSender) -> Self {
        let (prompts, queued) = mpsc::unbounded_channel();
        tokio::spawn(serve(agent, queued, events));
        Self {
            prompts,
            cancel: CancellationToken::new(),
        }
    }

    /// Starts a Turn on `text`, after any Turn that is already running.
    pub fn prompt(&mut self, text: String) {
        self.cancel = CancellationToken::new();
        let cancel = self.cancel.clone();
        let _ = self.prompts.send(Prompt { text, cancel });
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
    events: EventSender,
) {
    while let Some(prompt) = prompts.recv().await {
        let _ = events.send(Event::TurnStarted(prompt.text.clone()));
        let ended = run_turn(&mut agent, &prompt, &events).await;
        let _ = events.send(Event::TurnEnded(ended));
    }
}

/// Runs one Turn, passing on its events as they happen.
async fn run_turn(
    agent: &mut Agent,
    prompt: &Prompt,
    events: &EventSender,
) -> Result<TurnEnd, String> {
    let mut forward = |event| {
        let _ = events.send(Event::Agent(event));
    };
    let mut context = TurnContext {
        cancel: &prompt.cancel,
        on_event: &mut forward,
    };
    agent
        .run_turn(&prompt.text, &mut context)
        .await
        .map_err(|error| error.to_string())
}
