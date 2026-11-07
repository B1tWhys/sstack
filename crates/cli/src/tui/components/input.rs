//! The one place on screen where the person types, however they have chosen to.

use crossterm::cursor::SetCursorStyle;
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use sstack_core::vim::{Draft, VimEvent};

use super::prompt::Prompt;
use super::vim_prompt::VimPrompt;
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Event, SessionSender};

/// Lets a Session switch how the person types without the rest of the screen noticing, and falls
/// back to the plain prompt by itself if nvim is lost.
#[derive(Debug)]
pub enum Input {
    /// The plain prompt, with shell-style editing keys.
    Plain(Prompt),
    /// A real nvim.
    Vim(VimPrompt),
}

impl Default for Input {
    fn default() -> Self {
        Self::Plain(Prompt::default())
    }
}

impl Input {
    /// Starts typing into a real nvim for the Session reporting to `events`, keeping what is typed.
    pub fn start_vim(&mut self, events: &SessionSender) {
        if let Self::Plain(prompt) = self {
            *self = Self::Vim(VimPrompt::start(prompt.take_draft(), events.clone()));
        }
    }

    /// Switches between the two ways of typing, keeping what is typed.
    pub fn toggle(&mut self, events: &SessionSender) {
        match self {
            Self::Plain(_) => self.start_vim(events),
            Self::Vim(vim) => *self = Self::Plain(Prompt::with_draft(&vim.draft())),
        }
    }

    /// Returns how many rows the input needs, border included, when it is `width` columns wide.
    pub fn height(&self, width: u16) -> u16 {
        match self {
            Self::Plain(prompt) => prompt.height(width),
            Self::Vim(vim) => vim.height(width),
        }
    }

    /// Returns the cursor shape the input calls for.
    pub fn cursor_style(&self) -> SetCursorStyle {
        match self {
            Self::Plain(_) => SetCursorStyle::DefaultUserShape,
            Self::Vim(vim) => vim.cursor_style(),
        }
    }

    /// Returns the way of typing in use, as a component.
    fn current(&self) -> &dyn Component {
        match self {
            Self::Plain(prompt) => prompt,
            Self::Vim(vim) => vim,
        }
    }

    /// Returns the way of typing in use, as a component that can change.
    fn current_mut(&mut self) -> &mut dyn Component {
        match self {
            Self::Plain(prompt) => prompt,
            Self::Vim(vim) => vim,
        }
    }

    /// Falls back to the plain prompt starting from `draft`. A plain prompt stays as it is,
    /// because that report comes from an nvim the person already switched away from.
    fn fall_back(&mut self, draft: &Draft) {
        if matches!(self, Self::Vim(_)) {
            *self = Self::Plain(Prompt::with_draft(draft));
        }
    }
}

impl Component for Input {
    fn update(&mut self, event: &Event) {
        if let Event::Vim(VimEvent::Lost { draft, .. }) = event {
            self.fall_back(draft);
        }
        self.current_mut().update(event);
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        self.current_mut().key(key)
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        self.current().render(frame, area);
    }
}
