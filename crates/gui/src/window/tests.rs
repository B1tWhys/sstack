use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::futures::future::{self, BoxFuture, FutureExt};
use iced::futures::{Stream, StreamExt};
use iced::keyboard::{self, Key, key::Named};
use iced::mouse::{self, ScrollDelta};
use iced_test::{Simulator, runtime, simulator};
use serde_json::json;
use sstack::agent::{AgentEvent, ToolCallSummary};
use sstack::provider::scripted::{Scripted, ScriptedProvider};
use sstack::running_harness::SessionState;
use sstack::session::{Entry, StopReason, ToolCall};
use sstack::session_name::Naming;
use tempfile::TempDir;

use super::*;
use crate::test_support::{bash_call, readied, reply, typed, worktree_of};
use crate::transcript::{Approval, Block, Decision};

/// Opens a window on a new Session whose model gives the `script`ed responses in order.
fn window(script: Vec<Scripted>) -> (TempDir, Window) {
    let dir = TempDir::new().unwrap();
    let window = Window::open(readied(dir.path(), script, &[]));
    (dir, window)
}

/// Sends `text` from `window`'s prompt, returning what iced should do next.
fn send(window: &mut Window, text: &str) -> Task<Message> {
    for message in typed(text) {
        let _ = window.update(message);
    }
    window.update(Message::Submit)
}

/// Passes the Session's reports to `window` until a Turn ends, returning what iced should do
/// after the last. Takes the window's only stream of reports, so call it once a test.
async fn until_turn_ends(window: &mut Window) -> Task<Message> {
    let mut reports = reports(window);
    until_turn_ends_on(window, &mut reports).await
}

/// Passes `reports` to `window` until a Turn ends, returning what iced should do after the last.
async fn until_turn_ends_on(
    window: &mut Window,
    reports: &mut (impl Stream<Item = SessionReport> + Unpin),
) -> Task<Message> {
    loop {
        let report = reports.next().await.expect("the Turn should end");
        let ended = matches!(report, SessionReport::TurnEnded(_));
        let task = window.update(Message::Report("1".into(), report));
        if ended {
            return task;
        }
    }
}

/// Returns what the transcript of the Session on screen shows.
fn blocks(window: &Window) -> &[Block] {
    window.view_ref().transcript.blocks()
}

/// Takes the reports of the Session on screen, which only the first call gets.
fn reports(window: &Window) -> impl Stream<Item = SessionReport> + Unpin + use<> {
    window
        .view_ref()
        .reports()
        .expect("the Session should be open")
        .stream()
}

/// Returns the binary the window left to relaunch into, if a Rebuild closed it.
fn relaunched(window: &Window) -> Option<&PathBuf> {
    window.relaunch.get().map(|relaunch| &relaunch.binary)
}

/// Returns a notice as it appears in the transcript.
fn notice(text: &str) -> Block {
    Block::Notice(text.to_owned())
}

#[tokio::test]
async fn a_prompt_runs_a_turn_and_shows_the_reply() {
    let (_dir, mut window) = window(vec![Ok(reply("hi there"))]);

    let _ = send(&mut window, " hello\n");
    assert_eq!(window.view_ref().prompt.draft().text, "");
    let _ = until_turn_ends(&mut window).await;

    let said = [
        Block::Prompt("hello".into()),
        Block::Reply("hi there".into()),
    ];
    assert_eq!(blocks(&window), said);
    assert!(!window.view_ref().state().in_turn());
}

#[tokio::test]
async fn a_prompt_sent_during_a_turn_is_refused_and_kept() {
    let (_dir, mut window) = window(vec![Ok(reply("one"))]);
    let _ = send(&mut window, "first");

    let _ = send(&mut window, "second");

    assert_eq!(window.view_ref().prompt.draft().text, "second");
    let refused = notice("a Turn is already running in this Session");
    assert_eq!(blocks(&window).last(), Some(&refused));
}

#[tokio::test]
async fn a_failed_turn_says_why() {
    let (_dir, mut window) = window(vec![Err("the model is down".into())]);
    let _ = send(&mut window, "hello");
    let _ = until_turn_ends(&mut window).await;

    let failed = notice("the Turn failed: the model is down");
    assert_eq!(blocks(&window).last(), Some(&failed));
}

/// Where a test build leaves the new binary.
const NEW_BINARY: &str = "/new/sstack-gui";

/// A build that works at once without running cargo, for tests that need a Rebuild's build to
/// finish by itself.
fn built_at_once(_source: PathBuf) -> BoxFuture<'static, Result<PathBuf, String>> {
    future::ready(Ok(PathBuf::from(NEW_BINARY))).boxed()
}

/// A build that works at once without running cargo, leaving the new binary in the checkout it
/// was given, for tests that check which checkout a Rebuild builds.
fn built_in_source(mut source: PathBuf) -> BoxFuture<'static, Result<PathBuf, String>> {
    source.push("sstack-gui");
    future::ready(Ok(source)).boxed()
}

