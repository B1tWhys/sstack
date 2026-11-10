//! The window: the Sessions the person can switch between, and the one on screen, with what was
//! said in it and a prompt to say more.

mod input;
mod session;
mod sidebar;

use std::fmt::Display;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use iced::futures::future::{self, BoxFuture, FutureExt};
use iced::widget::scrollable::Viewport;
use iced::widget::text_editor::{Binding, KeyPress};
use iced::widget::{
    button, column, container, operation, row, scrollable, stack, text, text_editor,
};
use iced::{
    Element, Fill, Size, Subscription, Task, Theme, clipboard, keyboard, task, time, window,
};
use input::Input;
use session::SessionView;
use sstack::human_command::Command;
use sstack::rebuild::Checkout;
use sstack::running_harness::{
    Exit, InSession, Outcome, Phase, Refusal, RunningHarness, SessionState,
};
use sstack::session_controller::{OpenSessions, SessionController};
use sstack::session_name;
use sstack::session_report::{ReportSender, SessionReport};
use sstack::session_setup::{Place, Run, Start};
use sstack::vim::VimEvent;
use sstack::workspace::SessionSummary;

use crate::asked::Controls;
use crate::reports::Reports;
use crate::transcript::Handlers;
use crate::vim_prompt::KeyOutcome;

/// The id of the prompt, by which tests find it.
const PROMPT: &str = "prompt";

/// The id of the transcript's scrollable, by which the window keeps it at the newest output.
const TRANSCRIPT: &str = "transcript";

/// How far from the newest output, in logical pixels, the transcript can sit and still follow
/// it, so that rounding in the layout does not stop it following.
const FOLLOW_SLACK: f32 = 1.0;

/// Space between the parts of the window, and around them, in logical pixels.
const SPACING: f32 = 10.0;

/// What the pill says that floats over the transcript while a question waits out of view.
const WAITING: &str = "Waiting for your answer";

/// The size of the pill's words, in logical pixels, a little smaller than the rest of the window.
const PILL_TEXT: f32 = 14.0;

/// How round the pill's corners are, in logical pixels: enough to round its ends fully.
const PILL_RADIUS: f32 = 999.0;

/// What the person is told when what they sent waits for the running Turn.
const QUEUED: &str =
    "queued: the agent reads it after its current Step, and Interrupt gives it back";

/// What the person is told when Queued Prompts come back to the prompt unsent.
const WITHDRAWN: &str = "not sent: what you queued is back in the prompt";

/// How often the sidebar redraws how long each Session has waited.
const TICK: Duration = Duration::from_secs(1);

/// Builds the checkout of the Harness Source in the given directory for a Rebuild, ending in the
/// new binary or in why the build failed.
///
/// The window starts one when a Rebuild asks for it, and drops it to stop the build. Tests give
/// the window one that never runs cargo.
pub type Build = fn(PathBuf) -> BoxFuture<'static, Result<PathBuf, String>>;

/// A Session readied for the window, with the reports it will send the window.
pub struct Opened {
    /// The Session as setup readied it.
    pub run: Run,
    /// Where the window reads the Session's reports.
    pub reports: Reports,
    /// Where the Session's reports go, which the window hands the Turns it runs. The Session's
    /// Approver and Asker must ask through the same reports, or the window never sees their
    /// questions.
    pub sender: ReportSender,
}

/// Everything the window needs done to Sessions beyond itself.
pub trait Sessions {
    /// Readies the Session `start` asks for.
    ///
    /// # Errors
    /// Fails if the Session cannot be readied.
    fn open(&mut self, start: Start<'_>) -> Result<Opened>;

    /// Refuses, saying why, if archiving the Session with id `id` would lose work.
    ///
    /// # Errors
    /// Fails if there is work to lose, or if that cannot be told.
    fn check_archive(&self, id: &str) -> Result<()>;

    /// Puts the Session with id `id` away, deleting its worktree and any work there.
    ///
    /// # Errors
    /// Fails if the Session could not be put away.
    fn archive(&mut self, id: &str) -> Result<()>;

    /// Stores `name` as the Session Name of the Session with id `id`, so that it lasts beyond this
    /// run. With `None`, the Session is known by its first prompt again.
    ///
    /// # Errors
    /// Fails if the name could not be stored.
    fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()>;

    /// Returns the directory that a Rebuild of `checkout`, asked for from the Session with id
    /// `id`, builds.
    ///
    /// # Errors
    /// Fails, saying why for the person to read, if there is no such checkout to build, such as
    /// the worktree of a Session that works in the main checkout.
    fn harness_source(&self, checkout: Checkout, id: &str) -> Result<PathBuf, String>;
}

/// What the process does once the window has closed for a Rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    /// The `sstack-gui` the Rebuild built, to relaunch into.
    pub binary: PathBuf,
    /// Where the Session that was on screen is stored, which the new binary opens.
    pub session: PathBuf,
}

