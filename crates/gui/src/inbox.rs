//! The way values from other tasks reach the iced app, which reads them through a
//! [`Subscription`].

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use iced::Subscription;
use iced::futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use iced::futures::{Stream, StreamExt, stream};

/// Values as they arrive from another task, waiting for the app to read them.
///
/// Clones read the same values, and only the first stream taken from them gets any. Each inbox
/// is its own subscription to iced, so replacing one with another stops reading the first.
pub struct Inbox<T> {
    /// The values not yet read, until a stream takes them. Where it sits in memory also tells
    /// iced that the subscription is the same one from one call of `subscription` to the next,
    /// so that it keeps reading rather than starting over.
    unread: Arc<Mutex<Option<UnboundedReceiver<T>>>>,
}

impl<T: Send + 'static> Inbox<T> {
    /// Creates an empty inbox, returning it with the sender that fills it.
    pub fn new() -> (Self, UnboundedSender<T>) {
        let (sender, unread) = mpsc::unbounded();
        let inbox = Self {
            unread: Arc::new(Mutex::new(Some(unread))),
        };
        (inbox, sender)
    }

    /// Returns the subscription that delivers each value to the app as it arrives.
    pub fn subscription(&self) -> Subscription<T> {
        Subscription::run_with(self.clone(), Self::stream)
    }

    /// Takes the values as a stream that ends once nothing can send any more. A stream taken
    /// after the first is empty.
    pub fn stream(&self) -> impl Stream<Item = T> + use<T> {
        let unread = self.unread.lock().ok().and_then(|mut unread| unread.take());
        stream::iter(unread).flatten()
    }
}

impl<T> Clone for Inbox<T> {
    fn clone(&self) -> Self {
        Self {
            unread: Arc::clone(&self.unread),
        }
    }
}

impl<T> Hash for Inbox<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.unread).hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::executor::block_on;

    #[test]
    fn values_arrive_in_order_on_the_first_stream_taken() {
        let (inbox, sender) = Inbox::new();
        sender.unbounded_send(1).unwrap();
        sender.unbounded_send(2).unwrap();
        drop(sender);

        let read: Vec<_> = block_on(inbox.stream().collect());
        assert_eq!(read, [1, 2]);
        let again: Vec<i32> = block_on(inbox.clone().stream().collect());
        assert_eq!(again, Vec::<i32>::new());
    }
}
