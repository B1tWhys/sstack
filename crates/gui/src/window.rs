//! The window that shows one Session: what was said in it, and a prompt to say more.

use std::mem;

use iced::widget::{button, column, row, scrollable, text, text_editor};
use iced::{Element, Fill, Subscription, Task};
use sstack::agent::AgentEvent;
use sstack::running_harness::{Outcome, Refusal, RunningHarness};
use sstack::session;
use sstack::session_controller::{NoOtherSessions, SessionController};
use sstack::session_report::SessionReport;
use sstack::session_setup::Run;
use sstack::turn_runner::AgentHandle;
use sstack::workspace;

use crate::reports::Reports;

/// The id of the prompt, by which tests find it.
const PROMPT: &str = "prompt";

/// Space between the parts of the window, and around them, in logical pixels.
const SPACING: f32 = 10.0;

/// One Session open in a window, which takes the person's prompts and shows the replies.
pub struct Window {
    /// The Running Harness, whose only open Session is this one.
    harness: RunningHarness,
    /// The open Session.
    session: SessionController,
    /// What the person has typed and not yet sent.
    prompt: text_editor::Content,
    /// What has been said since the window opened, oldest first. What the Session held before
    /// is not shown.
    transcript: Vec<Block>,
    /// The Session's reports, on their way to [`Self::update`].
    reports: Reports,
    /// The id of the Session, which names the window.
    id: String,
}

/// Something that happened in the window, for [`Window::update`] to act on.
#[derive(Debug, Clone)]
pub enum Message {
    /// The person changed the prompt.
    Edit(text_editor::Action),
    /// The person sent the prompt.
    Submit,
    /// Core reported on the Session.
    Report(SessionReport),
}

/// One part of the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    /// What the person sent.
    Prompt(String),
    /// The model's reply, as much of it as has arrived. The text of every Step in a Turn runs
    /// together, since tool calls are not shown.
    Reply(String),
    /// Something the window tells the person, such as why it could not do what they asked.
    Notice(String),
}

impl Window {
    /// Opens a window on the readied Session, carrying on any Turn an earlier run left parked.
    ///
    /// Must be called inside a Tokio runtime, which runs the Session's Turns.
    pub fn open(run: Run) -> Self {
        let id = workspace::session_id(&run.session);
        let (reports, sender) = Reports::new(&id);
        let parked = session::is_parked(&run.session).unwrap_or(false);
        let agent = AgentHandle::spawn(run.agent, sender);
        let mut session = SessionController::new(agent, run.skills, parked);
        session.continue_parked();
        let notices = run
            .notices
            .into_iter()
            .chain(listing_shortened(run.descriptions_left_out));
        Self {
            harness: RunningHarness::default(),
            session,
            prompt: text_editor::Content::new(),
            transcript: notices.map(Block::Notice).collect(),
            reports,
            id,
        }
    }

    /// Acts on `message`, returning what iced should do next.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Edit(action) => {
                self.prompt.perform(action);
                Task::none()
            }
            Message::Submit => self.submit(),
            Message::Report(report) => self.report(&report),
        }
    }

    /// Draws the window.
    pub fn view(&self) -> Element<'_, Message> {
        let transcript = column(self.transcript.iter().map(Block::view)).spacing(SPACING);
        let prompt = text_editor(&self.prompt)
            .id(PROMPT)
            .placeholder("Say something, or type a / command")
            .on_action(Message::Edit);
        let send = button("Send").on_press(Message::Submit);
        column![
            scrollable(transcript).height(Fill).width(Fill),
            row![prompt, send].spacing(SPACING),
        ]
        .padding(SPACING)
        .spacing(SPACING)
        .into()
    }

    /// Returns the window's title, which names the Session.
    pub fn title(&self) -> String {
        format!("sstack {}", self.id)
    }

    /// Returns where the Session's reports come from.
    pub fn subscription(&self) -> Subscription<Message> {
        self.reports.subscription().map(Message::Report)
    }

    /// Acts on what the person typed, which stays in the prompt only if it was refused.
    fn submit(&mut self) -> Task<Message> {
        let draft = mem::take(&mut self.prompt);
        let typed = draft.text();
        let line = typed.trim();
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .submit(line);
        match outcome {
            Some(Outcome::TurnStarted(_)) => self.transcript.push(Block::Prompt(line.to_owned())),
            Some(Outcome::Refused(refusal)) => {
                self.prompt = draft;
                self.notice(refusal_text(&refusal));
            }
            Some(Outcome::Command(_)) => {
                self.notice(format!("{line} is not available in the GUI yet"));
            }
            other => return self.carry_out(other.as_ref()),
        }
        Task::none()
    }

    /// Shows what core reported, and acts on it.
    fn report(&mut self, report: &SessionReport) -> Task<Message> {
        match report {
            SessionReport::Agent(AgentEvent::Text(piece)) => self.reply(piece),
            SessionReport::TurnEnded(Err(error)) => {
                self.notice(format!("the Turn failed: {error}"));
            }
            _ => {}
        }
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .report(report);
        self.carry_out(outcome.as_ref())
    }

    /// Does what the Running Harness asked of the window, other than show something.
    fn carry_out(&mut self, outcome: Option<&Outcome<'_>>) -> Task<Message> {
        match outcome {
            Some(Outcome::StartBuild) => self.refuse_rebuild(),
            Some(Outcome::Exit(_)) => iced::exit(),
            _ => Task::none(),
        }
    }

    /// Backs out of the Rebuild the person asked for, which the GUI cannot do yet, so that the
    /// Session carries on.
    fn refuse_rebuild(&mut self) -> Task<Message> {
        self.notice("Rebuild is not available in the GUI yet".to_owned());
        let backed_out = self.harness.build_finished(None, &mut self.session);
        self.carry_out(backed_out.as_ref())
    }

    /// Adds `piece` to the reply that is arriving, or starts one.
    fn reply(&mut self, piece: &str) {
        if let Some(Block::Reply(reply)) = self.transcript.last_mut() {
            reply.push_str(piece);
        } else {
            self.transcript.push(Block::Reply(piece.to_owned()));
        }
    }

    /// Tells the person `words` in the transcript.
    fn notice(&mut self, words: String) {
        self.transcript.push(Block::Notice(words));
    }
}

impl Block {
    /// Draws this part of the transcript as plain text.
    fn view(&self) -> Element<'_, Message> {
        match self {
            Self::Prompt(prompt) => text(format!("> {prompt}")).into(),
            Self::Reply(words) | Self::Notice(words) => text(words).into(),
        }
    }
}

/// Returns the warning that `count` Skills are listed to the model without their descriptions,
/// if any are.
fn listing_shortened(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} Skill descriptions were left out of the system prompt, so the model may not \
             think to load those Skills"
        )
    })
}

/// Returns the words that tell the person why the Running Harness refused what they asked.
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Busy => "a Turn is already running in this Session".to_owned(),
        Refusal::Draining => "sstack is quitting, so nothing new can start".to_owned(),
        Refusal::OthersWorking => "other Sessions are still working".to_owned(),
        Refusal::UnknownCommand(line) => format!("{line} is neither a Human Command nor a Skill"),
        Refusal::AmbiguousSkill { name, names } => {
            let names = names.join(" or ");
            format!("/{name} could mean {names}, so type the full name")
        }
        Refusal::UnreadableSkill { name, error } => {
            format!("could not read the Skill {name}: {error}")
        }
    }
}

#[cfg(test)]
mod tests;
