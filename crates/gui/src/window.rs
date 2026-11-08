//! The window that shows one Session: what was said in it, and a prompt to say more.

use std::collections::VecDeque;
use std::mem;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use iced::futures::future::BoxFuture;
use iced::widget::scrollable::Viewport;
use iced::widget::{button, column, operation, row, scrollable, text, text_editor};
use iced::{Element, Fill, Subscription, Task, keyboard, task, window};
use sstack::running_harness::{Exit, Outcome, Phase, Refusal, RunningHarness};
use sstack::session;
use sstack::session_controller::{NoOtherSessions, SessionController};
use sstack::session_report::{ApprovalRequest, ReportSender, SessionReport};
use sstack::session_setup::Run;
use sstack::turn_runner::AgentHandle;
use sstack::workspace;

use crate::reports::Reports;
use crate::transcript::Transcript;

/// The id of the prompt, by which tests find it.
const PROMPT: &str = "prompt";

/// The id of the transcript's scrollable, by which the window keeps it at the newest output.
const TRANSCRIPT: &str = "transcript";

/// How far from the newest output, in logical pixels, the transcript can sit and still follow
/// it, so that rounding in the layout does not stop it following.
const FOLLOW_SLACK: f32 = 1.0;

/// Space between the parts of the window, and around them, in logical pixels.
const SPACING: f32 = 10.0;

/// Builds the Harness Source for a Rebuild, ending in the new binary or in why the build failed.
///
/// The window starts one when a Rebuild asks for it, and drops it to stop the build. Tests give
/// the window one that never runs cargo.
pub type Build = fn() -> BoxFuture<'static, Result<PathBuf, String>>;

/// A Session readied for a window, with the reports it will send the window that shows it.
pub struct Ready {
    /// The Session as setup readied it.
    pub run: Run,
    /// Where the window reads the Session's reports.
    pub reports: Reports,
    /// Where the Session's reports go, which the window hands the Turns it runs. The Session's
    /// Approver must ask through the same reports, or the window never sees its questions.
    pub sender: ReportSender,
    /// How the window builds `sstack-gui` when the person asks for a Rebuild.
    pub build: Build,
    /// Where the window leaves the binary a Rebuild built as it closes, for the process to
    /// relaunch into once iced has torn the window down. Empty if the window closed for any other
    /// reason.
    pub relaunch: Arc<OnceLock<PathBuf>>,
}

/// One Session open in a window, which takes the person's prompts and shows the replies.
pub struct Window {
    /// The Running Harness, whose only open Session is this one.
    harness: RunningHarness,
    /// The open Session.
    session: SessionController,
    /// What the person has typed and not yet sent.
    prompt: text_editor::Content,
    /// What has been said in the Session, including before the window opened.
    transcript: Transcript,
    /// Questions about shell commands that the Session's Turn is waiting on, oldest first. Only
    /// the oldest is shown. Dropping one unanswered, as closing the window does, denies it.
    questions: VecDeque<ApprovalRequest>,
    /// Whether the transcript keeps its newest output in view as more arrives. It stops once the
    /// person scrolls back, and starts again when they scroll to the end or start a Turn.
    following: bool,
    /// The Session's reports, on their way to [`Self::update`].
    reports: Reports,
    /// How the window builds `sstack-gui` for a Rebuild.
    build: Build,
    /// The build a Rebuild started, while it runs. Dropping it stops the build.
    building: Option<task::Handle>,
    /// Where the window leaves the binary to relaunch into, once a Rebuild is ready.
    relaunch: Arc<OnceLock<PathBuf>>,
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
    /// The person answered the question about a shell command: whether it may run.
    Answer(bool),
    /// The person asked to interrupt.
    Interrupt,
    /// The person asked for a Rebuild, as `/rebuild` does.
    Rebuild,
    /// The person asked to close the window, which quits as `/quit` does, and quits instead of
    /// relaunching during a Rebuild.
    CloseRequested,
    /// The build a Rebuild started finished, with the new binary or why it failed.
    Built(Result<PathBuf, String>),
    /// The person scrolled the transcript.
    Scrolled {
        /// Whether the newest output is in view.
        at_end: bool,
    },
}

