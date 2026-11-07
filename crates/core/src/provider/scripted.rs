//! A Provider that replays canned responses, for tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{ModelRequest, Provider, ProviderError, StreamEvent};
use crate::session::{AssistantEntry, Entry};

/// One canned response: its text is streamed before the response is returned.
pub type Scripted = Result<AssistantEntry, String>;

/// Stands in for a real Provider, so tests can script responses and inspect requests.
#[derive(Debug, Default)]
pub struct ScriptedProvider {
    /// The responses not yet returned.
    script: Mutex<VecDeque<Scripted>>,
    /// The Session history each request carried, oldest request first.
    requests: Mutex<Vec<Vec<Entry>>>,
}

impl ScriptedProvider {
    /// Creates a Provider that returns `script` in order.
    #[must_use]
    pub fn new(script: impl IntoIterator<Item = Scripted>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    /// Returns the entries seen by each request so far.
    ///
    /// # Panics
    /// Panics if a previous request panicked while recording.
    #[must_use]
    pub fn requests(&self) -> Vec<Vec<Entry>> {
        self.requests.lock().unwrap().clone()
    }

    /// Takes the next canned response, or an error once the script runs out.
    fn next(&self) -> Scripted {
        let next = self.script.lock().unwrap().pop_front();
        next.unwrap_or_else(|| Err("script exhausted".to_owned()))
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn respond(
        &self,
        request: ModelRequest<'_>,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<AssistantEntry, ProviderError> {
        self.requests.lock().unwrap().push(request.entries.to_vec());
        let response = self.next().map_err(ProviderError)?;
        on_event(StreamEvent::Text(response.text.clone()));
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{StopReason, Usage};

    fn request(entries: &[Entry]) -> ModelRequest<'_> {
        ModelRequest {
            model: "scripted",
            system: "",
            entries,
            tools: &[],
        }
    }

    fn reply(text: &str) -> AssistantEntry {
        AssistantEntry {
            text: text.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        }
    }

    #[tokio::test]
    async fn replays_its_script_in_order_then_fails() {
        let provider = ScriptedProvider::new([Ok(reply("one")), Err("boom".to_owned())]);
        let entries = [Entry::User { text: "hi".into() }];
        let mut events = Vec::new();

        let first = provider
            .respond(request(&entries), &mut |event| events.push(event))
            .await
            .unwrap();
        let second = provider.respond(request(&entries), &mut |_| {}).await;
        let third = provider.respond(request(&entries), &mut |_| {}).await;

        assert_eq!(first, reply("one"));
        assert_eq!(events, [StreamEvent::Text("one".into())]);
        assert_eq!(second.unwrap_err().to_string(), "boom");
        assert_eq!(third.unwrap_err().to_string(), "script exhausted");
        assert_eq!(provider.requests().len(), 3);
    }
}