/// Opens a window like [`window`], whose Rebuilds build with `build`.
fn building_with(build: Build, script: Vec<Scripted>) -> (TempDir, Window) {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), script, &[]);
    ready.build = build;
    (dir, Window::open(ready))
}

/// Runs what `task` asks of iced, returning the messages it sends the window. Never ends if the
/// task never does.
async fn produced(task: Task<Message>) -> Vec<Message> {
    let Some(actions) = runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let messages = actions.filter_map(|action| {
        future::ready(match action {
            runtime::Action::Output(message) => Some(message),
            _ => None,
        })
    });
    messages.collect().await
}

#[tokio::test]
async fn a_rebuild_builds_the_gui_and_ends_the_window_once_built() {
    let (_dir, mut window) = building_with(built_at_once, vec![]);

    let built = produced(send(&mut window, "/rebuild")).await;

    let [Message::Built(Ok(binary))] = built.as_slice() else {
        panic!("the build should finish: {built:?}");
    };
    assert_eq!(binary, Path::new(NEW_BINARY));
    assert_eq!(window.update(Message::Built(Ok(binary.clone()))).units(), 1);
    assert_eq!(relaunched(&window), Some(binary));
}

#[tokio::test]
async fn a_rebuild_of_the_worktree_builds_and_relaunches_into_the_sessions_worktree() {
    let (dir, mut window) = building_with(built_in_source, vec![]);
    let worktree = worktree_of(dir.path(), "1");
    std::fs::create_dir_all(&worktree).unwrap();

    let built = produced(send(&mut window, "/rebuild --worktree")).await;

    let [Message::Built(Ok(binary))] = built.as_slice() else {
        panic!("the build should finish: {built:?}");
    };
    assert_eq!(binary, &worktree.join("sstack-gui"));
    let _ = window.update(Message::Built(Ok(binary.clone())));
    assert_eq!(relaunched(&window), Some(binary));
}

#[tokio::test]
async fn a_rebuild_of_a_worktree_the_session_lacks_says_why_and_work_carries_on() {
    let (_dir, mut window) = building_with(built_in_source, vec![]);

    let built = produced(send(&mut window, "/rebuild --worktree")).await;
    for message in built {
        let _ = window.update(message);
    }

    assert!(!window.harness.phase().is_draining());
    let failure = notice("the Session works in the main checkout");
    assert_eq!(blocks(&window), [failure]);
}

#[tokio::test]
async fn a_rebuild_relaunches_only_once_the_running_turn_reaches_the_end_of_its_step() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("later"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let _ = send(&mut window, "/rebuild");

    let _ = window.update(Message::Built(Ok(NEW_BINARY.into())));
    assert_eq!(relaunched(&window), None);
    let _ = window.update(Message::Answer(false));
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    assert_eq!(relaunched(&window), Some(&PathBuf::from(NEW_BINARY)));
    let parked = notice("the Turn paused until sstack runs again");
    assert_eq!(blocks(&window).last(), Some(&parked));
}

#[tokio::test]
async fn quitting_relaunches_nothing() {
    let (_dir, mut window) = window(vec![]);

    assert_eq!(send(&mut window, "/quit").units(), 1);

    assert_eq!(relaunched(&window), None);
}

#[tokio::test]
async fn a_failed_build_says_why_and_work_carries_on() {
    let (_dir, mut window) = window(vec![Ok(reply("fine"))]);
    let _ = send(&mut window, "/rebuild");

    let failure = "the build did not succeed:\nerror[E0308]: mismatched types";
    let _ = window.update(Message::Built(Err(failure.into())));

    assert!(!window.harness.phase().is_draining());
    let _ = send(&mut window, "still here?");
    let _ = until_turn_ends(&mut window).await;
    let said = [
        notice(failure),
        Block::Prompt("still here?".into()),
        Block::Reply("fine".into()),
    ];
    assert_eq!(blocks(&window), said);
}

#[tokio::test]
async fn interrupting_a_rebuild_stops_the_build_and_work_carries_on() {
    let (_dir, mut window) = window(vec![]);
    let building = send(&mut window, "/rebuild");

    let _ = window.update(Message::Interrupt);

    // The test build never ends, so if it were not stopped, nextest would stop the test.
    assert!(produced(building).await.is_empty());
    assert!(!window.harness.phase().is_draining());
}

#[tokio::test]
async fn a_build_that_ends_after_it_was_stopped_changes_nothing() {
    let (_dir, mut window) = window(vec![]);
    let _ = send(&mut window, "/rebuild");
    let _ = window.update(Message::Interrupt);

    let _ = window.update(Message::Built(Err("the build did not succeed".into())));

    assert_eq!(blocks(&window), []);
}

/// What the window says while a Rebuild's build runs.
const BUILDING: &str = "Building sstack-gui for a Rebuild, which relaunches into this Session \
                        once built and once no Turn is running. Interrupt to back out, or close \
                        the window to quit instead.";