impl Window {
    /// Opens a window on the readied Session, carrying on any Turn an earlier run left parked.
    ///
    /// Must be called inside a Tokio runtime, which runs the Session's Turns.
    pub fn open(ready: Ready) -> Self {
        let Ready {
            run,
            reports,
            sender,
            build,
            relaunch,
        } = ready;
        let id = workspace::session_id(&run.session);
        let parked = session::is_parked(&run.session).unwrap_or(false);
        let agent = AgentHandle::spawn(run.agent, sender);
        let mut session = SessionController::new(agent, run.skills, parked);
        session.continue_parked();
        let mut transcript = Transcript::default();
        let notices = run
            .notices
            .into_iter()
            .chain(listing_shortened(run.descriptions_left_out));
        notices.for_each(|notice| transcript.notice(notice));
        Self {
            harness: RunningHarness::default(),
            session,
            prompt: text_editor::Content::new(),
            transcript,
            questions: VecDeque::new(),
            following: true,
            reports,
            build,
            building: None,
            relaunch,
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
            Message::Scrolled { at_end } => {
                self.following = at_end;
                Task::none()
            }
            Message::Answer(allowed) => {
                self.answer(allowed);
                Task::none()
            }
            Message::Interrupt => self.interrupt(),
            Message::Built(built) => self.built(built),
            Message::Rebuild => self.command("/rebuild"),
            Message::CloseRequested => self.close(),
        }
    }

    /// Draws the window.
    pub fn view(&self) -> Element<'_, Message> {
        let transcript = scrollable(self.transcript.view())
            .id(TRANSCRIPT)
            .on_scroll(|viewport| Message::Scrolled {
                at_end: at_end(&viewport),
            })
            .height(Fill)
            .width(Fill);
        let prompt = text_editor(&self.prompt)
            .id(PROMPT)
            .placeholder("Say something, or type a / command")
            .on_action(Message::Edit);
        let send = button("Send").on_press(Message::Submit);
        let draining = self.harness.phase().is_draining();
        let interruptible = self.session.state().in_turn() || draining;
        let interrupt =
            button("Interrupt").on_press_maybe(interruptible.then_some(Message::Interrupt));
        let rebuild = button("Rebuild").on_press_maybe((!draining).then_some(Message::Rebuild));
        let question = self.questions.front().map(question);
        let progress = progress(self.harness.phase()).map(text);
        column![transcript]
            .push(question)
            .push(progress)
            .push(row![prompt, send, interrupt, rebuild].spacing(SPACING))
            .padding(SPACING)
            .spacing(SPACING)
            .into()
    }

    /// Returns the window's title, which names the Session.
    pub fn title(&self) -> String {
        format!("sstack {}", self.id)
    }

    /// Returns where the Session's reports come from, with the keys and requests to close that
    /// the window acts on.
    ///
    /// The window hears a request to close only if iced is told not to exit on one itself.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            self.reports.subscription().map(Message::Report),
            keyboard::listen().filter_map(|event| interrupt_key(&event)),
            window::close_requests().map(|_| Message::CloseRequested),
        ])
    }

    /// Quits as `/quit` does: at once if no Turn is running, or once the running Turn reaches
    /// the end of its Step. Asking again while waiting quits at once.
    ///
    /// During a Rebuild, a close backs out of the Rebuild, which stops the build, and then quits
    /// as above, so closing twice quits at once. That choice is made here alone, so a close can
    /// be made to mean something else during a Rebuild by changing only this.
    fn close(&mut self) -> Task<Message> {
        let backed_out = if self.harness.phase().is_rebuilding() {
            self.interrupt()
        } else {
            Task::none()
        };
        Task::batch([backed_out, self.command("/quit")])
    }

    /// Gives the Human Command `line` as if the person had typed it, without touching what they
    /// have typed.
    fn command(&mut self, line: &str) -> Task<Message> {
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .submit(line);
        self.carry_out(outcome.as_ref())
    }

    /// Acts on the person's request to interrupt. It cancels a running Turn, backs out of a
    /// quit that is waiting for one, and otherwise quits.
    fn interrupt(&mut self) -> Task<Message> {
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .interrupt();
        self.carry_out(outcome.as_ref())
    }

    /// Acts on what the person typed, which stays in the prompt only if it was refused. A prompt
    /// that starts a Turn brings the newest output back into view.
    fn submit(&mut self) -> Task<Message> {
        let draft = mem::take(&mut self.prompt);
        let typed = draft.text();
        let line = typed.trim();
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .submit(line);
        match outcome {
            Some(Outcome::TurnStarted(_)) => {
                self.following = true;
                self.transcript.prompt(line.to_owned());
            }
            Some(Outcome::Refused(refusal)) => {
                self.prompt = draft;
                self.notice(refusal_text(&refusal));
            }
            Some(Outcome::Command(_)) => {
                self.notice(format!("{line} is not available in the GUI yet"));
            }
            other => return self.carry_out(other.as_ref()),
        }
        self.follow(true)
    }

    /// Answers the oldest open question about a shell command, if there is one: `allowed` says
    /// whether the command may run.
    fn answer(&mut self, allowed: bool) {
        if let Some(question) = self.questions.pop_front() {
            question.reply.send(allowed);
            self.session.answered();
        }
    }

    /// Keeps the open questions about shell commands current with what core reported. A Turn
    /// that ends has no more use for its questions.
    fn track_questions(&mut self, report: &SessionReport) {
        match report {
            SessionReport::Approval(question) => self.questions.push_back(question.clone()),
            SessionReport::TurnEnded(_) => self.questions.clear(),
            _ => {}
        }
    }

    /// Shows what core reported, and acts on it.
    fn report(&mut self, report: &SessionReport) -> Task<Message> {
        self.track_questions(report);
        let changed = self.transcript.report(report);
        let follow = self.follow(changed);
        let outcome = self
            .harness
            .in_session(&mut self.session, &mut NoOtherSessions)
            .report(report);
        Task::batch([follow, self.carry_out(outcome.as_ref())])
    }

    /// Returns the task that brings the newest output into view, if the transcript `changed`
    /// while following it.
    fn follow(&self, changed: bool) -> Task<Message> {
        if changed && self.following {
            operation::snap_to_end(TRANSCRIPT)
        } else {
            Task::none()
        }
    }

    /// Does what the Running Harness asked of the window, other than show something.
    fn carry_out(&mut self, outcome: Option<&Outcome<'_>>) -> Task<Message> {
        match outcome {
            Some(Outcome::StartBuild) => self.start_build(),
            Some(Outcome::CancelBuild) => {
                self.cancel_build();
                Task::none()
            }
            Some(Outcome::Exit(exit)) => self.exit(exit),
            _ => Task::none(),
        }
    }

    /// Closes the window, leaving the binary to relaunch into if the Running Harness stopped for a
    /// Rebuild.
    fn exit(&self, exit: &Exit) -> Task<Message> {
        if let Exit::Rebuild(binary) = exit {
            let _ = self.relaunch.set(binary.clone());
        }
        iced::exit()
    }

    /// Starts building `sstack-gui` for a Rebuild, returning the task that runs the build and
    /// reports its end as [`Message::Built`].
    fn start_build(&mut self) -> Task<Message> {
        let (build, handle) = Task::future((self.build)()).map(Message::Built).abortable();
        self.building = Some(handle.abort_on_drop());
        build
    }

    /// Stops the build that a Rebuild started, if it is still running.
    fn cancel_build(&mut self) {
        if let Some(building) = self.building.take() {
            building.abort();
        }
    }

    /// Acts on the end of the build that a Rebuild started, telling the person why it failed if
    /// it did. A build that was stopped before its end was reported is ignored.
    fn built(&mut self, built: Result<PathBuf, String>) -> Task<Message> {
        if self.building.take().is_none() {
            return Task::none();
        }
        let binary = built.map_err(|failure| self.notice(failure)).ok();
        let outcome = self.harness.build_finished(binary, &mut self.session);
        self.carry_out(outcome.as_ref())
    }

    /// Tells the person `words` in the transcript.
    fn notice(&mut self, words: String) {
        self.transcript.notice(words);
    }
}

