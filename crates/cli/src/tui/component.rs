//! The contract every part of the screen meets, so the App can treat them all alike.

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use super::event::{Action, Event};

/// One self-contained part of the screen, which keeps its own state up to date from events.
///
/// Components never call each other or the agent. They learn what happened through
/// [`Component::update`], and ask for anything beyond themselves by returning an [`Action`] from
/// [`Component::key`].
pub trait Component {
    /// Keeps the component's picture of the Session current. Every component sees every event
    /// except key presses, which go to [`Component::key`] instead.
    fn update(&mut self, _event: &Event) {}

    /// Offers a key press. The App offers it to components from the topmost down and stops at the
    /// first that does not return [`KeyOutcome::Ignored`].
    fn key(&mut self, _key: KeyEvent) -> KeyOutcome {
        KeyOutcome::Ignored
    }

    /// Draws the component into `area`, which the App chooses.
    fn render(&self, frame: &mut Frame, area: Rect);
}

/// What a component did with a key press.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The key was not for this component, so the next one down is offered it.
    Ignored,
    /// The component used the key, and nothing else needs to happen.
    Handled,
    /// The component used the key and asks the App to act on it.
    Act(Action),
}