/// What the window says once a Rebuild's build has worked and a Turn is still running.
const BUILT: &str = "Built. Relaunching once every running Turn reaches the end of its Step. \
                     Interrupt to back out, or close the window to quit instead.";

#[tokio::test]
async fn the_window_says_how_a_rebuild_is_going_until_it_is_backed_out_of() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let _ = send(&mut window, "/rebuild");
    assert!(simulator(window.view()).find(BUILDING).is_ok());
    let _ = window.update(Message::Built(Ok(NEW_BINARY.into())));
    assert!(simulator(window.view()).find(BUILT).is_ok());

    let _ = window.update(Message::Interrupt);
    let mut ui = simulator(window.view());
    assert!(ui.find(BUILDING).is_err() && ui.find(BUILT).is_err());
}

#[tokio::test]
async fn pressing_rebuild_starts_a_rebuild_and_pressing_it_again_does_nothing() {
    let (_dir, mut window) = window(vec![]);

    interact(&mut window, |ui| {
        let _ = ui.click("Rebuild").unwrap();
    });

    assert!(window.harness.phase().is_rebuilding());
    let mut ui = simulator(window.view());
    let _ = ui.click("Rebuild").unwrap();
    assert_eq!(ui.into_messages().count(), 0);
}

#[tokio::test]
async fn quitting_closes_the_window_at_once_when_nothing_is_running() {
    let (_dir, mut window) = window(vec![]);
    assert_eq!(window.update(Message::Submit).units(), 0);

    assert_eq!(send(&mut window, "/quit").units(), 1);
}

#[tokio::test]
async fn quitting_during_a_turn_waits_for_it_to_reach_the_end_of_its_step() {
    let (_dir, mut window) = window(vec![Ok(reply("done"))]);
    let _ = send(&mut window, "go");

    assert_eq!(send(&mut window, "/quit").units(), 0);
    let refused = notice("sstack is quitting, so nothing new can start");
    let _ = send(&mut window, "more");
    assert_eq!(blocks(&window).last(), Some(&refused));

    assert_eq!(until_turn_ends(&mut window).await.units(), 1);
}

#[tokio::test]
async fn opening_tells_the_person_about_the_setup_and_names_the_window() {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    ready.first.run.notices = vec!["working in the main checkout".into()];
    ready.first.run.descriptions_left_out = 3;

    let window = Window::open(ready);

    let warning = "3 Skill descriptions were left out of the system prompt, so the model may \
                   not think to load those Skills";
    let notices = [notice("working in the main checkout"), notice(warning)];
    assert_eq!(blocks(&window), notices);
    assert_eq!(window.title(), "sstack");
}

/// Opens a window on a new Session whose model replies "ok", and whose Session Name is made by a
/// model that gives `named`.
fn naming_window(named: Scripted) -> (TempDir, Window) {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![Ok(reply("ok"))], &[]);
    let provider = Arc::new(ScriptedProvider::new([named]));
    let log = ready.first.run.session.clone();
    ready.first.run.naming = Some(Naming::new(provider, "scripted".into(), log));
    (dir, Window::open(ready))
}

/// Passes the Session's reports to `window` until its Session Name has been made.
async fn until_named(window: &mut Window) {
    let mut reports = reports(window);
    loop {
        let report = reports.next().await.expect("the Session should be named");
        let named = matches!(report, SessionReport::Named(_));
        let _ = window.update(Message::Report("1".into(), report));
        if named {
            return;
        }
    }
}

#[tokio::test]
async fn the_window_is_titled_after_the_session_name_once_it_is_made() {
    let (_dir, mut window) = naming_window(Ok(reply("Say hello")));

    let _ = send(&mut window, "hello\nthere");
    assert_eq!(window.title(), "sstack");
    until_named(&mut window).await;

    assert_eq!(window.title(), "Say hello - sstack");
}

#[tokio::test]
async fn the_window_is_titled_after_the_first_line_if_no_name_is_made() {
    let (_dir, mut window) = naming_window(Err("down".into()));

    let _ = send(&mut window, "hello\nthere");
    until_named(&mut window).await;

    assert_eq!(window.title(), "hello - sstack");
}

#[tokio::test]
async fn opening_carries_on_a_parked_turn() {
    let dir = TempDir::new().unwrap();
    let go = Entry::User { text: "go".into() };
    let parked = [go, Entry::TurnParked];
    let mut window = Window::open(readied(dir.path(), vec![Ok(reply("carried on"))], &parked));

    assert!(window.view_ref().state().in_turn());
    let _ = until_turn_ends(&mut window).await;
    let carried = [
        Block::Prompt("go".into()),
        notice("the Turn paused until sstack runs again"),
        Block::Reply("carried on".into()),
    ];
    assert_eq!(blocks(&window), carried);
}