/// Draws the question whether the command `asked` about may run, with why the person is asked,
/// and the buttons that answer it.
fn question(asked: &ApprovalRequest) -> Element<'_, Message> {
    let answers = row![
        button("Allow").on_press(Message::Answer(true)),
        button("Deny").on_press(Message::Answer(false)),
    ]
    .spacing(SPACING);
    column![]
        .push(asked.note.as_deref().map(text))
        .push(text(format!("Run `{}`?", asked.command)))
        .push(answers)
        .spacing(SPACING)
        .into()
}

/// Returns what the window says about the Running Harness being on its way out in `phase`, if it
/// is.
fn progress(phase: &Phase) -> Option<&'static str> {
    match phase {
        Phase::DrainingToQuit => Some(
            "Quitting once the running Turn reaches the end of its Step. Quit or close the \
             window again to quit now, or interrupt to carry on.",
        ),
        Phase::DrainingToRebuild { binary: None } => Some(
            "Building sstack-gui for a Rebuild, which relaunches into this Session once built \
             and once no Turn is running. Interrupt to back out, or close the window to quit \
             instead.",
        ),
        Phase::DrainingToRebuild { binary: Some(_) } => Some(
            "Built. Relaunching once the running Turn reaches the end of its Step. Interrupt to \
             back out, or close the window to quit instead.",
        ),
        Phase::Running => None,
    }
}

/// Returns the message a key the window heard sends, if it is the key that interrupts.
fn interrupt_key(event: &keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        } => Some(Message::Interrupt),
        _ => None,
    }
}

/// Returns whether the transcript's `viewport` has its newest output in view.
fn at_end(viewport: &Viewport) -> bool {
    viewport.absolute_offset_reversed().y <= FOLLOW_SLACK
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
