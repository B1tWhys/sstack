//! Tests of [`Vim`](super::Vim). Most drive a real nvim, because how nvim behaves is the main risk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::*;

/// How long nvim gets to start in tests about other failures. The failures take milliseconds, plus
/// the first run of a script in a new checkout, which macOS scans first. This is shorter than
/// nextest lets a test run, so a failure that never comes shows up as [`VimFailure::TimedOut`]
/// rather than as the test being stopped.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

/// Returns the checked-in script `name` in `fake-nvim/`, which stands in for nvim to produce a
/// failure a real one will not produce on demand. The scripts are checked in rather than written
/// by each test, because macOS scans every new executable on its first run, so a script written
/// by the test would be scanned on every run.
fn fake_nvim(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/vim/fake-nvim")
        .join(name)
}

/// Waits for the [`VimEvent::Lost`] from a [`Vim`] running `program`.
async fn lost_running(program: PathBuf, ready_timeout: Duration) -> (Draft, VimFailure) {
    let (sender, mut events) = unbounded_channel();
    let _vim = Vim::builder()
        .on_event(move |event| drop(sender.send(event)))
        .draft(Draft {
            text: "kept".into(),
            cursor: 4,
        })
        .program(program)
        .ready_timeout(ready_timeout)
        .start();
    match events.recv().await {
        Some(VimEvent::Lost { draft, failure }) => (draft, failure),
        other => panic!("expected to lose nvim, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_program_counts_as_not_installed() {
    let directory = tempfile::tempdir().unwrap();
    let (draft, failure) = lost_running(directory.path().join("nope"), READY_TIMEOUT).await;
    assert_eq!(failure, VimFailure::NotInstalled);
    assert_eq!(draft.text, "kept");
}

#[tokio::test]
async fn a_program_that_cannot_run_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let (_, failure) = lost_running(directory.path().to_owned(), READY_TIMEOUT).await;
    assert!(matches!(failure, VimFailure::Unreachable(_)), "{failure:?}");
}

#[tokio::test]
async fn exiting_before_ready_fails_rather_than_respawning_forever() {
    let (_, failure) = lost_running(fake_nvim("exits.sh"), READY_TIMEOUT).await;
    assert!(matches!(failure, VimFailure::Stopped(_)), "{failure:?}");
}

#[tokio::test]
async fn hanging_while_starting_times_out() {
    let program = fake_nvim("hangs.sh");
    let (_, failure) = lost_running(program, Duration::from_millis(50)).await;
    assert_eq!(failure, VimFailure::TimedOut);
}

#[tokio::test]
async fn an_error_during_setup_is_reported() {
    let (_, failure) = lost_running(fake_nvim("fails-setup.sh"), READY_TIMEOUT).await;
    assert!(matches!(failure, VimFailure::Rejected(_)), "{failure:?}");
}

/// Tests that drive a real nvim, ignoring the person's config.
#[cfg(feature = "nvim-tests")]
mod real {
    use super::*;

    /// A [`Vim`] under test, with everything it reports collected.
    struct Harness {
        /// The nvim being driven.
        vim: Vim,
        /// What it has reported.
        events: UnboundedReceiver<VimEvent>,
        /// The latest state seen.
        state: Option<VimState>,
    }

    impl Harness {
        /// Starts on `draft`, once nvim has settled into Insert mode.
        async fn starting_with(draft: Draft) -> Self {
            let (sender, events) = unbounded_channel();
            let vim = Vim::builder()
                .on_event(move |event| drop(sender.send(event)))
                .draft(draft)
                .clean(true)
                .start();
            let mut harness = Self {
                vim,
                events,
                state: None,
            };
            harness.until(|state| state.mode == VimMode::Insert).await;
            harness
        }

        /// Starts with an empty buffer.
        async fn new() -> Self {
            Self::starting_with(Draft::default()).await
        }

        /// Returns the next report.
        async fn next(&mut self) -> VimEvent {
            let event = self.events.recv().await.expect("nvim was dropped");
            if let VimEvent::State(state) = &event {
                self.state = Some(state.clone());
            }
            event
        }

        /// Waits for a state that passes `check`, which may be the latest one already seen.
        async fn until(&mut self, check: impl Fn(&VimState) -> bool) -> VimState {
            loop {
                match &self.state {
                    Some(state) if check(state) => return state.clone(),
                    _ => {
                        if let VimEvent::Lost { failure, .. } = self.next().await {
                            panic!("lost nvim: {failure}");
                        }
                    }
                }
            }
        }

        /// Sends `keys` and waits for a state that passes `check`.
        async fn press(&mut self, keys: &str, check: impl Fn(&VimState) -> bool) -> VimState {
            self.vim.send_keys(keys);
            self.until(check).await
        }

        /// Types `text` in Insert mode and goes back to Normal mode.
        async fn typed(&mut self, text: &str) -> VimState {
            self.vim.send_keys(&text.replace('\n', "<CR>"));
            self.until(|state| state.text == text).await;
            self.press("<Esc>", |state| state.mode == VimMode::Normal)
                .await
        }

        /// Submits and returns the text.
        async fn submitted(&mut self) -> String {
            self.vim.submit();
            self.until_submitted().await
        }

        /// Waits for the text to be handed over, however that was asked for, and returns it.
        async fn until_submitted(&mut self) -> String {
            loop {
                if let VimEvent::Submitted(text) = self.next().await {
                    return text;
                }
            }
        }
    }

    #[tokio::test]
    async fn starts_in_insert_mode_and_reports_typing() {
        let mut vim = Harness::new().await;
        let state = vim.press("hello", |state| state.text == "hello").await;
        assert_eq!(state.cursor, 5);
        assert_eq!(state.mode, VimMode::Insert);
    }

    #[tokio::test]
    async fn real_motions_and_operators_work() {
        let mut vim = Harness::new().await;
        vim.typed("one two three").await;
        let state = vim.press("0wdw", |state| state.text == "one three").await;
        assert_eq!(state.cursor, 4);
    }

    #[tokio::test]
    async fn the_cursor_is_a_byte_offset_across_lines_and_multibyte_text() {
        let mut vim = Harness::new().await;
        vim.typed("\u{e9}\n\u{65e5}\u{672c}").await;
        let state = vim.press("gg", |state| state.cursor == 0).await;
        assert_eq!(state.text, "\u{e9}\n\u{65e5}\u{672c}");
        let state = vim.press("j$", |state| state.cursor > 0).await;
        assert_eq!(state.cursor, "\u{e9}\n\u{65e5}".len());
    }

    #[tokio::test]
    async fn a_key_nvim_waits_on_does_not_block_what_follows() {
        let mut vim = Harness::new().await;
        vim.typed("abc").await;
        vim.press("0", |state| state.cursor == 0).await;
        vim.vim.send_keys("f");
        vim.vim.resize(10, 40);
        vim.press("c", |state| state.cursor == 2).await;
    }

    #[tokio::test]
    async fn submitting_hands_over_the_text_and_starts_again_in_insert_mode() {
        let mut vim = Harness::new().await;
        vim.press("hello", |state| state.text == "hello").await;
        assert_eq!(vim.submitted().await, "hello");
        let state = vim
            .until(|state| state.text.is_empty() && state.mode == VimMode::Insert)
            .await;
        assert_eq!(state.cursor, 0);
    }

    #[tokio::test]
    async fn submitting_comes_after_every_key_sent_before_it() {
        let mut vim = Harness::new().await;
        vim.vim.send_keys("one two");
        vim.vim.send_keys(" three");
        assert_eq!(vim.submitted().await, "one two three");
    }

    #[tokio::test]
    async fn submitting_cancels_a_key_nvim_was_waiting_on() {
        let mut vim = Harness::new().await;
        vim.typed("abc").await;
        vim.vim.send_keys("f");
        assert_eq!(vim.submitted().await, "abc");
    }

    #[tokio::test]
    async fn enter_in_normal_mode_submits() {
        let mut vim = Harness::new().await;
        vim.typed("one\ntwo").await;
        vim.vim.send_keys("<CR>");
        assert_eq!(vim.until_submitted().await, "one\ntwo");
        vim.until(|state| state.text.is_empty() && state.mode == VimMode::Insert)
            .await;
    }

    #[tokio::test]
    async fn enter_typed_right_after_leaving_insert_mode_submits() {
        let mut vim = Harness::new().await;
        vim.vim.send_keys("hello<Esc><CR>");
        assert_eq!(vim.until_submitted().await, "hello");
    }

    #[tokio::test]
    async fn undo_brings_back_what_was_submitted() {
        let mut vim = Harness::new().await;
        vim.press("hello", |state| state.text == "hello").await;
        vim.submitted().await;
        vim.until(|state| state.text.is_empty() && state.mode == VimMode::Insert)
            .await;
        vim.press("<Esc>u", |state| state.text == "hello").await;
    }

    #[tokio::test]
    async fn a_draft_is_loaded_at_its_cursor_and_undo_cannot_remove_it() {
        let draft = Draft {
            text: "one\ntwo".into(),
            cursor: 5,
        };
        let mut vim = Harness::starting_with(draft).await;
        let state = vim.press("X", |state| state.text == "one\ntXwo").await;
        assert_eq!(state.cursor, 6);
        vim.press("<Esc>u", |state| state.text == "one\ntwo").await;
        vim.press("uuA!", |state| state.text == "one\ntwo!").await;
    }

    #[tokio::test]
    async fn a_draft_with_the_cursor_at_the_end_continues_typing_there() {
        let draft = Draft {
            text: "ab".into(),
            cursor: 2,
        };
        let mut vim = Harness::starting_with(draft).await;
        vim.press("c", |state| state.text == "abc").await;
    }

    #[tokio::test]
    async fn pasting_inserts_everything_in_one_piece() {
        let mut vim = Harness::new().await;
        vim.vim.paste("one\ntwo");
        vim.until(|state| state.text == "one\ntwo").await;
    }

    #[tokio::test]
    async fn resizing_makes_screen_motions_follow_the_new_width() {
        let mut vim = Harness::new().await;
        vim.vim.resize(10, 20);
        // A paste waits behind the resize, whereas typed keys could overtake it.
        let long = "x".repeat(30);
        vim.vim.paste(&long);
        vim.until(|state| state.text == long).await;
        vim.press("<Esc>0", |state| state.cursor == 0).await;
        vim.press("gj", |state| state.cursor == 20).await;
    }

    #[tokio::test]
    async fn line_numbers_follow_the_options_of_the_moment() {
        let mut vim = Harness::new().await;
        vim.typed("a\nb\nc").await;
        let state = vim.press("k", |state| state.cursor == 2).await;
        assert_eq!(state.line_numbers, Vec::<String>::new());
        let shows =
            |labels: [&'static str; 3]| move |state: &VimState| state.line_numbers == labels;
        vim.press(":set number<CR>", shows(["  1 ", "  2 ", "  3 "]))
            .await;
        vim.press(":set relativenumber<CR>", shows(["  1 ", "2   ", "  1 "]))
            .await;
        vim.press(":set nonumber<CR>", shows(["  1 ", "  0 ", "  1 "]))
            .await;
        vim.press("k", shows(["  0 ", "  1 ", "  2 "])).await;
    }

    #[tokio::test]
    async fn line_numbers_grow_with_the_number_column() {
        let mut vim = Harness::new().await;
        vim.press("<Esc>:set number<CR>", |state| {
            state.line_numbers.len() == 1
        })
        .await;
        vim.press("999o<Esc>", |state| state.line_numbers.len() == 1000)
            .await;
        let state = vim.until(|state| state.line_numbers[0].len() == 5).await;
        assert_eq!(state.line_numbers[999], "1000 ");
    }

    #[tokio::test]
    async fn the_number_column_comes_out_of_the_width_lines_wrap_at() {
        let mut vim = Harness::new().await;
        vim.press("<Esc>:set number<CR>", |state| {
            !state.line_numbers.is_empty()
        })
        .await;
        vim.vim.resize(10, 24);
        vim.vim.paste(&"x".repeat(30));
        vim.until(|state| state.text.len() == 30).await;
        vim.press("<Esc>0", |state| state.cursor == 0).await;
        vim.press("gj", |state| state.cursor == 20).await;
    }

    #[tokio::test]
    async fn charwise_selection_is_reported_in_bytes() {
        let mut vim = Harness::new().await;
        vim.typed("a\u{e9}\u{65e5}b").await;
        let state = vim
            .press("0lvl", |state| {
                state.mode == VimMode::Visual && state.cursor == 3
            })
            .await;
        assert_eq!(state.selection, vec![1..6]);
    }

    #[tokio::test]
    async fn linewise_selection_has_one_range_per_line() {
        let mut vim = Harness::new().await;
        vim.typed("ab\n\ncd").await;
        let state = vim.press("ggVG", |state| state.selection.len() == 2).await;
        assert_eq!(state.selection, [0..2, 4..6]);
        let state = vim
            .press("<Esc>", |state| state.mode == VimMode::Normal)
            .await;
        assert_eq!(state.selection, Vec::<Range<usize>>::new());
    }

    #[tokio::test]
    async fn the_command_line_is_reported_while_typing_and_cleared_after() {
        let mut vim = Harness::new().await;
        let state = vim
            .press("<Esc>:ec", |state| {
                state
                    .cmdline
                    .as_ref()
                    .is_some_and(|line| line.text == ":ec")
            })
            .await;
        assert_eq!(state.mode, VimMode::Command);
        assert_eq!(state.cmdline.unwrap().cursor, 3);
        vim.press("<Esc>", |state| state.cmdline.is_none()).await;
    }

    #[tokio::test]
    async fn an_error_shows_until_the_next_key() {
        let mut vim = Harness::new().await;
        vim.press("<Esc>:nosuchcommand<CR>", |state| {
            state
                .message
                .as_ref()
                .is_some_and(|text| text.contains("E492"))
        })
        .await;
        vim.press("h", |state| state.message.is_none()).await;
    }

    #[tokio::test]
    async fn quitting_starts_a_new_nvim_with_the_same_text() {
        let mut vim = Harness::new().await;
        vim.typed("keep me").await;
        vim.vim.send_keys(":q!<CR>");
        let state = vim
            .until(|state| state.text == "keep me" && state.mode == VimMode::Insert)
            .await;
        assert_eq!(state.cmdline, None);
        vim.press("!", |state| state.text == "keep m!e").await;
    }

    #[tokio::test]
    async fn a_crash_is_lost_with_the_text_so_far() {
        let mut vim = Harness::new().await;
        vim.typed("keep me").await;
        vim.vim.send_keys(":cq<CR>");
        loop {
            if let VimEvent::Lost { draft, failure } = vim.next().await {
                assert_eq!(draft.text, "keep me");
                assert!(matches!(failure, VimFailure::Stopped(_)), "{failure:?}");
                return;
            }
        }
    }

    #[tokio::test]
    async fn the_config_can_tell_it_is_running_inside_sstack() {
        let mut vim = Harness::new().await;
        let keys = "<Esc>:call setline(1, string(g:sstack))<CR>";
        vim.press(keys, |state| state.text == "1").await;
    }
}