#[test]
fn every_refusal_is_worded() {
    let worded = [
        (Refusal::Busy, "a Turn is already running in this Session"),
        (
            Refusal::Draining,
            "sstack is quitting, so nothing new can start",
        ),
        (
            Refusal::OthersWorking,
            "other Sessions are still working, so use /quit to stop them too",
        ),
        (
            Refusal::UnknownCommand("/nope".into()),
            "/nope is neither a Human Command nor a Skill",
        ),
        (
            Refusal::AmbiguousSkill {
                name: "docs".into(),
                names: vec!["a:docs".into(), "b:docs".into()],
            },
            "/docs could mean a:docs or b:docs, so type the full name",
        ),
        (
            Refusal::UnreadableSkill {
                name: "docs".into(),
                error: "gone".into(),
            },
            "could not read the Skill docs: gone",
        ),
    ];
    for (refusal, words) in worded {
        assert_eq!(refusal_text(&refusal), words);
    }
}

#[tokio::test]
async fn the_window_shows_the_transcript_and_the_prompt() {
    let (_dir, mut window) = window(vec![Ok(reply("hi there"))]);
    let _ = send(&mut window, "hello");
    let _ = until_turn_ends(&mut window).await;

    let mut ui = simulator(window.view());

    // The reply is drawn as Markdown, which tests cannot find, so only the prompt is looked for.
    for shown in ["> hello", "Send"] {
        assert!(ui.find(shown).is_ok(), "{shown} should be shown");
    }
    assert!(ui.find(iced::widget::Id::new(PROMPT)).is_ok());
}

#[tokio::test]
async fn typing_into_the_prompt_and_pressing_send_starts_a_turn() {
    let (_dir, mut window) = window(vec![Ok(reply("hi there"))]);

    interact(&mut window, |ui| {
        let _ = ui.click(iced::widget::Id::new(PROMPT)).unwrap();
        let _ = ui.typewrite("hello");
    });
    assert_eq!(window.view_ref().prompt.draft().text, "hello");
    interact(&mut window, |ui| {
        let _ = ui.click("Send").unwrap();
    });

    assert_eq!(blocks(&window), [Block::Prompt("hello".into())]);
}

