//! Tests of the window with several Sessions: switching between them, the sidebar, and the
//! Human Commands that open, archive and rename them.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use iced::keyboard::Modifiers;
use sstack::workspace::SessionSummary;

use super::*;
use crate::test_support::{Done, TestSessions, path_of, store};

/// Opens a window like [`window`], whose later Sessions come from `sessions` once `configure`
/// has set them up, returning what those Sessions did.
fn window_with(
    script: Vec<Scripted>,
    configure: impl FnOnce(&Path, &mut TestSessions),
) -> (TempDir, Window, Rc<RefCell<Done>>) {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), script, &[]);
    let mut sessions = TestSessions::new(dir.path());
    configure(dir.path(), &mut sessions);
    let done = Rc::clone(&sessions.done);
    ready.sessions = Box::new(sessions);
    (dir, Window::open(ready), done)
}

/// Returns the ids of the window's Sessions in the order the sidebar lists them.
fn listed(window: &Window) -> Vec<String> {
    let queue = window.queue();
    queue
        .iter()
        .map(|index| window.views[*index].id().to_owned())
        .collect()
}

/// Returns how the sidebar shows the Session with id `id`.
fn row_of(window: &Window, id: &str) -> sidebar::Row {
    let index = window
        .position(id)
        .expect("the window should have the Session");
    window.views[index].row(SystemTime::now())
}

/// Takes the reports of the Session with id `id`, which only the first call gets.
fn reports_of(window: &Window, id: &str) -> impl Stream<Item = SessionReport> + Unpin + use<> {
    let index = window
        .position(id)
        .expect("the window should have the Session");
    window.views[index]
        .reports()
        .expect("the Session should be open")
        .stream()
}

/// Passes `reports` of the Session with id `id` to `window` until a Turn ends there.
async fn until_turn_ends_in(
    window: &mut Window,
    id: &str,
    reports: &mut (impl Stream<Item = SessionReport> + Unpin),
) {
    loop {
        let report = reports.next().await.expect("the Turn should end");
        let ended = matches!(report, SessionReport::TurnEnded(_));
        let _ = window.update(Message::Report(id.to_owned(), report));
        if ended {
            return;
        }
    }
}

/// Describes the Session with id `id` stored in `dir`, which the person started by saying
/// "session" and its id, last changed `age` seconds before now. It is stored, blank, unless the
/// test already has.
fn summary(dir: &Path, id: &str, age: u64) -> SessionSummary {
    if !path_of(dir, id).exists() {
        store(dir, id, &[]);
    }
    SessionSummary {
        path: path_of(dir, id),
        first_prompt: Some(format!("session {id}")),
        name: None,
        modified: SystemTime::now() - Duration::from_secs(age),
        parked: false,
    }
}

/// Opens a window on Session 1 with Sessions 2, 3 and 4 stored, which have waited 30, 20 and 10
/// seconds.
fn with_stored() -> (TempDir, Window) {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    ready.stored = vec![
        summary(dir.path(), "3", 20),
        summary(dir.path(), "2", 30),
        summary(dir.path(), "4", 10),
    ];
    (dir, Window::open(ready))
}

#[tokio::test]
async fn a_new_session_opens_beside_the_others_and_is_shown() {
    let (_dir, mut window) = window(vec![]);

    let _ = send(&mut window, "/new");

    assert_eq!(window.views.len(), 2);
    assert_eq!(window.view_ref().id(), "2");
    assert_eq!(blocks(&window), []);
}

#[tokio::test]
async fn the_new_button_and_its_shortcut_open_a_session() {
    let (_dir, mut window) = window(vec![]);

    interact(&mut window, |ui| {
        let _ = ui.click("New").unwrap();
    });
    let _ = window.update(Message::NewSession);

    assert_eq!(window.view_ref().id(), "3");
}

#[tokio::test]
async fn sessions_run_side_by_side() {
    let (_dir, mut window, _) = window_with(vec![Ok(bash_call("touch ran"))], |_, sessions| {
        sessions.scripts.push_back(vec![Ok(reply("from two"))]);
    });
    let mut first = reports_of(&window, "1");
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut first).await;
    let _ = send(&mut window, "/new");

    let mut second = reports_of(&window, "2");
    let _ = send(&mut window, "hello");
    until_turn_ends_in(&mut window, "2", &mut second).await;

    assert_eq!(
        blocks(&window).last(),
        Some(&Block::Reply("from two".into()))
    );
    assert_eq!(row_of(&window, "1").activity, sidebar::Activity::Blocked);
    let _ = window.update(Message::Show("1".into()));
    assert!(simulator(window.view()).find("touch ran").is_ok());
}

