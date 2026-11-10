//! Prompts the person sends while a Turn is running, which wait until the agent can take them.

use std::sync::{Arc, Mutex, PoisonError};

/// What separates one Queued Prompt from the next when they reach the model together.
const SEPARATOR: &str = "\n\n";

/// The Queued Prompts of one Session, shared by the interface that adds them and the agent that
/// takes them at the end of a Step.
///
/// Clones share the same prompts. Whichever side takes them first gets them all, so a prompt is
/// never sent twice or lost between the two.
#[derive(Debug, Clone, Default)]
pub struct QueuedPrompts(Arc<Mutex<Vec<String>>>);

impl QueuedPrompts {
    /// Adds `text` after any prompts already waiting.
    pub fn push(&self, text: String) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text);
    }

    /// Takes every waiting prompt, joined into one message in the order they were sent, or
    /// returns `None` if none is waiting.
    #[must_use]
    pub fn take(&self) -> Option<String> {
        let taken = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        (!taken.is_empty()).then(|| taken.join(SEPARATOR))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_taken_together_in_order_and_only_once() {
        let queued = QueuedPrompts::default();
        let other_side = queued.clone();
        queued.push("one".into());
        queued.push("two".into());

        assert_eq!(other_side.take().as_deref(), Some("one\n\ntwo"));
        assert_eq!(queued.take(), None);
    }
}