/// What the window opens with.
pub struct Ready {
    /// The Session to show first, which may also be one of `stored`.
    pub first: Opened,
    /// The stored Sessions to list, which get an agent only once shown.
    pub stored: Vec<SessionSummary>,
    /// Where the window opens, archives and renames Sessions as the person asks.
    pub sessions: Box<dyn Sessions>,
    /// How the window builds `sstack-gui` when the person asks for a Rebuild.
    pub build: Build,
    /// Where the window leaves what to relaunch into as it closes for a Rebuild, for the process
    /// to carry out once iced has torn the window down. Empty if the window closed for any other
    /// reason.
    pub relaunch: Arc<OnceLock<Relaunch>>,
}

/// The window, which takes the person's prompts for the Session on screen, shows the replies,
/// and lets them switch between Sessions while the others carry on.
pub struct Window {
    /// The rules every open Session follows together, such as on the way to a Rebuild.
    harness: RunningHarness,
    /// Every Session the person can switch to, in no meaningful order.
    views: Vec<SessionView>,
    /// The position in `views` of the Session on screen.
    shown: usize,
    /// Where Sessions are opened, archived and renamed.
    sessions: Box<dyn Sessions>,
    /// How the window builds `sstack-gui` for a Rebuild.
    build: Build,
    /// The build a Rebuild started, while it runs. Dropping it stops the build.
    building: Option<task::Handle>,
    /// Where the window leaves what to relaunch into, once a Rebuild is ready.
    relaunch: Arc<OnceLock<Relaunch>>,
}

/// Something that happened in the window, for [`Window::update`] to act on.
#[derive(Debug, Clone)]
pub enum Message {
    /// The person changed the plain prompt.
    Edit(text_editor::Action),
    /// The person pressed a key while typing into nvim, which no widget kept for itself.
    Key(keyboard::Event),
    /// The nvim of the Session with this id reported on what the person is typing.
    Vim(String, VimEvent),
    /// The clipboard was read for a paste into nvim, and held this, if it held text.
    Pasted(Option<String>),
    /// The window laid out the prompt that nvim edits at this size, in logical pixels.
    PromptResized(Size),
    /// The person sent the prompt.
    Submit,
    /// Core reported on the Session with this id.
    Report(String, SessionReport),
    /// The person answered the question about a shell command: whether it may run.
    Answer(bool),
    /// The person clicked a link in a reply, which leads to this address.
    OpenLink(String),
    /// The person picked a row of one of the model's open Questions.
    Choose {
        /// The position of the Question in its set.
        question: usize,
        /// The row picked: a choice, or after every choice, the person's own words.
        row: usize,
    },
    /// The person changed their own words for one of the model's open Questions.
    Write {
        /// The position of the Question in its set.
        question: usize,
        /// Everything they have written for it now.
        words: String,
    },
    /// The person sent their answers to the model's open Questions.
    SendAnswers,
    /// The person chose to answer the model's open Questions in the conversation instead.
    Decline,
    /// The person asked to interrupt.
    Interrupt,
    /// The person asked for a Rebuild, as `/rebuild` does.
    Rebuild,
    /// The person asked to close the window, which quits as `/quit` does, and quits instead of
    /// relaunching during a Rebuild.
    CloseRequested,
    /// The build a Rebuild started finished, with the new binary or why it failed.
    Built(Result<PathBuf, String>),
    /// The person asked to see the newest output, as the pill over an open question that is
    /// out of view offers.
    JumpToEnd,
    /// The person scrolled the transcript.
    Scrolled {
        /// Whether the newest output is in view.
        at_end: bool,
    },
    /// The person picked the Session with this id in the sidebar.
    Show(String),
    /// The person asked for a new Session in its own worktree, as `/new` does.
    NewSession,
    /// The person asked to put the Session on screen away, as `/archive` does, which refuses if
    /// that would lose work.
    Archive,
    /// The person asked for the Session after the one on screen in the sidebar.
    NextSession,
    /// The person asked for the Session before the one on screen in the sidebar.
    PreviousSession,
    /// The person asked for the Session that has waited longest for them.
    LongestWaiting,
    /// Time passed, so how long each Session has waited needs redrawing.
    Tick,
}