#[tokio::test]
async fn a_turn_that_ends_unseen_is_marked_until_its_session_is_shown() {
    let (_dir, mut window) = window(vec![Ok(reply("done"))]);
    let mut first = reports_of(&window, "1");
    let _ = send(&mut window, "go");
    let _ = send(&mut window, "/new");

    until_turn_ends_in(&mut window, "1", &mut first).await;

    assert_eq!(row_of(&window, "1").activity, sidebar::Activity::Done);
    assert_eq!(blocks(&window), []);
    let _ = window.update(Message::Show("1".into()));
    assert_eq!(row_of(&window, "1").activity, sidebar::Activity::Idle);
    assert_eq!(blocks(&window).last(), Some(&Block::Reply("done".into())));
}

#[tokio::test]
async fn a_failed_turn_on_another_session_is_marked() {
    let (_dir, mut window) = window(vec![Err("the model is down".into())]);
    let mut first = reports_of(&window, "1");
    let _ = send(&mut window, "go");
    let _ = send(&mut window, "/new");

    until_turn_ends_in(&mut window, "1", &mut first).await;

    assert_eq!(row_of(&window, "1").activity, sidebar::Activity::Failed);
}

#[tokio::test]
async fn what_the_person_typed_stays_with_its_session() {
    let (_dir, mut window) = window(vec![]);
    for message in typed("half done") {
        let _ = window.update(message);
    }

    let _ = window.update(Message::NewSession);
    assert_eq!(window.view_ref().prompt.draft().text, "");
    let _ = window.update(Message::Show("1".into()));

    assert_eq!(window.view_ref().prompt.draft().text, "half done");
}

#[tokio::test]
async fn the_sidebar_lists_sessions_longest_wait_first() {
    let (_dir, window) = with_stored();

    assert_eq!(listed(&window), ["2", "3", "4", "1"]);
    let mut ui = simulator(window.view());
    for name in ["session 2", "session 3", "session 4", "new session"] {
        assert!(ui.find(name).is_ok(), "{name} should be listed");
    }
}

#[tokio::test]
async fn the_sidebar_draws_no_activity_words_and_cuts_long_names_short() {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    let mut long = summary(dir.path(), "2", 30);
    long.name = Some("a name far too long to fit in the sidebar".into());
    ready.stored = vec![long];
    let window = Window::open(ready);

    let mut ui = simulator(window.view());

    assert!(ui.find("a name far too long to fit i..").is_ok());
    for hidden in ["a name far too long to fit in the sidebar", "idle"] {
        assert!(ui.find(hidden).is_err(), "{hidden} should only be a hint");
    }
}

#[tokio::test]
async fn a_click_in_the_sidebar_shows_that_session_and_resumes_it() {
    let (_dir, mut window) = with_stored();
    assert!(!window.views[window.position("3").unwrap()].has_agent());

    interact(&mut window, |ui| {
        let _ = ui.click("session 3").unwrap();
    });

    assert_eq!(window.view_ref().id(), "3");
    assert!(window.view_ref().has_agent());
}

#[tokio::test]
async fn keys_move_through_the_sidebar_and_wrap() {
    let (_dir, mut window) = with_stored();

    let _ = window.update(Message::NextSession);
    assert_eq!(window.view_ref().id(), "2");
    let _ = window.update(Message::PreviousSession);
    assert_eq!(window.view_ref().id(), "1");
    let _ = window.update(Message::PreviousSession);
    assert_eq!(window.view_ref().id(), "4");
}

#[tokio::test]
async fn the_longest_wait_is_a_key_away_and_skips_the_session_on_screen() {
    let (_dir, mut window) = with_stored();

    let _ = window.update(Message::LongestWaiting);
    assert_eq!(window.view_ref().id(), "2");
    let _ = window.update(Message::LongestWaiting);
    assert_eq!(window.view_ref().id(), "3");
}

#[tokio::test]
async fn with_nothing_else_waiting_the_longest_wait_key_stays_put() {
    let (_dir, mut window) = window(vec![]);

    let _ = window.update(Message::LongestWaiting);

    assert_eq!(window.view_ref().id(), "1");
}

