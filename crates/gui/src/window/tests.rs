use iced::futures::StreamExt;
use iced_test::{Simulator, simulator};
use sstack::provider::scripted::Scripted;
use sstack::session::Entry;
use tempfile::TempDir;

use super::*;
use crate::test_support::{readied, reply, typed};

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
/// after the last.
async fn until_turn_ends(window: &mut Window) -> Task<Message> {
    let mut reports = window.reports.stream();
    loop {
        let report = reports.next().await.expect("the Turn should end");
        let ended = matches!(report, SessionReport::TurnEnded(_));
        let task = window.update(Message::Report(report));
        if ended {
            return task;
        }
    }
}

/// Returns a notice as it appears in the transcript.
fn notice(text: &str) -> Block {
    Block::Notice(text.to_owned())
}

#[tokio::test]
async fn a_prompt_runs_a_turn_and_shows_the_reply() {
    let (_dir, mut window) = window(vec![Ok(reply("hi there"))]);

    let _ = send(&mut window, " hello\n");
    assert_eq!(window.prompt.text(), "");
    let _ = until_turn_ends(&mut window).await;

    let said = [
        Block::Prompt("hello".into()),
        Block::Reply("hi there".into()),
    ];
    assert_eq!(window.transcript, said);
    assert!(!window.session.state().in_turn());
}

#[tokio::test]
async fn a_prompt_sent_during_a_turn_is_refused_and_kept() {
    let (_dir, mut window) = window(vec![Ok(reply("one"))]);
    let _ = send(&mut window, "first");

    let _ = send(&mut window, "second");

    assert_eq!(window.prompt.text(), "second");
    let refused = notice("a Turn is already running in this Session");
    assert_eq!(window.transcript.last(), Some(&refused));
}

#[tokio::test]
async fn a_failed_turn_says_why() {
    let (_dir, mut window) = window(vec![Err("the model is down".into())]);
    let _ = send(&mut window, "hello");
    let _ = until_turn_ends(&mut window).await;

    let failed = notice("the Turn failed: the model is down");
    assert_eq!(window.transcript.last(), Some(&failed));
}

#[tokio::test]
async fn human_commands_about_other_sessions_are_not_available_yet() {
    let (_dir, mut window) = window(vec![]);

    let _ = send(&mut window, "/new");

    let missing = notice("/new is not available in the GUI yet");
    assert_eq!(window.transcript, [missing]);
    assert_eq!(window.prompt.text(), "");
}

#[tokio::test]
async fn a_rebuild_is_not_available_yet_and_work_carries_on() {
    let (_dir, mut window) = window(vec![Ok(reply("fine"))]);

    let _ = send(&mut window, "/rebuild");
    assert!(!window.harness.phase().is_draining());
    let _ = send(&mut window, "still here?");
    let _ = until_turn_ends(&mut window).await;

    let said = [
        notice("Rebuild is not available in the GUI yet"),
        Block::Prompt("still here?".into()),
        Block::Reply("fine".into()),
    ];
    assert_eq!(window.transcript, said);
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
    assert_eq!(window.transcript.last(), Some(&refused));

    assert_eq!(until_turn_ends(&mut window).await.units(), 1);
}

#[tokio::test]
async fn opening_tells_the_person_about_the_setup_and_names_the_window() {
    let dir = TempDir::new().unwrap();
    let mut run = readied(dir.path(), vec![], &[]);
    run.notices = vec!["working in the main checkout".into()];
    run.descriptions_left_out = 3;

    let window = Window::open(run);

    let warning = "3 Skill descriptions were left out of the system prompt, so the model may \
                   not think to load those Skills";
    let notices = [notice("working in the main checkout"), notice(warning)];
    assert_eq!(window.transcript, notices);
    assert_eq!(window.title(), "sstack 1");
}

#[tokio::test]
async fn opening_carries_on_a_parked_turn() {
    let dir = TempDir::new().unwrap();
    let go = Entry::User { text: "go".into() };
    let parked = [go, Entry::TurnParked];
    let mut window = Window::open(readied(dir.path(), vec![Ok(reply("carried on"))], &parked));

    assert!(window.session.state().in_turn());
    let _ = until_turn_ends(&mut window).await;
    assert_eq!(window.transcript, [Block::Reply("carried on".into())]);
}

#[test]
fn every_refusal_is_worded() {
    let worded = [
        (Refusal::Busy, "a Turn is already running in this Session"),
        (
            Refusal::Draining,
            "sstack is quitting, so nothing new can start",
        ),
        (Refusal::OthersWorking, "other Sessions are still working"),
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

    for shown in ["> hello", "hi there", "Send"] {
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
    assert_eq!(window.prompt.text(), "hello");
    interact(&mut window, |ui| {
        let _ = ui.click("Send").unwrap();
    });

    assert_eq!(window.transcript, [Block::Prompt("hello".into())]);
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