impl Window {
    /// Opens the window on the readied Sessions, showing the first and carrying on every Turn an
    /// earlier run left parked.
    ///
    /// Must be called inside a Tokio runtime, which runs the Sessions' Turns.
    pub fn open(ready: Ready) -> Self {
        let Ready {
            first,
            stored,
            sessions,
            build,
            relaunch,
        } = ready;
        let mut views: Vec<SessionView> = stored.into_iter().map(SessionView::stored).collect();
        let found = views
            .iter()
            .position(|view| view.path() == first.run.session);
        let shown = found.unwrap_or(views.len());
        match views.get_mut(shown) {
            Some(view) => view.attach(first),
            None => views.push(SessionView::opened(first)),
        }
        let mut window = Self {
            harness: RunningHarness::default(),
            views,
            shown,
            sessions,
            build,
            building: None,
            relaunch,
        };
        window.continue_parked();
        window
    }

    /// Acts on `message`, returning what iced should do next.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Edit(action) => {
                self.view_mut().edit(action);
                Task::none()
            }
            Message::Key(event) => self.key(&event),
            Message::Vim(id, event) => self.vim(&id, event),
            Message::Pasted(pasted) => {
                self.view_ref().prompt.paste(pasted);
                Task::none()
            }
            Message::PromptResized(size) => {
                self.view_mut().prompt.resized(size);
                Task::none()
            }
            Message::Submit => self.submit(),
            Message::Report(id, report) => self.report(&id, &report),
            Message::Scrolled { at_end } => {
                self.view_mut().scrolled(at_end);
                Task::none()
            }
            Message::Answer(allowed) => {
                self.view_mut().answer(allowed);
                Task::none()
            }
            Message::OpenLink(uri) => self.open_link(&uri),
            message @ (Message::Choose { .. }
            | Message::Write { .. }
            | Message::SendAnswers
            | Message::Decline) => {
                self.questions(message);
                Task::none()
            }
            Message::JumpToEnd => self.jump_to_end(),
            Message::Interrupt => self.interrupt(),
            Message::Built(built) => self.built(built),
            Message::Rebuild => self.command("/rebuild"),
            Message::CloseRequested => self.close(),
            message => self.switch(message),
        }
    }

    /// Opens the link at `uri` in the person's browser if it is a web address, telling them in
    /// the transcript if that failed. Any other link, such as a path in the Workspace, does
    /// nothing, since the window has nowhere to show it yet.
    fn open_link(&mut self, uri: &str) -> Task<Message> {
        if let Some(failure) = open_web_link(uri, |uri: &str| opener::open_browser(uri)) {
            self.view_mut().notice(failure);
        }
        Task::none()
    }

    /// Acts on what the person did to the model's open Questions in the Session on screen.
    fn questions(&mut self, message: Message) {
        let view = self.view_mut();
        match message {
            Message::Choose { question, row } => view.choose(question, row),
            Message::Write { question, words } => view.write(question, words),
            Message::SendAnswers => view.send_answers(),
            Message::Decline => view.decline(),
            _ => {}
        }
    }

    /// Acts on a request to show another Session, or to start a new one.
    fn switch(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Show(id) => self.show_id(&id),
            Message::NewSession => self.command("/new"),
            Message::Archive => self.command("/archive"),
            Message::NextSession => self.step(1),
            Message::PreviousSession => self.step(-1),
            Message::LongestWaiting => self.show_longest_waiting(),
            _ => Task::none(),
        }
    }

    /// Draws the window: the sidebar, and the Session on screen beside it.
    pub fn view(&self) -> Element<'_, Message> {
        let now = SystemTime::now();
        let queue = self.queue();
        let rows: Vec<_> = queue
            .iter()
            .map(|index| self.views[*index].row(now))
            .collect();
        let on_screen = queue.iter().position(|index| *index == self.shown);
        let sidebar = sidebar::view(rows, on_screen.unwrap_or_default());
        row![sidebar, self.session_view()]
            .padding(SPACING)
            .spacing(SPACING)
            .into()
    }

    /// Draws the Session on screen: its transcript with any questions for the person, what the
    /// Running Harness is doing on its way out, and the prompt with its buttons. A pill floats
    /// over the bottom of the transcript while a question waits out of view. The button that
    /// archives the Session does nothing while a Turn runs in it.
    fn session_view(&self) -> Element<'_, Message> {
        let view = self.view_ref();
        let handlers = Handlers {
            answer: Message::Answer,
            open: Message::OpenLink,
            questions: Controls {
                choose: |question, row| Message::Choose { question, row },
                write: |question, words| Message::Write { question, words },
                send: Message::SendAnswers,
                decline: Message::Decline,
            },
        };
        let transcript = scrollable(view.transcript.view(&handlers))
            .id(TRANSCRIPT)
            .on_scroll(|viewport| Message::Scrolled {
                at_end: at_end(&viewport),
            })
            .height(Fill)
            .width(Fill);
        let transcript = stack![transcript].push(waiting_pill(view));
        let prompt = match &view.prompt {
            Input::Plain(content) => text_editor(content)
                .id(PROMPT)
                .placeholder("Say something, or type a / command")
                .on_action(Message::Edit)
                .key_binding(prompt_binding)
                .into(),
            Input::Vim(vim) => vim.view(),
        };
        let send = button("Send").on_press(Message::Submit);
        let draining = self.harness.phase().is_draining();
        let interruptible = view.state().in_turn() || draining;
        let interrupt =
            button("Interrupt").on_press_maybe(interruptible.then_some(Message::Interrupt));
        let rebuild = button("Rebuild").on_press_maybe((!draining).then_some(Message::Rebuild));
        let archive =
            button("Archive").on_press_maybe((!view.state().in_turn()).then_some(Message::Archive));
        let progress = progress(self.harness.phase()).map(text);
        column![transcript]
            .push(progress)
            .push(row![prompt, send, interrupt, rebuild, archive].spacing(SPACING))
            .spacing(SPACING)
            .width(Fill)
            .into()
    }

    /// Returns the window's title, which gives the Session Name of the Session on screen.
    pub fn title(&self) -> String {
        session_name::title(self.view_ref().known_name().as_deref())
    }

    /// Returns where every open Session's reports come from, with the keys, requests to close
    /// and passing time that the window acts on. While the person types into nvim in the Session
    /// on screen, every key goes to the window, to pass on to nvim.
    ///
    /// The window hears a request to close only if iced is told not to exit on one itself.
    pub fn subscription(&self) -> Subscription<Message> {
        let reports = self.views.iter().map(SessionView::subscription);
        let keys = match self.view_ref().prompt {
            Input::Plain(_) => keyboard::listen().filter_map(|event| shortcut(&event)),
            Input::Vim(_) => keyboard::listen().filter_map(pressed),
        };
        let window = [
            keys,
            window::close_requests().map(|_| Message::CloseRequested),
            time::every(TICK).map(|_| Message::Tick),
        ];
        Subscription::batch(reports.chain(window))
    }

    /// Returns the Session on screen.
    fn view_ref(&self) -> &SessionView {
        &self.views[self.shown]
    }

    /// Returns the Session on screen, to change it.
    fn view_mut(&mut self) -> &mut SessionView {
        &mut self.views[self.shown]
    }

    /// Quits as `/quit` does: at once if no Turn is running, or once every running Turn reaches
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

    /// Gives the Human Command `line` as if the person had typed it into the Session on screen,
    /// without touching what they have typed.
    fn command(&mut self, line: &str) -> Task<Message> {
        let outcome = self.drive(self.shown, Happened::Entered(line));
        self.carry_out(self.shown, outcome)
    }

    /// Acts on the person's request to interrupt. It cancels the Turn running in the Session on
    /// screen, backs out of a quit or Rebuild that is waiting, and otherwise quits, unless other
    /// Sessions are working.
    fn interrupt(&mut self) -> Task<Message> {
        let outcome = self.drive(self.shown, Happened::Interrupted);
        self.carry_out(self.shown, outcome)
    }

    /// Acts on what the person typed, which stays in the prompt only if it was refused. Nvim
    /// hands its text back as [`Message::Vim`] instead, once every key typed before has taken
    /// effect.
    fn submit(&mut self) -> Task<Message> {
        match self.view_mut().take_prompt() {
            Some(typed) => self.enter(self.shown, typed),
            None => Task::none(),
        }
    }

    /// Acts on `typed`, which the prompt of the Session at position `index` has already given up,
    /// putting it back if it was refused.
    fn enter(&mut self, index: usize, typed: text_editor::Content) -> Task<Message> {
        let line = typed.text();
        let outcome = self.drive(index, Happened::Entered(line.trim()));
        if matches!(outcome, Some(Outcome::Refused(_))) {
            self.views[index].restore_prompt(typed);
        }
        self.carry_out(index, outcome)
    }

    /// Acts on a key pressed while the person types into nvim. The window's shortcuts still
    /// work, and Alt-C interrupts, as in the TUI, because Escape belongs to nvim.
    fn key(&mut self, event: &keyboard::Event) -> Task<Message> {
        if let keyboard::Event::KeyPressed { key, modifiers, .. } = event
            && let Some(message) = command_shortcut(key, *modifiers)
        {
            return self.update(message);
        }
        if is_alt_c(event) {
            return self.interrupt();
        }
        match self.view_ref().prompt.key(event) {
            KeyOutcome::Paste => clipboard::read().map(Message::Pasted),
            KeyOutcome::Handled | KeyOutcome::Ignored => Task::none(),
        }
    }

    /// Acts on what the nvim of the Session with id `id` reported. A line it submitted is entered
    /// like one sent from the plain prompt, and the person may have switched away while nvim
    /// answered, so that Session comes back on screen to take it.
    fn vim(&mut self, id: &str, event: VimEvent) -> Task<Message> {
        let Some(index) = self.position(id) else {
            return Task::none();
        };
        if !self.views[index].vim_reported(&event) {
            return Task::none();
        }
        let VimEvent::Submitted(line) = event else {
            return Task::none();
        };
        let shown = self.show(index);
        let entered = self.enter(index, text_editor::Content::with_text(&line));
        Task::batch([shown, entered])
    }

    /// Shows what core reported about the Session with id `id`, and acts on it. Reports from a
    /// Session the window no longer has are ignored.
    fn report(&mut self, id: &str, report: &SessionReport) -> Task<Message> {
        let Some(index) = self.position(id) else {
            return Task::none();
        };
        let on_screen = index == self.shown;
        let changed = self.views[index].observe(report, on_screen);
        let follow = self.follow(changed && on_screen);
        let outcome = self.drive(index, Happened::Reported(report));
        Task::batch([follow, self.carry_out(index, outcome)])
    }

    /// Has the Running Harness act on what `happened` in the Session at position `index`, with
    /// every other Session open beside it. Does nothing to a Session with no agent yet.
    fn drive<'a>(&mut self, index: usize, happened: Happened<'a>) -> Option<Outcome<'a>> {
        let (before, rest) = self.views.split_at_mut(index);
        let (view, after) = rest.split_first_mut()?;
        let harness = &mut self.harness;
        let mut others = Views(before, after);
        let outcome = view.control(|focus| happened.act(harness.in_session(focus, &mut others)));
        outcome.flatten()
    }

    /// Does what the Running Harness asked of the window after acting on the Session at position
    /// `index`.
    fn carry_out(&mut self, index: usize, outcome: Option<Outcome<'_>>) -> Task<Message> {
        match outcome {
            Some(Outcome::TurnStarted(text)) => {
                self.views[index].turn_started(&text);
                self.follow(index == self.shown)
            }
            Some(Outcome::Queued) => {
                self.views[index].notice(QUEUED.to_owned());
                self.follow(index == self.shown)
            }
            Some(Outcome::Withdrawn(text)) => {
                self.views[index].give_back(&text);
                self.views[index].notice(WITHDRAWN.to_owned());
                self.follow(index == self.shown)
            }
            Some(Outcome::Refused(refusal)) => self.notice(refusal_text(&refusal)),
            Some(Outcome::Command(command)) => self.human_command(&command),
            Some(Outcome::StartBuild(checkout)) => self.start_build(checkout, index),
            Some(Outcome::CancelBuild) => {
                self.cancel_build();
                Task::none()
            }
            Some(Outcome::Exit(exit)) => self.exit(&exit),
            None => Task::none(),
        }
    }

    /// Carries out a Human Command that is about how the window keeps its Sessions.
    fn human_command(&mut self, command: &Command<'_>) -> Task<Message> {
        match *command {
            Command::New => self.open_new(Place::Worktree),
            Command::NewMain => self.open_new(Place::MainCheckout),
            Command::Archive { force } => self.archive(force),
            Command::Rename(name) => {
                self.rename(name);
                Task::none()
            }
            // Only /vim is left.
            _ => {
                self.view_mut().toggle_vim();
                Task::none()
            }
        }
    }

    /// Carries on every Turn an earlier run left parked. A Session that cannot open stays parked
    /// and says so.
    fn continue_parked(&mut self) {
        for index in 0..self.views.len() {
            if self.views[index].state() != SessionState::Parked {
                continue;
            }
            match self.ensure_agent(index) {
                Ok(()) => self.views[index].continue_parked(),
                Err(error) => {
                    let _ = self.notice(format!("could not carry on a Session: {error}"));
                }
            }
        }
    }

    /// Returns the position in `views` of the Session with id `id`.
    fn position(&self, id: &str) -> Option<usize> {
        self.views.iter().position(|view| view.id() == id)
    }

    /// Returns the position in `views` of each Session, in the order the sidebar lists them.
    fn queue(&self) -> Vec<usize> {
        let mut queue: Vec<usize> = (0..self.views.len()).collect();
        queue.sort_by_key(|index| self.views[*index].queue_position());
        queue
    }

    /// Shows the Session `offset` places along the sidebar from this one, wrapping at its ends.
    fn step(&mut self, offset: isize) -> Task<Message> {
        let queue = self.queue();
        let here = queue.iter().position(|index| *index == self.shown);
        let here = here.unwrap_or_default().cast_signed();
        let length = queue.len().cast_signed();
        self.show(queue[(here + offset).rem_euclid(length).cast_unsigned()])
    }

    /// Shows the Session that has waited longest for the person, besides the one on screen,
    /// which they have already got to.
    fn show_longest_waiting(&mut self) -> Task<Message> {
        let next = self
            .queue()
            .into_iter()
            .find(|index| *index != self.shown && self.views[*index].state().is_waiting());
        next.map_or_else(Task::none, |index| self.show(index))
    }

    /// Shows the Session with id `id`, if the window has it.
    fn show_id(&mut self, id: &str) -> Task<Message> {
        self.position(id)
            .map_or_else(Task::none, |index| self.show(index))
    }

    /// Puts the Session at position `index` on screen, starting its agent if it has none yet,
    /// with its newest output in view. If that fails, the current Session stays and the person
    /// is told why.
    fn show(&mut self, index: usize) -> Task<Message> {
        match self.ensure_agent(index) {
            Ok(()) => {
                self.shown = index;
                self.view_mut().viewed();
                self.follow(true)
            }
            Err(error) => self.notice(format!("could not open the Session: {error}")),
        }
    }

    /// Starts an agent for the Session at position `index`, unless it already has one.
    fn ensure_agent(&mut self, index: usize) -> Result<()> {
        if !self.views[index].has_agent() {
            let opened = self.sessions.open(Start::Resume(self.views[index].id()))?;
            self.views[index].attach(opened);
        }
        Ok(())
    }

    /// Starts a new Session working in `place` and puts it on screen, leaving the others running.
    fn open_new(&mut self, place: Place) -> Task<Message> {
        match self.sessions.open(Start::New(place)) {
            Ok(opened) => {
                self.views.push(SessionView::opened(opened));
                self.shown = self.views.len() - 1;
                Task::none()
            }
            Err(error) => self.notice(format!("could not start a new Session: {error}")),
        }
    }

    /// Makes `name` the Session Name of the Session on screen, or with `None` goes back to its
    /// first prompt. If the name cannot be stored, nothing changes and the person is told why.
    fn rename(&mut self, name: Option<&str>) {
        let view = &mut self.views[self.shown];
        match self.sessions.rename(view.id(), name) {
            Ok(()) => view.rename(name),
            Err(error) => view.notice(format!("not renamed: {error}")),
        }
    }

    /// Puts the Session on screen away and moves on to the next one, unless `force` is false and
    /// that would lose work. A Session with a Turn running is never put away.
    fn archive(&mut self, force: bool) -> Task<Message> {
        if self.view_ref().state().in_turn() {
            return self.notice(refusal_text(&Refusal::Busy));
        }
        let leaving = self.shown;
        let id = self.view_ref().id().to_owned();
        let allowed = if force {
            Ok(())
        } else {
            self.sessions.check_archive(&id)
        };
        if let Err(error) = allowed {
            return self.notice(format!("not archived: {error}"));
        }
        let moved = self.move_on();
        if self.shown != leaving {
            self.finish_archiving(leaving, &id);
        }
        moved
    }

    /// Shows the next Session in the queue, or a new one if there are no others. If that fails,
    /// the Session on screen stays.
    fn move_on(&mut self) -> Task<Message> {
        let shown = self.shown;
        match self.queue().into_iter().find(|index| *index != shown) {
            Some(next) => self.show(next),
            None => self.open_new(Place::Worktree),
        }
    }

    /// Puts away the Session at position `leaving`, whose id is `id`, now that it is off screen.
    fn finish_archiving(&mut self, leaving: usize, id: &str) {
        if let Err(error) = self.sessions.archive(id) {
            let _ = self.notice(format!("not archived: {error}"));
            return;
        }
        self.views.remove(leaving);
        if self.shown > leaving {
            self.shown -= 1;
        }
    }

    /// Brings the newest output of the Session on screen into view and follows it again,
    /// returning the task that scrolls there.
    fn jump_to_end(&mut self) -> Task<Message> {
        self.view_mut().scrolled(true);
        operation::snap_to_end(TRANSCRIPT)
    }

    /// Returns the task that brings the newest output of the Session on screen into view, if
    /// `changed` says its transcript changed while following it.
    fn follow(&self, changed: bool) -> Task<Message> {
        if changed && self.view_ref().following() {
            operation::snap_to_end(TRANSCRIPT)
        } else {
            Task::none()
        }
    }

    /// Closes the window, leaving what to relaunch into if the Running Harness stopped for a
    /// Rebuild.
    fn exit(&self, exit: &Exit) -> Task<Message> {
        if let Exit::Rebuild(binary) = exit {
            let _ = self.relaunch.set(Relaunch {
                binary: binary.clone(),
                session: self.view_ref().path().to_owned(),
            });
        }
        iced::exit()
    }

    /// Starts building `sstack-gui` from `checkout` for a Rebuild asked for in the Session at
    /// position `index`, returning the task that runs the build and reports its end as
    /// [`Message::Built`]. A checkout that cannot be built ends that way too, at once.
    fn start_build(&mut self, checkout: Checkout, index: usize) -> Task<Message> {
        let building = match self
            .sessions
            .harness_source(checkout, self.views[index].id())
        {
            Ok(source) => (self.build)(source),
            Err(failure) => future::ready(Err(failure)).boxed(),
        };
        let (build, handle) = Task::future(building).map(Message::Built).abortable();
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
        let all = &mut Views(&mut self.views, &mut []);
        let outcome = self.harness.build_finished(binary, all);
        self.carry_out(self.shown, outcome)
    }

    /// Tells the person `words` in the Session on screen, returning the task that has nothing
    /// more for iced to do, for the callers that end with a notice.
    fn notice(&mut self, words: String) -> Task<Message> {
        self.view_mut().notice(words);
        Task::none()
    }
}

