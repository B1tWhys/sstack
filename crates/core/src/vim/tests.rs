//! Tests of [`Vim`](super::Vim). Most drive a real nvim, because how nvim behaves is the main risk.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;

use super::*;

/// The longest a test waits for nvim before failing.
const PATIENCE: Duration = Duration::from_secs(10);

/// Writes an executable shell script standing in for nvim, for failures a real one will not
/// produce on demand.
fn fake_nvim(directory: &Path, script: &str) -> PathBuf {
    let path = directory.join("fake-nvim");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
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
    match timeout(PATIENCE, events.recv()).await.unwrap() {
        Some(VimEvent::Lost { draft, failure }) => (draft, failure),
        other => panic!("expected to lose nvim, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_program_counts_as_not_installed() {
    let directory = tempfile::tempdir().unwrap();
    let (draft, failure) = lost_running(directory.path().join("nope"), PATIENCE).await;
    assert_eq!(failure, VimFailure::NotInstalled);
    assert_eq!(draft.text, "kept");
}

#[tokio::test]
async fn a_program_that_cannot_run_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let (_, failure) = lost_running(directory.path().to_owned(), PATIENCE).await;
    assert!(matches!(failure, VimFailure::Unreachable(_)), "{failure:?}");
}

#[tokio::test]
async fn exiting_before_ready_fails_rather_than_respawning_forever() {
    let directory = tempfile::tempdir().unwrap();
    let program = fake_nvim(directory.path(), "exit 0");
    let (_, failure) = lost_running(program, PATIENCE).await;
    assert!(matches!(failure, VimFailure::Stopped(_)), "{failure:?}");
}

#[tokio::test]
async fn hanging_while_starting_times_out() {
    let directory = tempfile::tempdir().unwrap();
    let program = fake_nvim(directory.path(), "sleep 60");
    let (_, failure) = lost_running(program, Duration::from_millis(50)).await;
    assert_eq!(failure, VimFailure::TimedOut);
}

#[tokio::test]
async fn an_error_during_setup_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    // An array of [2, "nvim_error_event", [0, "bad"]], which is what nvim sends when a script fails.
    let bytes = r"\223\002\260nvim_error_event\222\000\243bad";
    let program = fake_nvim(directory.path(), &format!("printf '{bytes}'; sleep 60"));
    let (_, failure) = lost_running(program, PATIENCE).await;
    assert!(matches!(failure, VimFailure::Rejected(_)), "{failure:?}");
}

/// Drives a real nvim, ignoring the person's config.
#[cfg(feature = "nvim-tests")]
mod real {
    use super::*;

    /// Runs a [`Vim`] and collects what it reports.
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
            let event = timeout(PATIENCE, self.events.recv()).await;
            let event = event.expect("nvim went quiet").expect("nvim was dropped");
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
        vim.typed("é\n日本").await;
        let state = vim.press("gg", |state| state.cursor == 0).await;
        assert_eq!(state.text, "é\n日本");
        let state = vim.press("j$", |state| state.cursor > 0).await;
        assert_eq!(state.cursor, "é\n日".len());
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
    async fn charwise_selection_is_reported_in_bytes() {
        let mut vim = Harness::new().await;
        vim.typed("aé日b").await;
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
