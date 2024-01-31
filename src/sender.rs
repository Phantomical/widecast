use std::sync::atomic::Ordering;

use triomphe::Arc;

use crate::{ArcCache, Receiver, Shared};

/// Sending-half of the broadcast channel.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
    cache: ArcCache<T>,
}

impl<T> Sender<T> {
    /// Create a new sender with the requested capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared::with_capacity(capacity)),
            cache: ArcCache::new(),
        }
    }

    /// Create a new [`Receiver`] handle that will receive values sent after
    /// this call to `subscribe`.
    pub fn subscribe(&self) -> Receiver<T> {
        Receiver::new(self.shared.clone())
    }

    /// Send a value through the broadcast channel.
    ///
    /// This will notify all receivers that are currently awaiting in [`recv`].
    ///
    /// If you have multiple messages that are available at the same time it is
    /// much more efficient to call [`send_bulk`] instead of calling `send` in a
    /// loop.
    ///
    /// [`recv`]: Receiver::recv
    /// [`send_bulk`]: Sender::send_bulk
    pub fn send(&mut self, value: T) {
        self.shared.buffer.push(value, &mut self.cache);
        self.shared.notify.notify_all();
    }

    /// Send multiple values into the broadcast channel.
    ///
    /// This is more efficient than [`send`] since it will only notify awaiting
    /// [`Receiver`]s after writing all the contained items to the broadcast
    /// channel. With large numbers of receivers notifying all of them can be
    /// **much** more expensive than adding items to the channel buffer.
    ///
    /// [`send`]: Sender::send
    pub fn send_bulk<I: IntoIterator<Item = T>>(&mut self, items: I) {
        self.shared
            .buffer
            .push_bulk(items.into_iter(), &mut self.cache);
        self.shared.notify.notify_all();
    }

    /// Returns the number of active receivers.
    pub fn receiver_count(&self) -> usize {
        Arc::count(&self.shared) - 1
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.notify.notify_all();
    }
}