/// Something that happened in one Session, which the Running Harness acts on.
enum Happened<'a> {
    /// The person entered this line.
    Entered(&'a str),
    /// Core reported this about the Session.
    Reported(&'a SessionReport),
    /// The person asked to interrupt while looking at the Session.
    Interrupted,
}

impl<'a> Happened<'a> {
    /// Has `harness` act on what happened, returning what the window has to do about it.
    fn act(self, harness: InSession<'_>) -> Option<Outcome<'a>> {
        match self {
            Self::Entered(line) => harness.submit(line),
            Self::Reported(report) => harness.report(report),
            Self::Interrupted => harness.interrupt(),
        }
    }
}

/// The open Sessions among two runs of views, which the Running Harness reaches through
/// [`OpenSessions`].
struct Views<'v>(&'v mut [SessionView], &'v mut [SessionView]);

impl OpenSessions for Views<'_> {
    fn visit(&mut self, visit: &mut dyn FnMut(&mut SessionController)) {
        for view in self.0.iter_mut().chain(self.1.iter_mut()) {
            view.control(&mut *visit);
        }
    }
}

/// Draws the pill that floats at the bottom of `view`'s transcript while a question waits for
/// the person out of view, if one does. Pressing it jumps to the newest output, where the
/// question is.
fn waiting_pill(view: &SessionView) -> Option<Element<'_, Message>> {
    (view.asking() && !view.following()).then(|| {
        let pill = button(text(WAITING).size(PILL_TEXT))
            .on_press(Message::JumpToEnd)
            .padding([4, 12])
            .style(pill_style);
        container(pill)
            .center_x(Fill)
            .align_bottom(Fill)
            .padding(SPACING)
            .into()
    })
}