/// Draws `window`, does `act` to its widgets, and passes on the messages that come of it.
fn interact(window: &mut Window, act: impl FnOnce(&mut Simulator<'_, Message>)) {
    let mut ui = simulator(window.view());
    act(&mut ui);
    let messages: Vec<_> = ui.into_messages().collect();
    for message in messages {
        let _ = window.update(message);
    }
}

/// Passes `events` to `window` as if the running Turn reported them, returning what iced should
/// do after the last.
fn happened(window: &mut Window, events: impl IntoIterator<Item = AgentEvent>) -> Task<Message> {
    let reports = events.into_iter().map(SessionReport::Agent);
    reports.fold(Task::none(), |_, report| {
        window.update(Message::Report("1".into(), report))
    })
}

/// Returns a call of the tool `name` with the JSON text `input`, as a Turn reports it.
fn call(name: &str, input: &str) -> ToolCallSummary {
    ToolCallSummary {
        name: name.into(),
        input: input.into(),
    }
}

/// Returns a call of the tool `name` with the JSON text `input` as it appears in the transcript,
/// when the person was not asked about it.
fn tool(name: &str, input: &str, failed: bool) -> Block {
    Block::Tool {
        call: call(name, input),
        failed,
        approval: None,
    }
}

/// Returns the report that the tool `name` finished, and whether it failed.
fn finished(name: &str, is_error: bool) -> AgentEvent {
    AgentEvent::ToolFinished {
        name: name.into(),
        is_error,
    }
}

#[tokio::test]
async fn opening_shows_what_the_session_already_held_before_the_setup_notices() {
    let dir = TempDir::new().unwrap();
    let recorded = |id: &str, name: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        input: json!({"path": "a.rs"}),
    };
    let mut looked = reply("let me look");
    looked.tool_calls = vec![recorded("c1", "read"), recorded("c2", "search")];
    looked.stop = StopReason::ToolUse;
    let result = |id: &str, is_error| Entry::ToolResult {
        call_id: id.into(),
        output: String::new(),
        is_error,
    };
    let look = Entry::User {
        text: "look".into(),
    };
    let held = [
        look,
        Entry::Assistant(looked),
        result("c1", true),
        result("c2", false),
        Entry::TurnCancelled,
    ];
    let mut ready = readied(dir.path(), vec![], &held);
    ready.first.run.notices = vec!["working in the main checkout".into()];
    let mut window = Window::open(ready);

    let resumed = reports(&window).next().await.unwrap();
    let _ = window.update(Message::Report("1".into(), resumed));

    let shown = [
        Block::Prompt("look".into()),
        Block::Reply("let me look".into()),
        tool("read", r#"{"path":"a.rs"}"#, true),
        tool("search", r#"{"path":"a.rs"}"#, false),
        notice("the Turn was cancelled"),
        notice("working in the main checkout"),
    ];
    assert_eq!(blocks(&window), shown);
}

#[tokio::test]
async fn reasoning_and_the_reply_stream_in_as_they_arrive() {
    let (_dir, mut window) = window(vec![]);

    let _ = happened(
        &mut window,
        [
            AgentEvent::Reasoning("let me ".into()),
            AgentEvent::Reasoning("think".into()),
            AgentEvent::Text("Do".into()),
            AgentEvent::Text("ne.".into()),
        ],
    );

    let shown = [
        Block::Reasoning("let me think".into()),
        Block::Reply("Done.".into()),
    ];
    assert_eq!(blocks(&window), shown);
}

#[tokio::test]
async fn each_tool_call_shows_and_a_failed_one_is_marked() {
    let (_dir, mut window) = window(vec![]);

    let _ = happened(
        &mut window,
        [
            AgentEvent::Text("Looking.".into()),
            AgentEvent::ToolStarted(call("read", "{}")),
            finished("read", true),
            AgentEvent::ToolStarted(call("search", "{}")),
            finished("search", false),
            AgentEvent::Text("Found it.".into()),
        ],
    );

    let shown = [
        Block::Reply("Looking.".into()),
        tool("read", "{}", true),
        tool("search", "{}", false),
        Block::Reply("Found it.".into()),
    ];
    assert_eq!(blocks(&window), shown);
}

#[tokio::test]
async fn a_turn_that_calls_a_tool_shows_the_call_between_its_replies() {
    let mut looking = reply("Looking.");
    looking.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: "read".into(),
        input: json!({"path": "missing.txt"}),
    }];
    looking.stop = StopReason::ToolUse;
    let (_dir, mut window) = window(vec![Ok(looking), Ok(reply("Not there."))]);

    let _ = send(&mut window, "look");
    let _ = until_turn_ends(&mut window).await;

    let shown = [
        Block::Prompt("look".into()),
        Block::Reply("Looking.".into()),
        tool("read", r#"{"path":"missing.txt"}"#, true),
        Block::Reply("Not there.".into()),
    ];
    assert_eq!(blocks(&window), shown);
}

#[tokio::test]
async fn a_turn_cut_short_or_declined_says_so_and_a_finished_one_does_not() {
    let mut cut = reply("half");
    cut.stop = StopReason::MaxTokens;
    let mut declined = reply("no");
    declined.stop = StopReason::Refused;
    let (_dir, mut window) = window(vec![Ok(cut), Ok(declined), Ok(reply("yes"))]);

    let mut reports = reports(&window);
    for prompt in ["one", "two", "three"] {
        let _ = send(&mut window, prompt);
        let _ = until_turn_ends_on(&mut window, &mut reports).await;
    }

    let shown = [
        Block::Prompt("one".into()),
        Block::Reply("half".into()),
        notice("the reply stopped at the model's output limit"),
        Block::Prompt("two".into()),
        Block::Reply("no".into()),
        notice("the model declined this request"),
        Block::Prompt("three".into()),
        Block::Reply("yes".into()),
    ];
    assert_eq!(blocks(&window), shown);
}

#[tokio::test]
async fn a_nearly_full_context_is_warned_about() {
    let (_dir, mut window) = window(vec![]);

    let _ = happened(
        &mut window,
        [AgentEvent::ContextNearLimit { used: 8, limit: 10 }],
    );

    let warning = notice("the context window is 8 of 10 tokens full, so start a new Session soon");
    assert_eq!(blocks(&window), [warning]);
}

#[tokio::test]
async fn new_output_is_followed_until_the_person_scrolls_back() {
    let (_dir, mut window) = window(vec![]);
    let _ = happened(&mut window, [AgentEvent::ToolStarted(call("read", "{}"))]);

    assert_eq!(happened(&mut window, [finished("read", false)]).units(), 0);
    assert_eq!(
        happened(&mut window, [AgentEvent::Text("one".into())]).units(),
        1
    );
    let _ = window.update(Message::Scrolled { at_end: false });
    assert_eq!(
        happened(&mut window, [AgentEvent::Text("two".into())]).units(),
        0
    );
    let _ = window.update(Message::Scrolled { at_end: true });
    assert_eq!(
        happened(&mut window, [AgentEvent::Text("three".into())]).units(),
        1
    );
}

#[tokio::test]
async fn starting_a_turn_follows_new_output_again() {
    let (_dir, mut window) = window(vec![Ok(reply("hi"))]);
    let _ = window.update(Message::Scrolled { at_end: false });

    assert_eq!(send(&mut window, "hello").units(), 1);
    assert_eq!(
        happened(&mut window, [AgentEvent::Text("hi".into())]).units(),
        1
    );
}

#[tokio::test]
async fn the_window_labels_reasoning_tool_calls_and_notices() {
    let (_dir, mut window) = window(vec![]);
    let long = format!("{{\"command\":\"{}\"}}", "x".repeat(200));
    let _ = happened(
        &mut window,
        [
            AgentEvent::Reasoning("hmm".into()),
            AgentEvent::ToolStarted(call("bash", &long)),
            finished("bash", true),
            AgentEvent::ContextNearLimit { used: 8, limit: 10 },
        ],
    );

    let mut ui = simulator(window.view());

    let shown = [
        "Thinking: hmm".to_owned(),
        format!("Tool: bash {long} (failed)"),
        "Note: the context window is 8 of 10 tokens full, so start a new Session soon".to_owned(),
    ];
    for shown in shown {
        assert!(ui.find(shown.as_str()).is_ok(), "{shown} should be shown");
    }
}

#[tokio::test]
async fn scrolling_the_transcript_says_whether_the_newest_output_is_in_view() {
    let (_dir, mut window) = window(vec![]);
    let _ = happened(&mut window, [AgentEvent::Text("line\n\n".repeat(200))]);
    let mut ui = simulator(window.view());
    ui.point_at(iced::Point::new(600.0, 50.0));

    let wheel = |y| {
        iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y },
        })
    };
    let _ = ui.simulate([wheel(-10_000.0), wheel(3.0)]);

    let scrolled: Vec<_> = ui
        .into_messages()
        .filter_map(|message| match message {
            Message::Scrolled { at_end } => Some(at_end),
            _ => None,
        })
        .collect();
    assert_eq!(scrolled, [true, false]);
}