#[tokio::test]
async fn a_session_that_cannot_open_leaves_the_person_where_they_were() {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    ready.stored = vec![summary(dir.path(), "2", 30)];
    let mut sessions = TestSessions::new(dir.path());
    sessions.broken = true;
    ready.sessions = Box::new(sessions);
    let mut window = Window::open(ready);

    let _ = window.update(Message::Show("2".into()));
    let _ = send(&mut window, "/new");

    assert_eq!(window.view_ref().id(), "1");
    let failed = [
        notice("could not open the Session: the Session is open elsewhere"),
        notice("could not start a new Session: the Session is open elsewhere"),
    ];
    assert_eq!(blocks(&window), failed);
}

#[tokio::test]
async fn interrupting_refuses_to_quit_over_work_in_another_session() {
    let (_dir, mut window) = window(vec![Ok(bash_call("touch ran"))]);
    let mut first = reports_of(&window, "1");
    let _ = send(&mut window, "go");
    until_asked(&mut window, &mut first).await;
    let _ = send(&mut window, "/new");

    assert_eq!(window.update(Message::Interrupt).units(), 0);

    let refused = notice("other Sessions are still working, so use /quit to stop them too");
    assert_eq!(blocks(&window), [refused]);
}

#[tokio::test]
async fn reports_from_a_session_the_window_does_not_have_are_ignored() {
    let (_dir, mut window) = window(vec![]);

    let report = SessionReport::Notice("hi".into());
    let _ = window.update(Message::Report("9".into(), report));

    assert_eq!(blocks(&window), []);
}

#[tokio::test]
async fn opening_carries_on_parked_turns_in_stored_sessions() {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    let go = Entry::User { text: "go".into() };
    store(dir.path(), "2", &[go, Entry::TurnParked]);
    let mut parked = summary(dir.path(), "2", 30);
    parked.parked = true;
    ready.stored = vec![parked];
    let mut sessions = TestSessions::new(dir.path());
    sessions.scripts.push_back(vec![Ok(reply("carried on"))]);
    ready.sessions = Box::new(sessions);

    let window = Window::open(ready);

    let carried = &window.views[window.position("2").unwrap()];
    assert!(carried.state().in_turn());
    assert_eq!(window.view_ref().id(), "1");
}

#[tokio::test]
async fn a_parked_session_that_cannot_open_stays_parked_and_says_so() {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), vec![], &[]);
    let mut parked = summary(dir.path(), "2", 30);
    parked.parked = true;
    ready.stored = vec![parked];
    let mut sessions = TestSessions::new(dir.path());
    sessions.broken = true;
    ready.sessions = Box::new(sessions);

    let window = Window::open(ready);

    assert_eq!(row_of(&window, "2").activity, sidebar::Activity::Parked);
    let failed = notice("could not carry on a Session: the Session is open elsewhere");
    assert_eq!(blocks(&window), [failed]);
}

#[tokio::test]
async fn archiving_puts_the_session_away_and_moves_on_to_the_queue() {
    let (_dir, mut window, done) = window_with(vec![], |_, _| {});
    let _ = send(&mut window, "/new");

    let _ = send(&mut window, "/archive");

    assert_eq!(done.borrow().archived, ["2"]);
    assert_eq!(listed(&window), ["1"]);
    assert_eq!(window.view_ref().id(), "1");
}

#[tokio::test]
async fn archiving_the_last_session_starts_a_new_one() {
    let (_dir, mut window, done) = window_with(vec![], |_, _| {});

    let _ = send(&mut window, "/archive");

    assert_eq!(done.borrow().archived, ["1"]);
    assert_eq!(listed(&window), ["2"]);
}

#[tokio::test]
async fn a_session_with_a_turn_running_cannot_be_archived() {
    let (_dir, mut window, done) = window_with(vec![Ok(reply("busy"))], |_, _| {});
    let _ = send(&mut window, "go");

    let _ = send(&mut window, "/archive --force");

    assert_eq!(done.borrow().archived.len(), 0);
    let refused = notice("a Turn is already running in this Session");
    assert_eq!(blocks(&window).last(), Some(&refused));
}