/// Opens the link at `uri` with `open` if it is a web address, returning what to tell the person
/// if that failed. Any other link is left alone, and needs no telling.
fn open_web_link<E: Display>(
    uri: &str,
    open: impl FnOnce(&str) -> Result<(), E>,
) -> Option<String> {
    let web = uri.starts_with("https://") || uri.starts_with("http://");
    let failed = web.then(|| open(uri).err()).flatten();
    failed.map(|error| format!("could not open {uri}: {error}"))
}

/// Styles the pill over the transcript as a button of the theme's main color with round ends,
/// in its `status`.
fn pill_style(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::primary(theme, status);
    style.border.radius = PILL_RADIUS.into();
    style
}

/// Returns what the window says about the Running Harness being on its way out in `phase`, if it
/// is.
fn progress(phase: &Phase) -> Option<&'static str> {
    match phase {
        Phase::DrainingToQuit => Some(
            "Quitting once every running Turn reaches the end of its Step. Quit or close the \
             window again to quit now, or interrupt to carry on.",
        ),
        Phase::DrainingToRebuild { binary: None } => Some(
            "Building sstack-gui for a Rebuild, which relaunches into this Session once built \
             and once no Turn is running. Anything waiting for your answer comes back as it was \
             after the relaunch. Interrupt to back out, or close the window to quit instead.",
        ),
        Phase::DrainingToRebuild { binary: Some(_) } => Some(
            "Built. Relaunching once every running Turn reaches the end of its Step. Interrupt \
             to back out, or close the window to quit instead.",
        ),
        Phase::Running => None,
    }
}