/// Why the person is asked about a shell command in tests, where the Sandbox is off.
const HOST_NOTE: &str =
    "The Sandbox is unavailable, so this runs on the host: the Sandbox is turned off";

/// Returns the call of `bash` with `command` as it appears in the transcript once the question
/// about it reached `decision`, and whether the call `failed`.
fn asked_bash(command: &str, decision: Decision, failed: bool) -> Block {
    let input = serde_json::to_string(&json!({ "command": command })).unwrap();
    Block::Tool {
        call: call("bash", &input),
        failed,
        approval: Some(Approval {
            command: command.into(),
            note: Some(HOST_NOTE.into()),
            decision,
        }),
    }
}

/// Passes `reports` to `window` until the running Turn asks about a shell command.
async fn until_asked(
    window: &mut Window,
    reports: &mut (impl Stream<Item = SessionReport> + Unpin),
) {
    loop {
        let report = reports.next().await.expect("the Turn should ask");
        let asked = matches!(report, SessionReport::Approval(_));
        let _ = window.update(Message::Report("1".into(), report));
        if asked {
            return;
        }
    }
}

#[tokio::test]
async fn a_shell_command_the_sandbox_cannot_run_runs_once_the_person_allows_it() {
    let (dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("done"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    assert_eq!(window.view_ref().state(), SessionState::AwaitingApproval);

    let _ = window.update(Message::Answer(true));

    assert_eq!(window.view_ref().state(), SessionState::Working);
    let _ = until_turn_ends_on(&mut window, &mut reports).await;
    assert!(dir.path().join("ran").exists());
}

#[tokio::test]
async fn a_denied_shell_command_does_not_run_and_its_call_shows_as_failed() {
    let (dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let _ = window.update(Message::Answer(false));
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    assert!(!dir.path().join("ran").exists());
    let denied = asked_bash("touch ran", Decision::Denied, true);
    assert!(blocks(&window).contains(&denied));
}

#[tokio::test]
async fn closing_the_window_with_a_question_open_denies_the_command() {
    let (dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    drop(window);

    // If the question outlived the window, the Turn would wait for ever and nextest would stop
    // the test.
    while let Some(report) = reports.next().await {
        if matches!(report, SessionReport::TurnEnded(_)) {
            break;
        }
    }
    assert!(!dir.path().join("ran").exists());
}

#[tokio::test]
async fn the_window_asks_about_a_shell_command_with_its_note() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let mut ui = simulator(window.view());

    for shown in [HOST_NOTE, "touch ran", "Allow", "Deny"] {
        assert!(ui.find(shown).is_ok(), "{shown} should be shown");
    }
    let open = asked_bash("touch ran", Decision::Open, false);
    assert_eq!(blocks(&window).last(), Some(&open));
}

#[tokio::test]
async fn an_answered_question_shrinks_to_its_tool_line_but_is_kept_whole() {
    let command = "cat <<EOF > ran\none\ntwo\nEOF";
    let (_dir, mut window) = window(vec![Ok(bash_call(command)), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let _ = window.update(Message::Answer(true));
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    let allowed = asked_bash(command, Decision::Allowed, false);
    assert!(blocks(&window).contains(&allowed));
    let mut ui = simulator(window.view());
    let line = r#"Tool: bash {"command":"cat <<EOF > ran\none\ntwo\nEOF"} (allowed)"#;
    assert!(
        ui.find(line).is_ok(),
        "the call should show on its tool line"
    );
    for gone in [command, HOST_NOTE, "Allow", "Deny"] {
        assert!(ui.find(gone).is_err(), "{gone} should no longer be shown");
    }
}

#[tokio::test]
async fn a_question_too_long_for_the_window_leaves_the_prompt_on_screen() {
    let command = format!("cat <<EOF\n{}EOF", "a line of the heredoc\n".repeat(200));
    let (_dir, mut window) = window(vec![Ok(bash_call(&command))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let mut ui = simulator(window.view());

    let button = ui.find("Send").unwrap();
    assert!(
        button.visible_bounds().is_some(),
        "Send should be on screen"
    );
}

#[tokio::test]
async fn a_pill_shows_only_while_a_question_waits_out_of_view() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    assert!(simulator(window.view()).find(WAITING).is_err());

    let _ = window.update(Message::Scrolled { at_end: false });
    assert!(simulator(window.view()).find(WAITING).is_ok());

    let _ = window.update(Message::Answer(false));
    assert!(simulator(window.view()).find(WAITING).is_err());
}

#[tokio::test]
async fn pressing_the_pill_jumps_to_the_newest_output() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let _ = window.update(Message::Scrolled { at_end: false });

    let mut ui = simulator(window.view());
    let _ = ui.click(WAITING).unwrap();
    let pressed: Vec<_> = ui.into_messages().collect();
    assert!(matches!(pressed.as_slice(), [Message::JumpToEnd]));

    assert_eq!(window.update(Message::JumpToEnd).units(), 1);
    assert!(window.view_ref().following());
    assert!(simulator(window.view()).find(WAITING).is_err());
}

#[tokio::test]
async fn a_denied_call_says_so_on_its_line() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    let _ = window.update(Message::Answer(false));
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    let line = r#"Tool: bash {"command":"touch ran"} (denied, failed)"#;
    assert!(simulator(window.view()).find(line).is_ok());
}

#[tokio::test]
async fn pressing_allow_answers_the_question() {
    let (dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    interact(&mut window, |ui| {
        let _ = ui.click("Allow").unwrap();
    });

    assert_eq!(window.view_ref().state(), SessionState::Working);
    let _ = until_turn_ends_on(&mut window, &mut reports).await;
    assert!(dir.path().join("ran").exists());
    assert!(simulator(window.view()).find("Allow").is_err());
}

#[tokio::test]
async fn pressing_deny_answers_the_question() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("ok"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    interact(&mut window, |ui| {
        let _ = ui.click("Deny").unwrap();
    });

    assert_eq!(window.view_ref().state(), SessionState::Working);
}

#[tokio::test]
async fn interrupting_cancels_the_running_turn_and_its_question() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    assert_eq!(window.update(Message::Interrupt).units(), 0);
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    let cancelled = notice("the Turn was cancelled");
    assert_eq!(blocks(&window).last(), Some(&cancelled));
    let unanswered = asked_bash("touch ran", Decision::Unanswered, false);
    assert!(blocks(&window).contains(&unanswered));
    assert!(simulator(window.view()).find("Allow").is_err());
    assert!(!window.view_ref().state().in_turn());
}

#[tokio::test]
async fn interrupting_with_nothing_running_quits() {
    let (_dir, mut window) = window(vec![]);

    assert_eq!(window.update(Message::Interrupt).units(), 1);
}

#[tokio::test]
async fn interrupting_while_quitting_stops_quitting_and_leaves_the_turn_running() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let _ = send(&mut window, "/quit");

    let _ = window.update(Message::Interrupt);

    assert!(!window.harness.phase().is_draining());
    assert_eq!(window.view_ref().state(), SessionState::AwaitingApproval);
}

#[tokio::test]
async fn pressing_interrupt_cancels_the_running_turn() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;

    interact(&mut window, |ui| {
        let _ = ui.click("Interrupt").unwrap();
    });

    let _ = until_turn_ends_on(&mut window, &mut reports).await;
    let cancelled = notice("the Turn was cancelled");
    assert_eq!(blocks(&window).last(), Some(&cancelled));
}

#[tokio::test]
async fn the_interrupt_button_does_nothing_with_nothing_to_interrupt() {
    let (_dir, window) = window(vec![]);

    let mut ui = simulator(window.view());
    let _ = ui.click("Interrupt").unwrap();

    assert_eq!(ui.into_messages().count(), 0);
}

#[test]
fn escape_interrupts_and_other_keys_do_not() {
    let pressed = |key| keyboard::Event::KeyPressed {
        key,
        modified_key: Key::Unidentified,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    };

    let escape = pressed(Key::Named(Named::Escape));
    assert!(matches!(shortcut(&escape), Some(Message::Interrupt)));
    assert!(shortcut(&pressed(Key::Named(Named::Enter))).is_none());
}

#[tokio::test]
async fn escape_in_the_prompt_reaches_the_window() {
    let (_dir, window) = window(vec![]);
    let mut ui = simulator(window.view());
    let _ = ui.click(iced::widget::Id::new(PROMPT)).unwrap();

    // The window hears only the keys that no widget kept for itself.
    assert_eq!(ui.tap_key(Named::Escape), iced::event::Status::Ignored);
}

#[tokio::test]
async fn closing_the_window_quits_at_once_when_nothing_is_running() {
    let (_dir, mut window) = window(vec![]);

    assert_eq!(window.update(Message::CloseRequested).units(), 1);
}

#[tokio::test]
async fn closing_the_window_during_a_turn_waits_for_it_to_reach_the_end_of_its_step() {
    let (_dir, mut window) = window(vec![Ok(reply("done"))]);
    let _ = send(&mut window, "go");

    assert_eq!(window.update(Message::CloseRequested).units(), 0);
    assert!(window.harness.phase().is_draining());

    assert_eq!(until_turn_ends(&mut window).await.units(), 1);
}

#[tokio::test]
async fn closing_the_window_again_while_waiting_quits_at_once() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let _ = window.update(Message::CloseRequested);

    assert_eq!(window.update(Message::CloseRequested).units(), 1);
}

#[tokio::test]
async fn the_window_says_it_is_waiting_to_quit_until_it_stops_waiting() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let waiting = "Quitting once every running Turn reaches the end of its Step. Quit or \
                   close the window again to quit now, or interrupt to carry on.";
    assert!(simulator(window.view()).find(waiting).is_err());

    let _ = window.update(Message::CloseRequested);
    assert!(simulator(window.view()).find(waiting).is_ok());

    let _ = window.update(Message::Interrupt);
    assert!(simulator(window.view()).find(waiting).is_err());
}

