//! Tests of typing into a real nvim in the window: switching to it and back, entering what it
//! submits, losing it, and the keys that still reach the window.

use iced::Size;
use iced::keyboard::Modifiers;
use sstack::vim::{Draft, VimFailure};

use super::*;

/// Opens a window like [`window`], in which the person types into a real nvim.
fn vim_window(script: Vec<Scripted>) -> (TempDir, Window) {
    let dir = TempDir::new().unwrap();
    let mut ready = readied(dir.path(), script, &[]);
    ready.first.run.vim = true;
    (dir, Window::open(ready))
}

/// Passes on to `window` what the nvim of the Session with id 1 reports, as the window's
/// subscription would.
fn vim_reports(window: &mut Window, event: VimEvent) -> Task<Message> {
    window.update(Message::Vim("1".into(), event))
}

/// Returns a press of `key` with `modifiers` held.
fn press(key: Key, modifiers: Modifiers) -> keyboard::Event {
    keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    }
}

#[tokio::test]
async fn a_session_starts_in_vim_if_the_person_wants_it() {
    let (_dir, window) = vim_window(vec![]);
    assert!(matches!(window.view_ref().prompt, Input::Vim(_)));
    let (_dir, window) = super::window(vec![]);
    assert!(matches!(window.view_ref().prompt, Input::Plain(_)));
}

#[tokio::test]
async fn the_vim_command_switches_how_the_person_types_and_back() {
    let (_dir, mut window) = window(vec![]);

    let _ = send(&mut window, "/vim");
    assert!(matches!(window.view_ref().prompt, Input::Vim(_)));
    let _ = vim_reports(&mut window, VimEvent::Submitted("/vim".into()));

    assert!(matches!(window.view_ref().prompt, Input::Plain(_)));
    assert_eq!(blocks(&window), []);
}

#[tokio::test]
async fn a_line_submitted_through_nvim_starts_a_turn() {
    let (_dir, mut window) = vim_window(vec![Ok(reply("hi there"))]);

    let _ = vim_reports(&mut window, VimEvent::Submitted(" hello\n".into()));

    assert_eq!(blocks(&window), [Block::Prompt("hello".into())]);
}

#[tokio::test]
async fn a_line_submitted_through_nvim_during_a_turn_is_refused() {
    let (_dir, mut window) = vim_window(vec![Ok(reply("one"))]);
    let _ = vim_reports(&mut window, VimEvent::Submitted("first".into()));

    let _ = vim_reports(&mut window, VimEvent::Submitted("second".into()));

    let refused = notice("a Turn is already running in this Session");
    assert_eq!(blocks(&window).last(), Some(&refused));
}

#[tokio::test]
async fn sending_from_nvim_waits_for_nvim_to_hand_over_its_text() {
    let (_dir, mut window) = vim_window(vec![]);

    let _ = window.update(Message::Submit);

    assert_eq!(blocks(&window), []);
}

#[tokio::test]
async fn losing_nvim_falls_back_to_the_plain_prompt_with_the_text_so_far() {
    let (_dir, mut window) = vim_window(vec![]);
    let draft = Draft {
        text: "one\ntwo".into(),
        cursor: 5,
    };

    let failure = VimFailure::TimedOut;
    let _ = vim_reports(&mut window, VimEvent::Lost { draft, failure });

    assert!(matches!(window.view_ref().prompt, Input::Plain(_)));
    assert_eq!(window.view_ref().prompt.draft().text, "one\ntwo");
    let lost =
        notice("typing into the plain prompt: nvim took too long to start. /vim tries again");
    assert_eq!(blocks(&window), [lost]);
}

#[tokio::test]
async fn a_report_from_an_nvim_already_left_behind_changes_nothing() {
    let (_dir, mut window) = window(vec![]);

    let failure = VimFailure::TimedOut;
    let draft = Draft::default();
    let _ = vim_reports(&mut window, VimEvent::Lost { draft, failure });
    let _ = vim_reports(&mut window, VimEvent::Submitted("hello".into()));
    let _ = window.update(Message::Vim(
        "gone".into(),
        VimEvent::Submitted("hi".into()),
    ));

    assert_eq!(blocks(&window), []);
}