#[tokio::test]
async fn work_that_would_be_lost_stops_archiving_unless_forced() {
    let (_dir, mut window, done) = window_with(vec![], |_, sessions| {
        sessions.unfinished = Some("the worktree has changes".into());
    });

    let _ = send(&mut window, "/archive");
    assert_eq!(done.borrow().archived.len(), 0);
    let refused = notice("not archived: the worktree has changes");
    assert_eq!(blocks(&window), [refused]);

    let _ = send(&mut window, "/archive --force");
    assert_eq!(done.borrow().archived, ["1"]);
}

#[tokio::test]
async fn pressing_archive_puts_the_session_away_and_moves_on() {
    let (_dir, mut window, done) = window_with(vec![], |_, _| {});
    let _ = send(&mut window, "/new");

    interact(&mut window, |ui| {
        let _ = ui.click("Archive").unwrap();
    });

    assert_eq!(done.borrow().archived, ["2"]);
    assert_eq!(listed(&window), ["1"]);
}

#[tokio::test]
async fn the_archive_button_does_nothing_while_a_turn_runs() {
    let (_dir, mut window) = window(vec![Ok(reply("busy"))]);
    let _ = send(&mut window, "go");

    let mut ui = simulator(window.view());
    let _ = ui.click("Archive").unwrap();

    assert_eq!(ui.into_messages().count(), 0);
}

#[tokio::test]
async fn being_unable_to_move_on_keeps_the_session_on_screen() {
    let (_dir, mut window, done) = window_with(vec![], |_, sessions| sessions.broken = true);

    let _ = send(&mut window, "/archive");

    assert_eq!(done.borrow().archived.len(), 0);
    assert_eq!(window.view_ref().id(), "1");
}

#[tokio::test]
async fn renaming_lists_the_session_by_its_new_name_until_it_is_taken_away() {
    let (_dir, mut window) = window(vec![Ok(reply("hi"))]);
    let _ = send(&mut window, "hello");
    assert_eq!(row_of(&window, "1").name, "hello");

    let _ = send(&mut window, "/rename the plan");
    assert_eq!(row_of(&window, "1").name, "the plan");
    let _ = send(&mut window, "/rename");
    assert_eq!(row_of(&window, "1").name, "hello");
}

#[tokio::test]
async fn a_name_that_cannot_be_stored_changes_nothing() {
    let (_dir, mut window) = window(vec![]);

    let _ = send(&mut window, "/rename unstorable");

    assert_eq!(row_of(&window, "1").name, "new session");
    let refused = notice("not renamed: the disk is full");
    assert_eq!(blocks(&window), [refused]);
}

#[tokio::test]
async fn a_rebuild_relaunches_into_the_session_on_screen() {
    let (dir, mut window) = building_with(built_at_once, vec![]);
    let _ = send(&mut window, "/new");

    let built = produced(send(&mut window, "/rebuild")).await;
    for message in built {
        let _ = window.update(message);
    }

    let relaunch = window.relaunch.get().expect("the window should relaunch");
    assert_eq!(relaunch.session, path_of(dir.path(), "2"));
}

/// Returns the key `character` as pressed.
fn character(character: &str) -> keyboard::Key {
    Key::Character(character.into())
}

#[test]
fn shortcuts_are_pressed_with_the_command_key() {
    let command = Modifiers::COMMAND;
    let shortcuts = [
        (Key::Named(Named::Enter), "Submit"),
        (character("n"), "NewSession"),
        (character("]"), "NextSession"),
        (character("["), "PreviousSession"),
        (character("g"), "LongestWaiting"),
    ];
    for (key, message) in shortcuts {
        let found = command_shortcut(&key, command).map(|m| format!("{m:?}"));
        assert_eq!(found.as_deref(), Some(message));
        assert!(command_shortcut(&key, Modifiers::empty()).is_none());
    }
    assert!(command_shortcut(&character("q"), command).is_none());
    assert!(command_shortcut(&Key::Named(Named::Tab), command).is_none());
}

#[tokio::test]
async fn a_shortcut_pressed_in_the_prompt_reaches_the_window() {
    let (_dir, window) = window(vec![]);
    let mut ui = simulator(window.view());
    let _ = ui.click(iced::widget::Id::new(PROMPT)).unwrap();

    let _ = ui.simulate([iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: character("n"),
        modified_key: character("n"),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: Modifiers::COMMAND,
        text: Some("n".into()),
        repeat: false,
    })]);

    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages.iter().any(|m| matches!(m, Message::NewSession)),
        "{messages:?}"
    );
    assert_eq!(window.view_ref().prompt.draft().text, "");
}