/// Returns the message a key the window heard sends, if any: Escape interrupts, and the
/// shortcuts of [`command_shortcut`] work here too.
fn shortcut(event: &keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };
    match key {
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Interrupt),
        key => command_shortcut(key, *modifiers),
    }
}

/// Returns the message that `key`, pressed with `modifiers`, sends wherever the person is
/// typing, if it is one of the window's shortcuts. Each is pressed with Command on macOS and
/// Control elsewhere: Enter sends the prompt in any mode of nvim, N starts a new Session, ] and [
/// move along the sidebar, and G shows the Session that has waited longest. They need not match
/// the TUI's keys (ADR-0010).
fn command_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<Message> {
    use keyboard::Key::{Character, Named};
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        Named(keyboard::key::Named::Enter) => Some(Message::Submit),
        Character("n") => Some(Message::NewSession),
        Character("]") => Some(Message::NextSession),
        Character("[") => Some(Message::PreviousSession),
        Character("g") => Some(Message::LongestWaiting),
        _ => None,
    }
}

/// Returns what a key pressed in the prompt does: one of the window's shortcuts, or else what the
/// prompt does with it by default.
fn prompt_binding(press: KeyPress) -> Option<Binding<Message>> {
    match command_shortcut(&press.key, press.modifiers) {
        Some(message) => Some(Binding::Custom(message)),
        None => Binding::from_key_press(press),
    }
}

/// Returns the message for a key pressed while the person types into nvim. Releasing a key means
/// nothing to nvim.
fn pressed(event: keyboard::Event) -> Option<Message> {
    matches!(event, keyboard::Event::KeyPressed { .. }).then_some(Message::Key(event))
}

/// Returns whether `event` is a press of Alt-C, which interrupts while the person types into
/// nvim.
fn is_alt_c(event: &keyboard::Event) -> bool {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return false;
    };
    modifiers.alt() && key.as_ref() == keyboard::Key::Character("c")
}

/// Returns whether the transcript's `viewport` has its newest output in view.
fn at_end(viewport: &Viewport) -> bool {
    viewport.absolute_offset_reversed().y <= FOLLOW_SLACK
}

/// Returns the words that tell the person why the Running Harness refused what they asked.
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Busy => "a Turn is already running in this Session".to_owned(),
        Refusal::Draining => "sstack is quitting, so nothing new can start".to_owned(),
        Refusal::OthersWorking => {
            "other Sessions are still working, so use /quit to stop them too".to_owned()
        }
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