#[tokio::test]
async fn alt_c_interrupts_while_typing_into_nvim_and_escape_does_not() {
    let (_dir, mut window) = vim_window(vec![]);

    // With nothing running, interrupting quits.
    let escape = press(Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(window.update(Message::Key(escape)).units(), 0);
    let alt_c = press(Key::Character("c".into()), Modifiers::ALT);
    assert_eq!(window.update(Message::Key(alt_c)).units(), 1);
}

#[tokio::test]
async fn the_windows_shortcuts_work_while_typing_into_nvim() {
    let (_dir, mut window) = vim_window(vec![]);

    let new = press(Key::Character("n".into()), Modifiers::COMMAND);
    let _ = window.update(Message::Key(new));

    assert_eq!(window.view_ref().id(), "2");
}

#[tokio::test]
async fn command_v_pastes_into_nvim_from_the_clipboard() {
    let (_dir, mut window) = vim_window(vec![]);

    let paste = press(Key::Character("v".into()), Modifiers::LOGO);

    assert_eq!(window.update(Message::Key(paste)).units(), 1);
}

#[tokio::test]
async fn pastes_and_layouts_meant_for_nvim_leave_the_plain_prompt_alone() {
    let (_dir, mut window) = window(vec![]);
    for typed in typed("kept") {
        let _ = window.update(typed);
    }

    let _ = window.update(Message::Pasted(Some("pasted".into())));
    let _ = window.update(Message::PromptResized(Size::new(400.0, 30.0)));

    assert_eq!(window.view_ref().prompt.draft().text, "kept");
}

#[tokio::test]
async fn the_window_draws_the_prompt_nvim_edits() {
    let (_dir, window) = vim_window(vec![]);

    let mut ui = simulator(window.view());

    assert!(ui.find("INSERT").is_ok());
}

/// Passes on to `window` what its nvim reports until `wanted` is among it.
#[cfg(feature = "nvim-tests")]
async fn until_vim(
    window: &mut Window,
    events: &mut (impl Stream<Item = VimEvent> + Unpin),
    wanted: impl Fn(&VimEvent) -> bool,
) {
    loop {
        let event = events.next().await.expect("nvim should keep reporting");
        let found = wanted(&event);
        let _ = vim_reports(window, event);
        if found {
            return;
        }
    }
}

#[cfg(feature = "nvim-tests")]
#[tokio::test]
async fn a_prompt_typed_into_a_real_nvim_starts_a_turn() {
    let (_dir, mut window) = vim_window(vec![Ok(reply("hi there"))]);
    let Input::Vim(vim) = &window.view_ref().prompt else {
        panic!("the window should start in vim");
    };
    let mut events = vim.events();
    for key in ["h", "i"] {
        let typed = press(Key::Character(key.into()), Modifiers::empty());
        let _ = window.update(Message::Key(typed));
    }
    let typed = |event: &VimEvent| matches!(event, VimEvent::State(state) if state.text == "hi");
    until_vim(&mut window, &mut events, typed).await;
    let _ = window.update(Message::PromptResized(Size::new(400.0, 30.0)));
    let _ = window.update(Message::Pasted(Some(" there".into())));
    let pasted =
        |event: &VimEvent| matches!(event, VimEvent::State(state) if state.text == "hi there");
    until_vim(&mut window, &mut events, pasted).await;

    let submit = press(Key::Named(Named::Enter), Modifiers::COMMAND);
    let _ = window.update(Message::Key(submit));
    let submitted = |event: &VimEvent| matches!(event, VimEvent::Submitted(_));
    until_vim(&mut window, &mut events, submitted).await;

    assert_eq!(blocks(&window), [Block::Prompt("hi there".into())]);
}

/// Draws `window` frame after frame as the running window does, keeping what its widgets
/// remember between frames, which a [`Simulator`] forgets.
struct Frames {
    /// What the widgets remember from the last frame.
    cache: runtime::user_interface::Cache,
    /// Draws the frames, off screen.
    renderer: iced_test::renderer::Renderer,
}

impl Frames {
    /// Starts with nothing drawn yet.
    fn new() -> Self {
        use iced::advanced::renderer::Headless;
        let renderer = iced::futures::executor::block_on(iced_test::renderer::Renderer::new(
            iced::Font::with_name("Fira Sans"),
            iced::Pixels(16.0),
            None,
        ))
        .expect("a headless renderer should start");
        Self {
            cache: runtime::user_interface::Cache::default(),
            renderer,
        }
    }

    /// Draws one frame of `window`, returning the messages its widgets produced.
    fn draw(&mut self, window: &Window) -> Vec<Message> {
        let mut ui = runtime::UserInterface::build(
            window.view(),
            Size::new(1024.0, 768.0),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let redraw = iced::Event::Window(iced::window::Event::RedrawRequested(
            iced::time::Instant::now(),
        ));
        let mut messages = Vec::new();
        let _ = ui.update(
            &[redraw],
            mouse::Cursor::Unavailable,
            &mut self.renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        self.cache = ui.into_cache();
        messages
    }
}

#[tokio::test]
async fn the_prompt_of_a_new_session_learns_its_width() {
    let (_dir, mut window) = vim_window(vec![]);
    let mut frames = Frames::new();
    let first = frames.draw(&window);
    assert!(
        matches!(first[..], [Message::PromptResized(_)]),
        "{first:?}"
    );
    for message in first {
        let _ = window.update(message);
    }

    let _ = window.update(Message::NewSession);
    // Sessions a test opens never start in nvim, which outside tests they do if the person wants.
    window.view_mut().prompt.start_vim();
    let messages = frames.draw(&window);

    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Message::PromptResized(_))),
        "the new prompt should be told how wide it is"
    );
}