#[tokio::test]
async fn closing_the_window_during_a_rebuild_stops_the_build_and_quits_with_nothing_running() {
    let (_dir, mut window) = window(vec![]);
    let building = send(&mut window, "/rebuild");

    assert_eq!(window.update(Message::CloseRequested).units(), 1);

    assert!(produced(building).await.is_empty());
    assert_eq!(relaunched(&window), None);
}

#[tokio::test]
async fn closing_the_window_during_a_rebuild_quits_instead_once_the_turn_stops() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran")), Ok(reply("later"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let building = send(&mut window, "/rebuild");

    assert_eq!(window.update(Message::CloseRequested).units(), 0);
    assert!(produced(building).await.is_empty());
    let waiting = "Quitting once every running Turn reaches the end of its Step. Quit or \
                   close the window again to quit now, or interrupt to carry on.";
    assert!(simulator(window.view()).find(waiting).is_ok());
    let _ = window.update(Message::Answer(false));
    let _ = until_turn_ends_on(&mut window, &mut reports).await;

    assert!(!window.harness.phase().is_draining());
    assert_eq!(relaunched(&window), None);
}

#[tokio::test]
async fn closing_the_window_twice_during_a_rebuild_quits_at_once() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut reports = reports(&window);
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut reports).await;
    let _ = send(&mut window, "/rebuild");
    let _ = window.update(Message::CloseRequested);

    assert_eq!(window.update(Message::CloseRequested).units(), 1);
    assert_eq!(relaunched(&window), None);
}

#[tokio::test]
async fn a_response_with_no_text_before_its_tool_calls_shows_no_empty_reply() {
    let (_dir, mut window) = window(vec![]);

    let nothing = happened(&mut window, [AgentEvent::Text(String::new())]);
    let _ = happened(&mut window, [AgentEvent::ToolStarted(call("read", "{}"))]);

    assert_eq!(nothing.units(), 0);
    assert_eq!(blocks(&window), [tool("read", "{}", false)]);
}

#[test]
fn a_web_link_is_opened_and_a_failure_to_open_it_is_told() {
    let mut opened = Vec::new();
    let mut open = |uri: &str| {
        opened.push(uri.to_owned());
        Err("no browser")
    };

    let failure = open_web_link("https://example.com", &mut open);

    assert_eq!(opened, ["https://example.com"]);
    assert_eq!(
        failure.as_deref(),
        Some("could not open https://example.com: no browser")
    );
    assert_eq!(
        open_web_link("http://example.com", |_| Ok::<_, String>(())),
        None
    );
}

#[test]
fn a_link_that_is_not_a_web_address_is_left_alone() {
    let open = |uri: &str| -> Result<(), String> { panic!("{uri} should not be opened") };

    assert_eq!(open_web_link("src/main.rs", open), None);
}

#[tokio::test]
async fn clicking_a_link_that_leads_nowhere_changes_nothing() {
    let (_dir, mut window) = window(vec![]);

    let task = window.update(Message::OpenLink("src/main.rs".into()));

    assert_eq!(task.units(), 0);
    assert_eq!(blocks(&window), []);
}

mod sessions;
mod vim;
